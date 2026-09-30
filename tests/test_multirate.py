"""Channels at different rates in one capture: a slow channel keeps only the
samples it has, and the checks give the same verdicts as on the same signal
repeated to the fast rate, with the slow channel's resolution."""

import numpy as np
import pytest

from wireskein import wsc
from wireskein._engine import wscio
from wireskein.runlog import Recorder, square, uart
from wireskein.verify import check_square, check_uart, verify

TICK = 100_000_000
STEP = 32
N = 1_000_000                     # 10 ms of ticks


def signals():
    t = np.arange(N)
    fast = ((t % 100) < 25).astype(np.uint8)                 # 1 MHz, duty 0.25, every tick
    k = np.arange(N // STEP)                                 # the slow channel's own samples
    pwm = (((k * STEP) % 100_000) < 30_000).astype(np.uint8) # 1 kHz, duty 0.3, sampled every 32 ticks
    return fast, pwm


def uart_samples(n, u, payload=b"hello, wsc!"):
    """A UART at u samples per bit, one sample per element, blocks with idle gaps."""
    out = []
    while len(out) < n:
        for b in payload:
            for bit in [0] + [(b >> i) & 1 for i in range(8)] + [1]:
                out += [bit] * u
        out += [1] * 20 * u
    return np.array(out[:n], np.uint8)


def write(path, fast, slow, held=False):
    chans = [wsc.Channel("FAST", wsc.pack(fast.tobytes()), len(fast))]
    if held:
        chans.append(wsc.Channel("SLOW", wsc.pack(np.repeat(slow, STEP)[:N].tobytes()), N))
    else:
        chans.append(wsc.Channel("SLOW", wsc.pack(slow.tobytes()), len(slow), step=STEP))
    wsc.write(path, TICK, chans)
    return wscio.load(path)


def test_slow_channel_keeps_only_its_samples(tmp_path):
    fast, pwm = signals()
    native = write(tmp_path / "n.wsc", fast, pwm)
    held = write(tmp_path / "h.wsc", fast, pwm, held=True)
    assert (tmp_path / "n.wsc").stat().st_size < (tmp_path / "h.wsc").stat().st_size
    s_n, s_h = native.channel("SLOW"), held.channel("SLOW")
    assert (s_n.step, s_h.step) == (STEP, 1)
    assert np.array_equal(s_n.edges, s_h.edges)             # same edge ticks: nothing is invented or lost
    head, chans = wsc.read(tmp_path / "n.wsc")
    assert chans[1].n == N // STEP and head["ticks"] == N


def test_square_resolution_is_the_slow_channels_sample(tmp_path):
    fast, pwm = signals()
    native = write(tmp_path / "n.wsc", fast, pwm)
    held = write(tmp_path / "h.wsc", fast, pwm, held=True)
    x = square("SLOW", 1000, 0.3, tol_freq=0.001, tol_duty=0.001)
    ok_n, got_n, _ = check_square(native, x)
    ok_h, got_h, _ = check_square(held, x)
    assert ok_n and ok_h and got_n["freq_hz"] == got_h["freq_hz"] and got_n["duty"] == got_h["duty"]
    assert got_n["resolution"] == pytest.approx(STEP * got_h["resolution"])
    ok_f, got_f, _ = check_square(native, square("FAST", 1e6, 0.25, tol_freq=0.001, tol_duty=0.001))
    assert ok_f and got_f["resolution"] == pytest.approx(0.01)


def test_uart_on_a_slow_channel(tmp_path):
    fast, _ = signals()
    line = uart_samples(N // STEP, 27)                      # 115.7 kbaud at 3.125 Msps
    cap = write(tmp_path / "u.wsc", fast, line)
    ok, got, why = check_uart(cap, uart("SLOW", TICK / STEP / 27, tol_baud=0.005, max_errors=0))
    assert ok, why
    assert got["samples_per_bit"] == pytest.approx(27, rel=1e-3)
    assert got["data"].startswith(b"hello".hex())


def test_run_with_split_channels(tmp_path):
    fast, pwm = signals()
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=[square("SLOW", 1000, 0.3), square("FAST", 1e6, 0.25)]):
        rec.capture(rec.armed(), TICK, channels=[wsc.Channel("FAST", wsc.pack(fast.tobytes()), N),
                                                 wsc.Channel("SLOW", wsc.pack(pwm.tobytes()), N // STEP, step=STEP)])
    rec.close()
    rep = verify(tmp_path)
    assert [r["ok"] for r in rep["results"]] == [True, True]


def test_convert_wsc_sr_wsc_keeps_the_real_rate(tmp_path):
    import subprocess
    import sys
    import zipfile
    fast, pwm = signals()
    a = write(tmp_path / "a.wsc", fast, pwm)
    for src, dst in (("a.wsc", "b.sr"), ("b.sr", "c.wsc")):
        r = subprocess.run([sys.executable, "-m", "wireskein", "convert", str(tmp_path / src), str(tmp_path / dst)],
                           capture_output=True, text=True, check=True)
        assert "decimated: SLOW/32" in r.stdout
    with zipfile.ZipFile(tmp_path / "b.sr") as z:                   # a plain one-rate .sr for PulseView
        assert "samplerate=100000000 Hz" in z.read("metadata").decode()
        assert len(b"".join(z.read(n) for n in z.namelist() if n.startswith("logic-1-"))) == N
    c = wscio.load(tmp_path / "c.wsc")
    assert [(ch.name, ch.step) for ch in c.channels] == [("FAST", 1), ("SLOW", STEP)]
    assert all(np.array_equal(x.edges, y.edges) and x.initial == y.initial for x, y in zip(a.channels, c.channels))
    assert wsc.read(tmp_path / "c.wsc")[1][1].n == N // STEP        # only the samples that were taken


def test_attachments_and_notes_survive_conversion(tmp_path):
    import subprocess
    import sys
    fast, pwm = signals()
    write(tmp_path / "a.wsc", fast, pwm)
    wsc.attach(tmp_path / "a.wsc", "probe.json", {"fw": "1.2"})
    wsc.attach(tmp_path / "a.wsc", "readme.txt", "first")
    with pytest.raises(FileExistsError):
        wsc.attach(tmp_path / "a.wsc", "readme.txt", "again")
    wsc.attach(tmp_path / "a.wsc", "readme.txt", "replaced", replace=True)
    assert wsc.note(tmp_path / "a.wsc", "noisy PA5") == 1
    assert wsc.note(tmp_path / "a.wsc", {"i2c": [66]}, kind="analysis") == 2
    run = lambda *a: subprocess.run([sys.executable, "-m", "wireskein", *map(str, a)], capture_output=True, text=True, check=True)
    run("note", tmp_path / "a.wsc", "from the CLI")
    run("convert", tmp_path / "a.wsc", tmp_path / "b.sr")
    run("convert", tmp_path / "b.sr", tmp_path / "c.wsc")
    assert wsc.attachments(tmp_path / "c.wsc") == {"probe.json": b'{\n "fw": "1.2"\n}', "readme.txt": b"replaced"}
    assert [(n["content"], n.get("kind")) for n in wsc.notes(tmp_path / "c.wsc")] == \
        [("noisy PA5", None), ({"i2c": [66]}, "analysis"), ("from the CLI", None)]
    out = run("info", tmp_path / "c.wsc").stdout
    assert "SLOW" in out and "step 32" in out and "attach/probe.json" in out and "note 3" in out
    assert wsc.read(tmp_path / "c.wsc")[1][1].step == STEP                     # the channels are untouched


def test_recorder_attachments(tmp_path):
    from wireskein.runlog import Recorder
    rec = Recorder(tmp_path)
    rec.capture(rec.armed(), 1e6, interleaved=bytes(10), names=["P0"], attachments={"setup.txt": "10 kΩ pull-up"})
    rec.close()
    assert wsc.attachments(tmp_path / "c0001.wsc") == {"setup.txt": "10 kΩ pull-up".encode()}


def test_unknown_encoding_is_refused(tmp_path):
    import json
    import subprocess
    import sys
    import zipfile
    fast, pwm = signals()
    write(tmp_path / "a.wsc", fast, pwm)
    with zipfile.ZipFile(tmp_path / "a.wsc") as z:
        items = {n: z.read(n) for n in z.namelist()}
    head = json.loads(items["capture.json"])
    head["channels"].append({"name": "VBUS", "file": "ch/2.a16", "encoding": "analog", "n": 10, "step": 1, "phase": 0})
    items["capture.json"] = json.dumps(head).encode()
    items["ch/2.a16"] = bytes(20)
    with zipfile.ZipFile(tmp_path / "b.wsc", "w") as z:
        for n, d in items.items():
            z.writestr(n, d)
    with pytest.raises(ValueError, match=r"VBUS \('analog'\)"):
        wsc.read(tmp_path / "b.wsc")
    with pytest.raises(ValueError, match="does not read"):
        wscio.load(tmp_path / "b.wsc")
    r = subprocess.run([sys.executable, "-m", "wireskein", "info", str(tmp_path / "b.wsc")], capture_output=True, text=True)
    assert r.returncode != 0 and "VBUS ('analog')" in r.stderr
