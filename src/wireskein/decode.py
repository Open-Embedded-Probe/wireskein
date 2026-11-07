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

import functools
import itertools
import hashlib
from fractions import Fraction
from pathlib import Path
from types import SimpleNamespace

import numpy as np

from . import __version__, _fileformat as fileformat
from ._engine import export as _exp
from ._engine import fileio, staged, timing
from ._engine import kernels as K
from ._engine.export import dumps, export
from ._engine.model import Capture, Channel
from ._engine.srio import read_sr, write_sr

__all__ = ["Capture", "Channel", "analyze", "analyze_file", "as_logic", "decode_file", "dumps", "export", "load", "save"]

FORMAT = "wireskein-analysis/1"
IDLE_S = 1e-3          # a bus with no bit times of its own: activity apart by more than this is two bursts
IDLE_BITS = 16         # else: more than this many bit times without a change


def load(path, part: int | None = None) -> Capture:
    """A WireSkein file (a capture of several parts needs `part`), a sigrok .sr or a VCD, told apart by content; or a
    wireskein.capture.Capture in memory (one just taken, before or without saving it), its part as the checks read
    it."""
    from .capture import Capture as Whole
    if isinstance(path, Whole):
        if len(path.parts) > 1 and part is None:
            raise ValueError(f"a capture of {len(path.parts)} parts: name one (part, from 0)")
        k = part or 0
        v = path.parts[k].view()
        v.meta.update(complete=path.complete, lost=list(path.lost), part=k, n_parts=len(path.parts),
                      capture_id=path.id, **{key: val for key, val in path.meta.items() if key not in v.meta})
        return v
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
                             **({"idle_s": b["idle_s"]} if "idle_s" in b else {}),
                             **({"layers": list(b["layers"])} if b.get("layers") else {})})
    if out["depth"] not in _exp.DEPTHS:
        raise ValueError(f"depth: one of {', '.join(_exp.DEPTHS)}")
    return out


def _hints(spec: dict, strict: bool = False, names=()) -> dict:
    """The analysis' hints for a spec. strict: only the spec's protocols tried, on the spec's pins only (a piece of a
    long capture: no claims on other pins, and no other protocol tried on these)."""
    pins = {}
    for b in spec["buses"]:
        for role, pin in b["pins"].items():
            pins[pin] = {"protocol": b["protocol"], "role": role, **b["params"]}
    out = {"pins": pins, **({"devices": spec["devices"]} if spec.get("devices") else {})}
    if strict and spec["buses"]:
        out["protocols"] = sorted({b["protocol"] for b in spec["buses"]})
        out["exclude_pins"] = sorted(set(names) - set(pins))
    return out


def _sample_edge(cap: Capture, b: dict) -> str | None:
    """The edge to sample on: params' sample_edge when given (a long capture's pieces all read on the edge its first
    piece read on); SPI: from the mode given or else read from the lines (as verify's spi check)."""
    if b["params"].get("sample_edge") in ("rise", "fall"):
        return b["params"]["sample_edge"]
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


GOOD_HALF = 4        # samples a clock's high and low should last, at least, to be sure none falls between samples
TIGHT_HALF = 2.5     # a clock whose usual high or low, where it runs fastest, is shorter than this is told


SAME_SPEED = 1.25    # frames whose clock periods are within this of a speed's fastest are of that speed


def frame_clocks(cap: Capture, clock: str, items: list, least: int = 8) -> None:
    """Each frame a clocked bus read (`items`, with s and e) given its clock: `period_ns` (the median of its clock's
    periods; kept when the decoder gave it) and `high_ns` (the median of its highs), so a reflection's glitch does
    not count. A frame of fewer than `least` clocks is left as it is."""
    ch = cap.channel(clock)
    its = [x for x in items if isinstance(x, dict) and isinstance(x.get("s"), int) and isinstance(x.get("e"), int)
           and x["e"] > x["s"]]
    if ch.step != 1 or not its:
        return
    e = np.asarray(ch.edges, np.int64)
    lv = K.edge_levels(e, ch.initial)
    rise = np.flatnonzero(lv == 1)
    rise = rise[rise + 1 < len(e)]
    t = e[rise]
    high = e[rise + 1] - t
    se = np.asarray([(x["s"], x["e"]) for x in its], np.int64)
    lo = np.searchsorted(t, se[:, 0])
    hi = np.searchsorted(t, se[:, 1], side="right")
    ns = 1e9 / float(cap.rate)
    for x, i, j in zip(its, lo.tolist(), hi.tolist()):
        if j - i >= least:
            x.setdefault("period_ns", round(float(np.median(np.diff(t[i:j]))) * ns, 1))
            x["high_ns"] = round(float(np.median(high[i:j - 1])) * ns, 1)


