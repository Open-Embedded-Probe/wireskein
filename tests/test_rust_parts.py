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
    for k, v in (("hints", {"devices": ["i2c/**"]}), ("depth", "bits")):
        assert not _rust.analyzes(f, **{**base, k: v}), k
    for k, v in (("mode", "select"), ("select", ["uart"]), ("alternatives", True), ("depth", "frames"),
                 ("depth", "device"), ("segment", "x"), ("markers_hint", {"dialect": "heading"}), ("events", True),
                 ("thresholds", {"A0": 1.5}), ("pins", ["D0"]), ("items", "x.ndjson")):
        assert _rust.analyzes(f, **{**base, k: v}), k
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


# ---------------- round 3: decode, split, mark ----------------

def _spec_of(f):
    """A decode spec of an example: its truth's buses, or (an example without them) the buses Rust's analyze
    confirms."""
    t = f.with_name(f.name.replace(".wireskein", ".truth.json"))
    buses = json.loads(t.read_text()).get("buses") if t.exists() else None
    if buses:
        return {"buses": [{"protocol": b["protocol"], "pins": b["roles"]} for b in buses]}
    doc = _with({"WIRESKEIN_RUST": "analyze"}, lambda: decode.analyze_file(f))
    return {"buses": [{"protocol": c["protocol"], "pins": c["roles"]} for p in doc["parts"] for c in p["claims"]
                      if c["verdict"] == "confirmed"]}


def _differ(a, b, path="", tol=2e-4) -> list:
    """Where two documents differ, a float within tol of the other the same (a fit's score read through Python's
    upper stage and Rust's)."""
    if isinstance(a, dict) and isinstance(b, dict):
        return [x for k in sorted(set(a) | set(b)) for x in (
            _differ(a[k], b[k], f"{path}.{k}", tol) if k in a and k in b else [f"{path}.{k}: in one only"])]
    if isinstance(a, list) and isinstance(b, list):
        return ([f"{path}: {len(a)} / {len(b)}"] if len(a) != len(b) else []) + [
            x for i, (u, v) in enumerate(zip(a, b)) for x in _differ(u, v, f"{path}[{i}]", tol)]
    if isinstance(a, float) or isinstance(b, float):
        ok = isinstance(a, (int, float)) and isinstance(b, (int, float)) and abs(a - b) <= tol
        return [] if ok else [f"{path}: {a!r} / {b!r}"]
    return [] if a == b else [f"{path}: {a!r} / {b!r}"]


def _decode(env, f, spec, **kw):
    doc = _with(env, lambda: json.loads(dumps(decode.decode_file(f, spec, **kw))))
    doc.pop("wireskein")
    return doc


# RVSWD and SWIO: Python reads a decode spec of them with its own engine (the Rust engine's readings compared apart;
# SWIO reads the same whole)
PYTHON_SPEC = {"rvswd-v203-target-info.wireskein"}
PIECES = "max_edges=400,min_edges=50,overlap_edges=40"


@rust_only
@pytest.mark.parametrize("split", [None, "off", PIECES])
@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_decode_document_as_python_with_the_rust_engine(clean, f, split):
    """The decode document of WIRESKEIN_RUST=decode (all of it in Rust) is the one Python makes around the Rust
    engine: the buses' results, readings, unread stretches, devices, undeclared pins and claims; whole, in pieces (a
    fit's score may differ in its last digit: the upper stage's text reading of a UART piece, Python's or Rust's)."""
    if f.name in PYTHON_SPEC or (f.name.startswith("swio") and split == PIECES):
        pytest.skip("read by the Python engine in Python (test_decode_rvswd_against_the_python_engine)")
    spec = _spec_of(f)
    assert _differ(_decode({"WIRESKEIN_RUST": "decode"}, f, spec, split=split),
                   _decode({"WIRESKEIN_ENGINE": "rust"}, f, spec, split=split)) == []


