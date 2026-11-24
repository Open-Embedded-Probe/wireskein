"""WireSkein's parts in Rust, switched on a part at a time (bench/notes/2026-11-24-rust-plan.ja.md §28): the package
wireskein-core (module wireskein_core, built from crates/wireskein-py, installed apart) used in place of the Python
implementation of a part, to compare the two and to try the Rust parts before any is made the default.

    WIRESKEIN_RUST=io,verify     (comma-separated; "all" for every part; unset or empty: none, the Python package as
                                  it always is - nothing imports wireskein_core)

The parts:
    engine   the engine (the claims): the same as WIRESKEIN_ENGINE=rust (wireskein._engines), which stays
    io       a capture file read into the changes view: a WireSkein file's parts (wireskein._engine.fileio.load /
             load_parts, so load_runs, decode, verify, annotate ... read through it; the header, its meta and the
             attachments still read by Python, so the views are the same objects), a .sr (srio.read_sr: the
             session's extra and the entries carried with it read by Python) and a VCD (vcdio.read_vcd)
    write    a capture written: fileio.save (a WireSkein file), srio.write_sr, vcdio.write_vcd (so decode's and
             wireskein.capture's saving, `wireskein convert` ...) by wireskein-io's writers, the readers' pair
    verify   wireskein.verify.verify: the whole report from wireskein-verify (its checks read with the Rust engine);
             mark=True writes the markers with Python's code unless `mark` is on too; with definitions directories (protocols
             declared in them) or the engine's extra protocol tables (staged.PROTOCOL_TABLES) verify stays Python's
    analyze  wireskein.decode.analyze_file (and so `wireskein analyze`): the document from wireskein-analysis (the
             Rust engine, the Rust upper stage, the report beside each claim, a long part read in pieces, its items
             written out; pins, analog channels read as logic at a threshold, a marker segment, select, depth,
             alternatives - the Rust core keeps no runner-up); options it does not have (device patterns, definitions
             directories, the engine's extra protocol tables) stay Python's
    decode   wireskein.decode.decode_file (and so `wireskein decode`): the document from wireskein-analysis, every bus
             read by the Rust engine (RVSWD / SWIO and buses sharing lines too, which Python reads with its own
             engine), a long part in pieces, the items written out; device patterns, definitions directories and
             the engine's extra protocol tables stay Python's
    split    the sources a long capture is read from a block at a time (wireskein.split.FileSource for a WireSkein
             file's part, SrSource for a .sr: decode's and survey's Python ways) by wireskein-io's stream
    mark     verify(mark=True) (`wireskein verify --mark`): the markers written into each capture file by
             wireskein-verify
    states   wireskein.states.of_line (so of_capture, of_file, of_sections, `wireskein states`): wireskein-analysis
    survey   wireskein.survey.survey_file (`wireskein survey`): wireskein-analysis, a WireSkein file of one part read a
             block at a time from the file, anything else read whole; a plan of no split stays Python's
    annotate wireskein.annotate.build_file (`wireskein annotate`): each part decoded by the Rust core (as analyze
             with the Rust engine), its rows made in Rust (`wireskein annotate --threshold` too); another step before
             decoding, device patterns, definitions directories and the engine's extra protocol tables stay Python's
    align    wireskein.align.find and between (`wireskein align`): the offset and scale of an analog channel, or of
             another capture, from the edges of one signal on both, by wireskein-analysis's align
    segments `wireskein segments`: the marker segment tree (markers.find and describe) by wireskein-analysis, the
             claims the Rust core's (Python reads them with its own engine); device patterns, definitions
             directories and the engine's extra protocol tables stay Python's
    note     wireskein._fileformat.note (`wireskein note`, a capture's --note): the entry appended to the capture's
             log by wireskein-io, its time the local time with its offset from UTC
    attach   wireskein._fileformat.attach (`wireskein attach`, an alignment saved, ...): the file stored as
             attach/NAME by wireskein-io (the data encoded by Python as before)

A part switched on whose Rust cannot do what was asked falls back to Python without a word (the result is the same
kind); a part switched on without wireskein_core installed is an error."""
from __future__ import annotations

