"""The can check reads classic CAN as a receiving node does (stuff, form and CRC errors, the ACK slot, error and
overload frames, resynchronization). The lines are made by an encoder written here, apart from the reader
(the CRC from the polynomial division written out bit by bit, the stuffing from the bits as sent)."""

import numpy as np
import pytest

from wireskein._engine.model import Capture, Channel
from wireskein.runlog import can
from wireskein.verify import check_can

RATE = 20e6


def crc15(bits):
    reg = [0] * 15                     # x^15 + x^14 + x^10 + x^8 + x^7 + x^4 + x^3 + 1, shifted bit by bit
    taps = [14, 10, 8, 7, 4, 3, 0]
    for b in bits:
        fb = b ^ reg[0]
        reg = reg[1:] + [0]
        if fb:
            for t in taps:
                reg[14 - t] ^= 1
    return int("".join(map(str, reg)), 2)


def bits_of(v, n):
    return [(v >> (n - 1 - i)) & 1 for i in range(n)]


def frame_bits(ident, data=b"", ext=None, rtr=False, dlc=None, ack=True, bad_crc=False, no_stuff_at=None):
    """The bits on the bus of one frame, SOF to the end of frame, and where the stuffed part ends."""
    ext = ident > 0x7FF if ext is None else ext
    dlc = len(data) if dlc is None else dlc
    if ext:
        head = [0] + bits_of(ident >> 18, 11) + [1, 1] + bits_of(ident & 0x3FFFF, 18) + [int(rtr), 0, 0]
    else:
        head = [0] + bits_of(ident, 11) + [int(rtr), 0, 0]
    body = head + bits_of(dlc, 4) + ([] if rtr else [b for x in data for b in bits_of(x, 8)])
    crc = crc15(body) ^ (1 if bad_crc else 0)
    body += bits_of(crc, 15)
    out, run, last, k = [], 0, None, 0
    for b in body:
        out.append(b)
        run = run + 1 if b == last else 1
        last = b
        if run == 5:
            k += 1
            if k == no_stuff_at:
                run = 5                                  # this stuff bit left out
                continue
            out.append(1 - b)
            last, run = 1 - b, 1
    return out + [1, 0 if ack else 1, 1] + [1] * 7


class Bus:
    def __init__(self, bitrate, fall_delay=0.0, rise_delay=0.0):
        self.bt = 1 / bitrate
        self.segs = [(1, 30 * self.bt)]
        self.fd, self.rd = fall_delay, rise_delay

    def bits(self, bits):
        self.segs += [(b, self.bt) for b in bits]
        return self

    def frame(self, *a, ifs=3, **k):
        return self.bits(frame_bits(*a, **k) + [1] * ifs)

    def error_flag(self, at_bit, frame, dominant=6, delimiter=8, ifs=3):
        """A frame cut at at_bit by an error flag, its delimiter and the intermission."""
        return self.bits(frame[:at_bit] + [0] * dominant + [1] * (delimiter + ifs))

    def line(self, tail=30):
        t, edges, lvl = 0.0, [], 1
        for level, d in self.segs + [(1, tail * self.bt)]:
            if level != lvl:
                edges.append(int(round((t + (self.fd if level == 0 else self.rd)) * RATE)))
                lvl = level
            t += d
        return Capture(RATE, int(round(t * RATE)), [Channel("CAN", 1, np.array(edges, np.int64))])


FRAMES = [(0x123, b"\x11\x22\x33"), (0x7FF, b""), (0x1ABCDE0, b"\x00\x00\x00\x00\xff"), (0x000, b"\x00" * 8),
          (0x555, b"\xaa\x55"), (0x1FFFFFFF, b"\xff" * 8)]
WANT = [{"id": i, "data": d.hex()} for i, d in FRAMES]


def bus_of(bitrate=500e3, **k):
    b = Bus(bitrate, **k)
    for i, d in FRAMES:
        b.frame(i, d)
    return b


@pytest.mark.parametrize("bitrate,k", [(500e3, {}), (1e6, {}), (125e3, {}), (500e3 * 1.008, {}),
                                       (500e3, {"fall_delay": 60e-9, "rise_delay": 250e-9})])