def clock_speeds(rate: float, items: list) -> list[dict]:
    """The speeds a bus's clock ran at, from its frames' clocks (frame_clocks): frames whose periods are within
    SAME_SPEED of a speed's fastest are that speed, wherever they are. -> [{"period_ns", "freq_hz", "high_ns",
    "low_ns" (medians), "frames", "s", "e" (its first frame's start, its last's end), "rate_ok"}], fastest first;
    rate_ok: false when its high or low lasts under TIGHT_HALF samples (a high may fall between two samples, its clock
    lost)."""
    rows = sorted((x["period_ns"], x["high_ns"], x["s"], x["e"]) for x in items
                  if isinstance(x, dict) and "high_ns" in x and x.get("period_ns"))
    groups: list[list] = []
    for r in rows:
        if groups and r[0] <= groups[-1][0][0] * SAME_SPEED:
            groups[-1].append(r)
        else:
            groups.append([r])
    return [_speed(np.asarray(g, np.float64), rate) for g in groups]


def _speed(g: np.ndarray, rate: float, frames: int | None = None) -> dict:
    p, h = float(np.median(g[:, 0])), float(np.median(g[:, 1]))
    tight = TIGHT_HALF * 1e9 / rate
    return {"period_ns": round(p, 1), "freq_hz": round(1e9 / p, 1), "high_ns": round(h, 1), "low_ns": round(p - h, 1),
            "frames": int(frames if frames is not None else len(g)), "s": int(g[:, 2].min()), "e": int(g[:, 3].max()),
            "rate_ok": min(h, p - h) >= tight}


def undersampled(cap: Capture, speeds: list[dict], clock: str) -> dict | None:
    """The speeds a clock was sampled too slowly at (clock_speeds' rate_ok false): {"clock", "rate_hz", "frames"
    (read at those speeds), "high_ns", "low_ns", "period_ns" (the fastest of them), "suggest_rate_hz" (GOOD_HALF
    samples in the shorter of its high and low)}, or None."""
    bad = [g for g in speeds if not g["rate_ok"]]
    if not bad:
        return None
    rate = float(cap.rate)
    worst = min(bad, key=lambda g: min(g["high_ns"], g["low_ns"]))
    shortest = max(1.0, min(worst["high_ns"], worst["low_ns"]) * rate / 1e9)
    return {"clock": clock, "rate_hz": rate, "frames": sum(g["frames"] for g in bad),
            "period_ns": worst["period_ns"], "high_ns": worst["high_ns"], "low_ns": worst["low_ns"],
            "suggest_rate_hz": float(np.ceil(GOOD_HALF * rate / shortest / 1e6) * 1e6)}


def _frame_items(lay: dict, n) -> list:
    """The items a clocked bus read frame by frame: RVSWD's and SWIO's DMI accesses (not the Debug Module's, which
    span several), else its frames layer (export.LOWER)."""
    if isinstance(lay.get("dmi"), list):
        return lay["dmi"]
    k = _exp.LOWER.get(n.analyzer) if n is not None else None
    v = lay.get(k) if k else None
    return v if isinstance(v, list) else []


def _note_undersampled(claims: list, cap: Capture, res) -> None:
    """analyze's claims on a clock sampled too slowly: told as `undersampled` (as decode's buses), judged on the
    claim's frames (its lower layers, whatever the export gives)."""
    nodes = {(c.protocol, tuple(sorted(c.roles.items()))): c.node for c in res.claims}
    for c in claims:
        clk = (c.get("roles") or {}).get("clk")
        n = nodes.get((c["protocol"], tuple(sorted((c.get("roles") or {}).items()))))
        if not clk or n is None:
            continue
        frames = _frame_items(_exp.layers_of(n), n)
        frame_clocks(cap, clk, frames)
        speeds = clock_speeds(float(cap.rate), frames)
        if speeds:
            c["clock_speeds"] = speeds
        us = undersampled(cap, speeds, clk)
        if us:
            c["undersampled"] = us


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
        for k in (lay if "*" in b.get("layers", ()) else b.get("layers", ())):
            if k in lay:                               # lower layers asked for by name (a pulse, a sampling edge)
                out["result"][k] = lay[k]
    idle = _idle(cap, b, n)
    # a clocked bus's bits and frames, SWIO's pulses, are where it was cut, not what was read: a frame no layout
    # read is unread
    skip = ()
    if n is not None and n.analyzer in staged.SYNC_PROTOCOLS:
        skip = ("bits", "frames")
    elif n is not None and n.analyzer == "swio":
        skip = ("pulses",)
    items = _items(lay, skip)
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
    clk = b["pins"].get("clk")
    speeds = []
    if clk:
        frames = _frame_items(lay, n)
        frame_clocks(cap, clk, frames)
        speeds = clock_speeds(float(cap.rate), frames)
    if speeds:
        out["clock_speeds"] = speeds
    us = undersampled(cap, speeds, clk)
    if us:
        us["restored"] = sum(1 for x in frames if isinstance(x, dict) and "restored" in x)
        out["undersampled"] = us
    return out