import json
import os
from fractions import Fraction
from pathlib import Path

RUST_ENV = "WIRESKEIN_RUST"
PARTS = ("engine", "io", "write", "verify", "analyze", "decode", "split", "mark", "states", "survey", "annotate",
         "align", "segments", "note", "attach")


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


RUST_LEAVES = "not read on the wire by the Rust verify yet"


def left_to_python(rep: dict) -> bool:
    """Whether the Rust report left a check unchecked that Python's verify reads (a protocol declared in WireSkein
    itself, read on the wire by the Python engine's declared readers): then the whole report is Python's."""
    return any(r.get("status") == "unchecked" and RUST_LEAVES in (r.get("reason") or "")
               for r in rep.get("results", []))


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
    return (mode in ("final", "all", "select") and depth in ("transport", "frames", "protocol", "device")
            and not (hints or {}).get("devices") and _split_spec(split) is not False and Path(path).is_file()
            and not plugins.definitions() and not staged.PROTOCOL_TABLES)


def _dialects() -> str:
    """The marker dialects (markers.load_dialects: WireSkein's and the definitions directories') as JSON."""
    from ._engine import markers
    return json.dumps({name: {k: ([{x: y for x, y in m.items() if x != "_re"} for m in v] if k == "marker" else v)
                              for k, v in d.items()} for name, d in markers.load_dialects().items()}, default=str)


def analyze_file(path, hints: dict | None, mode: str, window_s, part, split, pins=None, thresholds=None,
                 items=None, segment=None, markers_hint=None, events=False, select=None, alternatives=False,
                 depth="protocol") -> dict:
    """The analysis document from wireskein-analysis (as decode.analyze_file gives it, with analysis_seconds; select
    makes the mode "select", alternatives adds nothing: the Rust core keeps no runner-up)."""
    import time
    from . import __version__
    t0 = time.perf_counter()
    th = {k: list(map(float, v)) if isinstance(v, (list, tuple)) else float(v) for k, v in (thresholds or {}).items()}
    doc = json.loads(core().analyze_file(
        str(path), json.dumps(hints), part, mode, tuple(map(float, window_s)) if window_s else None,
        _split_spec(split), list(pins) if pins else None, json.dumps(th) if th else None,
        None if items is None else str(items), None if items is None else str(items), segment,
        None if markers_hint is None else json.dumps(markers_hint), _dialects() if segment else None, bool(events),
        list(select) if select else None, depth, bool(alternatives)))
    doc["wireskein"] = __version__
    doc["analysis_seconds"] = round(time.perf_counter() - t0, 3)
    return doc


# ---------------- decode ----------------

def decodes(path, spec, split) -> bool:
    """Whether decode_file with this spec is Rust's: switched on, a file, a split it can take, nothing the Rust core
    does not read (device patterns, definitions directories, the engine's extra protocol tables)."""
    if not on("decode"):
        return False
    from . import plugins
    from ._engine import staged
    return (Path(path).is_file() and isinstance(spec, dict) and not spec.get("devices")
            and _split_spec(split) is not False and not plugins.definitions() and not staged.PROTOCOL_TABLES)


def decode_file(path, spec: dict, split, pins, items) -> dict:
    """The decode document from wireskein-analysis (as decode.decode_file gives it): every bus read by the Rust core,
    RVSWD / SWIO and buses sharing lines too."""
    from . import __version__
    from ._engine.export import dumps
    doc = json.loads(core().decode_file(str(path), dumps(spec), _split_spec(split), list(pins) if pins else None,
                                        None if items is None else str(items),
                                        None if items is None else str(items)))
    doc["wireskein"] = __version__
    return doc


# ---------------- split: the sources read a block at a time ----------------

