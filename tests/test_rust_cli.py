"""`wireskein-rs` (crates/wireskein-cli) against `wireskein` (src/wireskein/cli.py): the same subcommands run side by
side on the repository's examples, recorded runs and synthetic captures, their stdout, stderr, exit codes and the files
they write compared (bench/notes/2026-11-24-rust-plan.ja.md §34). Skipped where wireskein-rs is not built (cargo build
-p wireskein-cli; WIRESKEIN_RS names the binary); the subcommands that analyze run only where wireskein_core is
installed too (Python's side reads with the Rust engine there, WIRESKEIN_ENGINE=rust).

A JSON document on stdout is compared as data (the Rust command line writes its keys sorted: serde_json's map; the
values, floats as Python writes them, are the same); "analysis_seconds" is a time and left out."""
import contextlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
from fractions import Fraction
from pathlib import Path

import numpy as np
import pytest

from wireskein import _engines, _fileformat as fileformat, _rust, cli
from wireskein.decode import _load

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "tests" / "data"
EXAMPLES = sorted((DATA / "examples").glob("*.wireskein")) + sorted((ROOT / "examples" / "protocols").glob("*.wireskein"))
HAS_CORE = importlib.util.find_spec("wireskein_core") is not None


def _binary() -> str | None:
    if os.environ.get("WIRESKEIN_RS"):
        return os.environ["WIRESKEIN_RS"]
    dirs = [Path(os.environ["CARGO_TARGET_DIR"])] if os.environ.get("CARGO_TARGET_DIR") else []
    for d in [*dirs, ROOT / "target"]:
        for kind in ("release", "debug"):
            if (d / kind / "wireskein-rs").is_file():
                return str(d / kind / "wireskein-rs")
    return shutil.which("wireskein-rs")


RS = _binary()
pytestmark = pytest.mark.skipif(RS is None, reason="wireskein-rs not built (cargo build -p wireskein-cli)")
engine = pytest.mark.skipif(not HAS_CORE, reason="wireskein_core (crates/wireskein-py) not installed")
CMDS = ("analyze", "decode", "segments", "survey", "states", "convert", "info", "annotate", "align")


@pytest.fixture
def env(monkeypatch):
    for k in (_rust.RUST_ENV, _engines.ENGINE_ENV):
        monkeypatch.delenv(k, raising=False)
    return monkeypatch


def py(*args) -> tuple[str, str, int]:
    """`wireskein ARGS` in this process: (stdout, stderr, exit code)."""
    out, err = io.StringIO(), io.StringIO()
    code = 0
    old = sys.argv
    sys.argv = ["wireskein", *map(str, args)]
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                cli.main()
            except SystemExit as e:
                code = e.code if isinstance(e.code, int) else 1
                if not isinstance(e.code, int) and e.code is not None:
                    err.write(f"{e.code}\n")
    finally:
        sys.argv = old
    for c in CMDS:                       # the program's name in its messages
        err = io.StringIO(err.getvalue().replace(f"wireskein {c}:", f"wireskein-rs {c}:"))
    return out.getvalue(), err.getvalue(), code


def rs(*args) -> tuple[str, str, int]:
    r = subprocess.run([RS, *map(str, args)], capture_output=True, text=True, cwd=ROOT)
    return r.stdout, r.stderr, r.returncode


def _json(text):
    try:
        d = json.loads(text)
    except ValueError:
        return None
    if isinstance(d, dict):
        d.pop("analysis_seconds", None)
    return d


def same(*args, stdout_json=False):
    """Both commands' stdout (bytes, or as JSON data), stderr and exit code the same."""
    p, r = py(*args), rs(*args)
    assert p[2] == r[2], (p, r)
    assert p[1] == r[1]
    if p[0] != r[0]:
        assert stdout_json and _json(p[0]) is not None and _json(p[0]) == _json(r[0]), (p[0][:2000], r[0][:2000])
    return p


# ---------------- what reads no claims ----------------