def _analyze_split(source, hints, opts, depth: str, head: dict, items=None) -> dict | None:
    """A long part analysed in pieces: what the first piece holds (analyze's claims, confirmed or likely) is taken as
    the buses' roles, and every piece decoded with them (decode's reading, joined); each claim's result is the
    joined one. None when the part is one piece (analyzed as usual)."""
    from . import split as _split
    noisy: dict = {}
    gen = _split.run(source, opts, noisy=noisy)
    p0, c0 = next(gen)
    if p0.cut == "end" and p0.a == 0:
        return None
    # the roles from the first piece that holds any (up to roles_from pieces: the start may be before the bus)
    tried = [(p0, c0)]
    while True:
        p_, c_ = tried[-1]
        ex = export(staged.analyze(c_, hints), c_, "final", None, False, None, depth)
        keep = [c for c in ex["claims"] if c.get("verdict") in ("confirmed", "likely")
                and c["protocol"] != "sync_unknown"]
        if keep or len(tried) >= int(opts.get("roles_from", 3)):
            break
        nxt = next(gen, None)
        if nxt is None:
            break
        tried.append(nxt)
    p0 = tried[-1][0]
    spec = _spec({"buses": [{"protocol": c["protocol"], "pins": c["roles"],
                             "params": {x: c["params"][x] for x in ("baud", "mode") if x in c.get("params", {})}}
                            for c in keep], "depth": depth})
    # every piece on the edge the roles' piece read each bus on
    edges = {j: c["params"]["sample_edge"] for j, c in enumerate(keep)
             if (c.get("params") or {}).get("sample_edge") in ("rise", "fall")}
    work = functools.partial(_piece, spec=spec, head=head, reanalyze=bool((opts or {}).get("reanalyze", 1)),
                             edges=edges)
    sink = _Items(items, spec, head.get("part", 0)) if items is not None else None
    outs = []
    try:
        for p, o in [(p, work((p, c))) for p, c in tried] + [
                (p, o) for (p, _), o in _split.mapped(work, gen, int(opts.get("workers", 1)))]:
            if sink is not None:
                sink.take(p, o)
            outs.append((p, o))
        if sink is not None:
            sink.close()
    finally:
        if sink is not None:
            sink.f.close()
    joined = _join_part(outs, spec, source, opts, noisy)
    if sink is not None:
        for j, b in enumerate(joined["buses"]):
            b["result"] = {"items": {"file": str(items), "counts": sink.counts[j]}}
            b["uninterpreted"] = []
            b["uninterpreted_summary"] = sink.unread[j].doc()
    claims = [{**c, "result": b.get("result", {}), "fit_pieces": b.get("fit", {}).get("pieces"),
               **({"uninterpreted": b["uninterpreted"]} if b.get("uninterpreted") else {}),
               "uninterpreted_summary": b.get("uninterpreted_summary")} for c, b in zip(keep, joined["buses"])]
    return {"claims": claims, "split": {**joined["split"], "roles_from": [p0.s, p0.e],
                                        **({} if keep else {"no_roles": f"no bus claimed in the first {len(tried)} pieces "
                                                                         "(roles_from): analyse a stretch with --from / "
                                                                         "--to, or decode with a spec"}),
                                        "not_carried": [{"protocol": c["protocol"], "roles": c["roles"],
                                                         "verdict": c.get("verdict")} for c in ex["claims"]
                                                        if c not in keep]}}