def _source(make, first, rate: float, meta: dict):
    """A wireskein.split.Source over wireskein_core's Blocks (`first`; made again by `make` for each read after)."""
    from . import split

    class RustSource(split.Source):
        def __init__(self):
            self._first = first
            self.names, self.n = list(self._first.names), int(self._first.n)
            self.initial = {k: int(v) for k, v in zip(self.names, self._first.initial)}
            self.rate, self.meta = rate, meta

        def blocks(self):
            got, self._first = (self._first or make()), None
            for t1, es in got:
                yield int(t1), dict(zip(self.names, es))

    return RustSource()


def file_source(path, part: int = 0, pins=None):
    """split.FileSource through wireskein-io's stream (each bits line decompressed by a thread of its own, a block at a
    time); ValueError where a line is not bits / events at one tick a sample, as FileSource's."""
    from . import _fileformat as ff
    from ._engine import fileio
    head = ff.part_view(ff.read_header(path), part)
    pins = list(pins) if pins else None
    make = lambda: core().file_blocks(str(path), part, pins)       # noqa: E731
    first = make()
    if first is None:
        raise ValueError("a line of its own step: read whole")
    return _source(make, first, float(ff.tick_hz(head)), fileio.view(head, [], path).meta)


def sr_source(path, pins=None):
    """split.SrSource through wireskein-io's stream; ValueError for analog channels or lines of their own step, and
    for pins the session does not have, as SrSource's."""
    pins = list(pins) if pins else None
    make = lambda: core().sr_blocks(str(path), pins)               # noqa: E731
    first = make()
    if first is None:
        raise ValueError("analog channels or lines of their own step: read whole")
    return _source(make, first, float(first.rate), {"file": str(path)})


# ---------------- mark ----------------

def mark(run_dir, rep: dict) -> None:
    """verify's markers written into each capture file by wireskein-verify (as verify._mark writes them)."""
    core().mark(str(run_dir), json.dumps(rep, default=str))


# ---------------- io: a .sr and a VCD ----------------

def read_sr(path):
    """srio.read_sr through the Rust reader: the logic lines (each at its own step), the analog channels; what the
    session's extra (wireskein/sr-extra.json) says of the capture and the entries carried with it read here, as
    read_sr gives them in meta."""
    import zipfile
    import numpy as np
    from ._engine.model import AnalogTrace, Capture, Channel
    from ._engine.srio import EXTRA
    got = core().read_file(str(path))
    with zipfile.ZipFile(path) as z:
        names = z.namelist()
        extra = json.loads(z.read(EXTRA)) if EXTRA in names else {}
        if extra.get("format") != "wireskein-sr-extra/1":
            extra = {}
        listed = set(extra.get("extras", []))
        carried = {n: z.read(n) for n in names if n.startswith(("attach/", "notes/")) or n in listed}
    p = got["parts"][0]
    meta = {"sr_file": str(path), "unitsize": got["meta"]["unitsize"]}
    rate = float(Fraction(*got["tick_hz"]))
    if extra.get("tick_hz"):
        meta["tick_hz"] = Fraction(*extra["tick_hz"])
        rate = float(meta["tick_hz"])
    meta.update(extra.get("meta", {}))
    if extra.get("id"):
        meta["capture_id"] = extra["id"]
    if carried:
        meta["extras"] = carried
    channels = [Channel(x["name"], int(x["initial"]), x["edges"], int(x["step"]), int(x["phase"]), x["acquisition"])
                for x in p["lines"]]
    analog = []
    for a in p["analog"]:
        rate_hz, t0 = Fraction(*a["rate_hz"]), Fraction(*a["t0_ticks"])
        if a["encoding"] == "analog":                       # written by write_sr: the raw values come back
            dt = {8: np.uint8, 16: "<u2", 32: "<u4"}[a["width"]]
            analog.append(AnalogTrace(a["name"], a["values"].astype(dt), rate_hz, t0, "analog", a["width"],
                                      a["value_bits"], a["zero"], a["scale_nv"], "V", a["acquisition"]))
        elif a["name"] in extra.get("analog", {}):          # analog-f32 written by write_sr: its own rate
            analog.append(AnalogTrace(a["name"], a["values"].astype(np.float32), rate_hz, t0, "analog-f32",
                                      unit=a["unit"], acquisition=a["acquisition"]))
        else:
            analog.append(AnalogTrace(a["name"], a["values"].astype(np.float32), rate_hz, Fraction(0), "analog-f32"))
    return Capture(rate, int(p["ticks"]), channels, meta=meta, analog=analog)