def test_version():
    p, r = py("--version"), rs("--version")
    assert p[0].replace("wireskein ", "") == r[0].replace("wireskein-rs ", "") and p[2] == r[2] == 0


@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_info(f):
    same("info", f.relative_to(ROOT))


def _rich(tmp_path) -> Path:
    """A WireSkein file of every kind of channel (step-1 changes and bits, a line of its own step, any_high, events at
    a coarser step, change_latch, raw and volts analog with clipped samples), meta of the capture and its parts, three
    parts with a loss, attachments and notes."""
    n = 20_000
    rng = np.random.default_rng(5)
    level = np.zeros(n, np.uint8)
    level[100:200] = 1
    level[5000:5003] = 1
    clk = (np.cumsum(rng.integers(1, 3, n)) % 2).astype(np.uint8)
    chans = [fileformat.Channel("A", fileformat.pack(level.tobytes()), n, acquisition={"pin": "PA0"}),
             fileformat.Channel("CLK", fileformat.pack(clk.tobytes()), n),
             fileformat.Channel("S", fileformat.pack(bytes([1, 0, 0, 1] * 25)), 100, 32, 0),
             fileformat.analog_volts("T", np.linspace(0, 3.3, 50).tolist(), Fraction(5000), t0_ticks=Fraction(40))]
    raw = (np.arange(100) * 37 % 4096).astype("<u2")
    raw[:3] = 0
    raw[3:5] = 4095
    chans.append(fileformat.AnalogChannel("V", raw.tobytes(), 100, Fraction(10_000), Fraction(0), "analog", 16, 12,
                                          2048, 805664, "V", {"pin": "PA1"}))
    p = tmp_path / "rich.wireskein"
    fileformat.write(p, 1_000_000, chans, probe={"model": "x"}, start_ns=123_456_789, start_uncertainty_ns=2500,
                     trigger_tick=1500, time_base_slipped=True, label="ünïcode")
    fileformat.attach(p, "probe.json", {"fw": 1})
    fileformat.attach(p, "readme.txt", "hello")
    fileformat.note(p, "first")
    fileformat.note(p, {"k": [1, 2.5]}, kind="analysis")
    return p


def _parts(tmp_path) -> Path:
    """Three parts of a capture that lost data, as segments, each with meta of its own."""
    from wireskein import capture as wc
    n = 4000
    parts = [wc.Part(n, [wc.logic("D0", 0, [100 * (k + 1), 100 * (k + 1) + 50], n)], meta={"start_ns": 1000 * k})
             for k in range(3)]
    c = wc.Capture(Fraction(1_000_000), parts, layout="segments",
                   lost=[{"after_part": 0, "reason": "a test", "ticks": 7, "stopped": False},
                         {"after_part": 1, "reason": "again", "stopped": True}])
    return wc.write(tmp_path / "parts.wireskein", c)


def test_info_of_every_kind_of_channel(tmp_path):
    p = _rich(tmp_path)
    out = same("info", p)[0]
    assert "clipped" in out and "note 2" in out and "attach/readme.txt" in out and "trigger_tick" in out


def test_info_of_parts_with_a_loss(tmp_path):
    out = same("info", _parts(tmp_path))[0]
    assert "INCOMPLETE" in out and "part 2" in out


def test_info_of_what_is_not_a_wireskein_file(tmp_path):
    (tmp_path / "x.wireskein").write_text("nothing")
    p, r = py("info", tmp_path / "x.wireskein"), rs("info", tmp_path / "x.wireskein")
    assert p[2] == r[2] == 1 and p[0] == r[0] == ""


@pytest.mark.parametrize("args", [
    [], ["--json"], ["--plan"], ["--plan", "--split", "max_edges=400", "--json"], ["--pins", "D1,D2", "--bins", "50",
                                                                                     "--top", "3"]])