def test_frames_and_the_bit_rate_are_read(bitrate, k):
    ok, got, why = check_can(bus_of(bitrate, **k).line(), can("CAN", bitrate, frames=WANT))
    assert ok is True, (why, got)
    assert abs(got["bitrate_error"]) < 1e-3 and got["frame_count"] == 6
    assert [f["ext"] for f in got["frames"]] == [False, False, True, False, False, True]


def test_the_rate_off_its_tolerance_is_ng_and_near_it_a_notice():
    cap = bus_of(500e3 * 1.008).line()
    ok, got, why = check_can(cap, can("CAN", 500e3, frames=WANT))
    assert ok is True and [n["kind"] for n in got["notices"]] == ["bitrate_near_limit"], (why, got)
    ok, _, why = check_can(cap, can("CAN", 500e3, bitrate_rel=0.005))
    assert ok is False and "% off" in why, why


def test_a_remote_frame_and_a_dlc_above_8():
    b = Bus(500e3).frame(0x321, rtr=True, dlc=4).frame(0x10, b"\x01" * 8, dlc=15)
    ok, got, why = check_can(b.line(), can("CAN", 500e3, frames=[{"id": 0x321, "rtr": True}, {"id": 0x10, "data": "01" * 8}]))
    assert ok is True, (why, got)
    assert [f["dlc"] for f in got["frames"]] == [4, 15]


def test_ack():
    b = Bus(500e3).frame(0x123, b"\x01", ack=False).frame(0x124, b"\x02")
    ok, got, why = check_can(b.line(), can("CAN", 500e3))
    assert ok is False and "1 of 2 frames not acknowledged" in why
    assert check_can(b.line(), can("CAN", 500e3, ack=None))[0] is True
    tx = Bus(500e3).frame(0x123, b"\x01", ack=False).line()        # a controller's TX alone
    assert check_can(tx, can("CAN", 500e3, ack=False))[0] is True


def test_a_missing_stuff_bit_is_a_stuff_error_and_the_reading_gets_in_step_after_the_error_frame():
    bad = frame_bits(0x000, b"\x00" * 8, no_stuff_at=2)
    b = Bus(500e3).frame(0x111, b"\x01")
    b.error_flag(30, bad)                                          # the transmitter's flag after its error
    b.frame(0x000, b"\x00" * 8).frame(0x222, b"\x02")              # retransmitted, then the next
    ok, got, why = check_can(b.line(), can("CAN", 500e3))
    assert ok is False and "1 stuff, 0 form, 0 crc errors, not 0, 0, 0" in why, why
    # six dominant bits or more twice: where the stuff bit is missing, and the error flag
    assert got["stuff_errors"] == 1 and got["frame_count"] == 3 and got["error_flags"] == 2, got
    ok, got, why = check_can(b.line(), can("CAN", 500e3, errors={"stuff": 1},
                                           frames=[{"id": 0x111, "data": "01"}, {"id": 0, "data": "00" * 8},
                                                   {"id": 0x222, "data": "02"}]))
    assert ok is True, (why, got)


def test_an_error_flag_mid_frame_reads_as_a_stuff_error():
    f = frame_bits(0x2AA, b"\x5a\xa5")
    b = Bus(500e3).error_flag(20, f).frame(0x2AA, b"\x5a\xa5")
    ok, got, why = check_can(b.line(), can("CAN", 500e3, errors={"stuff": 1}, frames=[{"id": 0x2AA, "data": "5aa5"}]))
    assert ok is True, (why, got)


def test_a_bad_crc_and_a_dominant_delimiter():
    b = Bus(500e3).frame(0x100, b"\x10\x20", bad_crc=True)
    ok, got, why = check_can(b.line(), can("CAN", 500e3))
    assert ok is False and got["crc_errors"] == 1, got
    f = frame_bits(0x101, b"\x10")
    k = len(f) - 8                                                # the ACK delimiter
    f[k] = 0
    b = Bus(500e3).bits(f + [1] * 14).frame(0x102, b"\x11")
    ok, got, why = check_can(b.line(), can("CAN", 500e3, errors={"form": 1}, frames=[{"id": 0x102, "data": "11"}]))
    assert ok is True and got["errors_at"][0]["field"] == "ack_delimiter", (why, got)


def test_an_overload_frame_is_a_notice():
    f = frame_bits(0x0F0, b"\x01")
    b = Bus(500e3).bits(f[:-1] + [0] * 6 + [1] * 11).frame(0x0F1, b"\x02")   # dominant at the last EOF bit
    ok, got, why = check_can(b.line(), can("CAN", 500e3, frames=[{"id": 0x0F0, "data": "01"}, {"id": 0x0F1, "data": "02"}]))
    assert ok is True and [n["kind"] for n in got["notices"]] == ["overload"], (why, got)