def read_vcd(path):
    """vcdio.read_vcd through the Rust reader."""
    import numpy as np
    from ._engine.model import AnalogTrace, Capture, Channel
    got = core().read_file(str(path))
    p = got["parts"][0]
    tick = Fraction(*got["tick_hz"])
    own = dict(got["meta"])
    late = {k: own.pop(k) for k in ("vcd_x_or_z", "vcd_not_read") if k in own}
    meta = {"vcd_file": str(path), "tick_hz": tick, **own}
    if got["id"]:
        meta["capture_id"] = got["id"]
    meta.update(late)
    channels = [Channel(x["name"], int(x["initial"]), x["edges"], int(x["step"]), int(x["phase"]), x["acquisition"])
                for x in p["lines"]]
    analog = [AnalogTrace(a["name"], a["values"] if a["unit"] == "raw" else a["values"].astype(np.float32),
                          Fraction(*a["rate_hz"]), Fraction(*a["t0_ticks"]), "analog-f32", unit=a["unit"],
                          acquisition=a["acquisition"]) for a in p["analog"]]
    return Capture(float(tick), int(p["ticks"]), channels, meta=meta, analog=analog)


# ---------------- write ----------------

def write(path, cap, meta: dict, form: str):
    """fileio.save (form "wireskein"), srio.write_sr ("sr"), vcdio.write_vcd ("vcd") through wireskein-io's writers:
    the same file as Python's for its readers. meta: the capture file's metadata (as the writer is given it)."""
    import numpy as np
    from . import _fileformat as fileformat
    tick = cap.meta.get("tick_hz", cap.rate)
    tick = fileformat._fraction(tick) if form == "wireskein" else Fraction(tick).limit_denominator(10**9)
    meta = dict(meta)
    meta.pop("capture_id", None)
    if form == "wireskein":
        for k in ("complete", "lost", "layout", "part", "n_parts"):
            meta.pop(k, None)
        kept = {t.name for t in cap.events}             # their Channel view is not written; the events are
        lines = [c for c in cap.channels if c.name not in kept]
        events, analog = list(cap.events), list(cap.analog)
    else:
        lines, events, analog = list(cap.channels), [], list(cap.analog)
    info = {"tick_hz": [tick.numerator, tick.denominator], "ticks": int(cap.n_samples),
            "id": cap.meta.get("capture_id"), "lost": (cap.meta.get("lost") or []) if form == "wireskein" else [],
            "meta": meta,
            # a .sr's or a VCD's extra keeps a reduced line's acquisition whole (its reduction named in it)
            "lines": [{"step": int(c.step), "phase": int(c.phase),
                       "reduction": c.acquisition.get("reduction") if form == "wireskein" else None,
                       "acquisition": {k: v for k, v in c.acquisition.items()
                                       if k != "reduction" or form != "wireskein"}} for c in lines],
            "events": [{"step": int(t.step), "phase": int(t.phase), "n": int(t.n), "start_level": int(t.start_level),
                        "reduction": t.acquisition.get("reduction"),
                        "acquisition": {k: v for k, v in t.acquisition.items() if k != "reduction"}} for t in events],
            "analog": [{"encoding": a.encoding, "rate_hz": [a.rate_hz.numerator, a.rate_hz.denominator],
                        "t0_ticks": [a.t0_ticks.numerator, a.t0_ticks.denominator], "width": a.width,
                        "value_bits": a.value_bits, "zero": a.zero, "scale_nv": a.scale_nv, "unit": a.unit,
                        "acquisition": a.acquisition} for a in analog]}
    extras = [(k, bytes(v)) for k, v in cap.meta.get("extras", {}).items()] if form != "vcd" else []
    i64 = lambda x: np.ascontiguousarray(x, np.int64)               # noqa: E731
    u8 = lambda x: np.ascontiguousarray(x, np.uint8)                # noqa: E731
    core().write_file(str(path), json.dumps(info, default=str),
                      [(c.name, int(c.initial), i64(c.edges)) for c in lines],
                      [(t.name, i64(t.quanta), u8(t.levels), u8(t.counts)) for t in events],
                      [(a.name, np.ascontiguousarray(a.values, np.float64)) for a in analog], extras, form)
    return Path(path)


