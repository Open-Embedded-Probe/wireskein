"""Decoding a capture: finding what it holds (`analyze`), or decoding the buses a spec names (`decode_file`).

    cap = load("capture.wireskein", part=0)  # one part as the decoders read it; a .sr or a VCD too
    res = analyze(cap, {"protocols": ["i2c"]})
    doc = export(res, cap)                   # JSON-ready dict, as `wireskein analyze` prints
    doc = decode_file("c0001.wireskein", {"buses": [{"protocol": "i2c", "pins": {"scl": "SCL", "sda": "SDA"}}]})

analyze finds what a capture holds and claims only what the evidence carries: a short case (one transfer) is too
little to claim. When the roles are known (the run wired them), the decode of exactly those roles is what is
wanted, however weak the evidence (verify does the same). This keeps it, with what is needed to trust it later:

- the version, the spec, the capture (file, sha256, id, tick rate, complete / lost, layout)
- every part on its own (analysis never crosses a gap): its start on the probe's clock, the declared buses'
  results (the export's layers, ticks from the part's tick 0), the declared lines' changes
- what was not read: activity on a bus's pins that no decoded item covers (uninterpreted), and the pins the spec
  did not name (undeclared, with analyze's claims on them for reference). With the parts and `lost`, "nothing
  happened" (no activity), "not seen" (lost, between parts) and "not read" (uninterpreted) are told apart.

spec: {"buses": [{"protocol": "i2c", "pins": {"scl": "SCL", "sda": "SDA"}, "params": {...}, "idle_s": ...}],
       "lines": ["DRDY"], "devices": ["i2c/**"], "depth": "device"}
"""

from __future__ import annotations

import hashlib
from fractions import Fraction
from pathlib import Path
from types import SimpleNamespace

import numpy as np

from . import __version__, _fileformat as fileformat
from ._engine import export as _exp
from ._engine import fileio, staged, timing
from ._engine.export import dumps, export
from ._engine.model import Capture, Channel
from ._engine.srio import read_sr, write_sr

__all__ = ["Capture", "Channel", "analyze", "analyze_file", "as_logic", "decode_file", "dumps", "export", "load", "save"]

FORMAT = "wireskein-analysis/1"
IDLE_S = 1e-3          # a bus with no bit times of its own: activity apart by more than this is two bursts
IDLE_BITS = 16         # else: more than this many bit times without a change


def load(path: str | Path, part: int | None = None) -> Capture:
    """A WireSkein file (a capture of several parts needs `part`), a sigrok .sr or a VCD, told apart by content."""
    path = Path(path)
    kind = fileformat.sniff(path)
    if kind == "wireskein":
        return fileio.load(path, part)
    if kind == "sr":
        return read_sr(path)
    if kind == "vcd":
        from ._engine.vcdio import read_vcd
        return read_vcd(path)
    from . import formats
    name = formats.kind(path)
    if name is None:
        raise ValueError(f"{path}: not a WireSkein file, a sigrok session (.sr), a VCD, or a format added "
                         f"(wireskein.formats: {', '.join(formats.registered()) or 'none'})")
    cap = formats.read(path, name)
    if len(cap.parts) > 1 and part is None:
        raise ValueError(f"{path} ({name}) has {len(cap.parts)} parts: name one (part, from 0)")
    k = part or 0
    v = cap.parts[k].view()
    v.meta.update(file=str(path), format=name, complete=cap.complete, lost=list(cap.lost), part=k,
                  n_parts=len(cap.parts), capture_id=cap.id, **{key: val for key, val in cap.meta.items()
                                                                if key not in v.meta})
    return v


def save(path: str | Path, cap: Capture, **meta) -> Path:
    """By the name: .sr (one rate: slow channels are repeated, their real rate
    kept in wireskein.json inside the zip), .vcd (the changes; wireskein's own
    data in a $comment), anything else a WireSkein file
    (each channel at its own rate; .wireskein is the usual name)."""
    path = Path(path)
    fileio.refuse_if_skipped(cap, path)
    if path.suffix in (".sr", ".vcd"):
        fileio.refuse_if_hidden(cap, path)
    if path.suffix == ".sr":
        return write_sr(path, cap, **meta)
    if path.suffix == ".vcd":
        from ._engine.vcdio import write_vcd
        return write_vcd(path, cap, **meta)
    return fileio.save(path, cap, **meta)