@pytest.mark.parametrize("f", EXAMPLES[:2] + EXAMPLES[4:5], ids=lambda f: f.name)
def test_survey(f, args):
    if "--pins" in args and f.parent.name != "examples":
        args = ["--bins", "50", "--top", "3"]
    same("survey", f.relative_to(ROOT), *args)


def test_survey_out(tmp_path):
    f = EXAMPLES[0].relative_to(ROOT)
    same("survey", f, "--json", "--out", tmp_path / "s.json")
    p = json.loads((tmp_path / "s.json").read_text())
    rs("survey", f, "--out", tmp_path / "r.json")
    assert json.loads((tmp_path / "r.json").read_text()) == p


@pytest.mark.parametrize("args", [[], ["--json"], ["--max-odd", "0"], ["--tol", "0.05", "--min-periods", "5"]])
@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_states(f, args):
    same("states", f.relative_to(ROOT), *args)


def test_states_pins_and_part(tmp_path):
    f = EXAMPLES[0].relative_to(ROOT)
    same("states", f, "--pin", "D1", "--pin", "D3", "--part", "0")
    same("states", _rich(tmp_path), "--json")


@pytest.mark.parametrize("args", [[], ["--json"], ["--section", "*", "--json"], ["--section", "nothing*"]])
@pytest.mark.parametrize("run", sorted(p.parent for p in (DATA / "i2c_bench").glob("*/run.json"))[:3],
                         ids=lambda p: p.name)
def test_states_of_a_run(run, args):
    same("states", run.relative_to(ROOT), *args)


def _view(path):
    c = _load(path)
    meta = {k: v for k, v in c.meta.items() if k not in ("file", "sr_file", "vcd_file", "extras", "capture_id")}
    return (c.rate, c.n_samples, meta,
            [(x.name, int(x.initial), list(map(int, x.edges)), x.step, x.phase, x.acquisition) for x in c.channels],
            [(a.name, a.encoding, np.asarray(a.values, float).tolist(), a.rate_hz, a.t0_ticks) for a in c.analog],
            sorted((c.meta.get("extras") or {}).items()))


CONVERTS = [("examples/mixed-0", ".wireskein", []), ("examples/mixed-0", ".sr", []), ("examples/mixed-0", ".vcd", []),
            ("protocols/demo_link_p4", ".wireskein", ["--pins", "sck,mosi"]),
            ("protocols/demo_link_p4", ".sr", ["--from", "0.01", "--to", "0.02"]),
            ("protocols/demo_cmd_p4", ".vcd", ["--from", "0.05"]),
            ("examples/rvswd-v203-target-info", ".wireskein", ["--to", "0.001"]),
            ("examples/mixed-1", ".wireskein", ["--pins", "nope"]), ("examples/mixed-1", ".wireskein", ["--from", "5"])]


@pytest.mark.parametrize("src, suffix, args", CONVERTS, ids=lambda x: str(x))
def test_convert(tmp_path, src, suffix, args):
    f = next(e for e in EXAMPLES if f"{e.parent.name}/{e.stem}" == src).relative_to(ROOT)
    a, b = tmp_path / f"py{suffix}", tmp_path / f"rs{suffix}"
    p, r = py("convert", f, a, *args), rs("convert", f, b, *args)
    assert (p[0].replace(str(a), "OUT"), p[1], p[2]) == (r[0].replace(str(b), "OUT"), r[1], r[2])
    assert a.exists() == b.exists()
    if a.exists():
        assert _view(a) == _view(b)


@pytest.mark.parametrize("suffix", [".wireskein", ".sr", ".vcd"])
def test_convert_every_kind_of_channel(tmp_path, suffix):
    f = _rich(tmp_path)
    a, b = tmp_path / f"py{suffix}", tmp_path / f"rs{suffix}"
    p, r = py("convert", f, a), rs("convert", f, b)
    assert (p[0].replace(str(a), "OUT"), p[1], p[2]) == (r[0].replace(str(b), "OUT"), r[1], r[2])
    if suffix == ".wireskein" or p[2] == 0:
        assert _view(a) == _view(b)


