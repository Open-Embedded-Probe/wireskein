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
