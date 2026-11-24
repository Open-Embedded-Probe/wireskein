"""S/PDIF: biphase mark subframes with their B / M / W preambles, 24-bit samples, V U C and the even parity.
The encoder written here."""
import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen

PRE = {"B": [1, 1, 1, 0, 1, 0, 0, 0], "M": [1, 1, 1, 0, 0, 0, 1, 0], "W": [1, 1, 1, 0, 0, 1, 0, 0]}


def subframe_halves(pre, value, last):
    """The subframe as half cells' levels, the preamble's pattern inverted after a cell ending low."""
    p = PRE[pre] if last == 0 else [1 - x for x in PRE[pre]]
    halves = list(p)
    bits = [(value >> k) & 1 for k in range(24)] + [0, 0, 0]
    bits.append(sum(bits) & 1)
    lvl = halves[-1]
    for b in bits:
        lvl ^= 1
        halves.append(lvl)
        if b:
            lvl ^= 1
        halves.append(lvl)
    return halves


@pytest.mark.parametrize("rate", [24e6, 16e6])
def test_samples(rate):
    rng = np.random.default_rng(int(rate))
    w = gen.Wave(0)
    t, half = 20e-6, 1 / (48000 * 128)
    want = []
    lvl = 0
    for f in range(40):
        for ch in ("A", "B"):
            pre = ("B" if f % 192 == 0 else "M") if ch == "A" else "W"
            v = int(rng.integers(0, 1 << 24))
            want.append((pre, v))
            for x in subframe_halves(pre, v, lvl):
                w.set(t, x)
                t += half
            lvl = x
    cap = gen.quantize({"SPDIF": w}, rate, t + 10e-6, rng)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "spdif"
    got = [(x["preamble"], x["audio"]) for x in c["result"]["subframes"] if x["ok"]]
    # all of them in order but maybe the last (no edge closes it)
    k = want.index(got[0])
    assert got == want[k:k + len(got)] and len(got) >= len(want) - 1


# ---- the check of a test (runlog.spdif): subframes in order, audio, channel status, rate ----

def spdif_line(frames, status=0, flip=None, rate=24e6, fs=48000):
    """A line of `frames` (left, right) at fs, channel status `status` in the C bits of each block's first 32
    frames on both channels, the subframe at index `flip` with its parity broken."""
    w = gen.Wave(0)
    t, half = 20e-6, 1 / (fs * 128)
    lvl, k = 0, 0
    for f, pair in enumerate(frames):
        for ch, v in enumerate(pair):
            pre = ("B" if f % 192 == 0 else "M") if ch == 0 else "W"
            c = (status >> (f % 192)) & 1 if f % 192 < 32 else 0
            p = PRE[pre] if lvl == 0 else [1 - x for x in PRE[pre]]
            halves = list(p)
            bits = [(v >> b) & 1 for b in range(24)] + [0, 0, c]
            bits.append((sum(bits) & 1) ^ (1 if k == flip else 0))
            x = halves[-1]
            for b in bits:
                x ^= 1
                halves.append(x)
                if b:
                    x ^= 1
                halves.append(x)
            for x in halves:
                w.set(t, x)
                t += half
            lvl = x
            k += 1
    return gen.quantize({"SPDIF": w}, rate, t + 10e-6, np.random.default_rng(0))


def judge(cap, x):
    from wireskein.verify import check_spdif
    return check_spdif(cap, x)


def test_a_test_checks_the_order_the_audio_and_the_status():
    from wireskein.runlog import spdif
    cap = spdif_line([(0x123456, 0x654321)] * 400, status=0x02000004)
    ok, got, why = judge(cap, spdif("SPDIF", audio={"A": 0x123456, "B": 0x654321}, status=0x02000004, fs_hz=48000))
    assert ok is True, (why, got)
    assert got["blocks"] == 3 and got["errors"] == 0 and abs(got["fs_hz"] / 48000 - 1) < 0.01
    ok, _, why = judge(cap, spdif("SPDIF", audio={"A": 0x123456, "B": 0x654320}))
    assert ok is False and "channel B" in why
    ok, _, why = judge(cap, spdif("SPDIF", status=0x02000000))
    assert ok is False and "channel status 0x02000004" in why
    ok, _, why = judge(cap, spdif("SPDIF", fs_hz=44100))
    assert ok is False and "frame rate" in why


def test_a_parity_failing_is_an_error_within_errors_max():
    from wireskein.runlog import spdif
    cap = spdif_line([(1, 2)] * 200, flip=101)
    ok, got, why = judge(cap, spdif("SPDIF", audio={"A": 1}))
    assert ok is False and got["parity_errors"] == 1 and "parity" in why
    ok, _, why = judge(cap, spdif("SPDIF", audio={"A": 1}, errors_max=1))
    assert ok is True, why


def test_values_in_a_row_and_measured_alone():
    from wireskein.runlog import spdif
    from wireskein.verify import MEASURED
    frames = [(i, -i & 0xFFFFFF) for i in range(300)]
    cap = spdif_line(frames)
    ok, _, why = judge(cap, spdif("SPDIF", audio={"A": [10, 11, 12, 13]}))
    assert ok is True, why
    ok, _, _ = judge(cap, spdif("SPDIF", audio={"A": [10, 12]}))
    assert ok is False
    ok, got, _ = judge(cap, spdif("SPDIF"))
    assert ok is MEASURED and got["subframes"] >= 598


def test_through_a_run(tmp_path):
    from wireskein import verify
    from wireskein.runlog import Recorder, spdif
    cap = spdif_line([(0x0A0B0C, 0x0D0E0F)] * 250, status=0x02000004)
    lv = cap.channels[0].level_at(np.arange(cap.n_samples)).astype(np.uint8)
    rec = Recorder(tmp_path)
    with rec.section(1, "audio out", expect=[spdif("SPDIF", audio={"A": 0x0A0B0C, "B": 0x0D0E0F}, status=0x02000004)]):
        rec.capture(rec.armed(), 24_000_000, interleaved=lv.tobytes(), names=["SPDIF"])
    rec.close()
    (r,) = verify.verify(tmp_path)["results"]
    assert r["status"] == "ok", (r["reason"], r["measured"])


def test_stretches_at_two_rates_with_silence_between():
    # a capture of two transmissions, 48 kHz then 44.1 kHz, the line quiet between (a test's sections): each read at
    # its own half cell (one estimated for the whole line read the second as nothing)
    from wireskein._engine import spdif
    w = gen.Wave(0)
    t = gen.spdif(w, 20e-6, [(0x5a3c96, 0x0f1e2d)] * 300, fs=48000)
    t = gen.spdif(w, t + 2e-3, [(0x123456, 0x654321)] * 300, fs=44100)
    cap = gen.quantize({"SPDIF": w}, 50e6, t + 20e-6, np.random.default_rng(1))
    sf = spdif.subframes(np.asarray(cap.channels[0].edges, np.int64))
    vals = [x["audio"] for x in sf if x["ok"]]
    assert vals.count(0x5a3c96) >= 299 and vals.count(0x654321) >= 299, len(sf)
    ex = decode.export(decode.analyze(cap), cap)
    (c,) = [c for c in ex["claims"] if c["protocol"] == "spdif"]
    assert c["verdict"] in ("confirmed", "likely") and len(c["result"]["subframes"]) >= 1196
