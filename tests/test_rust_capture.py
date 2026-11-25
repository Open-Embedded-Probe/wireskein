"""WIRESKEIN_RUST=capture: the oep source's capture by wireskein-oep (the Rust OEP client) beside oep-client-python's
acquire (bench/notes/2026-11-24-rust-plan.ja.md §37).

Two kinds of comparison, both without a probe:
- a stream's data pushes given as frames (positions, gaps, segment records, format 0 and format 1) put together by
  both: wireskein_core.oep_assemble against acquire._Spool (and _stream_parts) - the parts, their ticks and times,
  each channel's bits or values, the losses;
- whole captures from oep-client-python's virtual bench (virtual_bench_serve, on TCP), the same request through
  `wireskein capture` with and without WIRESKEIN_RUST=capture: the files' header, channels and data.

Each runs only where its pieces are installed (wireskein_core, oep-client-python)."""
import builtins
import importlib.util
import re
import subprocess
import sys

import numpy as np
import pytest

from wireskein import _fileformat as fileformat, _rust, sources

HAS_CORE = importlib.util.find_spec("wireskein_core") is not None
HAS_CLIENT = importlib.util.find_spec("oep_client") is not None
rust_only = pytest.mark.skipif(not HAS_CORE, reason="wireskein_core (crates/wireskein-py) not installed")
both = pytest.mark.skipif(not (HAS_CORE and HAS_CLIENT), reason="wireskein_core and oep-client-python needed")


@pytest.fixture
def clean(monkeypatch):
    monkeypatch.delenv(_rust.RUST_ENV, raising=False)
    return monkeypatch


@pytest.fixture(autouse=True)
def own_sessions(monkeypatch, tmp_path):
    """Each test's kept sessions (oep-client-python's and the Rust client's) in its own directory."""
    monkeypatch.setenv("OEP_SESSION_DIR", str(tmp_path / "sessions"))


# ---------------- the switch ----------------

def test_unset_never_imports_the_core(clean):
    real = builtins.__import__

    def no_core(name, *a, **k):
        if name == "wireskein_core":
            pytest.fail("wireskein_core imported with WIRESKEIN_RUST unset")
        return real(name, *a, **k)

    clean.setattr(builtins, "__import__", no_core)
    req = sources.Request([("A", "GPIO30")], 1_000_000, 100)
    assert not _rust.captures("oep://30eda0ea068b", req)


def test_what_the_rust_client_takes(clean):
    clean.setenv(_rust.RUST_ENV, "capture")
    logic = sources.Request([("A", "GPIO30")], 1_000_000, 100)
    assert _rust.captures("oep://30eda0ea068b", logic)
    assert _rust.captures("oep://30eda0ea068b?via=usb", logic) and _rust.captures("tcp://127.0.0.1:7450", logic)
    assert _rust.captures("oep://30eda0ea068b?via=tcp:10.0.0.2:7450", logic)
    serial = sources.Request([("A", "GPIO30")], 1_000_000, 100, fast=[])
    assert _rust.captures("/dev/ttyACM0", serial) and _rust.captures("oep://30eda0ea068b?via=tty:/dev/ttyACM0", serial)
    for target in ("/dev/ttyACM0", "oep://30eda0ea068b?via=hid", "oep://30eda0ea068b?via=cdc",
                   "oep://30eda0ea068b?via=net", "oep://30eda0ea068b?via=tty:/dev/ttyACM0", "usb", "tcp://host"):
        assert not _rust.captures(target, logic), target
    analog = sources.Request([("A", "GPIO30")], 1_000_000, 100, analog=[("V", "GPIO17", None)],
                             trigger=("V", "rise", 1.0))
    assert _rust.captures("oep://30eda0ea068b", analog)
    label = sources.Request([("A", "x035.swdio")], 1_000_000, 100)
    alabel = sources.Request([], 1_000_000, 100, analog=[("V", "x035.vdd", None)])
    assert not _rust.captures("oep://30eda0ea068b", label) and not _rust.captures("oep://30eda0ea068b", alabel)
    clean.setenv("OEP_BROKER", "127.0.0.1:7460")
    assert not _rust.captures("oep://30eda0ea068b", logic)


# ---------------- a stream's pushes put together ----------------

def _bytes(n, seed):
    return np.random.default_rng(seed).integers(0, 256, n, dtype=np.uint8).tobytes()


