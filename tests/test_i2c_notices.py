"""The i2c check's notices: scl_stretch (SCL held low longer than its usual low) and bus_glitch (a pulse shorter
than the spikes I2C inputs suppress). The lines are made by an encoder written here, apart from the decoder."""

import numpy as np

from wireskein._engine.model import Capture, Channel
from wireskein.runlog import i2c
from wireskein.verify import _notices, check_i2c

RATE = 50e6
F = 100e3
Q = 1 / F / 4


def lines(addr, data, stretch_at=None, stretch_s=0.0, spike=None):
    """A write of data to addr, every byte ACKed: (SCL, SDA) as lists of change times, both starting high.
    stretch_at: the bit number whose SCL low is longer by stretch_s; spike: (line, time, width_s)."""
    scl, sda = [], []
    lv = {"scl": 1, "sda": 1}
    ev = []

    def put(name, t, v):
        if lv[name] != v:
            ev.append((t, name))
            lv[name] = v

    t = 20e-6
    put("sda", t, 0)                                    # START
    put("scl", t + Q, 0)
    t += Q
    bits = []
    for b in [addr << 1] + list(data):
        bits += [(b >> k) & 1 for k in range(7, -1, -1)] + [0]
    for i, v in enumerate(bits):
        extra = stretch_s if i == stretch_at else 0.0
        put("sda", t + Q, v)
        put("scl", t + 2 * Q + extra, 1)
        put("scl", t + 4 * Q + extra, 0)
        t += 4 * Q + extra
    put("sda", t + Q, 0)                                # STOP
    put("scl", t + 2 * Q, 1)
    put("sda", t + 3 * Q, 1)
    t += 4 * Q + 20e-6
    if spike:
        name, at, w = spike
        ev += [(at, name), (at + w, name)]
    for tt, name in sorted(ev):
        (scl if name == "scl" else sda).append(int(round(tt * RATE)))
    e = lambda xs: np.array(sorted(xs), np.int64)
    return Capture(RATE, int(round(t * RATE)), [Channel("SCL", 1, e(scl)), Channel("SDA", 1, e(sda))])


WANT = [{"addr": 0x42, "rw": "write", "bytes": [0x10, 0x20, 0x30]}]


def run(cap, **k):
    x = i2c("SCL", "SDA", WANT, **k)
    ok, got, why = check_i2c(cap, x)
    ok, why, found = _notices(ok, why, got, x, cap)
    return ok, got, why, found


def test_a_plain_write_has_no_notices():
    ok, got, why, found = run(lines(0x42, [0x10, 0x20, 0x30]))
    assert ok is True and found == [], (why, got, found)


def test_a_clock_stretch_after_the_address_ack():
    cap = lines(0x42, [0x10, 0x20, 0x30], stretch_at=9, stretch_s=40e-6)
    ok, got, why, found = run(cap)
    assert ok is True and [n["kind"] for n in found] == ["scl_stretch"], (why, found)
    n = found[0]
    assert abs(n["length_ticks"] / RATE - (2 * Q + 40e-6)) < 1e-7 and abs(n["usual_ticks"] / RATE - 2 * Q) < 1e-7
    ok, _, why, _ = run(cap, notices={"scl_stretch": "ng"})
    assert ok is False and "scl_stretch" in why


def test_a_spike_on_sda_while_scl_low_is_a_glitch_and_still_reads():
    # 30 ns on SDA in the middle of a low SCL: inputs suppress it, the bytes read the same
    t_bit3 = 20e-6 + Q + 3 * 4 * Q + 0.5 * Q
    cap = lines(0x42, [0x10, 0x20, 0x30], spike=("sda", t_bit3, 30e-9))
    ok, got, why, found = run(cap)
    assert ok is True and [(n["kind"], n["line"]) for n in found] == [("bus_glitch", "sda")], (why, found)
    assert found[0]["width_ticks"] <= 2



def test_sda_changing_in_the_same_sample_as_scl_is_told():
    # the first data bit's SDA change put on the very sample SCL rises (a controller whose setup is under a sample):
    # SDA reads as moving while SCL is high, and the reason says why nothing matches
    cap = lines(0x42, [0x10, 0x20, 0x30])
    scl, sda = cap.channels
    e = sda.edges.copy()
    k = 1                                                # e[0] is START; e[1] the first data change
    rise = int(scl.edges[np.searchsorted(scl.edges, e[k])])
    e[k] = rise
    bad = Capture(cap.rate, cap.n_samples, [scl, Channel("SDA", 1, e)])
    ok, got, why, found = run(bad)
    same = [n for n in found if n["kind"] == "same_sample"]
    assert same and same[0]["tick"] == rise and same[0]["scl"] == "rise" and got["same_sample"] == 1
    assert ok is False and "same sample as SCL rose" in why
    ok, got, _, _ = run(cap)                             # the clean line: none
    assert ok and got["same_sample"] == 0