def as_logic(cap: Capture, thresholds: dict) -> Capture:
    """The capture with these analog channels also read as logic (name -> volts,
    or (low, high) for hysteresis), so the decoders see them: a UART or I2C line
    captured by an ADC. The analog channels stay as they are."""
    from ._engine import analog
    names = {c.name for c in cap.channels}
    extra = []
    for name, th in thresholds.items():
        trace = next((a for a in cap.analog if a.name == name), None)
        if trace is None:
            raise ValueError(f"{name}: not an analog channel in this capture")
        if name in names:
            raise ValueError(f"{name}: already a logic channel")
        extra.append(analog.to_logic(trace, cap.meta.get("tick_hz", cap.rate), th))
    return Capture(cap.rate, cap.n_samples, cap.channels + extra, cap.meta, cap.analog, cap.events)


def analyze(cap: Capture, hints: dict | None = None):
    """hints (all optional): {"protocols": [...], "pins": {pin: {"protocol", "role", "baud"}},
    "exclude_pins": [...], "devices": ["i2c/**", ...]} restrict what is tried."""
    return staged.analyze(cap, hints)


def _spec(spec: dict) -> dict:
    out = {"buses": [], "lines": list(spec.get("lines") or []), "devices": spec.get("devices"),
           "depth": spec.get("depth", "device")}
    for b in spec.get("buses") or []:
        if not b.get("protocol") or not b.get("pins"):
            raise ValueError(f"a bus needs protocol and pins: {b}")
        out["buses"].append({"protocol": b["protocol"], "pins": dict(b["pins"]), "params": dict(b.get("params") or {}),
                             **({"idle_s": b["idle_s"]} if "idle_s" in b else {})})
    if out["depth"] not in _exp.DEPTHS:
        raise ValueError(f"depth: one of {', '.join(_exp.DEPTHS)}")
    return out


def _hints(spec: dict) -> dict:
    pins = {}
    for b in spec["buses"]:
        for role, pin in b["pins"].items():
            pins[pin] = {"protocol": b["protocol"], "role": role, **b["params"]}
    return {"pins": pins, **({"devices": spec["devices"]} if spec.get("devices") else {})}


def _sample_edge(cap: Capture, b: dict) -> str | None:
    """SPI: the edge to sample on, from the mode given or else read from the lines (as verify's spi check)."""
    if b["protocol"] != "spi":
        return None
    p = b["pins"]
    if "mode" in b["params"]:
        m = int(b["params"]["mode"])
        return "rise" if m >> 1 == m & 1 else "fall"
    data = [cap.channel(p[r]) for r in ("mosi", "miso") if r in p]
    cs = cap.channel(p["cs"]) if "cs" in p else None
    m = timing.spi_mode(cap.channel(p["clk"]), data, cap.n_samples, cs, b["params"].get("cs_active", 0))
    line = next((v for v in m.get("lines", {}).values() if v.get("cpha") is not None), None)
    if line is None or m.get("cpol") is None:
        return None
    return "rise" if m["cpol"] == line["cpha"] else "fall"


def _node(res, cap: Capture, b: dict):
    """The decode with exactly the declared pins (the best one, when several readings have them)."""
    pins = set(b["pins"].values())
    cands = [n for n in res.roots if n.analyzer == b["protocol"] and n.output is not None
             and set(n.roles.values()) == pins]
    edge = _sample_edge(cap, b)
    if edge is not None:
        cands = [n for n in cands if n.params.get("sample_edge") == edge] or cands
    return max(cands, key=lambda n: n.total, default=None)


