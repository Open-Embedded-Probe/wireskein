"""WireSkein's parts in Rust, switched on a part at a time (bench/notes/2026-11-24-rust-plan.ja.md §28): the package
wireskein-core (module wireskein_core, built from crates/wireskein-py, installed apart) used in place of the Python
implementation of a part, to compare the two and to try the Rust parts before any is made the default.

    WIRESKEIN_RUST=io,verify     (comma-separated; "all" for every part; unset or empty: none, the Python package as
                                  it always is - nothing imports wireskein_core)

The parts:
    engine   the engine (the claims): the same as WIRESKEIN_ENGINE=rust (wireskein._engines), which stays
    io       a WireSkein file's parts read into the changes view (wireskein._engine.fileio.load / load_parts, so
             load_runs, decode, verify, annotate ... read through it); the header, its meta and the attachments are
             still read by Python, so the views are the same objects; a .sr and a VCD stay Python's
    verify   wireskein.verify.verify: the whole report from wireskein-verify (its checks read with the Rust engine);
             mark=True writes the markers with Python's code as always; with definitions directories (protocols
             declared in them) or the engine's extra protocol tables (staged.PROTOCOL_TABLES) verify stays Python's
    analyze  wireskein.decode.analyze_file (and so `wireskein analyze`): the document from wireskein-analysis (the
             Rust engine, the Rust upper stage, the report beside each claim, a long part read in pieces); options it
             does not have (select, alternatives, another depth, segments, thresholds, pins, items, device patterns,
             definitions directories, the engine's extra protocol tables) stay Python's

A part switched on whose Rust cannot do what was asked falls back to Python without a word (the result is the same
kind); a part switched on without wireskein_core installed is an error."""
from __future__ import annotations

import json
import os
from fractions import Fraction
from pathlib import Path

RUST_ENV = "WIRESKEIN_RUST"
PARTS = ("engine", "io", "verify", "analyze")


def parts() -> frozenset:
    """The parts WIRESKEIN_RUST switches to Rust (none when it is unset or empty)."""
    v = os.environ.get(RUST_ENV, "").strip().lower()
    if not v:
        return frozenset()
    names = {x.strip() for x in v.split(",") if x.strip()}
    if "all" in names:
        names = (names - {"all"}) | set(PARTS)
    bad = sorted(names - set(PARTS))
    if bad:
        raise ValueError(f"{RUST_ENV}={v!r}: {', '.join(bad)}: parts are {', '.join(PARTS)} (or all)")
    return frozenset(names)


def on(part: str) -> bool:
    """Whether WIRESKEIN_RUST switches `part` to Rust."""
    return part in parts()


def core():
    """The module wireskein_core (an error that says how to get it when it is not installed)."""
    try:
        import wireskein_core
    except ImportError as e:
        raise RuntimeError(f"{RUST_ENV}={os.environ.get(RUST_ENV, '')} needs the package wireskein-core (module "
                           f"wireskein_core), built from crates/wireskein-py (pip install ./crates/wireskein-py): {e}"
                           ) from None
    return wireskein_core


# ---------------- io ----------------

def _decoded(head: dict, got: dict) -> list:
    """The decoded channels of one part (as fileio.decode gives them, in the header's order) from what the Rust reader
    read of it; each channel's acquisition from the header."""
    import numpy as np
    from ._engine.model import AnalogTrace, Channel, EventTrace
    lines = {x["name"]: x for x in got["lines"]}
    events = {x["name"]: x for x in got["events"]}
    analog = {x["name"]: x for x in got["analog"]}
    out = []
    for c in head["channels"]:
        name, enc, acq = c["name"], c.get("encoding"), dict(c.get("acquisition", {}))
        if enc in ("analog", "analog-f32"):
            a = analog[name]
            dtype = np.float32 if enc == "analog-f32" else {8: np.uint8, 16: "<u2", 32: "<u4"}[c.get("width", 16)]
            out.append(AnalogTrace(name, np.asarray(a["values"]).astype(dtype), Fraction(*c["rate_hz"]),
                                   Fraction(*c["t0_ticks"]), enc, c.get("width", 16), c.get("value_bits"),
                                   c.get("zero"), c.get("scale_nv"), c.get("unit", "V"), acq))
        elif name in events and (enc == "change_latch" or (enc == "events" and c["step"] != 1)):
            e = events[name]
            if enc == "change_latch":
                acq = {**acq, "reduction": enc}
            out.append(EventTrace(name, e["step"], e["phase"], e["n"], e["start_level"], e["quanta"], e["levels"],
                                  e["counts"], acq))
        elif enc in ("bits", "events", "any_high", "any_low") and name in lines:
            x = lines[name]
            if enc in ("any_high", "any_low"):
                acq = {**acq, "reduction": enc}
            out.append(Channel(name, int(x["initial"]), x["edges"], x["step"], x["phase"], acq))
    return out


