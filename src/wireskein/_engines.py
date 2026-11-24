"""The analysis: WireSkein's engine reads the buses, then the upper stage (wireskein._upper) reads what rides on the
claims it chose (bench/notes/2026-11-23-extension-spec.ja.md).

The engine is Python's unless WIRESKEIN_ENGINE=rust names the Rust core (the package wireskein-core, module
wireskein_core, installed apart: bench/notes/2026-11-24-rust-plan.ja.md §2), to compare the two and as a way out. The
Rust core's claims, in the analysis format, go through the same upper stage; what needs the Python engine's own
readings (a decode spec's buses, segments, verify's I2C / SPI / UART checks) stays Python's, said once on stderr."""
from __future__ import annotations

import json
import os
import sys
from dataclasses import dataclass, field

from . import _upper
from ._engine import staged

ENGINE_ENV = "WIRESKEIN_ENGINE"
ENGINES = ("python", "rust")


def engine() -> str:
    """The engine WIRESKEIN_ENGINE names: "python" (the default) or "rust"."""
    v = os.environ.get(ENGINE_ENV, "").strip().lower() or "python"
    if v not in ENGINES:
        raise ValueError(f"{ENGINE_ENV}={v!r}: one of {', '.join(ENGINES)}")
    return v


def analyze(view, hints: dict | None = None):
    """The engine's result after the upper stage."""
    if engine() == "rust":
        return rust(view, hints)
    return main(view, hints)


def main(view, hints: dict | None = None):
    """The main engine's claims, then the upper stage on them (wireskein._upper): each claim's upper readings
    (claim.upper), its score raised by them, its margin raised as much, its verdict made again by the engine's rule;
    the UART pairs' relations (res.relations)."""
    res = staged.analyze(view, hints)
    res.hints = hints
    mine = [c for c in res.claims if not c.bus and c.node.output is not None]
    ups, res.relations = _upper.run([reading(c.node, c.total) for c in mine], float(view.rate), hints)
    for c, up in zip(mine, ups):
        c.upper = up
        d = up.score - c.total
        if d:
            c.total, c.margin = up.score, c.margin + d
            c.verdict = staged.regrade(c, c.total, c.margin)
    return res


_told: set = set()


def python_only(what: str) -> None:
    """Under WIRESKEIN_ENGINE=rust, say once that `what` is read by the Python engine (it needs its readings)."""
    if engine() == "rust" and what not in _told:
        _told.add(what)
        print(f"wireskein: {ENGINE_ENV}=rust: {what} read by the Python engine", file=sys.stderr)


# ---------------- the Rust core ----------------

@dataclass
class ForeignNode:
    """A reading of another engine as the export and the upper stage take it: the analysis format's claim (its
    layers already as the format has them: own_layers, export.layers_of)."""
    analyzer: str
    params: dict
    roles: dict
    total: float
    metrics: dict
    own_layers: dict
    output: object = True          # (a reading with output, as the Python engine's nodes that read something)


@dataclass
class ForeignResult:
    """The claims of another engine, as wireskein.decode and the export take a result."""
    claims: list
    hints: dict | None = None
    relations: list = field(default_factory=list)
    engine: str = "rust"

    @property
    def roots(self) -> list:
        return [c.node for c in self.claims]


_RANK = {"ambiguous": 0, "likely": 1, "confirmed": 2}


def _core():
    try:
        import wireskein_core
    except ImportError as e:
        raise RuntimeError(f"{ENGINE_ENV}=rust needs the package wireskein-core (module wireskein_core), built from "
                           f"crates/wireskein-py (pip install ./crates/wireskein-py): {e}") from None
    return wireskein_core


def rust(view, hints: dict | None = None) -> ForeignResult:
    """The Rust core's claims (wireskein_core.analyze, the analysis format), then the upper stage on them as main
    does: each claim's score raised by its upper readings, its margin as much, its verdict made again by the same rule
    (never lowered: the Rust core's rival readings are not told), the UART pairs' relations."""
    import numpy as np
    core = _core()
    chans = [(c.name, int(c.initial), np.ascontiguousarray(c.edges, dtype=np.int64)) for c in view.channels]
    # the engine's claims alone: the upper stage is Python's here (its device packs, definitions directories)
    out = json.loads(core.analyze(float(view.rate), int(view.n_samples), chans, json.dumps(hints), upper=False))
    claims = []
    for d in out:
        n = ForeignNode(d["protocol"], dict(d.get("params") or {}), dict(d["roles"]), float(d["score"]),
                        dict(d.get("evidence") or {}), dict(d.get("layers") or {}))
        claims.append(staged.Claim(d["protocol"], dict(d["roles"]), dict(n.params), float(d["score"]),
                                   float(d["margin"]), d["verdict"], n, None))
    res = ForeignResult(claims, hints)
    ups, res.relations = _upper.run([reading(c.node, c.total) for c in claims], float(view.rate), hints)
    for c, up in zip(claims, ups):
        c.upper = up
        d = up.score - c.total
        if d:
            c.total, c.margin = up.score, c.margin + d
            v = staged.regrade(c, c.total, c.margin)
            if _RANK.get(v, -1) > _RANK.get(c.verdict, -1):
                c.verdict = v
    return res


def reading(node, score: float | None = None) -> dict:
    """A reading of the main engine as the upper stage takes it: the bus as the analysis exports it."""
    from ._engine import export
    return {"protocol": node.analyzer, "roles": dict(node.roles), "params": dict(node.params),
            "score": float(node.total if score is None else score), "layers": export.layers_of(node)}


def upper_of(node, rate: float, hints: dict | None = None, claims=()):
    """The upper stage on one reading of the main engine (a bus named by a spec, a test's): its claim's when it is
    one, else read alone (no pairs)."""
    c = next((c for c in claims if c.node is node and getattr(c, "upper", None) is not None), None)
    if c is not None:
        return c.upper
    return _upper.finish(_upper.read(reading(node), rate, hints))