def test_convert_from_sr_and_a_part(tmp_path):
    f = EXAMPLES[0]
    sr = tmp_path / "m.sr"
    py("convert", f, sr)
    a, b = tmp_path / "a.wireskein", tmp_path / "b.wireskein"
    p, r = py("convert", sr, a), rs("convert", sr, b)
    assert (p[0].replace(str(a), "OUT"), p[2]) == (r[0].replace(str(b), "OUT"), r[2])
    assert _view(a)[3] == _view(b)[3]
    parts = _parts(tmp_path)
    for args in ([], ["--part", "1"]):
        a, b = tmp_path / "pa.wireskein", tmp_path / "pb.wireskein"
        p, r = py("convert", parts, a, *args), rs("convert", parts, b, *args)
        assert (p[0].replace(str(a), "OUT"), p[1], p[2]) == (r[0].replace(str(b), "OUT"), r[1], r[2])
        if p[2] == 0:
            assert _view(a) == _view(b)


def _close(a, b, tol=1e-6):
    if isinstance(a, dict):
        return a.keys() == b.keys() and all(_close(a[k], b[k], tol) for k in a)
    if isinstance(a, list):
        return len(a) == len(b) and all(_close(x, y, tol) for x, y in zip(a, b))
    if isinstance(a, float):
        return a == b or abs(a - b) <= tol * max(1.0, abs(a))
    return a == b


def test_align(tmp_path):
    from test_align import make, two_probes
    p = make(tmp_path, 200, 0)
    q = tmp_path / "q.wireskein"
    shutil.copy(p, q)
    out = same("align", p, "--reference", "SYNC", "--via", "SYNC_A", "--threshold", "1.0,2.3", "--save")[0]
    rs("align", q, "--reference", "SYNC", "--via", "SYNC_A", "--threshold", "1.0,2.3", "--save")
    assert "applies to: SYNC_A, VBUS" in out
    assert _close(json.loads(fileformat.attachments(p)["alignment.json"]),
                  json.loads(fileformat.attachments(q)["alignment.json"]))
    same("align", p, "--reference", "SYNC", "--via", "SYNC_A", "--threshold", "1.65", "--max-offset", "300us",
         "--apply-to", "SYNC_A")
    for bad in (["--via", "VBUS"], ["--via", "NOPE", "--threshold", "1.6"], ["--via", "SYNC_A"]):
        p_, r_ = py("align", p, "--reference", "SYNC", *bad), rs("align", p, "--reference", "SYNC", *bad)
        assert p_[2] == r_[2] == 1 and p_[0] == r_[0]
    a, b = two_probes(tmp_path, start_us=-1200.0, ppm=-40.0)
    b2 = tmp_path / "B2.wireskein"
    shutil.copy(b, b2)
    same("align", b, "--to", a, "--reference", "SYNC", "--via", "SYNC", "--save")
    rs("align", b2, "--to", a, "--reference", "SYNC", "--via", "SYNC", "--save")
    assert _close(json.loads(fileformat.attachments(b)["alignment.json"]),
                  json.loads(fileformat.attachments(b2)["alignment.json"]))


def test_errors_and_usage():
    f = EXAMPLES[0].relative_to(ROOT)
    for args in (["survey", "tests/data/examples/missing.wireskein"], ["states", "tests/data/nothing"],
                 ["convert", f, "x.wireskein", "--from", "5"]):
        p, r = py(*args), rs(*args)
        assert (p[0], p[2]) == (r[0], r[2]) and p[2] != 0, (p, r)
    for args in (["analyze"], ["survey", f, "--bins", "x"], ["nothing"], ["analyze", f, "--mode", "x"]):
        p, r = py(*args), rs(*args)
        assert p[2] == r[2] == 2, (args, p, r)
    assert rs("analyze", f, "--devices", "i2c/**")[2] == 2         # Python's only: said so


# ---------------- what reads the claims (the Rust engine on both sides) ----------------