# ---------------- states ----------------

def states_of_line(edges, initial, n, rate, step, tol, min_periods, max_odd) -> list:
    """states.of_line from wireskein-analysis (the stretches as Python gives them)."""
    import numpy as np
    return json.loads(core().states_of_line(np.ascontiguousarray(edges, np.int64), int(initial), int(n), float(rate),
                                            int(step), float(tol), int(min_periods),
                                            None if max_odd is None else int(max_odd)))


# ---------------- survey ----------------

def _plan_spec(plan) -> str | None | bool:
    """A survey's plan as the Rust overview takes it (None: no plan; "": the split's defaults), False when it cannot
    (a plan of no split)."""
    if plan is None:
        return None
    s = _split_spec(plan)
    if s is False or s == "off":
        return False
    return s or ""


def surveys(plan) -> bool:
    """Whether survey_file with this plan is Rust's: switched on, and a plan it can give."""
    return on("survey") and _plan_spec(plan) is not False


def survey_file(path, pins, bins: int, top: int, plan) -> dict:
    """survey.survey_file from wireskein-analysis (its "wireskein" put by Python)."""
    from . import __version__
    doc = json.loads(core().survey_file(str(path), list(pins) if pins else None, int(bins), int(top),
                                        _plan_spec(plan)))
    cap = doc.pop("capture")
    return {**doc, "wireskein": __version__, "capture": cap}


# ---------------- annotate ----------------

def annotates(path, hints, prepare) -> bool:
    """Whether annotate.build_file is Rust's: switched on, a file, no step before decoding, nothing the Rust core
    does not read (device patterns, definitions directories, the engine's extra protocol tables)."""
    if not on("annotate"):
        return False
    from . import plugins
    from ._engine import staged
    return (prepare is None and Path(path).is_file() and not (hints or {}).get("devices")
            and not plugins.definitions() and not staged.PROTOCOL_TABLES)


def annotations_file(path, hints: dict | None, part, thresholds: dict | None = None) -> dict:
    """The annotations document from wireskein-analysis (as annotate.build_file gives it); thresholds: analog
    channels also read as logic first (`wireskein annotate --threshold`, {name: V | (LOW, HIGH)})."""
    from . import __version__
    return json.loads(core().annotations_file(str(path), json.dumps(hints), part, f"wireskein {__version__} analyze",
                                              _thresholds_json(thresholds)))


def _thresholds_json(thresholds: dict | None) -> str | None:
    th = {k: list(map(float, v)) if isinstance(v, (list, tuple)) else float(v) for k, v in (thresholds or {}).items()}
    return json.dumps(th) if th else None


# ---------------- segments ----------------

def segments_runs(path, hints) -> bool:
    """Whether `wireskein segments` is Rust's: switched on, a file, nothing the Rust core does not read (device
    patterns, definitions directories, the engine's extra protocol tables)."""
    if not on("segments"):
        return False
    from . import plugins
    from ._engine import staged
    return (Path(path).is_file() and not (hints or {}).get("devices") and not plugins.definitions()
            and not staged.PROTOCOL_TABLES)


def segments_file(path, hints: dict | None, thresholds: dict | None, markers_hint: dict | None, results: bool,
                  events: bool) -> dict | None:
    """The marker segment tree from wireskein-analysis (as markers.describe gives it, the claims the Rust core's), or
    None when no marker stream is found."""
    got = core().segments_file(str(path), json.dumps(hints), _thresholds_json(thresholds),
                               None if markers_hint is None else json.dumps(markers_hint), _dialects(), bool(results),
                               bool(events))
    return None if got is None else json.loads(got)


# ---------------- note, attach ----------------