def _part(cap: Capture, spec: dict, head: dict, strict: bool = False) -> dict:
    names = {c.name for c in cap.channels}
    for b in spec["buses"]:
        miss = [p for p in b["pins"].values() if p not in names]
        if miss:
            raise ValueError(f"{b['protocol']}: pins not in the capture: {', '.join(miss)}")
    miss = [p for p in spec["lines"] if p not in names]
    if miss:
        raise ValueError(f"lines not in the capture: {', '.join(miss)}")
    res = staged.analyze(cap, _hints(spec, strict, [c.name for c in cap.channels]))
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


def decode_file(path: str | Path, spec: dict, split=None, pins: list[str] | None = None, items=None) -> dict:
    """The document for a capture (a WireSkein file, a .sr, a VCD or a format added): every part decoded with the
    spec's roles. A part of more changes than the split's max_edges is read in pieces (wireskein.split; `split`
    overrides spec["split"]: a dict, "key=value,...", or "off"); a WireSkein file of one part is then read from the
    file a block at a time, only the piece being read held. pins: only these channels are read. items: a file the
    decoded items are written to as they are read (ndjson; the document gives their counts), for a capture whose
    items are more than memory holds."""
    from . import split as _split
    path = Path(path)
    spec = _spec(spec)
    opts = _split.options(spec.get("split") if split is None else split)
    srcs = None
    if opts:
        kind = fileformat.sniff(path) if path.is_file() else None
        srcs = _file_sources(path, pins) if kind == "wireskein" else (
            [s] if (s := _file_source(path, pins)) is not None else None)
    if srcs:
        if items is not None:
            Path(items).write_text("")                # written part after part
        doc = {"format": FORMAT, "wireskein": __version__, "spec": spec, "capture": _capture_of(path, srcs[0].raw),
               "parts": [_part_split(src, spec, src.head, opts, items) for src in srcs]}
        return doc
    raw, heads, views, _, spans = _open(path)
    doc = {"format": FORMAT, "wireskein": __version__, "spec": spec, "capture": _capture_of(path, raw), "parts": []}
    if items is not None:
        Path(items).write_text("")                    # written part after part
    for head, cap, span in zip(heads, views, spans):
        cap = only(cap, pins)
        out = (_part_split(_split.CaptureSource(cap), spec, head, opts, items) if opts or items
               else _part(cap, spec, head))
        if span:
            out["parts"] = span
        doc["parts"].append(out)
    return doc


def _file_source(path: Path, pins):
    """A WireSkein file of one part read a block at a time (wireskein.split.FileSource), or None: another kind of
    file, several parts, analog lines or lines of their own step (read whole)."""
    from . import split as _split
    kind = fileformat.sniff(path) if path.is_file() else None
    if kind == "sr":                                # a sigrok session: a block at a time too
        try:
            src = _split.SrSource(path, pins)
        except ValueError:
            return None
        tick = Fraction(src.rate).limit_denominator(10**9)
        src.raw = {"id": None, "tick_hz": [tick.numerator, tick.denominator], "complete": True, "lost": [],
                   "layout": "timeline", "parts": [{}]}
        src.head = {"meta": src.meta, "tick0": 0}
        return src
    srcs = _file_sources(path, pins) if kind == "wireskein" else None
    return srcs[0] if srcs and len(srcs) == 1 else None


def _file_sources(path: Path, pins):
    """Each part of a WireSkein file read a block at a time, or None: parts that follow on without a gap (read
    joined, whole), analog lines, lines of their own step."""
    from . import split as _split
    raw = fileformat.read_header(path)
    parts = raw["parts"]
    if raw.get("layout", "timeline") == "timeline":
        for k in range(1, len(parts)):              # parts that follow on are one stretch: read joined, whole
            if parts[k - 1].get("tick0", 0) + parts[k - 1].get("ticks", 0) >= parts[k].get("tick0", 0):
                return None
    out = []
    for k in range(len(parts)):
        head = fileformat.part_view(raw, k)
        if any(c.get("encoding") not in ("bits", "events") for c in head["channels"]
               if not pins or c["name"] in pins):
            return None
        try:
            src = _split.FileSource(path, k, pins)
        except ValueError:
            return None
        src.raw, src.head = raw, head
        out.append(src)
    return out