@rust_only
@pytest.mark.parametrize("variant", ["transport", "frames", "protocol", "layers", "pins", "lines", "params"])
def test_decode_spec_options_as_python(clean, variant):
    f = DATA / "examples" / "mixed-0.wireskein"
    spec, kw = _spec_of(f), {}
    if variant in ("transport", "frames", "protocol"):
        spec["depth"] = variant
    elif variant == "layers":
        spec["buses"] = [{**b, "layers": ["*"]} for b in spec["buses"]]
    elif variant == "pins":
        spec, kw = {"buses": spec["buses"][:1]}, {"pins": ["D6", "D11", "D5"]}
    elif variant == "lines":
        spec["lines"] = ["D5", "D9"]
    else:
        spec["buses"] = [{**b, "params": {"mode": 3}} if b["protocol"] == "spi" else b for b in spec["buses"]]
    assert _decode({"WIRESKEIN_RUST": "decode"}, f, spec, **kw) == _decode({"WIRESKEIN_ENGINE": "rust"}, f, spec, **kw)


def _rows(p):
    return sorted(json.dumps(json.loads(x), sort_keys=True) for x in p.read_text().splitlines())


@rust_only
@pytest.mark.parametrize("split", [None, PIECES])
def test_decode_items_written_out_as_python(clean, tmp_path, split):
    """Items written out as the pieces go: the same rows, the same document - the reading's counts the pieces'
    summed (Python's were 0 before it was fixed: its frames went to the file before they were counted; and it left its
    internal _owned on the buses of a part of one piece)."""
    f = DATA / "examples" / "mixed-0.wireskein"
    spec = _spec_of(f)
    rs = _decode({"WIRESKEIN_RUST": "decode"}, f, spec, split=split, items=tmp_path / "rs.ndjson")
    py = _decode({"WIRESKEIN_ENGINE": "rust"}, f, spec, split=split, items=tmp_path / "py.ndjson")
    assert _rows(tmp_path / "rs.ndjson") == _rows(tmp_path / "py.ndjson")
    for d in (rs, py):
        for b in d["parts"][0]["buses"]:
            b["result"]["items"]["file"] = "-"
    assert rs == py
    held = _decode({"WIRESKEIN_ENGINE": "rust"}, f, spec, split=split)
    for a, b in zip(py["parts"][0]["buses"], held["parts"][0]["buses"]):
        assert "_owned" not in a
        assert a["reading"]["frames"] >= b["reading"]["frames"] > 0      # (pieces overlap: counted in each)
        assert sum(a["result"]["items"]["counts"].values()) > 0


@rust_only
def test_decode_buses_sharing_lines_with_the_rust_engine(clean, tmp_path):
    """Buses sharing lines (one SPI bus, a target on each CS): Python reads them with its own engine, Rust with the
    Rust engine's bus of targets - each target's bytes, its shared and unread stretches, its reading the same (the
    transfers start at the select's edge in the Rust engine, at the first clock in Python's)."""
    import test_spi_shared as shared
    cap, spec, sent = shared._bus(n_cs=3)
    p = decode._save(tmp_path / "c.wireskein", cap)
    rs = _decode({"WIRESKEIN_RUST": "decode"}, p, spec)
    py = _decode({}, p, spec)
    keys = ("frames", "ok", "unread", "shared", "shared_edges")
    for k, (a, b) in enumerate(zip(rs["parts"][0]["buses"], py["parts"][0]["buses"])):
        assert [x["MOSI"] for x in a["result"]["transfers"]] == [f[0].hex() for f in sent[k]]
        assert [x["MISO"] for x in a["result"]["transfers"]] == [x["MISO"] for x in b["result"]["transfers"]]
        assert a["fit"]["claimed"] and a["uninterpreted"] == b["uninterpreted"] == []
        assert [(x["edges"], x["bus"]) for x in a["shared"]] == [(x["edges"], x["bus"]) for x in b["shared"]]
        assert {x: a["reading"][x] for x in keys} == {x: b["reading"][x] for x in keys}
        assert "stopped" not in a["reading"]