def load(path, part: int | None = None):
    """fileio.load through the Rust reader: one part of a WireSkein file (a capture of several needs `part`)."""
    from . import _fileformat as fileformat
    from ._engine import fileio
    raw = fileformat.read_header(path)
    n = len(raw["parts"])
    if part is None:
        if n > 1:
            raise ValueError(f"{path}: {n} parts ({raw.get('layout')}): read one with part=, or all with read_parts()")
        part = 0
    head = fileformat.part_view(raw, part)
    got = core().read_file(str(path), part=part)
    return fileio.view(head, _decoded(head, got["parts"][0]), path)


def load_parts(path) -> list:
    """fileio.load_parts through the Rust reader: every part of a WireSkein file, in order."""
    from . import _fileformat as fileformat
    from ._engine import fileio
    raw = fileformat.read_header(path)
    got = core().read_file(str(path), join=False)
    ex = fileformat.extras(path)
    out = []
    for k, p in enumerate(got["parts"]):
        head = fileformat.part_view(raw, k)
        out.append(fileio.view(head, _decoded(head, p), path, ex))
    return out


# ---------------- verify ----------------

def verifies() -> bool:
    """Whether verify is Rust's: switched on, and no definitions directory (protocols declared in one are checked by
    Python's verify only)."""
    if not on("verify"):
        return False
    from . import plugins
    from ._engine import staged
    return not plugins.definitions() and not staged.PROTOCOL_TABLES


def verify(run_dir) -> dict:
    """The report of a recorded run from wireskein-verify (as wireskein.verify.verify gives it, its log kept)."""
    return json.loads(core().verify(str(run_dir)))


# ---------------- analyze ----------------

def _split_spec(split) -> str | None | bool:
    """A split option as the Rust document takes it ("key=value,...", "off", None: the defaults), False when it cannot."""
    if split is None or split is True:
        return None
    if split is False or split == "off":
        return "off"
    if isinstance(split, str):
        return split
    if isinstance(split, dict):
        return ",".join(f"{k}={';'.join(map(str, v)) if isinstance(v, (list, tuple)) else v}"
                        for k, v in split.items())
    return False


def analyzes(path, hints, mode, select, alternatives, depth, window_s, segment, markers_hint, events, thresholds,
             split, pins, items) -> bool:
    """Whether analyze_file with these options is Rust's: switched on, and nothing asked that the Rust document does
    not have."""
    if not on("analyze"):
        return False
    from . import plugins
    from ._engine import staged
    return (mode in ("final", "all") and not select and not alternatives and depth == "protocol" and segment is None
            and markers_hint is None and not events and not thresholds and not pins and items is None
            and not (hints or {}).get("devices") and _split_spec(split) is not False and Path(path).is_file()
            and not plugins.definitions() and not staged.PROTOCOL_TABLES)


def analyze_file(path, hints: dict | None, mode: str, window_s, part, split) -> dict:
    """The analysis document from wireskein-analysis (as decode.analyze_file gives it, with analysis_seconds)."""
    import time
    from . import __version__
    t0 = time.perf_counter()
    doc = json.loads(core().analyze_file(str(path), json.dumps(hints), part, mode,
                                         tuple(map(float, window_s)) if window_s else None, _split_spec(split)))
    doc["wireskein"] = __version__
    doc["analysis_seconds"] = round(time.perf_counter() - t0, 3)
    return doc
