"""Channels at different rates in one capture: a slow channel keeps only the
samples it has, and the checks give the same verdicts as on the same signal
repeated to the fast rate, with the slow channel's resolution."""

import json
import zipfile

import numpy as np
import pytest

from wireskein import fileformat
from wireskein.analyze import load, save
from wireskein._engine import fileio
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


def uart_samples(n, u, payload=b"hello, skein!"):
    """A UART at u samples per bit, one sample per element, blocks with idle gaps."""
    out = []
    while len(out) < n:
        for b in payload:
            for bit in [0] + [(b >> i) & 1 for i in range(8)] + [1]:
                out += [bit] * u
        out += [1] * 20 * u
    return np.array(out[:n], np.uint8)


def write(path, fast, slow, held=False):
    chans = [fileformat.Channel("FAST", fileformat.pack(fast.tobytes()), len(fast))]
    if held:
        chans.append(fileformat.Channel("SLOW", fileformat.pack(np.repeat(slow, STEP)[:N].tobytes()), N))
    else:
        chans.append(fileformat.Channel("SLOW", fileformat.pack(slow.tobytes()), len(slow), step=STEP))
    fileformat.write(path, TICK, chans)
    return fileio.load(path)


def test_slow_channel_keeps_only_its_samples(tmp_path):
    fast, pwm = signals()
    native = write(tmp_path / "n.wireskein", fast, pwm)
    held = write(tmp_path / "h.wireskein", fast, pwm, held=True)
    assert (tmp_path / "n.wireskein").stat().st_size < (tmp_path / "h.wireskein").stat().st_size
    s_n, s_h = native.channel("SLOW"), held.channel("SLOW")
    assert (s_n.step, s_h.step) == (STEP, 1)
    assert np.array_equal(s_n.edges, s_h.edges)             # same edge ticks: nothing is invented or lost
    head, chans = fileformat.read(tmp_path / "n.wireskein")
    assert chans[1].n == N // STEP and head["ticks"] == N


def test_square_resolution_is_the_slow_channels_sample(tmp_path):
    fast, pwm = signals()
    native = write(tmp_path / "n.wireskein", fast, pwm)
    held = write(tmp_path / "h.wireskein", fast, pwm, held=True)
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
    cap = write(tmp_path / "u.wireskein", fast, line)
    ok, got, why = check_uart(cap, uart("SLOW", TICK / STEP / 27, tol_baud=0.005, max_errors=0))
    assert ok, why
    assert got["samples_per_bit"] == pytest.approx(27, rel=1e-3)
    assert got["data"].startswith(b"hello".hex())