def only(cap: Capture, pins: list[str] | None) -> Capture:
    """The capture with only these channels (the others dropped before anything reads them)."""
    if not pins:
        return cap
    names = {c.name for c in cap.channels} | {a.name for a in cap.analog} | {t.name for t in cap.events}
    miss = [p for p in pins if p not in names]
    if miss:
        raise ValueError(f"pins not in the capture: {', '.join(miss)}")
    keep = set(pins)
    return Capture(cap.rate, cap.n_samples, [c for c in cap.channels if c.name in keep], cap.meta,
                   [a for a in cap.analog if a.name in keep], [t for t in cap.events if t.name in keep])


def _part_split(source, spec: dict, head: dict, opts, items=None) -> dict:
    """_part, in pieces when the part is long (wireskein.split), their results joined; only the piece being read is
    held, and of each piece only what it decoded. items: a file the items are written to as the pieces are read
    (one JSON object a line: {"bus", "layer", ...the item}), the document then giving their counts only."""
    from . import split as _split
    watch = [p for b in spec["buses"] for p in b["pins"].values()] + list(spec["lines"])
    noisy: dict = {}
    sink = _Items(items, spec, head.get("part", 0)) if items is not None else None
    outs = []
    try:
        reanalyze = bool((opts or {}).get("reanalyze", 1))
        pieces = _split.run(source, opts, watch=watch or None, noisy=noisy)
        edges, first = _edges_from_first(pieces, spec, head, int((opts or {}).get("roles_from", 3)))
        work = functools.partial(_piece, spec=spec, head=head, reanalyze=reanalyze, edges=edges)
        for (p, _), o in _split.mapped(work, itertools.chain(first, pieces), int((opts or {}).get("workers", 1))):
            if sink is not None:
                sink.take(p, o)
            outs.append((p, o))
        if sink is not None:
            sink.close()
    finally:
        if sink is not None:
            sink.f.close()
    out = _join_part(outs, spec, source, opts, noisy)
    if sink is not None:
        for j, b in enumerate(out["buses"]):
            b["result"] = {"items": {"file": str(items), "counts": sink.counts[j]}}
            b.pop("uninterpreted", None)
            b["uninterpreted_summary"] = sink.unread[j].doc()
    return out


def _edges_from_first(pieces, spec: dict, head: dict, most: int = 3):
    """The edge each clocked bus samples on, from the first of up to `most` pieces that decodes it: a piece read on its
    own may pick the other edge where the lines leave it open (a LinkE at its fastest at 160 MS/s), reading every
    frame a clock early, and is read again on this one (_piece). -> (bus -> edge, the pieces read to find them, to be
    read again with the others)."""
    want = [j for j, b in enumerate(spec["buses"]) if "sample_edge" not in b["params"]]
    first, edges = [], {}
    if not want:
        return edges, first
    for pw in pieces:
        first.append(pw)
        o = _part(pw[1], spec, head, strict=True)
        for j in want:
            b = o["buses"][j]
            edge = (b.get("params") or {}).get("sample_edge")
            if b.get("decoded") and j not in edges and edge in ("rise", "fall"):
                edges[j] = edge
        if len(edges) == len(want) or len(first) >= most:
            break
    return edges, first


class _Items:
    """Items written out as the pieces are read: each piece's list layers, in the capture's ticks, kept by the piece
    they start in; the Debug Module read across the pieces as one."""

    def __init__(self, path, spec: dict, part: int = 0):
        from ._engine.riscv_dm import Across
        self.part = part
        self.f = open(path, "a")                     # the caller empties it first; parts are appended
        self.counts = [{} for _ in spec["buses"]]
        from . import split as _split
        self.unread = [_split.Unread() for _ in spec["buses"]]
        self.dm = {j: Across() for j, b in enumerate(spec["buses"]) if b["protocol"] in ("rvswd", "swio")}

    def _write(self, j: int, layer: str, rows) -> None:
        for r in rows:
            self.f.write(dumps({"part": self.part, "bus": j, "layer": layer, **r}) + "\n")
        self.counts[j][layer] = self.counts[j].get(layer, 0) + len(rows)

    def _dm(self, j: int, entries) -> None:
        self._write(j, "dm", [{**({"s": int(t0)} if t0 is not None else {}), **({"e": int(t1)} if t1 is not None else {}),
                               "op": op, "addr": ad, "value": v, **({"size": sz} if sz else {})}
                              for op, ad, v, sz, t0, t1 in entries])

    def take(self, p, o: dict) -> None:
        from . import split as _split
        for j, b in enumerate(o["buses"]):
            b["_owned"] = _owned(p, b)               # counted before the items are written and dropped
            res = b.get("result", {})
            for k, v in list(res.items()):
                if isinstance(v, list) and not (k == "dm" and j in self.dm):
                    rows = _split.join_items([(p, v)])
                    self._write(j, k, rows)
                    if k == "dmi" and j in self.dm:
                        self.dm[j].feed([{"t": x["s"], "e": x.get("e"), "op": x["op"], "addr": x["addr"],
                                          "data": x["data"], **{q: x[q] for q in ("ok", "form", "status") if q in x}}
                                         for x in rows])
                        self._dm(j, self.dm[j].ready())
                    res[k] = []                       # written: not held
            un = _split.join_items([(p, b.get("uninterpreted", []))])
            self.unread[j].add(un)
            self._write(j, "uninterpreted", un)
            b["uninterpreted"] = []
            res.pop("dm", None)

    def close(self) -> None:
        for j, st in self.dm.items():
            self._dm(j, st.ready(final=True))


