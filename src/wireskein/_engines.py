"""The analysis: WireSkein's engine reads the buses, then the upper stage (wireskein._upper) reads what rides on the
claims it chose (bench/notes/2026-11-23-extension-spec.ja.md)."""
from __future__ import annotations

from . import _upper
from ._engine import staged

def analyze(view, hints: dict | None = None):
    """The engine's result after the upper stage."""
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