ANALYZE = [[], ["--mode", "all"], ["--mode", "all", "--alternatives"], ["--select", "uart.lines", "i2c.*"],
           ["--select", "*.final"], ["--mode", "select"], ["--depth", "transport"], ["--depth", "frames"],
           ["--depth", "device"], ["--select", "spi.transfers", "--depth", "frames"]]


@engine
@pytest.mark.parametrize("args", ANALYZE, ids=lambda a: " ".join(a) or "final")
@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_analyze(env, f, args):
    env.setenv(_engines.ENGINE_ENV, "rust")
    if f.name.startswith(("rvswd", "swio")) and args[:2] == ["--mode", "all"]:
        pytest.skip("RVSWD / SWIO's layers in all mode: the Python stage's and the Rust stage's differ (§30)")
    if "all" not in args:
        same("analyze", f.relative_to(ROOT), *args, stdout_json=True)
        return
    # all mode: the evidence's "_support" is Python's upper stage's (its support of the claim), not told by Rust's
    p, r = py("analyze", f.relative_to(ROOT), *args), rs("analyze", f.relative_to(ROOT), *args)
    assert (p[1], p[2]) == (r[1], r[2])
    a, b = _json(p[0]), _json(r[0])
    for part in a["parts"]:
        for c in part["claims"]:
            c["evidence"].pop("_support", None)
    assert a == b


@engine
def test_analyze_split_out_and_items(env, tmp_path):
    env.setenv(_engines.ENGINE_ENV, "rust")
    f = (DATA / "examples" / "mixed-0.wireskein").relative_to(ROOT)
    for args in (["--split", "max_edges=400", "--depth", "frames"], ["--split", "off", "--pins", "D1,D2"]):
        same("analyze", f, *args, stdout_json=True)
    p = py("analyze", f, "--out", tmp_path / "p.json", "--split", "max_edges=400", "--items", tmp_path / "p.ndjson")
    r = rs("analyze", f, "--out", tmp_path / "r.json", "--split", "max_edges=400", "--items", tmp_path / "r.ndjson")
    assert p[0] == r[0] == "" and p[2] == r[2] == 0
    assert p[1].replace("p.json", "X").split("bytes")[1:] == r[1].replace("r.json", "X").split("bytes")[1:]
    a, b = _json((tmp_path / "p.json").read_text()), _json((tmp_path / "r.json").read_text())
    for d in (a, b):
        for part in d["parts"]:
            for c in part["claims"]:
                c["result"].get("items", {}).pop("file", None)
    assert a == b
    # the same items (Rust writes each line's keys sorted)
    rows = [[json.loads(x) for x in (tmp_path / f).read_text().splitlines()] for f in ("p.ndjson", "r.ndjson")]
    assert rows[0] == rows[1]


def _spec(f) -> dict:
    t = f.with_name(f.name.replace(".wireskein", ".truth.json"))
    buses = json.loads(t.read_text()).get("buses") if t.exists() else None
    return {"buses": [{"protocol": b["protocol"], "pins": dict(reversed(b["roles"].items()))} for b in buses or []]}


@engine
@pytest.mark.parametrize("f", [e for e in EXAMPLES if not e.name.startswith(("rvswd", "swio"))],
                         ids=lambda f: f.name)
def test_decode(env, tmp_path, f):
    env.setenv(_engines.ENGINE_ENV, "rust")
    spec = _spec(f)
    if not spec["buses"]:
        pytest.skip("no buses in its truth")
    (tmp_path / "s.json").write_text(json.dumps(spec))
    rel = f.relative_to(ROOT)
    for s in (json.dumps(spec), f"@{tmp_path / 's.json'}", tmp_path / "s.json"):
        same("decode", rel, "--spec", s, stdout_json=True)
    same("decode", rel, "--spec", json.dumps({**spec, "depth": "frames"}), "--split", "max_edges=400",
         stdout_json=True)
    p = py("decode", rel, "--spec", json.dumps(spec), "--out", tmp_path / "p.json")
    r = rs("decode", rel, "--spec", json.dumps(spec), "--out", tmp_path / "r.json")
    assert p[2] == r[2] == 0 and p[0] == r[0] == ""
    assert p[1].splitlines()[:-1] == r[1].splitlines()[:-1]                  # (its last: the paths differ)
    assert _json((tmp_path / "p.json").read_text()) == _json((tmp_path / "r.json").read_text())