def _piece(pw, spec: dict, head: dict, reanalyze: bool = False, edges: dict | None = None) -> dict:
    """One piece decoded (in a worker process, when there are several): (Piece, its capture) -> its part document.
    reanalyze: a piece the roles read badly is analysed on its own too, what it holds given as "_reanalysis" (the
    capture may change on the way: another bus, another speed). edges: bus -> the edge the first piece read it on
    (_edges_from_first); a piece that read a bus on the other is read again on that one."""
    p, wcap = pw
    whole = p.cut == "end" and p.a == 0
    o = _part(wcap, spec, head, strict=not whole)                # one piece: read as always
    other = {j: e for j, e in (edges or {}).items()
             if (o["buses"][j].get("params") or {}).get("sample_edge") not in (None, e)}
    if other:
        given = {**spec, "buses": [{**b, "params": {**b["params"], "sample_edge": other[j]}} if j in other else b
                                   for j, b in enumerate(spec["buses"])]}
        o = _part(wcap, given, head, strict=not whole)
    if reanalyze and not whole and p.edges and _doubt_why(p, o):
        ex = export(staged.analyze(wcap, None), wcap, "final", None, False, None, "protocol")
        o["_reanalysis"] = [{"protocol": c["protocol"], "roles": c["roles"], "verdict": c["verdict"],
                             "score": c["score"], **({"params": c["params"]} if c.get("params") else {})}
                            for c in ex["claims"] if c.get("verdict") in ("confirmed", "likely")]
    return o


def _doubt_why(p, o: dict) -> list[str]:
    """Why the roles read a piece badly: a bus not decoded there, a poor fit, or nothing read well in its own ticks
    (the window reaches past them, so its fit counts the neighbours' too)."""
    why = []
    for j, b in enumerate(o["buses"]):
        owned = b["_owned"] if "_owned" in b else _owned(p, b)
        if not b.get("decoded"):
            why.append(f"bus {j} ({b['protocol']}) not decoded")
        elif b.get("fit", {}).get("score", 1.0) < DOUBT:
            why.append(f"bus {j} ({b['protocol']}) fits {b['fit']['score']}")
        elif not owned:
            why.append(f"bus {j} ({b['protocol']}) read nothing well in this piece's own ticks")
    return why


DOUBT = 0.3       # a piece whose bus fits worse than this is told as doubtful


def _owned(p, b: dict) -> int:
    """The items of a piece's bus that start in its own ticks and read well (not ok: false): items as dicts, or a
    layer of columns keyed by "s" (UART's bytes)."""
    n = 0
    for v in b.get("result", {}).values():
        if isinstance(v, list):
            n += sum(1 for x in v if isinstance(x, dict) and isinstance(x.get("s"), int)
                     and p.s <= x["s"] + p.a < p.e and x.get("ok", True) is not False)
        elif isinstance(v, dict) and isinstance(v.get("s"), list):
            s = np.asarray(v["s"], np.int64) + p.a
            n += int(np.sum((s >= p.s) & (s < p.e)))
    return n