def test_a_window_cut_inside_a_frame_is_no_error():
    cap = bus_of().line()
    e = cap.channels[0].edges
    mid = int(e[len(e) // 2])
    from wireskein.verify import _window
    ok, got, why = check_can(_window(cap, mid, cap.n_samples), can("CAN", 500e3))
    assert ok is True and got["frame_count"] >= 1 and got["cut_at_end"] is False, (why, got)
    ok, got, why = check_can(_window(cap, 0, mid), can("CAN", 500e3))
    assert ok is True and got["cut_at_end"] is True, (why, got)


def test_measure_only():
    ok, got, why = check_can(bus_of(250e3).line(), can("CAN", None))
    assert ok == "measured" and abs(got["bitrate"] / 250e3 - 1) < 1e-3 and got["frame_count"] == 6, (why, got)


def test_unknown_error_kind():
    ok, _, why = check_can(bus_of().line(), can("CAN", 500e3, errors={"bit": 1}))
    assert ok is False and "unknown kind bit" in why


def test_random_frames_with_jitter_and_skew():
    rng = np.random.default_rng(7)
    for _ in range(20):
        bitrate = float(rng.choice([125e3, 250e3, 500e3, 1e6])) * (1 + rng.uniform(-0.004, 0.004))
        b = Bus(bitrate, fall_delay=rng.uniform(0, 0.1) / bitrate, rise_delay=rng.uniform(0, 0.2) / bitrate)
        want = []
        for _ in range(8):
            ext = bool(rng.integers(2))
            ident = int(rng.integers(1 << (29 if ext else 11)))
            data = bytes(rng.integers(0, 256, int(rng.integers(0, 9))).astype(np.uint8))
            b.frame(ident, data, ext=ext, ifs=int(rng.integers(3, 20)))
            want.append({"id": ident, "ext": ext, "data": data.hex()})
        ok, got, why = check_can(b.line(), can("CAN", bitrate, frames=want, bitrate_rel=0.002))
        assert ok is True, (bitrate, why)


# ---- the truth from hardware: an OEP probe's ESP-IDF TWAI driver (esp_twai_onchip), its TX and RX on one pad,
# captured at 20 MS/s (tests/data/can_truth; each .truth.json has the setup, what was sent and the driver's state) ----

TRUTH = __import__("pathlib").Path(__file__).parent / "data" / "can_truth"


def _truth(name):
    import json
    from wireskein._engine import fileio
    (cap,) = fileio.load_runs(TRUTH / f"{name}.wireskein")
    t = json.loads((TRUTH / f"{name}.truth.json").read_text())
    assert t["interface"].startswith("io.github.open-embedded-probe.bench.twai")
    sent = [{"id": s["id"], "ext": s["ext"], "rtr": s["rtr"], "data": s["data"]} for s in t["steps"] if s["step"] == "send"]
    return cap, t["setup"]["nodes"][0]["timing"]["bitrate"], sent


@pytest.mark.parametrize("name", ["can_B_self_test", "can_C_125k", "can_C_250k", "can_C_1M"])
def test_hardware_self_test_frames(name):
    cap, bitrate, sent = _truth(name)
    # self-test: the node needs no ACK and sends each frame once, so the line has them unacknowledged
    ok, got, why = check_can(cap, can("bus", bitrate, frames=sent, ack=False, bitrate_rel=0.001))
    assert ok is True, (why, got)


def test_hardware_no_ack_retries():
    cap, bitrate, sent = _truth("can_B_no_ack")
    # no other node: each ACK error, while error active, an active error flag from the ACK delimiter on (a form
    # error there); after 16 of them (TEC 128) the node is error passive, its flags recessive: the frames then
    # read whole and unacknowledged, sent again and again until stopped
    ok, got, why = check_can(cap, can("bus", bitrate, ack=False, errors={"form": 16}))
    assert ok is True, (why, got)
    assert {e["field"] for e in got["errors_at"]} == {"ack_delimiter"}
    assert got["frame_count"] == 41 and all(f["id"] == sent[0]["id"] and f["data"] == "0102" for f in got["frames"])
    assert got["error_flags"] == 16