def test_run_with_split_channels(tmp_path):
    fast, pwm = signals()
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=[square("SLOW", 1000, 0.3), square("FAST", 1e6, 0.25)]):
        rec.capture(rec.armed(), TICK, channels=[fileformat.Channel("FAST", fileformat.pack(fast.tobytes()), N),
                                                 fileformat.Channel("SLOW", fileformat.pack(pwm.tobytes()), N // STEP, step=STEP)])
    rec.close()
    rep = verify(tmp_path)
    assert [r["ok"] for r in rep["results"]] == [True, True]


def test_convert_wsc_sr_wsc_keeps_the_real_rate(tmp_path):
    import subprocess
    import sys
    import zipfile
    fast, pwm = signals()
    a = write(tmp_path / "a.wireskein", fast, pwm)
    for src, dst in (("a.wireskein", "b.sr"), ("b.sr", "c.wireskein")):
        r = subprocess.run([sys.executable, "-m", "wireskein", "convert", str(tmp_path / src), str(tmp_path / dst)],
                           capture_output=True, text=True, check=True)
        assert "decimated: SLOW/32" in r.stdout
    with zipfile.ZipFile(tmp_path / "b.sr") as z:                   # a plain one-rate .sr for PulseView
        assert "samplerate=100000000 Hz" in z.read("metadata").decode()
        assert len(b"".join(z.read(n) for n in z.namelist() if n.startswith("logic-1-"))) == N
    c = fileio.load(tmp_path / "c.wireskein")
    assert [(ch.name, ch.step) for ch in c.channels] == [("FAST", 1), ("SLOW", STEP)]
    assert all(np.array_equal(x.edges, y.edges) and x.initial == y.initial for x, y in zip(a.channels, c.channels))
    assert fileformat.read(tmp_path / "c.wireskein")[1][1].n == N // STEP        # only the samples that were taken


def test_attachments_and_notes_survive_conversion(tmp_path):
    import subprocess
    import sys
    fast, pwm = signals()
    write(tmp_path / "a.wireskein", fast, pwm)
    fileformat.attach(tmp_path / "a.wireskein", "probe.json", {"fw": "1.2"})
    fileformat.attach(tmp_path / "a.wireskein", "readme.txt", "first")
    with pytest.raises(FileExistsError):
        fileformat.attach(tmp_path / "a.wireskein", "readme.txt", "again")
    fileformat.attach(tmp_path / "a.wireskein", "readme.txt", "replaced", replace=True)
    assert fileformat.note(tmp_path / "a.wireskein", "noisy PA5") == 1
    assert fileformat.note(tmp_path / "a.wireskein", {"i2c": [66]}, kind="analysis") == 2
    run = lambda *a: subprocess.run([sys.executable, "-m", "wireskein", *map(str, a)], capture_output=True, text=True, check=True)
    run("note", tmp_path / "a.wireskein", "from the CLI")
    run("convert", tmp_path / "a.wireskein", tmp_path / "b.sr")
    run("convert", tmp_path / "b.sr", tmp_path / "c.wireskein")
    assert fileformat.attachments(tmp_path / "c.wireskein") == {"probe.json": b'{\n "fw": "1.2"\n}', "readme.txt": b"replaced"}
    assert [(n["content"], n.get("kind")) for n in fileformat.notes(tmp_path / "c.wireskein")] == \
        [("noisy PA5", None), ({"i2c": [66]}, "analysis"), ("from the CLI", None)]
    out = run("info", tmp_path / "c.wireskein").stdout
    assert "SLOW" in out and "step 32" in out and "attach/probe.json" in out and "note 3" in out
    assert fileformat.read(tmp_path / "c.wireskein")[1][1].step == STEP                     # the channels are untouched


def test_recorder_attachments(tmp_path):
    from wireskein.runlog import Recorder
    rec = Recorder(tmp_path)
    rec.capture(rec.armed(), 1e6, interleaved=bytes(10), names=["P0"], attachments={"setup.txt": "10 kΩ pull-up"})
    rec.close()
    assert fileformat.attachments(tmp_path / "c0001.wireskein") == {"setup.txt": "10 kΩ pull-up".encode()}


def test_unknown_encoding_is_skipped_and_rewriting_refused(tmp_path):
    import json
    import subprocess
    import sys
    import zipfile
    fast, pwm = signals()
    write(tmp_path / "a.wireskein", fast, pwm)
    with zipfile.ZipFile(tmp_path / "a.wireskein") as z:
        items = {n: z.read(n) for n in z.namelist()}
    head = json.loads(items["capture.json"])
    head["channels"].append({"name": "VBUS", "file": "ch/2.a16", "encoding": "edges", "n": 10,
                             "rate_hz": [48000, 1], "t0_ticks": [0, 1]})
    items["capture.json"] = json.dumps(head).encode()
    items["ch/2.a16"] = bytes(20)
    with zipfile.ZipFile(tmp_path / "b.wireskein", "w") as z:
        for n, d in items.items():
            z.writestr(n, d)
    h, chans = fileformat.read(tmp_path / "b.wireskein")                               # the logic channels still read
    assert [c.name for c in chans] == ["FAST", "SLOW"]
    assert fileformat.skipped(h) == [{"name": "VBUS", "encoding": "edges"}]
    cap = fileio.load(tmp_path / "b.wireskein")
    assert [c.name for c in cap.channels] == ["FAST", "SLOW"] and cap.meta["skipped_channels"][0]["name"] == "VBUS"
    run = lambda *a: subprocess.run([sys.executable, "-m", "wireskein", *map(str, a)], capture_output=True, text=True)
    r = run("convert", tmp_path / "b.wireskein", tmp_path / "c.sr")             # would drop VBUS
    assert r.returncode != 0 and "VBUS ('edges')" in r.stderr and not (tmp_path / "c.sr").exists()
    r = run("info", tmp_path / "b.wireskein")
    assert r.returncode == 0 and "VBUS" in r.stdout and "not read by this version" in r.stdout
    assert run("note", tmp_path / "b.wireskein", "fine").returncode == 0      # adding to the file is fine
    rec = __import__("wireskein.runlog", fromlist=["Recorder"]).Recorder(tmp_path / "run")
    with rec.section(1, "t", expect=[square("VBUS", 1000)]):
        pass
    rec.close()
    import shutil
    shutil.copy(tmp_path / "b.wireskein", tmp_path / "run" / "c0001.wireskein")
    doc = json.loads((tmp_path / "run" / "run.json").read_text())
    doc["captures"] = [{"file": "c0001.wireskein", "t0": doc["log"][0]["t"], "channels": ["FAST", "SLOW", "VBUS"]}]
    (tmp_path / "run" / "run.json").write_text(json.dumps(doc))
    (res,) = verify(tmp_path / "run")["results"]
    assert res["ok"] is None and "VBUS has encoding 'edges'" in res["reason"]


# ---- the file as a container (wireskein-format §2) ----

def run(*args, check=True):
    import subprocess
    import sys
    return subprocess.run([sys.executable, "-m", "wireskein", *map(str, args)], capture_output=True, text=True,
                          check=check)


def test_identified_by_content_not_name(tmp_path):
    p = tmp_path / "capture.bin"                                          # any name
    fileformat.write(p, TICK, [fileformat.Channel("P0", fileformat.pack(bytes(16)), 16)])
    raw = p.read_bytes()
    assert raw[:4] == b"PK\x03\x04" and raw[30:44] == b"wireskein.json"   # the first entry, stored
    with zipfile.ZipFile(p) as z:
        assert json.loads(z.read("wireskein.json")) == {"format": "wireskein/0"}
        assert "format" not in json.loads(z.read("capture.json"))
    assert fileformat.sniff(p) == "wireskein"
    assert load(p).channel("P0")
    save(tmp_path / "x.sr", load(p))
    assert fileformat.sniff(tmp_path / "x.sr") == "sr"
    renamed = tmp_path / "x.wireskein"
    (tmp_path / "x.sr").rename(renamed)                                    # a .sr under the wrong name still reads as one
    assert fileformat.sniff(renamed) == "sr" and load(renamed).channel("P0")


def test_other_files_are_refused(tmp_path):
    old = tmp_path / "old.wsc"                                             # the format before wireskein/0
    with zipfile.ZipFile(old, "w") as z:
        z.writestr("capture.json", json.dumps({"format": "wireskein-capture/0", "tick_hz": [1, 1], "ticks": 0,
                                               "channels": [], "meta": {}}))
    (tmp_path / "text.wireskein").write_text("hello")
    newer = tmp_path / "new.wireskein"
    with zipfile.ZipFile(newer, "w") as z:
        z.writestr("wireskein.json", '{"format": "wireskein/9"}')
    for p, why in ((old, "neither"), (tmp_path / "text.wireskein", "neither"), (newer, "wireskein/9")):
        with pytest.raises(ValueError, match=why):
            load(p)
    with pytest.raises(ValueError, match="not a WireSkein file"):
        fileformat.note(old, "x")
    r = run("info", newer, check=False)
    assert r.returncode != 0 and "wireskein/9" in r.stderr


def test_unknown_parts_are_carried_over(tmp_path):
    a = tmp_path / "a.wireskein"
    fileformat.write(a, TICK, [fileformat.Channel("P0", fileformat.pack(bytes([0, 1] * 8)), 16)])
    with zipfile.ZipFile(a, "a") as z:                                     # a part a newer version wrote
        z.writestr("markers/0001.json", '{"t": 5}')
    fileformat.attach(a, "r.txt", "one")
    fileformat.attach(a, "r.txt", "two", replace=True)                    # rewrites the file
    assert fileformat.extras(a)["markers/0001.json"] == b'{"t": 5}'
    run("convert", a, tmp_path / "b.sr")
    run("convert", tmp_path / "b.sr", tmp_path / "c.wireskein")
    run("convert", tmp_path / "c.wireskein", tmp_path / "d.wireskein")
    assert fileformat.extras(tmp_path / "d.wireskein") == {"attach/r.txt": b"two", "markers/0001.json": b'{"t": 5}'}