def _join_part(outs: list, spec: dict, source, opts, noisy: dict) -> dict:
    """The pieces' part documents as one (the whole's when there was one piece)."""
    if len(outs) == 1 and outs[0][0].a == 0:
        return outs[0][1]
    pieces = [p for p, _ in outs]
    o0 = outs[0][1]
    out = {k: v for k, v in o0.items() if k not in ("buses", "lines", "undeclared", "claims", "_reanalysis")}
    out["ticks"] = source.n
    out["buses"] = [_join_bus([(p, o["buses"][j]) for p, o in outs], float(source.rate))
                    for j in range(len(spec["buses"]))]
    lines = []
    for j, ln in enumerate(o0["lines"]):
        edges = []
        for p, o in outs:
            e = np.asarray(o["lines"][j]["edges"], np.int64) + p.a
            edges.extend(e[(e >= p.s) & (e < p.e)].tolist())
        lines.append({"pin": ln["pin"], "initial": ln["initial"], "edges": edges})
    out["lines"] = lines
    und: dict = {}
    for p, o in outs:
        for u in o.get("undeclared", []):
            x = und.setdefault(u["pin"], {"pin": u["pin"], "edges": 0, "s": None, "e": None, "bursts": 0})
            x["edges"] += u["edges"]
            x["bursts"] += u["bursts"]
            x["s"] = u["s"] + p.a if x["s"] is None else min(x["s"], u["s"] + p.a)
            x["e"] = u["e"] + p.a if x["e"] is None else max(x["e"], u["e"] + p.a)
    out["undeclared"] = list(und.values())
    # pieces the roles read badly (the bus not decoded there, or a poor fit): the capture may change on the way
    # (another bus, another speed); worth analysing on their own (--from / --to)
    doubtful = []
    for p, o in outs:
        why = _doubt_why(p, o)
        if why and p.edges:
            doubtful.append({"s": p.s, "e": p.e, "seconds": [p.s / source.rate, p.e / source.rate],
                             "why": "; ".join(why),
                             **({"holds": o["_reanalysis"]} if "_reanalysis" in o else {})})
    out["split"] = {"options": opts, "pieces": [p.doc() for p in pieces], **({"trembling": noisy} if noisy else {}),
                    **({"doubtful": doubtful} if doubtful else {})}
    return out


def _join_speeds(groups: list[dict], rate: float) -> list[dict]:
    """Clock speeds from the pieces as one (a speed seen in several pieces joined: its frames summed, its medians
    weighed by them, its first start and last end)."""
    out: list[dict] = []
    for g in sorted(groups, key=lambda g: g["period_ns"]):
        h = out[-1] if out else None
        if h is not None and g["period_ns"] <= h["_fastest"] * SAME_SPEED:
            n = h["frames"] + g["frames"]
            for k in ("period_ns", "high_ns"):
                h[k] = (h[k] * h["frames"] + g[k] * g["frames"]) / n
            h.update(s=min(h["s"], g["s"]), e=max(h["e"], g["e"]), frames=n)
        else:
            out.append({**g, "_fastest": g["period_ns"]})
    return [_speed(np.asarray([[h["period_ns"], h["high_ns"], h["s"], h["e"]]], np.float64), rate, h["frames"])
            for h in out]


def _join_bus(per: list, rate: float) -> dict:
    """A bus's results from the pieces, as one: items in the capture's ticks, each from the piece it starts in."""
    from . import split as _split
    first = next((b for _, b in per if b.get("decoded")), per[0][1])
    out = {k: v for k, v in first.items() if k not in ("result", "uninterpreted", "fit", "_owned")}
    out["decoded"] = any(b.get("decoded") for _, b in per)
    fits = [b.get("fit") for _, b in per if b.get("fit")]
    if fits:
        worst = min(fits, key=lambda f: f["score"])
        out["fit"] = {**worst, "pieces": [f["score"] for f in fits]}
    res: dict = {}
    keys = []
    for _, b in per:
        keys += [k for k in b.get("result", {}) if k not in keys]
    for k in keys:
        vals = [(p, b["result"][k]) for p, b in per if k in b.get("result", {})]
        v0 = vals[0][1]
        if isinstance(v0, list):
            res[k] = _split.join_items(vals)
        elif isinstance(v0, dict) and any(isinstance(x, list) for x in v0.values()) and ("t" in v0 or "s" in v0):
            res[k] = _split.join_columns(vals)
        else:
            res[k] = v0
    if out.get("protocol") in ("rvswd", "swio") and "dmi" in res:
        # the Debug Module keeps state across frames: read again over the pieces' accesses joined
        from ._engine.riscv_dm import accesses
        tx = [{"t": x["s"], "e": x.get("e"), "op": x["op"], "addr": x["addr"], "data": x["data"],
               **{k: x[k] for k in ("ok", "form", "status") if k in x}} for x in res["dmi"]]
        res["dm"] = [{**({"s": int(t0)} if t0 is not None else {}), **({"e": int(t1)} if t1 is not None else {}),
                      "op": op, "addr": ad, "value": v, **({"size": sz} if sz else {})}
                     for op, ad, v, sz, t0, t1 in accesses(tx)]
    out["result"] = res
    out.pop("undersampled", None)
    out.pop("clock_speeds", None)
    speeds = _join_speeds(_split.join_items([(p, b.get("clock_speeds", [])) for p, b in per]), rate)
    if speeds:
        out["clock_speeds"] = speeds
    clk = out.get("pins", {}).get("clk")
    us = undersampled(SimpleNamespace(rate=rate), speeds, clk) if clk else None
    if us:
        # the frames each piece reads past its own ticks are counted by the piece they start in
        us["restored"] = sum(1 for x in res.get("dmi", []) if isinstance(x, dict) and "restored" in x) \
            if "dmi" in res else sum(b.get("undersampled", {}).get("restored", 0) for _, b in per)
        out["undersampled"] = us
    out["uninterpreted"] = _split.join_items([(p, b.get("uninterpreted", [])) for p, b in per])
    un = _split.Unread()
    un.add(out["uninterpreted"])
    out["uninterpreted_summary"] = un.doc()
    return out