def _items(lay: dict, skip: tuple = ()) -> list[tuple[int, int]]:
    """(s, e) of every item with a time in the bus's layers but `skip` (e = s when an item has no end)."""
    out = []
    for key, v in lay.items():
        if key in skip:
            continue
        if isinstance(v, list):
            out += [(int(x["s"]), int(x.get("e") if x.get("e") is not None else x["s"]))
                    for x in v if isinstance(x, dict) and x.get("s") is not None]
        elif isinstance(v, dict) and isinstance(v.get("s"), list):
            out += [(int(s), int(s)) for s in v["s"]]
    return out


def _bursts(cap: Capture, pins: list[str], idle: int) -> list[np.ndarray]:
    """The changes on `pins`, cut where none comes for more than `idle` ticks."""
    e = np.sort(np.concatenate([np.asarray(cap.channel(p).edges, np.int64) for p in pins] or [np.zeros(0, np.int64)]))
    if not len(e):
        return []
    return np.split(e, np.flatnonzero(np.diff(e) > idle) + 1)


def _uncovered(e: np.ndarray, spans: np.ndarray, margin: int) -> np.ndarray:
    """The changes `e` outside every span [s - margin, e + margin] (spans sorted by start)."""
    if not len(spans):
        return e
    lo, hi = spans[:, 0] - margin, np.maximum.accumulate(spans[:, 1]) + margin
    k = np.searchsorted(lo, e, side="right") - 1
    inside = (k >= 0) & (e <= hi[np.maximum(k, 0)])
    return e[~inside]


def _idle(cap: Capture, b: dict, n) -> int:
    if "idle_s" in b:
        return int(round(b["idle_s"] * cap.rate))
    sb = (getattr(n, "layers", None) or {}).get("bits") if n is not None else None
    if sb is not None and len(sb.t) > 1:
        return int(IDLE_BITS * float(np.median(np.diff(sb.t))))
    return int(round(IDLE_S * cap.rate))