def note(path, content, fields: dict) -> int:
    """_fileformat.note by wireskein-io: the entry appended to the capture's log, its time the local time with its
    offset from UTC (written there), the content and fields as Python's json.dumps gives them (default=str)."""
    return int(core().note(str(path), json.dumps(content, ensure_ascii=False, default=str),
                           json.dumps(fields, ensure_ascii=False, default=str)))


def attach(path, name: str, data: bytes, replace: bool) -> None:
    """_fileformat.attach by wireskein-io (the data as Python stores it: _fileformat._encode)."""
    core().attach(str(path), name, bytes(data), bool(replace))


# ---------------- align ----------------

def _ratio(x) -> tuple[int, int]:
    """A rate or a tick as (num, den) in 64 bits (exact where it fits)."""
    f = Fraction(x)
    if max(abs(f.numerator), f.denominator) >= 2 ** 62:
        f = f.limit_denominator(10 ** 9)
    return f.numerator, f.denominator


def _analog_args(a) -> tuple:
    import numpy as np
    return (a.name, a.encoding, np.ascontiguousarray(a.values, np.float64), _ratio(a.rate_hz), _ratio(a.t0_ticks),
            int(a.width), a.value_bits, None if a.zero is None else float(a.zero),
            None if a.scale_nv is None else float(a.scale_nv))


def _entry(text: str) -> dict:
    return {k: (float("inf") if v == "Infinity" else v) for k, v in json.loads(text).items()}


def align_find(cap, reference: str, via: str, threshold, window_ticks, max_ppm: float, uncertainty) -> dict:
    """align.find's entry for a channel from wireskein-analysis (the reference's channel and the analog trace found
    by the caller; uncertainty: the probe's start uncertainties in ticks, or None)."""
    import numpy as np
    from ._engine import analog
    ref = cap.channel(reference)
    trace = next(a for a in cap.analog if a.name == via)
    low, high = analog.thresholds(threshold)
    return _entry(core().align_find(np.ascontiguousarray(ref.edges, np.int64), int(ref.initial), int(ref.step),
                                    reference, _analog_args(trace), _ratio(cap.meta.get("tick_hz", cap.rate)),
                                    low, high, None if window_ticks is None else float(window_ticks),
                                    None if uncertainty is None else float(uncertainty), int(cap.n_samples),
                                    float(max_ppm)))


def align_between(ref_cap, reference: str, cap, via: str, threshold, window_ticks, max_ppm: float) -> dict:
    """align.between from wireskein-analysis: `reference` a logic channel of ref_cap, `via` one of cap (analog read at
    threshold)."""
    import numpy as np
    from ._engine import analog
    try:
        ref = ref_cap.channel(reference)
    except KeyError:
        ref = None
    if ref is None:
        if any(a.name == reference for a in ref_cap.analog):
            raise ValueError(f"{reference} is analog: give a threshold")
        raise ValueError(f"{reference}: no such channel")
    try:
        ch = cap.channel(via)
    except KeyError:
        ch = None
    kw = {}
    if ch is not None:
        kw["logic"] = (np.ascontiguousarray(ch.edges, np.int64), int(ch.initial), int(ch.step))
    else:
        trace = next((a for a in cap.analog if a.name == via), None)
        if trace is None:
            raise ValueError(f"{via}: no such channel")
        if threshold is None:
            raise ValueError(f"{via} is analog: give a threshold")
        kw.update(analog=_analog_args(trace), tick_hz=_ratio(cap.meta.get("tick_hz", cap.rate)),
                  threshold=analog.thresholds(threshold))
    tick_ref, tick = float(ref_cap.meta.get("tick_hz", ref_cap.rate)), float(cap.meta.get("tick_hz", cap.rate))
    return _entry(core().align_between(np.ascontiguousarray(ref.edges, np.int64), int(ref.initial), int(ref.step),
                                       reference, via, tick_ref, tick, int(ref_cap.n_samples),
                                       window_ticks=None if window_ticks is None else float(window_ticks),
                                       max_ppm=float(max_ppm), **kw))
