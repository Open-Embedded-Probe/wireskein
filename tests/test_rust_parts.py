"""WIRESKEIN_RUST: the parts around the engine switched to Rust one at a time (wireskein._rust). Unset, nothing
changes and wireskein_core is never imported; the comparisons of Python's and Rust's answers run only where
wireskein_core (crates/wireskein-py) is installed, over the repository's examples and recorded runs."""
import builtins
import importlib.util
import json
from pathlib import Path

import numpy as np
import pytest

from wireskein import _engines, _rust, decode, verify
from wireskein._engine import fileio
from wireskein._engine.export import dumps

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "tests" / "data"
EXAMPLES = sorted((DATA / "examples").glob("*.wireskein")) + sorted((ROOT / "examples" / "protocols").glob("*.wireskein"))
HAS_CORE = importlib.util.find_spec("wireskein_core") is not None
rust_only = pytest.mark.skipif(not HAS_CORE, reason="wireskein_core (crates/wireskein-py) not installed")


@pytest.fixture
def clean(monkeypatch):
    for k in (_rust.RUST_ENV, _engines.ENGINE_ENV):
        monkeypatch.delenv(k, raising=False)
    return monkeypatch


# ---------------- the switch ----------------

def test_unset_is_python_and_never_imports_the_core(clean, tmp_path):
    real = builtins.__import__

    def no_core(name, *a, **k):
        if name == "wireskein_core":
            pytest.fail("wireskein_core imported with WIRESKEIN_RUST unset")
        return real(name, *a, **k)

    clean.setattr(builtins, "__import__", no_core)
    assert _rust.parts() == frozenset() and not _rust.on("io")
    f = DATA / "examples" / "mixed-0.wireskein"
    assert fileio.load_parts(f) and fileio.load(f)
    doc = decode.analyze_file(f)
    assert doc["parts"][0]["claims"]
    assert verify.verify(DATA / "i2c_bench" / "x035_100k")["results"]
    from wireskein import annotate, states, survey
    assert states.of_file(f)["parts"] and survey.survey_file(f)["lines"] and annotate.build_file(f)["rows"]
    for suffix in (".wireskein", ".sr", ".vcd"):
        assert decode._load(decode._save(tmp_path / f"c{suffix}", fileio.load(f))).channels


def test_parts_named(clean):
    clean.setenv(_rust.RUST_ENV, " IO , verify")
    assert _rust.parts() == {"io", "verify"}
    clean.setenv(_rust.RUST_ENV, "all")
    assert _rust.parts() == set(_rust.PARTS)
    clean.setenv(_rust.RUST_ENV, "")
    assert _rust.parts() == frozenset()
    clean.setenv(_rust.RUST_ENV, "io,gui")
    with pytest.raises(ValueError, match="gui"):
        _rust.parts()


def test_engine_part_is_the_rust_engine(clean):
    clean.setenv(_rust.RUST_ENV, "engine")
    assert _engines.engine() == "rust"
    clean.setenv(_engines.ENGINE_ENV, "rust")
    assert _engines.engine() == "rust"
    clean.setenv(_engines.ENGINE_ENV, "python")
    with pytest.raises(ValueError, match="name one engine"):
        _engines.engine()
    clean.setenv(_rust.RUST_ENV, "io")
    assert _engines.engine() == "python"


def test_a_part_without_the_core_names_it(clean):
    real = builtins.__import__

    def no_core(name, *a, **k):
        if name == "wireskein_core":
            raise ImportError("no module named wireskein_core")
        return real(name, *a, **k)

    clean.setattr(builtins, "__import__", no_core)
    clean.setenv(_rust.RUST_ENV, "io")
    with pytest.raises(RuntimeError, match="crates/wireskein-py"):
        fileio.load_parts(DATA / "examples" / "mixed-0.wireskein")


