"""`ws decode`: the buses a spec names, decoded with exactly those roles, as one document for a data repository.

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

from . import __version__, fileformat
from ._engine import export, fileio, staged, timing
from ._engine.model import Capture

FORMAT = "wireskein-analysis/1"
IDLE_S = 1e-3          # a bus with no bit times of its own: activity apart by more than this is two bursts
IDLE_BITS = 16         # else: more than this many bit times without a change


def _spec(spec: dict) -> dict:
    out = {"buses": [], "lines": list(spec.get("lines") or []), "devices": spec.get("devices"),
           "depth": spec.get("depth", "device")}
    for b in spec.get("buses") or []:
        if not b.get("protocol") or not b.get("pins"):
            raise ValueError(f"a bus needs protocol and pins: {b}")
        out["buses"].append({"protocol": b["protocol"], "pins": dict(b["pins"]), "params": dict(b.get("params") or {}),
                             **({"idle_s": b["idle_s"]} if "idle_s" in b else {})})
    if out["depth"] not in export.DEPTHS:
        raise ValueError(f"depth: one of {', '.join(export.DEPTHS)}")
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


def _items(lay: dict) -> list[tuple[int, int]]:
    """(s, e) of every item with a time in the bus's layers (e = s when an item has no end)."""
    out = []
    for v in lay.values():
        if isinstance(v, list):
            out += [(int(x["s"]), int(x.get("e") if x.get("e") is not None else x["s"]))
                    for x in v if isinstance(x, dict) and x.get("s") is not None]
        elif isinstance(v, dict) and isinstance(v.get("s"), list):
            out += [(int(s), int(s)) for s in v["s"]]
    return out


def _bursts(cap: Capture, pins: list[str], idle: int) -> list[tuple[int, int, int]]:
    """(first change, last change, changes) of the activity on `pins`, apart by more than `idle` ticks."""
    e = np.sort(np.concatenate([np.asarray(cap.channel(p).edges, np.int64) for p in pins] or [np.zeros(0, np.int64)]))
    if not len(e):
        return []
    cut = np.flatnonzero(np.diff(e) > idle) + 1
    return [(int(x[0]), int(x[-1]), len(x)) for x in np.split(e, cut)]


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
        lay = export.layers_of(n)
        c = next((c for c in claimed if c.node is n), None)
        out.update(decoded=True, params={k: (float(v) if isinstance(v, (np.floating, float)) else v)
                                                    for k, v in n.params.items()},
                   fit={"score": round(float(n.total), 4), "claimed": c is not None,
                        **({"verdict": c.verdict} if c is not None else {})})
        k = export.LOWER.get(n.analyzer)
        if depth == "transport":
            out["result"] = {}
        elif depth == "frames":
            out["result"] = {k: lay[k]} if k and k in lay else {}
        else:
            out["result"] = export.final_of(n, lay)
            if k and k in lay and k not in out["result"]:
                out["result"][k] = lay[k]          # the transfers stay beside a higher reading (sd over spi)
            if depth == "device":
                ds = export.devices_of(n, with_candidates=True)
                if ds:
                    out["devices"] = ds
    idle = _idle(cap, b, n)
    items = _items(lay)
    starts = np.sort(np.array([s for s, _ in items], np.int64))
    un = []
    for s, e, k in _bursts(cap, list(b["pins"].values()), idle):
        j = np.searchsorted(starts, s - idle)
        if j < len(starts) and starts[j] <= e:
            continue
        if any(a <= s and e <= z for a, z in items):
            continue
        un.append({"s": s, "e": e, "edges": k})
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
        rest.append({"pin": c.name, "edges": int(len(c.edges)), "s": bs[0][0], "e": bs[-1][1], "bursts": len(bs)})
    out["undeclared"] = rest
    others = [c for c in res.claims if not set(c.roles.values()) & named]
    if others:
        out["claims"] = export.export(SimpleNamespace(claims=others), cap)["claims"]
    return out


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def decode_file(path: str | Path, spec: dict) -> dict:
    """The document for a WireSkein file: every part decoded with the spec's roles."""
    path = Path(path)
    spec = _spec(spec)
    raw = fileformat.read_header(path)
    tick = Fraction(*raw["tick_hz"])
    doc = {"format": FORMAT, "wireskein": __version__, "spec": spec,
           "capture": {"file": path.name, "sha256": _sha256(path), "id": raw.get("id"),
                       "tick_hz": [tick.numerator, tick.denominator], "complete": raw.get("complete", True),
                       **({"lost": raw["lost"]} if raw.get("lost") else {}), "layout": raw.get("layout", "timeline"),
                       "parts": len(raw["parts"])},
           "parts": []}
    for k in range(len(raw["parts"])):
        head = fileformat.part_view(raw, k)
        cap = fileio.load(path, k)
        doc["parts"].append(_part(cap, spec, head))
    return doc