@rust_only
def test_decode_rvswd_against_the_python_engine(clean):
    """RVSWD, which Python decodes with its own engine: the Rust engine reads the same Debug Module accesses (the
    V203's last read with its value, which the Python engine leaves out) and as many frames or more."""
    f = DATA / "examples" / "rvswd-v203-target-info.wireskein"
    spec = _spec_of(f)
    rs, py = _decode({"WIRESKEIN_RUST": "decode"}, f, spec), _decode({}, f, spec)
    a, b = rs["parts"][0]["buses"][0], py["parts"][0]["buses"][0]
    assert [(y["op"], y["addr"]) for y in a["result"]["dm"]] == [(y["op"], y["addr"]) for y in b["result"]["dm"]]
    assert sum(y.get("value") is not None for y in a["result"]["dm"]) >= sum(
        y.get("value") is not None for y in b["result"]["dm"])
    assert a["reading"]["frames"] >= b["reading"]["frames"] and a["reading"]["ok"] >= b["reading"]["ok"]
    # SWIO in short pieces (the Python engine reads fewer of them)
    f = DATA / "examples" / "swio-v003-target-info.wireskein"
    spec = _spec_of(f)
    rs, py = _decode({"WIRESKEIN_RUST": "decode"}, f, spec, split=PIECES), _decode({}, f, spec, split=PIECES)
    a, b = rs["parts"][0]["buses"][0], py["parts"][0]["buses"][0]
    assert a["reading"]["frames"] >= b["reading"]["frames"] and a["reading"]["ok"] >= b["reading"]["ok"]
    assert _differ(rs["parts"][0]["split"]["pieces"], py["parts"][0]["split"]["pieces"]) == []


@rust_only
def test_decode_falls_back_for_device_patterns(clean):
    f = DATA / "examples" / "mixed-0.wireskein"
    spec = _spec_of(f)
    assert _with({"WIRESKEIN_RUST": "decode"}, lambda: _rust.decodes(f, spec, None))
    assert not _with({"WIRESKEIN_RUST": "decode"}, lambda: _rust.decodes(f, {**spec, "devices": ["i2c/**"]}, None))
    assert not _with({"WIRESKEIN_RUST": "decode"}, lambda: _rust.decodes(f, spec, 3.5))


def _blocks(src):
    got = {k: [] for k in src.names}
    for _, es in src.blocks():
        for k, e in es.items():
            got[k] += np.asarray(e).tolist()
    return src.names, src.n, src.rate, src.initial, got


@rust_only
@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_split_sources_as_python(clean, tmp_path, f):
    """The sources a long capture is read from a block at a time (a WireSkein file's part, a .sr): the same lines,
    ticks, rate, levels and changes as Python's FileSource and SrSource, pins given or not."""
    from wireskein import split
    names = [c.name for c in fileio.load(f).channels]
    for pins in (None, names[:2][::-1]):
        a, b = _rust.file_source(f, 0, pins), split.FileSource(f, 0, pins)
        assert _blocks(a) == _blocks(b) and a.meta == b.meta
        assert _blocks(a) == _blocks(b)                  # read again
    sr = decode._save(tmp_path / "c.sr", fileio.load(f))
    for pins in (None, names[:2][::-1]):
        assert _blocks(_rust.sr_source(sr, pins)) == _blocks(split.SrSource(sr, pins))
    with pytest.raises(ValueError, match="pins not in the capture"):
        _rust.sr_source(sr, ["nope"])


@rust_only
@pytest.mark.parametrize("f", EXAMPLES[:4], ids=lambda f: f.name)
def test_split_part_decodes_and_surveys_as_python(clean, tmp_path, f):
    """decode's and survey's Python ways read from the Rust sources give the same documents."""
    from wireskein import survey
    spec = _spec_of(f)
    sr = decode._save(tmp_path / "c.sr", fileio.load(f))
    for p in (f, sr):
        assert _decode({"WIRESKEIN_RUST": "split"}, p, spec) == _decode({}, p, spec)
        assert _with({"WIRESKEIN_RUST": "split"}, lambda: survey.survey_file(p, plan=False)) == \
            _with({}, lambda: survey.survey_file(p, plan=False))