def test_options_rust_does_not_have_stay_python(clean):
    clean.setenv(_rust.RUST_ENV, "analyze")
    f = DATA / "examples" / "mixed-0.wireskein"
    base = dict(hints=None, mode="final", select=None, alternatives=False, depth="protocol", window_s=None,
                segment=None, markers_hint=None, events=False, thresholds=None, split=None, pins=None, items=None)
    assert _rust.analyzes(f, **base)
    for k, v in (("mode", "select"), ("select", ["uart"]), ("alternatives", True), ("depth", "frames"),
                 ("segment", "x"), ("thresholds", {"A0": 1.5}), ("pins", ["D0"]), ("items", "x.ndjson"),
                 ("hints", {"devices": ["i2c/**"]})):
        assert not _rust.analyzes(f, **{**base, k: v}), k
    assert _rust.analyzes(f, **{**base, "split": {"max_edges": 1000, "cuts": [5, 9]}})
    assert _rust._split_spec({"max_edges": 1000, "cuts": [5, 9]}) == "max_edges=1000,cuts=5;9"
    assert _rust._split_spec(False) == "off"


# ---------------- Python and Rust compared ----------------

def _chans(c):
    return [(x.name, int(x.initial), x.edges.dtype, x.edges.tolist(), x.step, x.phase, x.acquisition)
            for x in c.channels]


def _events(c):
    return [(e.name, e.step, e.phase, e.n, e.start_level, list(map(int, e.quanta)), list(map(int, e.levels)),
             list(map(int, e.counts)), e.acquisition) for e in c.events]


def _analog(c):
    return [(a.name, a.values.dtype, a.values.tolist(), a.rate_hz, a.t0_ticks, a.encoding, a.width, a.value_bits,
             a.zero, a.scale_nv, a.unit, a.acquisition) for a in c.analog]


def _same_view(a, b):
    assert (a.rate, a.n_samples) == (b.rate, b.n_samples)
    assert a.meta == b.meta
    assert _chans(a) == _chans(b)
    assert _events(a) == _events(b)
    assert _analog(a) == _analog(b)


def _files():
    """The repository's WireSkein files (the bench's long I2C stream left out: slow to read in Python)."""
    return sorted(f for f in [*DATA.glob("**/*.wireskein"), *EXAMPLES] if "h417_slow" not in f.parts)


@rust_only
@pytest.mark.parametrize("f", _files(), ids=lambda f: str(f.relative_to(ROOT)))
def test_io_reads_as_python(clean, f):
    py, runs = fileio.load_parts(f), fileio.load_runs(f)
    one = fileio.load(f, 0 if len(py) > 1 else None)
    clean.setenv(_rust.RUST_ENV, "io")
    rs, rruns = fileio.load_parts(f), fileio.load_runs(f)
    assert len(py) == len(rs) and len(runs) == len(rruns)
    for a, b in zip(py + runs, rs + rruns):
        _same_view(a, b)
    _same_view(one, fileio.load(f, 0 if len(py) > 1 else None))


@rust_only
@pytest.mark.parametrize("f", EXAMPLES[:4], ids=lambda f: f.name)
@pytest.mark.parametrize("suffix", [".sr", ".vcd"])
def test_read_file_of_sr_and_vcd(clean, tmp_path, f, suffix):
    """read_file of a .sr and a VCD written by Python: each line's changes as Python's readers read them."""
    import wireskein_core
    out = decode._save(tmp_path / (f.stem + suffix), fileio.load(f))
    py = decode._load(out)
    got = wireskein_core.read_file(str(out))
    assert got["kind"] == suffix[1:] and len(got["parts"]) == 1
    lines = {x["name"]: x for x in got["parts"][0]["lines"]}
    assert set(lines) == {c.name for c in py.channels}
    for c in py.channels:
        x = lines[c.name]
        assert isinstance(x["edges"], np.ndarray) and x["edges"].dtype == np.int64
        assert (int(x["initial"]), x["edges"].tolist()) == (int(c.initial), c.edges.tolist()), c.name
    assert got["parts"][0]["ticks"] == py.n_samples