@engine
def test_decode_errors(env):
    env.setenv(_engines.ENGINE_ENV, "rust")
    f = (DATA / "examples" / "mixed-0.wireskein").relative_to(ROOT)
    for spec in ('{"buses": [{"protocol": "uart"}]}', "missing.json", '{"buses": [], "depth": "bits"}'):
        p, r = py("decode", f, "--spec", spec), rs("decode", f, "--spec", spec)
        assert p[2] == r[2] == 1 and p[0] == r[0] == "", (p, r)


@engine
@pytest.mark.parametrize("f", EXAMPLES, ids=lambda f: f.name)
def test_annotate(env, tmp_path, f):
    env.setenv(_engines.ENGINE_ENV, "rust")
    a, b = tmp_path / f"a{f.suffix}", tmp_path / f"b{f.suffix}"
    shutil.copy(f, a)
    shutil.copy(f, b)
    p, r = py("annotate", a, "--save"), rs("annotate", b, "--save")
    if f.name.startswith(("rvswd", "swio")):           # (the DM layer's detail: Python's and Rust's upper stage)
        assert p[2] == r[2] == 0 and len(p[0].splitlines()) == len(r[0].splitlines())
        return
    assert p == r
    assert json.loads(fileformat.get(a, "decode/annotations.json")) == json.loads(
        fileformat.get(b, "decode/annotations.json"))


@engine
def test_annotate_with_a_threshold(env, tmp_path):
    """An analog UART read as logic at a threshold (test_rust_parts' capture of one seen by an ADC)."""
    env.setenv(_engines.ENGINE_ENV, "rust")
    from test_rust_parts import _adc_uart
    p = _adc_uart(tmp_path)
    env.setenv(_rust.RUST_ENV, "annotate")             # (Python's own, around the Rust engine, reads the same rows)
    a = py("annotate", p, "--threshold", "TX=1.65")
    env.delenv(_rust.RUST_ENV)
    assert a == py("annotate", p, "--threshold", "TX=1.65") == rs("annotate", p, "--threshold", "TX=1.65")
    same("annotate", p, "--threshold", "TX=1.0,2.3", "--threshold", "TX2=1.5", "--hint", "uart")


@engine
@pytest.mark.parametrize("args", [[], ["--results"], ["--events"], ["--results", "--events"]])
def test_segments(env, tmp_path, args):
    """The marker tree: Rust's part in Python (WIRESKEIN_RUST=segments) and wireskein-rs the same; against Python's
    own (its engine) the tree, the marker stream and the activity the same."""
    from test_rust_parts import _marked
    f = _marked(tmp_path)
    env.setenv(_rust.RUST_ENV, "segments")
    same("segments", f, *args, stdout_json=True)
    env.delenv(_rust.RUST_ENV)
    mine, theirs = _json(py("segments", f, *args)[0]), _json(rs("segments", f, *args)[0])

    def tree(segs):
        return [(s["path"], s["raw"], s["begin_s"], s["end_s"], s["activity"], s.get("events"), tree(s["children"]))
                for s in segs]
    assert mine["marker"] == theirs["marker"] and tree(mine["segments"]) == tree(theirs["segments"])
    assert mine.get("events_outside") == theirs.get("events_outside")


@engine
def test_segments_without_markers(env):
    env.setenv(_rust.RUST_ENV, "segments")
    f = (DATA / "examples" / "mixed-0.wireskein").relative_to(ROOT)
    same("segments", f, "--markers", '{"pin": "NOPE"}')