@rust_only
@pytest.mark.parametrize("run", sorted(p.parent for p in DATA.glob("**/run.json")), ids=lambda p: p.name)
def test_mark_writes_the_markers_as_python(clean, tmp_path, run):
    """verify's markers written by wireskein-verify (WIRESKEIN_RUST=mark): the same markers in every capture file as
    Python's _mark writes, the markers of others kept, verify's earlier ones replaced (not doubled)."""
    import shutil
    from wireskein import _fileformat as ff
    rep = _with({}, lambda: verify.verify(run))
    files = sorted({c["file"] for c in json.loads((run / "run.json").read_text())["captures"]})
    out = {}
    for rust in (False, True):
        d = tmp_path / ("rs" if rust else "py")
        shutil.copytree(run, d)
        for f in files:
            ff.add_markers(d / f, [{"t": 5, "label": "mine"}], "me")
        for _ in range(2):
            _rust.mark(d, rep) if rust else verify._mark(d, rep)
        out[rust] = {f: ff.markers(d / f) for f in files}
        assert all(any(m["by"] == "me" for m in ms) for ms in out[rust].values())
    assert out[True] == out[False]
    for env in ({"WIRESKEIN_RUST": "mark"},):           # through verify(mark=True)
        d = tmp_path / "v"
        shutil.copytree(run, d)
        _with(env, lambda: verify.verify(d, mark=True))
        assert {f: ff.markers(d / f) for f in files} == {f: [m for m in ms if m["by"] != "me"]
                                                          for f, ms in out[False].items()}


# ---------------- round 4: analyze's pins, items, thresholds and segments; align ----------------

def _adoc(env, f, **kw):
    d = _with(env, lambda: json.loads(dumps(decode.analyze_file(f, **kw))))
    d.pop("analysis_seconds")
    d.pop("wireskein")
    return d


def _confirmed_pins(f):
    doc = _adoc({"WIRESKEIN_ENGINE": "rust"}, f)
    return sorted({p for c in doc["parts"][0]["claims"] if c["verdict"] == "confirmed" for p in c["roles"].values()})


@rust_only
@pytest.mark.parametrize("split", [None, PIECES])
@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_analyze_pins_and_items_as_python(clean, tmp_path, f, split):
    """analyze with pins (only those channels read) and with items (a part read in pieces writing them out as it
    goes): the document Python makes around the Rust engine, the same rows written; whole and in short pieces (the
    pieces read as decode reads a spec's buses, as Python's _analyze_split does; a piece's fit may differ in its last
    digit, the upper stage's text reading of a UART piece, as decode's)."""
    pins = _confirmed_pins(f)[:3]
    assert _differ(_adoc({"WIRESKEIN_RUST": "analyze"}, f, pins=pins, split=split),
                   _adoc({"WIRESKEIN_ENGINE": "rust"}, f, pins=pins, split=split)) == []
    assert _differ(_adoc({"WIRESKEIN_RUST": "analyze"}, f, split=split),
                   _adoc({"WIRESKEIN_ENGINE": "rust"}, f, split=split)) == []
    rs = _adoc({"WIRESKEIN_RUST": "analyze"}, f, split=split, items=tmp_path / "rs.ndjson")
    py = _adoc({"WIRESKEIN_ENGINE": "rust"}, f, split=split, items=tmp_path / "py.ndjson")
    for d in (rs, py):
        for c in d["parts"][0]["claims"]:
            if "items" in c.get("result", {}):
                c["result"]["items"]["file"] = "-"
    assert _differ(rs, py) == []
    assert _rows(tmp_path / "rs.ndjson") == _rows(tmp_path / "py.ndjson")


def _adc_uart(tmp_path):
    """A UART seen only by an ADC (and a copy of it backwards on another, its first sample half a tick late)."""
    from fractions import Fraction
    from wireskein import _fileformat as ff
    rng = np.random.default_rng(7)
    rate, baud = 1_000_000, 57_600
    bits = [1] * 400
    for byte in b"hello adc\n" * 6:
        bits += [0] + [(byte >> k) & 1 for k in range(8)] + [1, 1]
    bits += [1] * 400
    per = rate / baud
    idx = (np.arange(int(len(bits) * per)) / per).astype(int)
    v = np.array(bits)[idx] * 3.3 + rng.normal(0, 0.05, len(idx))
    p = tmp_path / "a.wireskein"
    ff.write(p, rate, [ff.Channel("IDLE", ff.pack(bytes(1000)), 1000), ff.analog_volts("TX", v.tolist(), rate),
                       ff.analog_volts("TX2", v[::-1].tolist(), rate, t0_ticks=Fraction(7, 2))])
    return p