@rust_only
@pytest.mark.parametrize("run", sorted(p.parent for p in DATA.glob("**/run.json")), ids=lambda p: p.name)
def test_verify_reports_as_python(clean, run):
    """The report of each recorded run: the same JSON to the byte."""
    py = verify.dumps(verify.verify(run))
    clean.setenv(_rust.RUST_ENV, "verify")
    rs = verify.dumps(verify.verify(run))
    assert rs == py


def _doc(f, **env):
    import os
    for k in (_rust.RUST_ENV, _engines.ENGINE_ENV):
        os.environ.pop(k, None)
    os.environ.update(env)
    try:
        d = json.loads(dumps(decode.analyze_file(f)))
    finally:
        for k in env:
            os.environ.pop(k, None)
    assert isinstance(d.pop("analysis_seconds"), float)
    return d


@rust_only
@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_analyze_document_as_python_with_the_rust_engine(clean, f):
    """The document of WIRESKEIN_RUST=analyze (all of it in Rust) is the one Python makes around the Rust engine
    (WIRESKEIN_ENGINE=rust): the same claims, results, reports and relations."""
    assert _doc(f, WIRESKEIN_RUST="analyze") == _doc(f, WIRESKEIN_ENGINE="rust")


@rust_only
@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_analyze_claims_the_truth_as_python(clean, f):
    """Against the Python engine (the claims' params, margins and items may differ: the engines are compared on the
    bench): the same buses confirmed on the same lines, every bus the example's truth names among them."""
    truth = json.loads(f.with_name(f.name.replace(".wireskein", ".truth.json")).read_text())
    py, rs = _doc(f), _doc(f, WIRESKEIN_RUST="analyze")

    def held(doc):
        return {(c["protocol"], frozenset(c["roles"].values())) for p in doc["parts"] for c in p["claims"]
                if c["verdict"] == "confirmed"}

    want = {(b["protocol"], frozenset(b["roles"].values())) for b in truth.get("buses", [])
            if b.get("protocol") and b.get("roles")}
    assert held(py) == held(rs) and want <= held(rs)


# ---------------- this round's parts: states, survey, write, io of a .sr / VCD, annotate ----------------

def _with(env: dict, fn):
    """fn() with WIRESKEIN_RUST / WIRESKEIN_ENGINE as env says (both cleared first), put back after."""
    import os
    keep = {k: os.environ.pop(k, None) for k in (_rust.RUST_ENV, _engines.ENGINE_ENV)}
    os.environ.update(env)
    try:
        return fn()
    finally:
        for k in (_rust.RUST_ENV, _engines.ENGINE_ENV):
            os.environ.pop(k, None)
            if keep[k] is not None:
                os.environ[k] = keep[k]