def _stream0(seg_samples, n_segs, width=8, chunk=37, skip=(), seed=1, rate=1_000_000):
    """Format 0 pushes: segment k at its byte position, sent `chunk` bytes a push, each push with its segment's
    record; the segments in `skip` never sent (the probe dropped them)."""
    seg_bytes = (seg_samples * width + 7) // 8
    data = _bytes(seg_bytes * n_segs, seed)
    pushes = []
    for k in range(n_segs):
        pos = k * seg_bytes
        info = (pos, seg_samples, 1_000 + k * seg_samples * 10**9 // rate, 7)
        if k in skip:
            continue
        for at in range(0, seg_bytes, chunk):
            pushes.append((pos + at, data[pos + at:pos + min(seg_bytes, at + chunk)], info))
    return pushes


def _received(pushes):
    """oep-client-python's Received from pushes, as LogicCapture._collect fills it."""
    from oep_client import capture as oc
    got = oc.Received()
    for pos, data, info in pushes:
        if info is not None:
            got.segments[info[0]] = oc.SegmentInfo(*info)
        if got.start is None:
            got.start = pos
        else:
            skipped = pos - (got.start + len(got.data) + sum(n for _, n in got.gaps))
            if skipped:
                got.gaps.append((len(got.data), skipped))
        got.data += data
        got.frames += 1
    return got


def _python(cfg, names, pushes, on_loss, write_pos=None, spool=True, tmp=None):
    """Python's parts and losses: acquire._Spool (as a stream is put together on disk), or _stream_parts."""
    from oep_client import acquire, capture as oc
    cap = oc.LogicCapture.__new__(oc.LogicCapture)
    cap.config = cfg
    ids = list(range(10, 10 + len(names)))
    req = acquire.Request([(n, n) for n in names], 1, 0, mode="stream", duration_s=1.0, on_loss=on_loss,
                          spool_dir=str(tmp) if tmp else None)
    got = _received(pushes)
    if not spool:
        return acquire._stream_parts(cap, cfg, got, names, ids, req)
    s = acquire._Spool(cap, cfg, names, ids, req)
    s.give(got, write_pos=write_pos)
    s.finish()
    return s.parts, s.done_lost


def _py_parts(parts):
    from oep_client import acquire
    out = []
    for p in parts:
        chans = []
        for c in p.channels:
            if isinstance(c, acquire.Reduced):
                chans.append((c.name, c.kind, c.n, c.step, c.start_level, bytes(c.data)))
            else:
                data = c.bits[:] if isinstance(c.bits, acquire.OnDisk) else bytes(c.bits)
                chans.append((c.name, "bits", c.n, c.step, 0, bytes(data)))
        out.append((p.tick0, p.samples, dict(p.meta), chans))
    return out


def _rust_parts(cfg_doc, pushes, on_loss, write_pos=None):
    import json
    import wireskein_core
    text, datas = wireskein_core.oep_assemble(json.dumps({**cfg_doc, "on_loss": on_loss}), pushes, write_pos)
    doc = json.loads(text)
    out = []
    for p in doc["parts"]:
        chans = []
        for c in p["channels"]:
            if c["kind"] == "reduced":
                chans.append((c["name"], c["reduction"], c["n"], c["step"], c["start_level"], bytes(datas[c["data"]])))
            else:
                chans.append((c["name"], "bits", c["n"], c["step"], 0, bytes(datas[c["data"]])))
        out.append((p["tick0"], p["samples"], dict(p["meta"]), chans))
    return out, doc["lost"]


def _cfg0(width, positions, rate=1_000_000):
    from fractions import Fraction
    from oep_client import capture as oc
    return oc.Config(rate=Fraction(rate), width=width, positions=positions), \
        {"names": [f"C{k}" for k in range(len(positions))], "ids": list(range(10, 10 + len(positions))),
         "rate": [rate, 1], "format": 0, "width": width, "positions": positions}


@both
@pytest.mark.parametrize("on_loss", ["stop", "continue"])
@pytest.mark.parametrize("skip", [(), (2,), (1, 3), (0,)], ids=["none", "one", "two", "first"])
def test_a_format_0_stream_is_put_together_as_python_does(on_loss, skip, tmp_path):
    cfg, doc = _cfg0(8, [0, 3, 7])
    names = doc["names"]
    pushes = _stream0(200, 5, skip=skip)
    if skip and skip[0] == 0 and on_loss == "stop":           # the first bytes gone: nothing to keep
        with pytest.raises(RuntimeError, match="dropped the stream's first"):
            _python(cfg, names, pushes, on_loss, tmp=tmp_path)
        with pytest.raises(RuntimeError, match="dropped the stream's first"):
            _rust_parts(doc, pushes, on_loss)
        return
    py_parts, py_lost = _python(cfg, names, pushes, on_loss, tmp=tmp_path)
    rs_parts, rs_lost = _rust_parts(doc, pushes, on_loss)
    assert rs_parts == _py_parts(py_parts)
    assert rs_lost == py_lost
    assert len(rs_lost) == (0 if not skip else 1 if on_loss == "stop" else len(skip))


@both
def test_an_error_stops_write_pos_is_read_as_python_reads_it(tmp_path):
    """After an error stop, write_pos bounds what is still to be put together (as _Spool: bytes already put together
    stay)."""
    cfg, doc = _cfg0(8, [0, 1])
    pushes = _stream0(160, 4)
    py_parts, py_lost = _python(cfg, doc["names"], pushes, "stop", write_pos=333, tmp=tmp_path)
    rs_parts, rs_lost = _rust_parts(doc, pushes, "stop", write_pos=333)
    assert rs_parts == _py_parts(py_parts) and rs_lost == py_lost


@both
@pytest.mark.parametrize("width,skip", [(4, ()), (4, (2,)), (12, (1,)), (1, ())])
def test_samples_across_bytes_are_put_together_segment_by_segment(width, skip, tmp_path):
    """Format 0 of samples that are not whole bytes (Python puts them together in memory, _stream_parts): each
    segment's own bytes, an odd count of samples leaving its last byte part full."""
    positions = list(range(min(width, 3)))
    cfg, doc = _cfg0(width, positions)
    pushes = _stream0(201, 4, width=width, skip=skip)
    py_parts, py_lost = _python(cfg, doc["names"], pushes, "continue", spool=False)
    rs_parts, rs_lost = _rust_parts(doc, pushes, "continue")
    assert rs_parts == _py_parts(py_parts)
    assert rs_lost == py_lost


# format 1: the bytes a probe sends for given lines (capture §1.3), written here from the levels

def _f1_segment(levels, prev, raw_roles, reduced, block):
    """One format 1 segment: levels[role] the samples of each role, prev[role] the level before its first."""
    from oep_client import capture_blocks as cb
    samples = len(levels[0])
    out = bytearray()
    red = sorted(reduced)
    latches = [r for r in red if reduced[r][0] == cb.CHANGE_LATCH]
    for first in range(0, samples, block):
        r = min(block, samples - first)
        bits = []
        for i in range(first, first + r):
            bits += [levels[role][i] for role in sorted(raw_roles)]
        bits += [0] * (-len(bits) % 8)
        part = []
        for role in latches:
            part.append(levels[role][first - 1] if first else prev[role])
        for role in red:
            kind, d = reduced[role]
            for lo in range(first, first + r, d):
                iv = levels[role][lo:min(lo + d, first + r)]
                before = levels[role][lo - 1] if lo else prev[role]
                if kind == cb.SAMPLE:
                    part.append(iv[0])
                elif kind == cb.ANY_HIGH:
                    part.append(int(max(iv)))
                elif kind == cb.ANY_LOW:
                    part.append(int(min(iv)))
                else:
                    seq = [before, *iv]
                    part += [iv[-1], int(any(a != b for a, b in zip(seq, seq[1:])))]
        part += [0] * (-len(part) % 8)
        out += np.packbits(np.array(bits + part, np.uint8), bitorder="little").tobytes()
    return bytes(out)


def _stream1(seg_samples, n_segs, raw_roles, reduced, block, roles, skip=(), chunk=29, seed=3, rate=1_000_000):
    rng = np.random.default_rng(seed)
    total = seg_samples * n_segs
    # runs of random lengths: lines that change now and then (and sometimes inside an interval twice)
    levels = [np.repeat(rng.integers(0, 2, total), 1)[:total] for _ in range(roles)]
    for role in range(roles):
        lv = np.zeros(total, np.uint8)
        at, v = 0, int(rng.integers(0, 2))
        while at < total:
            n = int(rng.integers(1, 40))
            lv[at:at + n] = v
            v ^= 1
            at += n
        levels[role] = lv.tolist()
    pushes, pos = [], 0
    for k in range(n_segs):
        lo = k * seg_samples
        seg = _f1_segment([lv[lo:lo + seg_samples] for lv in levels], [lv[lo - 1] if lo else 0 for lv in levels],
                          raw_roles, reduced, block)
        info = (pos, seg_samples, 5_000 + lo * 10**9 // rate, 3)
        if k not in skip:
            for at in range(0, len(seg), chunk):
                pushes.append((pos + at, seg[at:at + chunk], info))
        pos += len(seg)
    return pushes


def _cfg1(raw_roles, reduced, block, rate=1_000_000):
    from fractions import Fraction
    from oep_client import capture as oc
    roles = len(raw_roles) + len(reduced)
    cfg = oc.Config(rate=Fraction(rate), width=len(raw_roles), positions=list(range(len(raw_roles))),
                    format=oc.DATA_FORMAT["logic_blocks"], block=block)
    cfg.raw_roles, cfg.reduced = sorted(raw_roles), dict(reduced)
    doc = {"names": [f"C{k}" for k in range(roles)], "ids": list(range(10, 10 + roles)), "rate": [rate, 1],
           "format": 1, "width": len(raw_roles), "positions": list(range(len(raw_roles))), "block": block,
           "raw_roles": sorted(raw_roles), "reduced": [[r, k, d] for r, (k, d) in sorted(reduced.items())]}
    return cfg, doc


@both
@pytest.mark.parametrize("seg_samples,d", [(256, 8), (200, 8), (200, 64)], ids=["whole-blocks", "last-block", "grid"])
@pytest.mark.parametrize("on_loss,skip", [("stop", ()), ("continue", (2,)), ("stop", (1,))])
def test_a_format_1_stream_is_put_together_as_python_does(seg_samples, d, on_loss, skip, tmp_path):
    """Raw, sample, any_high, any_low and change_latch lines; segments of whole blocks or not, an interval D that
    divides them or not (a grid restart: a new part without a loss), segments the probe dropped."""
    from oep_client import capture_blocks as cb
    reduced = {2: (cb.SAMPLE, 4), 3: (cb.ANY_HIGH, d), 4: (cb.ANY_LOW, d), 5: (cb.CHANGE_LATCH, d)}
    cfg, doc = _cfg1([0, 1], reduced, 64)
    pushes = _stream1(seg_samples, 5, [0, 1], reduced, 64, 6, skip=skip)
    py_parts, py_lost = _python(cfg, doc["names"], pushes, on_loss, tmp=tmp_path)
    rs_parts, rs_lost = _rust_parts(doc, pushes, on_loss)
    assert rs_parts == _py_parts(py_parts)
    assert rs_lost == py_lost
    if d == 64 and not skip:
        assert len(rs_parts) > 1 and not rs_lost                # the grid restarts: parts, no loss


# ---------------- whole captures from the virtual bench ----------------

SERVE = "oep_client.virtual_bench_serve"


@pytest.fixture
def bench():
    procs = []

    def start(*extra, profile="p4-x035"):
        p = subprocess.Popen([sys.executable, "-m", SERVE, "--tcp", "0", "--framing", "length", "--profile", profile,
                              *extra], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        procs.append(p)
        port = re.search(r"PORT (\d+)", p.stdout.readline()).group(1)
        return f"oep:tcp://127.0.0.1:{port}"
    yield start
    for p in procs:
        p.stdin.close()
        p.wait(5)


def _take(monkeypatch, rust: bool, src, req, out):
    if rust:
        monkeypatch.setenv(_rust.RUST_ENV, "capture")
    else:
        monkeypatch.delenv(_rust.RUST_ENV, raising=False)
    return sources.capture(src, req, out)


def _content(path):
    """A file's header without what differs run to run (ids, times, the link, the clock) and its channels' data."""
    head = fileformat.read_header(path)
    parts = []
    for k, (h, chans) in enumerate(fileformat.read_parts(path)):
        cs = []
        for c in chans:
            if isinstance(c, fileformat.ReducedChannel):
                cs.append((c.name, c.encoding, c.n, c.step, c.start_level, bytes(c.data)))
            else:
                cs.append((c.name, "bits", c.n, c.step, c.phase, bytes(fileformat.unpack(c)[:c.n])))
        p = head["parts"][k]
        # a triggered segment's tick0 is from its time (when the trigger came): not the same run to run
        tick0 = p.get("tick0") if head["layout"] == "timeline" else None
        parts.append((p["ticks"], tick0, sorted(p["meta"]), p["meta"].get("trigger_tick"), cs))
    meta = head["meta"]
    return {"tick_hz": head["tick_hz"], "layout": head["layout"], "complete": head["complete"],
            "lost": head.get("lost"), "channels": meta.get("probe_channels"), "repeat": meta.get("repeat"),
            "probe": sorted(set(meta["probe"]) - {"clock"}), "data": meta["probe"].get("data"), "parts": parts}


CASES = {
    "three": ([("A", "GPIO10"), ("B", "GPIO11"), ("C", "GPIO12")], 20_000_000, 1000, {}),
    "trigger": ([("A", "GPIO10"), ("B", "GPIO11")], 20_000_000, 400, {"trigger": ("B", "rise"), "pretrigger": 50}),
    "arm-now": ([("A", "GPIO10"), ("B", "GPIO11")], 20_000_000, 400,
                {"trigger": ("B", "rise"), "pretrigger": 50, "arm": "now"}),
    "rate": ([("A", "GPIO10")], 3_000_000, 100, {}),
    "format-1": (sources.parse_channels("A=GPIO10,B=GPIO11,C=GPIO12/sample:4,D=GPIO13/any_high:4,E=GPIO14/any_low:64,"
                                        "F=GPIO15/change_latch:64,G=GPIO16/change_latch:64"), 20_000_000, 4096,
                 {"resolution": sources.parse_resolution("C=GPIO12/sample:4,D=GPIO13/any_high:4,E=GPIO14/any_low:64,"
                                                         "F=GPIO15/change_latch:64,G=GPIO16/change_latch:64")}),
    "reduced-only": ([("A", "GPIO10"), ("B", "GPIO11")], 20_000_000, 4000,
                     {"resolution": {"A": ("any_high", 64), "B": ("change_latch", 64)}}),
    "repeat": ([("A", "GPIO10"), ("B", "GPIO11")], 1_000_000, 2000, {"mode": "repeat", "segments": 4}),
    "repeat-trigger": ([("A", "GPIO10")], 1_000_000, 500,
                       {"mode": "repeat", "segments": 40, "trigger": ("A", "rise"), "pretrigger": 100}),
}


@both
@pytest.mark.parametrize("case", list(CASES))
def test_a_capture_from_the_virtual_bench_is_the_same(case, bench, monkeypatch, tmp_path):
    chans, rate, samples, kw = CASES[case]
    extra = ("--capture-slipped",) if case == "trigger" else ()
    got = []
    for rust in (False, True):
        req = sources.Request(list(chans), rate, samples, **kw)
        got.append(_content(_take(monkeypatch, rust, bench(*extra), req, tmp_path / f"{case}-{rust}.wireskein")))
    py, rs = got
    assert rs == py


@both
def test_the_esp32_sampler_profile(bench, monkeypatch, tmp_path):
    got = []
    for rust in (False, True):
        req = sources.Request([("A", "GPIO4"), ("B", "GPIO5")], 1_000_000, 200)
        got.append(_content(_take(monkeypatch, rust, bench(profile="esp32-v003"), req, tmp_path / f"v{rust}.wireskein")))
    assert got[1] == got[0]


@both
@pytest.mark.parametrize("res", [{}, {"D": ("change_latch", 64)}], ids=["format-0", "format-1"])
def test_a_stream_from_the_virtual_bench(res, bench, monkeypatch, tmp_path):
    """A stream's length is the time it ran: each one complete, in one timeline of parts (format 1: a grid restart may
    cut it) with the bench's counter throughout, the same channels and meta as Python's."""
    chans = [("A", "GPIO10"), ("D", "GPIO13")]
    got = {}
    for rust in (False, True):
        req = sources.Request(chans, 1_000_000, 0, mode="stream", duration_s=0.3, resolution=dict(res))
        out = _take(monkeypatch, rust, bench(), req, tmp_path / f"s{rust}.wireskein")
        head = fileformat.read_header(out)
        assert head["complete"] is True and head["meta"]["stream"]["frames"] >= 2
        got[rust] = head
        a = fileformat.read_parts(out)[0][1][0]
        lv = np.frombuffer(fileformat.unpack(a), np.uint8)[:a.n]
        assert np.array_equal(lv, np.arange(a.n) & 1)            # bit 0 of the counter, sample by sample
    assert sorted(got[True]["meta"]["stream"]) == sorted(got[False]["meta"]["stream"])
    assert sorted(got[True]["meta"]["probe"]) == sorted(got[False]["meta"]["probe"])
    assert [c["encoding"] for c in got[True]["parts"][0]["channels"]] == \
        [c["encoding"] for c in got[False]["parts"][0]["channels"]]
    assert not list((tmp_path).glob(".oep-acquire-stream-*"))     # the spool removed once written


@both
@pytest.mark.parametrize("case", ["unknown-line", "no-trigger"])
def test_the_same_errors(case, bench, monkeypatch, tmp_path):
    if case == "unknown-line":
        req, extra, kind = sources.Request([("A", "GPIO99")], 1_000_000, 100), (), ValueError
    else:                                       # role 0 never high: the trigger does not come
        req = sources.Request([("A", "GPIO10")], 1_000_000, 100, trigger=("A", "high"), timeout_s=0.3)
        extra, kind = ("--logic-wave", "0=quiet:0"), TimeoutError
    with pytest.raises(kind) as e:
        _take(monkeypatch, True, bench(*extra), req, tmp_path / "e.wireskein")
    want = "GPIO99: no channel of that name" if case == "unknown-line" else \
        "no trigger (A:high) within 0.3 s (--timeout)"
    assert str(e.value).startswith(want)
    try:                     # Python's, when it gets that far (a host under load may lose its session first)
        _take(monkeypatch, False, bench(*extra), req, tmp_path / "p.wireskein")
    except kind as py:
        assert str(e.value) == str(py)
    except RuntimeError:
        pass


# ---------------- analog lines and the capture group (on the virtual bench) ----------------

def _analog_content(c):
    acq = {k: v for k, v in c.acquisition.items() if k != "vrefint_ns"}       # a time: run to run
    assert isinstance(c.acquisition.get("vrefint_ns", 0), int)
    return (c.name, "analog", c.n, c.rate_hz, c.t0_ticks, c.width, c.value_bits, c.zero, c.scale_nv,
            bytes(c.data), sorted(acq.items(), key=lambda kv: kv[0]))


ANALOG_CASES = {
    "alone": ([], 20_000_000, 512, {"analog": [("SQ", "GPIO16", None), ("SINE", "GPIO17", 3)],
                                     "analog_rate_hz": 48_000}),
    "with-logic": ([("A", "GPIO10"), ("B", "GPIO11")], 20_000_000, 2000,
                   {"analog": [("SQ", "GPIO16", None)], "analog_rate_hz": 48_000}),
    "logic-trigger": ([("A", "GPIO10"), ("B", "GPIO11")], 20_000_000, 2000,
                      {"trigger": ("B", "rise"), "pretrigger": 100, "analog": [("SQ", "GPIO16", None)],
                       "analog_rate_hz": 48_000, "analog_samples": 64}),
    "analog-trigger": ([("A", "GPIO10")], 20_000_000, 2000,
                       {"trigger": ("SQ", "rise", 1.0), "pretrigger": 100, "analog": [("SQ", "GPIO16", None)],
                        "analog_rate_hz": 48_000, "analog_samples": 64}),
    "analog-only-trigger": ([], 20_000_000, 256,
                            {"trigger": ("SQ", "fall", 1.0), "pretrigger": 10, "analog": [("SQ", "GPIO16", None)],
                             "analog_rate_hz": 48_000}),
}


def _analog_file(path):
    """The file's header (without what differs run to run) and every channel, analog ones included."""
    head = fileformat.read_header(path)
    parts = []
    for k, (h, chans) in enumerate(fileformat.read_parts(path)):
        cs = []
        for c in chans:
            if isinstance(c, fileformat.AnalogChannel):
                cs.append(_analog_content(c))
            elif isinstance(c, fileformat.ReducedChannel):
                cs.append((c.name, c.encoding, c.n, c.step, c.start_level, bytes(c.data)))
            else:
                cs.append((c.name, "bits", c.n, c.step, c.phase, bytes(fileformat.unpack(c)[:c.n])))
        p = head["parts"][k]
        parts.append((p["ticks"], p.get("tick0"), sorted(p["meta"]), p["meta"].get("trigger_tick"), cs))
    meta = head["meta"]
    probe = meta["probe"]
    return {"tick_hz": head["tick_hz"], "layout": head["layout"], "complete": head["complete"],
            "lost": head.get("lost"), "channels": meta.get("probe_channels"),
            "probe": sorted(set(probe) - {"clock"}), "calibration": probe.get("calibration"),
            "trigger_caps": probe.get("trigger_caps"), "judge": probe.get("judge_latency_ns"),
            "generation": sorted(probe.get("generation", {})), "parts": parts}


@both
@pytest.mark.parametrize("case", list(ANALOG_CASES))
def test_analog_lines_from_the_virtual_bench_are_the_same(case, bench, monkeypatch, tmp_path):
    """Analog lines alone, beside logic in a capture group started at once, and on the group's trigger source (a
    logic line's edge, an analog line crossing a voltage): every value, its first tick, rate, conversion and what it
    was taken with (frontend, reference, Vrefint, the start's uncertainty, the trigger's index) as Python's."""
    chans, rate, samples, kw = ANALOG_CASES[case]
    got = []
    for rust in (False, True):
        req = sources.Request(list(chans), rate, samples, **kw)
        got.append(_analog_file(_take(monkeypatch, rust, bench(), req, tmp_path / f"{case}-{rust}.wireskein")))
    py, rs = got
    assert rs == py


@both
def test_a_refused_pretrigger_together_says_the_same(bench, monkeypatch, tmp_path):
    """Both tracks asked first (capture §3.3 query): the analog's pretrigger, the logic's counted at the analog rate,
    is not below its 4 samples - refused, the words as Python's."""
    req = sources.Request([("A", "GPIO10")], 20_000_000, 2000, trigger=("A", "rise"), pretrigger=1900,
                          analog=[("SQ", "GPIO16", None)], analog_rate_hz=48_000, analog_samples=4)
    said = []
    for rust in (False, True):
        with pytest.raises(RuntimeError) as e:
            _take(monkeypatch, rust, bench(), req, tmp_path / f"r{rust}.wireskein")
        said.append(re.sub(r"tcp://127\.0\.0\.1:\d+", "tcp://BENCH", str(e.value)))
    assert "each track asked before either was set" in said[1]
    assert said[1] == said[0]


@both
@pytest.mark.parametrize("logic", [False, True], ids=["analog-alone", "with-logic"])
def test_an_analog_stream_from_the_virtual_bench(logic, bench, monkeypatch, tmp_path):
    """--mode stream with analog lines: alone (a timeline at the analog rate) or with logic in one group (each part
    given the analog samples of its time): complete, the bench's values, the same channels and meta as Python's."""
    from oep_client.virtual_bench_capture import analog_value
    chans = [("A", "GPIO10")] if logic else []
    got = {}
    for rust in (False, True):
        req = sources.Request(list(chans), 1_000_000 if logic else 20_000, 0, mode="stream", duration_s=0.3,
                              analog=[("SQ", "GPIO16", None)], analog_rate_hz=20_000)
        out = _take(monkeypatch, rust, bench(), req, tmp_path / f"a{rust}.wireskein")
        head = fileformat.read_header(out)
        assert head["complete"] is True, head.get("lost")
        sq = fileformat.read_parts(out)[0][1][-1]
        assert isinstance(sq, fileformat.AnalogChannel) and sq.n > 1000
        assert list(sq.values()[:100]) == [analog_value(0, i) for i in range(100)]
        got[rust] = (head, sq)
    (ph, psq), (rh, rsq) = got[False], got[True]
    assert sorted(rh["meta"]["stream"]) == sorted(ph["meta"]["stream"])
    assert sorted(rh["meta"]["probe"]) == sorted(ph["meta"]["probe"])
    assert rh["meta"]["probe_channels"] == ph["meta"]["probe_channels"]
    assert (rsq.rate_hz, rsq.width, rsq.value_bits, rsq.zero, rsq.scale_nv) == \
        (psq.rate_hz, psq.width, psq.value_bits, psq.zero, psq.scale_nv)
    assert sorted(rsq.acquisition) == sorted(psq.acquisition)
    if logic:
        assert 0 <= rsq.t0_ticks < 1e6 / 20_000 * 2


# ---------------- the session a killed run left ----------------

def _session_file(d):
    files = list(d.glob("*.session"))
    assert len(files) == 1, files
    return files[0]


@both
def test_a_session_python_left_is_ended_by_rust(bench, monkeypatch, tmp_path):
    """oep-client-python's kept session (host guide §5): a run that closed its link without end leaves the session and
    its lock on the probe (lease 30 s); the Rust client reads the same file, opens that session again and ends it, so
    it takes the probe at once - not after the lease, nor 'in use'."""
    import time
    from oep_client import core, link
    sessions = tmp_path / "sessions"
    monkeypatch.setenv("OEP_SESSION_DIR", str(sessions))
    src = bench()
    hst = link.open_host(src[len("oep:"):])
    core.take(hst, 30_000, owner="a killed run")
    hst.link.close()                                   # no end: the probe keeps the session and its lock
    sid, corr = _session_file(sessions).read_text().split()
    assert int(sid, 16) == hst.session and int(corr, 16) > 0
    t0 = time.monotonic()
    out = _take(monkeypatch, True, src, sources.Request([("A", "GPIO10")], 1_000_000, 100), tmp_path / "k.wireskein")
    assert time.monotonic() - t0 < 4 and fileformat.read_header(out)["complete"]
    assert _session_file(sessions).read_text() == ""   # ended: nothing kept


@both
def test_a_session_rust_left_is_ended_by_python(bench, monkeypatch, tmp_path):
    """The other way: wireskein-rs killed in a stream leaves its session in the file (the id and a corr reserved
    ahead); oep-client-python's next run ends it and takes the probe at once."""
    import os
    import signal
    import time
    rs = _wireskein_rs()
    if rs is None:
        pytest.skip("wireskein-rs not built")
    sessions = tmp_path / "sessions"
    monkeypatch.setenv("OEP_SESSION_DIR", str(sessions))
    src = bench()
    p = subprocess.Popen([rs, "capture", "--source", src, "--channels", "A=GPIO10", "--rate", "1M", "--mode", "stream",
                          "--duration", "30", "-o", str(tmp_path / "killed.wireskein")],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env={**os.environ})
    try:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            files = list(sessions.glob("*.session"))
            if files and files[0].read_text().strip():
                break
            time.sleep(0.05)
        else:
            pytest.fail("wireskein-rs kept no session")
        time.sleep(0.3)
    finally:
        p.send_signal(signal.SIGKILL)
        p.wait(5)
    assert len(_session_file(sessions).read_text().split()) == 2
    t0 = time.monotonic()
    out = _take(monkeypatch, False, src, sources.Request([("A", "GPIO10")], 1_000_000, 100), tmp_path / "p.wireskein")
    assert time.monotonic() - t0 < 4 and fileformat.read_header(out)["complete"]
    assert _session_file(sessions).read_text() == ""


def _wireskein_rs():
    import shutil
    from pathlib import Path
    root = Path(__file__).resolve().parents[1] / "target"
    for kind in ("release", "debug"):
        if (root / kind / "wireskein-rs").is_file():
            return str(root / kind / "wireskein-rs")
    return shutil.which("wireskein-rs")


# ---------------- a lost frame boundary (transports §5) ----------------

class _Proxy:
    """A TCP proxy in front of the virtual bench: the first connection gets, after `after` data pushes towards the
    host, a length no frame can have (past max_frame) - the boundaries lost; later connections go through clean."""

    def __init__(self, upstream: str, after: int):
        import socket
        import threading
        host, port = upstream[len("oep:tcp://"):].split(":")
        self.up = (host, int(port))
        self.after, self.conns, self.injected = after, 0, False
        self.srv = socket.socket()
        self.srv.bind(("127.0.0.1", 0))
        self.srv.listen(4)
        self.port = self.srv.getsockname()[1]
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self):
        import socket
        import threading
        while True:
            try:
                down, _ = self.srv.accept()
            except OSError:
                return
            up = socket.create_connection(self.up)
            k = self.conns
            self.conns += 1
            threading.Thread(target=self._pipe, args=(down, up, None), daemon=True).start()
            threading.Thread(target=self._pipe, args=(up, down, k == 0), daemon=True).start()

    def _pipe(self, a, b, inject):
        import struct
        buf, pushes = b"", 0
        try:
            while True:
                data = a.recv(65536)
                if not data:
                    break
                if not inject or self.injected:
                    b.sendall(data)
                    continue
                buf += data                            # frame by frame, to put the bad length between two frames
                while len(buf) >= 2:
                    n = struct.unpack_from("<H", buf)[0]
                    if len(buf) < 2 + n:
                        break
                    frame, buf = buf[:2 + n], buf[2 + n:]
                    b.sendall(frame)
                    if n and frame[2] == 0x04:
                        pushes += 1
                        if pushes == self.after:
                            b.sendall(b"\xff\xff" + bytes(16))
                            self.injected = True
                            b.sendall(buf)
                            buf = b""
                            break
        except OSError:
            pass
        finally:
            for s in (a, b):
                try:
                    s.close()
                except OSError:
                    pass

    def close(self):
        self.srv.close()


@both
def test_a_lost_boundary_in_a_stream_resyncs_as_python_does(bench, monkeypatch, tmp_path):
    """transports §5 on TCP: a length past max_frame in the middle of a stream - the connection closed and opened
    again, confirmed, the stream's subscription made again (core §10.3); the pushes that went to the closed one are
    a loss. Both clients go on to the end and say the same (on_loss continue)."""
    got = {}
    for rust in (False, True):
        proxy = _Proxy(bench(), after=20)
        try:
            req = sources.Request([("A", "GPIO10")], 1_000_000, 0, mode="stream", duration_s=1.5, on_loss="continue")
            out = _take(monkeypatch, rust, f"oep:tcp://127.0.0.1:{proxy.port}", req, tmp_path / f"f{rust}.wireskein")
        finally:
            proxy.close()
        assert proxy.injected and proxy.conns >= 2           # the connection was opened again
        head = fileformat.read_header(out)
        got[rust] = head
        assert head["complete"] is False and len(head["parts"]) >= 2
        for (_, chans) in fileformat.read_parts(out):
            a = chans[0]
            lv = np.frombuffer(fileformat.unpack(a), np.uint8)[:a.n]
            d = np.diff(lv.astype(np.int16))
            assert np.all(d != 0)                            # the bench's counter, bit 0: no sample lost inside a part
    py, rs = got[False], got[True]
    assert [x["reason"].split(" - ")[0] for x in rs["lost"]] == [x["reason"].split(" - ")[0] for x in py["lost"]]


# ---------------- a serial port (COBS, transports §4) ----------------

@pytest.fixture
def pty_bench():
    procs = []

    def start(*extra, profile="p4-x035"):
        p = subprocess.Popen([sys.executable, "-m", SERVE, "--pty", "--framing", "cobs", "--profile", profile, *extra],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        procs.append(p)
        path = re.search(r"PTY (\S+)", p.stdout.readline()).group(1)
        return f"oep:{path}"
    yield start
    for p in procs:
        p.stdin.close()
        p.wait(5)


@both
@pytest.mark.skipif(sys.platform != "linux", reason="a pty")
@pytest.mark.parametrize("fault", [(), ("--corrupt", "9"), ("--drop", "9")], ids=["clean", "a-broken-answer",
                                                                                   "a-lost-answer"])
@pytest.mark.parametrize("case", ["three", "format-1", "repeat"])
def test_a_capture_over_a_serial_port_is_the_same(case, fault, pty_bench, monkeypatch, tmp_path):
    """The virtual bench on a pty, COBS frames at the boot speed (fast=[]: no port_speed): the same file as Python's;
    a broken answer (a bad CRC) is sent again at once, a lost one after its wait (core §5.1), as Python does."""
    chans, rate, samples, kw = CASES[case]
    got = []
    for rust in (False, True):
        req = sources.Request(list(chans), rate, samples, fast=[], **kw)
        got.append(_content(_take(monkeypatch, rust, pty_bench(*fault), req, tmp_path / f"{case}-{rust}.wireskein")))
    py, rs = got
    assert rs == py


@both
@pytest.mark.skipif(sys.platform != "linux", reason="a pty")
def test_a_stream_over_a_serial_port(pty_bench, monkeypatch, tmp_path):
    chans = [("A", "GPIO10")]
    got = {}
    for rust in (False, True):
        req = sources.Request(chans, 20_000, 0, mode="stream", duration_s=0.3, fast=[])   # under the bench's pty
        out = _take(monkeypatch, rust, pty_bench(), req, tmp_path / f"s{rust}.wireskein")
        head = fileformat.read_header(out)
        assert head["complete"] is True
        a = fileformat.read_parts(out)[0][1][0]
        lv = np.frombuffer(fileformat.unpack(a), np.uint8)[:a.n]
        assert np.array_equal(lv, np.arange(a.n) & 1)
        got[rust] = head
    assert "rate" not in got[True]["meta"]["probe"]["link"] and "rate" not in got[False]["meta"]["probe"]["link"]
    assert sorted(got[True]["meta"]["probe"]) == sorted(got[False]["meta"]["probe"])


@both
def test_wireskein_rs_takes_analog_lines(bench, monkeypatch, tmp_path):
    """`wireskein-rs capture --analog ... --trigger NAME:KIND:VOLTS` as `wireskein capture`: the same file."""
    rs = _wireskein_rs()
    if rs is None:
        pytest.skip("wireskein-rs not built")
    monkeypatch.delenv(_rust.RUST_ENV, raising=False)
    args = ["--channels", "A=GPIO10", "--analog", "SQ=GPIO16,SINE=GPIO17@3", "--analog-rate", "48k",
            "--analog-samples", "64", "--rate", "20M", "--samples", "2000", "--trigger", "SQ:rise:1.0V",
            "--pretrigger", "100"]
    outs = []
    for cmd in ([sys.executable, "-m", "wireskein"], [rs]):
        out = tmp_path / f"{len(outs)}.wireskein"
        r = subprocess.run([*cmd, "capture", "--source", bench(), *args, "-o", str(out)], capture_output=True,
                           text=True)
        assert r.returncode == 0, r.stderr
        outs.append((_analog_file(out), r.stdout.splitlines()[0].split(": ", 1)[1]))
    (py, py_line), (rs, rs_line) = outs
    assert rs_line == py_line
    for k in py:
        if k != "parts":
            assert rs[k] == py[k], k
    # the analog lines the same; a logic line as many samples as Python's and they the same (wireskein-io's line has
    # no length of its own: written by wireskein-rs it runs to the part's end, the analog's, idle past its samples)
    (pt, p0, pm, ptr, pc), (rt, r0, rm, rtr, rc) = py["parts"][0], rs["parts"][0]
    assert (rt, r0, rm, rtr) == (pt, p0, pm, ptr)
    for a, b in zip(pc, rc):
        if a[1] == "analog":
            assert b == a
        else:
            assert b[0] == a[0] and b[5][:a[2]] == a[5]