@rust_only
@pytest.mark.parametrize("th", [{"TX": 1.65}, {"TX": (1.0, 2.3)}, {"TX": (1.0, 2.3), "TX2": 1.5}], ids=str)
@pytest.mark.parametrize("pins", [None, ["TX"]], ids=str)
def test_analyze_thresholds_as_python(clean, tmp_path, th, pins):
    """Analog channels read as logic at a threshold (as_logic): the same document as Python's around the Rust
    engine (Rust's also names the channels it did not read as logic, skipped_channels); the same errors."""
    p = _adc_uart(tmp_path)
    rs = _adoc({"WIRESKEIN_RUST": "analyze"}, p, thresholds=th, pins=pins)
    py = _adoc({"WIRESKEIN_ENGINE": "rust"}, p, thresholds=th, pins=pins)
    skipped = rs["parts"][0].pop("skipped_channels", [])
    assert rs == py
    assert {c["name"] for c in skipped} == ({"TX", "TX2"} - set(th)) & set(pins or {"TX", "TX2"})
    assert any("hello adc" in json.dumps(c["result"]) for c in rs["parts"][0]["claims"])
    for bad in ({"NOPE": 1.0}, {"IDLE": 1.0}, {"TX": (2.0, 1.0)}):
        errs = []
        for env in ({"WIRESKEIN_RUST": "analyze"}, {"WIRESKEIN_ENGINE": "rust"}):
            with pytest.raises(ValueError) as e:
                _adoc(env, p, thresholds=bad)
            errs.append(str(e.value))
        assert errs[0] == errs[1]


def _uart_edges(text, baud=115_200, rate=2_000_000, t=200.0):
    bit = rate / baud
    edges, lv = [], 1
    for b in text:
        for x in [0] + [(b >> i) & 1 for i in range(8)] + [1, 1]:
            if x != lv:
                edges.append(int(round(t)))
                lv = x
            t += bit
        t += 3 * bit
    return edges, t


def _marked(tmp_path):
    """A UART with heading markers (a level skipped, a heading that only closes, text between) beside another UART
    of text lines."""
    from fractions import Fraction
    from wireskein import capture
    e, t = _uart_edges(b"boot\n# session\n## pwm\nPING\nPONG\n### deep\n##\n## pwm\nOK\n#### skip\n# end\nbye\n")
    other, t2 = _uart_edges(b"x=1\ny=2\nz=3\nw=4\n" * 3, baud=9600, t=500.0)
    n = int(max(t, t2)) + 400
    cap = capture.Capture(Fraction(2_000_000), [capture.Part(n, [capture.logic("TX", 1, e, n),
                                                                 capture.logic("LOG", 1, other, n)])])
    return capture.write(tmp_path / "s.wireskein", cap)


@rust_only
@pytest.mark.parametrize("segment", ["session", "session/pwm", "session/pwm[1]", "*deep", "*skip", "end"])
@pytest.mark.parametrize("kw", [{}, {"events": True}, {"mode": "all"}, {"markers_hint": {"pin": "TX"}}], ids=str)
def test_analyze_segment_as_python(clean, tmp_path, segment, kw):
    """A marker segment (Python reads it with its own engine): the same segment (path, raw marker, issues), window,
    events and claims (a margin in its last digit, 0.6999 / 0.7; in mode all the engines' evidence differs: compared on
    the bench); the same errors."""
    p = _marked(tmp_path)
    a = _adoc({"WIRESKEIN_RUST": "analyze"}, p, segment=segment, **kw)["parts"][0]
    b = _adoc({}, p, segment=segment, **kw)["parts"][0]
    if kw.get("mode") == "all":
        for x in (a, b):
            for c in x["claims"]:
                c.pop("evidence")
    assert _differ(a, b) == []
    assert a["segment"]["path"] in ("session", "session/pwm", "session/pwm[1]", "session/pwm/deep",
                                    "session/pwm[1]/skip", "end")
    if segment == "*skip":
        assert a["segment"]["issues"] == [{"kind": "level_skip", "from": 2, "to": 4}]
    for bad in ({"segment": "nope"}, {"segment": "x", "markers_hint": {"pin": "LOG"}}):
        errs = []
        for env in ({"WIRESKEIN_RUST": "analyze"}, {}):
            with pytest.raises(ValueError) as e:
                _adoc(env, p, **bad)
            errs.append(str(e.value))
        assert errs[0] == errs[1]