def _mixed():
    """A capture of every kind of line a file holds: a step-1 line kept as changes and one as bits, a line of its own
    step, a reduced one (any_high), lines kept as changes at a coarser step (one hiding none, a change_latch), raw and
    volts analog channels, meta of the capture and of its part, an attachment."""
    from fractions import Fraction
    from wireskein._engine.model import AnalogTrace, Capture, Channel, EventTrace
    n = 20_000
    rng = np.random.default_rng(5)
    a = Channel("A", 0, np.array([100, 200, 5000, 5003], np.int64), acquisition={"pin": "PA0"})
    clk = Channel("CLK", 1, np.cumsum(rng.integers(2, 9, 4000)).astype(np.int64))
    clk.edges = clk.edges[clk.edges < n]
    slow = Channel("S", 1, np.array([64, 320, 9600], np.int64), 32, 0)
    high = Channel("H", 0, np.array([40, 80], np.int64), 4, 0, {"reduction": "any_high", "pin": "PB1"})
    ev = EventTrace("E", 8, 0, n // 8, 0, np.array([3, 10, 11], np.int64), np.array([1, 0, 1], np.uint8),
                    np.array([1, 1, 1], np.uint8), {"pin": "PC2"})
    latch = EventTrace("L", 16, 0, n // 16, 0, np.array([2, 5, 9], np.int64), np.array([1, 1, 0], np.uint8),
                       np.array([1, 2, 1], np.uint8), {"reduction": "change_latch"})
    raw = AnalogTrace("V", (np.arange(100) * 37 % 4096).astype("<u2"), Fraction(10_000), Fraction(0), "analog", 16, 12,
                      2048.0, 805664.0, "V", {"pin": "PA1"})
    volts = AnalogTrace("T", np.linspace(0, 3.3, 50).astype(np.float32), Fraction(5000), Fraction(40), "analog-f32")
    chans = [a, clk, slow, high, ev.as_channel(), latch.as_channel()]
    meta = {"tick_hz": Fraction(1_000_000), "capture_id": "ab" * 16, "extras": {"attach/probe.json": b'{"fw": 1}'}}
    return Capture(1e6, n, [c for c in chans if c is not None], meta, [raw, volts], [ev, latch])


def _plain(c):
    """A view's meta without what names the file it was read from."""
    for k in ("file", "sr_file", "vcd_file"):
        c.meta.pop(k, None)
    return c


@rust_only
@pytest.mark.parametrize("f", EXAMPLES[:4], ids=lambda f: f.name)
@pytest.mark.parametrize("pins", [None, "two"])
def test_states_as_python(clean, f, pins):
    """Every line's stretches the same, with or without odd pulses run together."""
    from wireskein import states
    names = [c.name for c in fileio.load(f).channels][:2] if pins else None
    for max_odd in (3, None):
        def run():
            return states.of_file(f, names, max_odd=max_odd)
        assert _with({"WIRESKEIN_RUST": "states"}, run) == _with({}, run)


@rust_only
@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_survey_as_python(clean, f):
    """The overview of a WireSkein file (read a block at a time from the file), with the plan of its pieces."""
    from wireskein import survey
    for plan in (None, "max_edges=2000,min_edges=200,overlap_edges=50"):
        def run():
            return survey.survey_file(f, bins=50, top=5, plan=plan)
        assert _with({"WIRESKEIN_RUST": "survey"}, run) == _with({}, run)


@rust_only
@pytest.mark.parametrize("suffix", [".sr", ".vcd"])
def test_survey_of_sr_and_vcd(clean, tmp_path, suffix):
    """The overview of a .sr and a VCD (read whole), pins chosen: the same but for a .sr's id, which Python's block
    reader of a .sr leaves out and the Rust one reads from the session's extra."""
    from wireskein import survey
    f = EXAMPLES[0]
    out = decode._save(tmp_path / (f.stem + suffix), fileio.load(f))
    pins = [c.name for c in fileio.load(f).channels][1:3]

    def run():
        return survey.survey_file(out, pins=pins, bins=40, top=4)
    rs, py = _with({"WIRESKEIN_RUST": "survey"}, run), _with({}, run)
    if suffix == ".sr":
        assert py["capture"]["id"] is None and rs["capture"]["id"] == fileio.load(f).meta["capture_id"]
        rs["capture"]["id"] = None
    assert rs == py


@rust_only
@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_annotations_as_python_with_the_rust_engine(clean, f):
    """The annotations of WIRESKEIN_RUST=annotate are the ones Python makes around the Rust engine: the same rows,
    items, texts and details (a detail's keys of RVSWD's and SWIO's Debug Module accesses come in another order:
    Python's upper stage makes them there, the Rust one here)."""
    from wireskein import annotate

    def run():
        return json.loads(json.dumps(annotate.build_file(f)))
    assert _with({"WIRESKEIN_RUST": "annotate"}, run) == _with({"WIRESKEIN_ENGINE": "rust"}, run)