def analyze_file(path: str | Path, hints: dict | None = None, mode: str = "final", select: list[str] | None = None,
                 alternatives: bool = False, depth: str = "protocol", window_s: tuple[float, float] | None = None,
                 segment: str | None = None, markers_hint: dict | None = None, events: bool = False,
                 thresholds: dict | None = None, part: int | None = None, split=None,
                 pins: list[str] | None = None, items=None) -> dict:
    """What analyze finds in a capture, as the same family of document as decode_file (wireskein-analysis): the
    capture, then each part with its claims (the export of `mode`, ticks from the part's tick 0). A WireSkein file
    is analysed part by part (analysis never crosses a gap); a .sr or a VCD is one part.
    window_s / segment pick a time range inside one part: `part` names it when the file has several."""
    import time
    from ._engine import markers
    path = Path(path)
    from . import split as _split
    opts = _split.options(split)
    if opts and not (window_s or segment or thresholds) and part is None:
        # a .sr, or each part of a WireSkein file, read from the file as it goes (a short part: loaded, as always)
        kind = fileformat.sniff(path) if path.is_file() else None
        srcs = _file_sources(path, pins) if kind == "wireskein" else (
            [s] if (s := _file_source(path, pins)) is not None else None)
        if srcs:
            t0 = time.perf_counter()
            if items is not None:
                Path(items).write_text("")
            parts = []
            for k, src in enumerate(srcs):
                got = _analyze_split(src, hints, opts, depth, src.head, items)
                if got is None:                     # short: as always
                    cap = only(fileio.load(path, k) if kind == "wireskein" else load(path), pins)
                    res = staged.analyze(cap, hints)
                    ex = export(res, cap, mode if not select else "select", select, alternatives, None, depth)
                    _note_undersampled(ex.get("claims", []), cap, res)
                    got = {"claims": ex["claims"], **{x: ex[x] for x in ("relations",) if x in ex}}
                parts.append({**_part_head(src.head, k, src.n, None), **got})
            doc = {"format": FORMAT, "wireskein": __version__, "hints": hints,
                   "capture": _capture_of(path, srcs[0].raw), "parts": parts}
            doc["analysis_seconds"] = round(time.perf_counter() - t0, 3)
            return doc
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
    if items is not None:
        Path(items).write_text("")                    # written part after part
    for k in ks:
        cap = views[k]
        if thresholds:
            cap = as_logic(cap, thresholds)
        cap = only(cap, pins)
        if not (window_s or segment) and opts:
            got = _analyze_split(_split.CaptureSource(cap), hints, opts, depth, heads[k], items)
            if got is not None:                    # long: roles from the first piece, then every piece read with them
                doc["parts"].append({**_part_head(heads[k], firsts[k], cap.n_samples, spans[k]), **got})
                continue
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
        _note_undersampled(ex.get("claims", []), cap, res)
        out = {**_part_head(heads[k], firsts[k], cap.n_samples, spans[k]), "claims": ex["claims"],
               **{x: ex[x] for x in ("window", "relations") if x in ex}}
        if hit:
            out["segment"] = {"path": hit[0].path(), "raw": hit[0].raw, "issues": hit[0].issues}
            if events or mode == "all":
                out["events"] = seg_events
        doc["parts"].append(out)
    doc["analysis_seconds"] = round(time.perf_counter() - t0, 3)
    return doc