def _near(a: dict, b: dict, rel=1e-9, abs_=1e-8):
    """Where two alignment entries differ beyond rounding (a least squares fit by numpy's lstsq and by its centered
    form)."""
    out = []
    for k in sorted(set(a) | set(b)):
        x, y = a.get(k), b.get(k)
        if isinstance(x, float) and isinstance(y, float):
            if not (x == y or abs(x - y) <= abs_ + rel * max(abs(x), abs(y))):
                out.append(f"{k}: {x!r} / {y!r}")
        elif x != y:
            out.append(f"{k}: {x!r} / {y!r}")
    return out


@rust_only
@pytest.mark.parametrize("off, ppm", [(200, 0), (-150, 1500), (37.3, -800), (201.25, 1500)])
def test_align_find_as_python(clean, tmp_path, off, ppm):
    """align.find by wireskein-analysis (WIRESKEIN_RUST=align): the same matched edges and counts, the offset, scale
    and their uncertainty as Python's to rounding (numpy's lstsq against the centered least squares)."""
    import test_align as ta
    from wireskein import align
    cap = fileio.load(ta.make(tmp_path, off, ppm))
    for kw in ({"threshold": (1.0, 2.3)}, {"threshold": 1.65}, {"threshold": 1.65, "window_ticks": 3000},
               {"threshold": 1.65, "apply_to": ["SYNC_A"]}):
        py = _with({}, lambda: align.find(cap, "SYNC", "SYNC_A", **kw))
        rs = _with({"WIRESKEIN_RUST": "align"}, lambda: align.find(cap, "SYNC", "SYNC_A", **kw))
        assert py["format"] == rs["format"] and list(py["channels"]) == list(rs["channels"])
        for k in py["channels"]:
            assert _near(py["channels"][k], rs["channels"][k]) == [], kw


@rust_only
def test_align_errors_as_python(clean, tmp_path):
    import test_align as ta
    from wireskein import align
    cap = fileio.load(ta.make(tmp_path, 200, 0, periodic=True, uncertainty_ns=5_000_000))
    for kw in ({"threshold": 1.65}, {"threshold": 1.65, "apply_to": ["NOPE"]}, {"threshold": 9.0}):
        errs = []
        for env in ({}, {"WIRESKEIN_RUST": "align"}):
            with pytest.raises(ValueError) as e:
                _with(env, lambda: align.find(cap, "SYNC", "SYNC_A", **kw))
            errs.append(str(e.value))
        assert errs[0] == errs[1], kw
    py = _with({}, lambda: align.find(cap, "SYNC", "SYNC_A", threshold=1.65, window_ticks=3_000))
    rs = _with({"WIRESKEIN_RUST": "align"}, lambda: align.find(cap, "SYNC", "SYNC_A", threshold=1.65,
                                                                window_ticks=3_000))
    assert _near(py["channels"]["SYNC_A"], rs["channels"]["SYNC_A"]) == []


@rust_only
@pytest.mark.parametrize("start, ppm", [(3700.0, 80.0), (-1200.0, -40.0)])
def test_align_between_as_python(clean, tmp_path, start, ppm):
    """align.between (another probe's capture onto this one's ticks): as Python's to rounding; an analog via too."""
    import test_align as ta
    from wireskein import align
    a, b = ta.two_probes(tmp_path, start_us=start, ppm=ppm)
    A, B = fileio.load(a), fileio.load(b)
    py = _with({}, lambda: align.between(A, "SYNC", B, "SYNC"))
    rs = _with({"WIRESKEIN_RUST": "align"}, lambda: align.between(A, "SYNC", B, "SYNC"))
    assert _near(py, rs) == []
    d = tmp_path / "an"
    d.mkdir()
    cap = fileio.load(ta.make(d, 200, 1500))
    kw = {"threshold": 1.65, "max_ppm": 2000}
    py = _with({}, lambda: align.between(cap, "SYNC", cap, "SYNC_A", **kw))
    rs = _with({"WIRESKEIN_RUST": "align"}, lambda: align.between(cap, "SYNC", cap, "SYNC_A", **kw))
    assert _near(py, rs) == []
    for args in (("SYNC_A", "SYNC"), ("SYNC", "SYNC_A"), ("SYNC", "NOPE")):
        errs = []
        for env in ({}, {"WIRESKEIN_RUST": "align"}):
            with pytest.raises(ValueError) as e:
                _with(env, lambda: align.between(cap, args[0], cap, args[1]))
            errs.append(str(e.value))
        assert errs[0] == errs[1], args