def _read_back(path):
    return _plain(_with({}, lambda: decode._load(path)))


@rust_only
@pytest.mark.parametrize("suffix", [".wireskein", ".sr", ".vcd"])
@pytest.mark.parametrize("f", ["mixed", *EXAMPLES], ids=lambda f: f if isinstance(f, str) else f.name)
def test_write_reads_back_as_python(clean, tmp_path, f, suffix):
    """A capture written by WIRESKEIN_RUST=write reads back (with Python's readers) as Python's own file does."""
    cap = _mixed() if f == "mixed" else fileio.load(f)
    if suffix != ".wireskein":
        cap.events = [t for t in cap.events if not t.hidden]
    (tmp_path / "py").mkdir()
    (tmp_path / "rs").mkdir()
    py = _with({}, lambda: decode._save(tmp_path / "py" / f"c{suffix}", cap, probe="x", start_ns=12))
    rs = _with({"WIRESKEIN_RUST": "write"}, lambda: decode._save(tmp_path / "rs" / f"c{suffix}", cap, probe="x",
                                                                 start_ns=12))
    _same_view(_read_back(rs), _read_back(py))


@rust_only
def test_write_a_line_hiding_changes(clean, tmp_path):
    """A WireSkein file keeps a line whose quanta hide changes; a .sr and a VCD refuse it either way."""
    from wireskein._engine.model import EventTrace
    cap = _mixed()
    cap.events.append(EventTrace("X", 8, 0, cap.n_samples // 8, 0, np.array([4], np.int64), np.array([0], np.uint8),
                                 np.array([2], np.uint8)))
    py = _with({}, lambda: decode._save(tmp_path / "py.wireskein", cap))
    rs = _with({"WIRESKEIN_RUST": "write"}, lambda: decode._save(tmp_path / "rs.wireskein", cap))
    _same_view(_read_back(rs), _read_back(py))
    for env in ({}, {"WIRESKEIN_RUST": "write"}):
        with pytest.raises(ValueError, match="hide changes"):
            _with(env, lambda: decode._save(tmp_path / "x.sr", cap))


@rust_only
@pytest.mark.parametrize("suffix", [".sr", ".vcd"])
@pytest.mark.parametrize("f", ["mixed", *EXAMPLES[:4]], ids=lambda f: f if isinstance(f, str) else f.name)
def test_io_reads_sr_and_vcd_as_python(clean, tmp_path, f, suffix):
    """A .sr and a VCD read by WIRESKEIN_RUST=io as Python's readers read them: lines, steps, analog, meta."""
    cap = _mixed() if f == "mixed" else fileio.load(f)
    cap.events = [t for t in cap.events if not t.hidden]
    out = decode._save(tmp_path / f"c{suffix}", cap, probe="x")
    _same_view(_with({"WIRESKEIN_RUST": "io"}, lambda: decode._load(out)), _with({}, lambda: decode._load(out)))


@rust_only
def test_io_reads_a_vcd_from_elsewhere_as_python(clean, tmp_path):
    """A VCD of another tool: the tick from the times, vectors a line a bit, x and z, an evenly spaced real and one
    that is not."""
    p = tmp_path / "x.vcd"
    p.write_text("$timescale 10 ns $end\n$scope module top $end\n$var wire 1 ! clk $end\n$var wire 4 \" bus[3:0] $end\n"
                 "$var real 64 # vin $end\n$var real 64 $ odd $end\n$upscope $end\n$enddefinitions $end\n#0\n$dumpvars\n"
                 "0!\nb0000 \"\nr1.5 #\nr0 $\n$end\n#3\n1!\nbx01z \"\nr1.25 #\n#6\n0!\nr2 #\nr1 $\n#9\n1!\nb1111 \"\n"
                 "r0.1 #\n#10\nr3 $\n")
    _same_view(_with({"WIRESKEIN_RUST": "io"}, lambda: decode._load(p)), _with({}, lambda: decode._load(p)))