def _bus(res, cap: Capture, b: dict, depth: str, claimed: list) -> dict:
    n = _node(res, cap, b)
    out = {"protocol": b["protocol"], "pins": b["pins"]}
    if n is None:
        out.update(decoded=False, result={})
        lay = {}
    else:
        lay = _exp.layers_of(n)
        c = next((c for c in claimed if c.node is n), None)
        out.update(decoded=True, params={k: (float(v) if isinstance(v, (np.floating, float)) else v)
                                                    for k, v in n.params.items()},
                   fit={"score": round(float(n.total), 4), "claimed": c is not None,
                        **({"verdict": c.verdict} if c is not None else {})})
        k = _exp.LOWER.get(n.analyzer)
        if depth == "transport":
            out["result"] = {}
        elif depth == "frames":
            out["result"] = {k: lay[k]} if k and k in lay else {}
        else:
            out["result"] = _exp.final_of(n, lay)
            if k and k in lay and k not in out["result"]:
                out["result"][k] = lay[k]          # the transfers stay beside a higher reading (sd over spi)
            if depth == "device":
                ds = _exp.devices_of(n, with_candidates=True)
                if ds:
                    out["devices"] = ds
    idle = _idle(cap, b, n)
    # a clocked bus's bits and frames are where it was cut, not what was read: a frame no layout read is unread
    sync = n is not None and n.analyzer in staged.SYNC_PROTOCOLS
    items = _items(lay, ("bits", "frames") if sync else ())
    starts = np.sort(np.array([s for s, _ in items], np.int64))
    # items with an end (frames): what they cover is known, and activity between them inside a burst is not read
    spans = np.array(sorted((s, e) for s, e in items if e > s), np.int64).reshape(-1, 2)
    margin = max(1, idle // IDLE_BITS * 2)                 # a START or STOP a clock or two outside the sampled bits
    un = []
    for x in _bursts(cap, list(b["pins"].values()), idle):
        s, e = int(x[0]), int(x[-1])
        if len(spans) and ((spans[:, 0] <= e) & (spans[:, 1] >= s)).any():
            left = _uncovered(x, spans, margin)
            for y in (np.split(left, np.flatnonzero(np.diff(left) > idle) + 1) if len(left) else []):
                un.append({"s": int(y[0]), "e": int(y[-1]), "edges": len(y)})
            continue
        j = np.searchsorted(starts, s - idle)
        if j < len(starts) and starts[j] <= e:
            continue
        if any(a <= s and e <= z for a, z in items):
            continue
        un.append({"s": s, "e": e, "edges": len(x)})
    out["uninterpreted"] = un
    out["idle_ticks"] = idle
    return out


def _part(cap: Capture, spec: dict, head: dict) -> dict:
    names = {c.name for c in cap.channels}
    for b in spec["buses"]:
        miss = [p for p in b["pins"].values() if p not in names]
        if miss:
            raise ValueError(f"{b['protocol']}: pins not in the capture: {', '.join(miss)}")
    miss = [p for p in spec["lines"] if p not in names]
    if miss:
        raise ValueError(f"lines not in the capture: {', '.join(miss)}")
    res = staged.analyze(cap, _hints(spec))
    out = {"part": head.get("part", 0), "ticks": cap.n_samples,
           **({"tick0": head["tick0"]} if "tick0" in head else {}),
           **{k: head["meta"][k] for k in ("start_ns", "start_uncertainty_ns", "boot_id") if k in head.get("meta", {})},
           "buses": [_bus(res, cap, b, spec["depth"], res.claims) for b in spec["buses"]],
           "lines": [{"pin": p, "initial": int(cap.channel(p).initial),
                      "edges": np.asarray(cap.channel(p).edges).tolist()} for p in spec["lines"]]}
    named = {p for b in spec["buses"] for p in b["pins"].values()} | set(spec["lines"])
    rest = []
    for c in cap.channels:
        if c.name in named or not len(c.edges):
            continue
        bs = _bursts(cap, [c.name], int(round(IDLE_S * cap.rate)))
        rest.append({"pin": c.name, "edges": int(len(c.edges)), "s": int(bs[0][0]), "e": int(bs[-1][-1]),
                     "bursts": len(bs)})
    out["undeclared"] = rest
    others = [c for c in res.claims if not set(c.roles.values()) & named]
    if others:
        out["claims"] = _exp.export(SimpleNamespace(claims=others), cap)["claims"]
    return out


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def _capture_of(path: Path, raw: dict) -> dict:
    tick = Fraction(*raw["tick_hz"])
    return {"file": path.name, "sha256": _sha256(path) if path.is_file() else None, "id": raw.get("id"),
            "tick_hz": [tick.numerator, tick.denominator], "complete": raw["complete"],
            **({"lost": raw["lost"]} if raw.get("lost") else {}), "layout": raw["layout"], "parts": len(raw["parts"])}


def _part_head(head: dict, k: int, ticks: int, span: list | None = None) -> dict:
    meta = head.get("meta", {})
    return {"part": k, "ticks": ticks, **({"parts": span} if span else {}),
            **({"tick0": head["tick0"]} if "tick0" in head else {}),
            **{x: meta[x] for x in ("start_ns", "start_uncertainty_ns", "boot_id") if x in meta}}


def _runs(path: Path, raw: dict) -> list:
    """(the first part's index, [first, last] when parts were joined, the Capture) of each run of parts that follow
    on without a gap (fileio.load_runs): the analysis reads such parts as one stretch of time."""
    out = []
    for cap in fileio.load_runs(path):
        span = cap.meta.get("parts")
        out.append((span[0] if span else int(cap.meta.get("part", 0)), span, cap))
    return out


def _open(path: Path, part: int | None = None):
    """(raw header, heads, views, first parts, spans) of a capture: a WireSkein file's runs of parts (parts that follow
    on without a gap are one stretch), or a .sr, a VCD or a format added as one part."""
    if path.is_file() and fileformat.sniff(path) == "wireskein":
        raw = fileformat.read_header(path)
        runs = _runs(path, raw)
        return (raw, [fileformat.part_view(raw, k) for k, _, _ in runs], [cap for _, _, cap in runs],
                [k for k, _, _ in runs], [span for _, span, _ in runs])
    v = load(path, part)
    tick = Fraction(v.meta.get("tick_hz") or Fraction(v.rate).limit_denominator(10**9))
    raw = {"id": v.meta.get("capture_id"), "tick_hz": [tick.numerator, tick.denominator],
           "complete": v.meta.get("complete", True), "lost": v.meta.get("lost", []),
           "layout": v.meta.get("layout", "timeline"), "parts": [{}]}
    return raw, [{"meta": v.meta, "tick0": 0}], [v], [0], [None]


def decode_file(path: str | Path, spec: dict) -> dict:
    """The document for a capture (a WireSkein file, a .sr, a VCD or a format added): every part decoded with the
    spec's roles."""
    path = Path(path)
    spec = _spec(spec)
    raw, heads, views, _, spans = _open(path)
    doc = {"format": FORMAT, "wireskein": __version__, "spec": spec, "capture": _capture_of(path, raw), "parts": []}
    for head, cap, span in zip(heads, views, spans):
        out = _part(cap, spec, head)
        if span:
            out["parts"] = span
        doc["parts"].append(out)
    return doc


def analyze_file(path: str | Path, hints: dict | None = None, mode: str = "final", select: list[str] | None = None,
                 alternatives: bool = False, depth: str = "protocol", window_s: tuple[float, float] | None = None,
                 segment: str | None = None, markers_hint: dict | None = None, events: bool = False,
                 thresholds: dict | None = None, part: int | None = None) -> dict:
    """What analyze finds in a capture, as the same family of document as decode_file (wireskein-analysis): the
    capture, then each part with its claims (the export of `mode`, ticks from the part's tick 0). A WireSkein file
    is analysed part by part (analysis never crosses a gap); a .sr or a VCD is one part.
    window_s / segment pick a time range inside one part: `part` names it when the file has several."""
    import time
    from ._engine import markers
    path = Path(path)
    raw, heads, views, firsts, spans = _open(path, part)
    if (window_s or segment) and part is None and len(heads) > 1:
        raise ValueError(f"{path}: {len(heads)} parts: give the part a window or segment is inside (--part)")
    if part is not None:                       # the stretch holding that part
        hit = [i for i, (f, s) in enumerate(zip(firsts, spans)) if (s and s[0] <= part <= s[1]) or f == part]
        if not hit:
            raise ValueError(f"{path}: part {part} of {len(raw['parts'])}")
        ks = hit
    else:
        ks = range(len(heads))
    doc = {"format": FORMAT, "wireskein": __version__, "hints": hints, "capture": _capture_of(path, raw), "parts": []}
    t0 = time.perf_counter()
    for k in ks:
        cap = views[k]
        if thresholds:
            cap = as_logic(cap, thresholds)
        res = staged.analyze(cap, hints)
        window = (int(window_s[0] * cap.rate), int(window_s[1] * cap.rate)) if window_s else None
        seg_events = hit = None
        if segment:
            found = markers.find(res, cap, hint=markers_hint)
            if found is None:
                raise ValueError("no marker stream found (give --markers)")
            hit = markers.select(found["tree"], segment)
            if not hit:
                raise ValueError(f"no segment matches {segment!r}; see: wireskein segments CAPTURE")
            window = (hit[0].begin, hit[0].end)
            seg_events = markers.events_in(res, cap, found, window)
        ex = export(res, cap, "select" if select else mode, select, alternatives, window, depth)
        out = {**_part_head(heads[k], firsts[k], cap.n_samples, spans[k]), "claims": ex["claims"],
               **{x: ex[x] for x in ("window", "relations") if x in ex}}
        if hit:
            out["segment"] = {"path": hit[0].path(), "raw": hit[0].raw, "issues": hit[0].issues}
            if events or mode == "all":
                out["events"] = seg_events
        doc["parts"].append(out)
    doc["analysis_seconds"] = round(time.perf_counter() - t0, 3)
    return doc
