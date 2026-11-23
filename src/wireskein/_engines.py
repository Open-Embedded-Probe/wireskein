"""Engines side by side (bench/notes/2026-11-23-extension-spec.ja.md §3.2): WireSkein's own engine reads the
buses it knows; engines added from outside (wireskein.plugins.register_engine) read protocols of their own, each to
claims in the analysis format. Their claims join the main engine's by one rule (merge): on lines the main engine left
unexplained (no claim, or only unknown-sync / ambiguous ones), or first where a hint names their protocol; between
added engines the higher score, then the name. Protocols of others that ride on an added engine's protocol
(over = its name) read its claims there (upper)."""
from __future__ import annotations

from . import _upper
from ._engine import staged

SURE = ("confirmed", "likely")


def analyze(view, hints: dict | None = None):
    """The main engine's result after the upper stage, with the added engines' claims beside it (res.side)."""
    res = main(view, hints)
    res.side = _side(view, hints)
    res.hints = hints
    return res


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


def _side(view, hints) -> list[dict]:
    from . import plugins
    out = []
    asked = set((hints or {}).get("protocols") or [])
    for name, (engine, protocols) in sorted(plugins.engines().items()):
        if asked and not (asked & set(protocols)):
            continue
        try:
            claims = engine(view, hints) or []
        except Exception as e:  # noqa: BLE001 - another's engine: its failure is shown, the analysis goes on
            out.append({"protocol": protocols[0], "roles": {}, "verdict": "error", "score": 0.0, "margin": 0.0,
                        "engine": name, "error": f"{type(e).__name__}: {e}"})
            continue
        for c in claims:
            if c.get("protocol") not in protocols:
                raise ValueError(f"engine {name}: a claim of {c.get('protocol')!r}, not one of its protocols "
                                 f"({', '.join(protocols)})")
            out.append({**c, "engine": name})
    return out


def merge(claims: list[dict], side: list[dict], hints: dict | None = None) -> list[dict]:
    """The main engine's claims and the added engines' by the rule of the module's docstring."""
    if not side:
        return claims
    asked = set((hints or {}).get("protocols") or [])
    first = [c for c in side if c.get("protocol") in asked and c.get("verdict") in SURE]
    held = {p for c in first for p in c["roles"].values()}
    main = [c for c in claims if not (held & set(c["roles"].values()))]
    explained = {p for c in main if c.get("verdict") in SURE and c.get("protocol") != "sync_unknown"
                 for p in c["roles"].values()}
    out = list(first)
    taken = set(held)
    for c in sorted((c for c in side if c not in first), key=lambda c: (-float(c.get("score", 0)), c["engine"])):
        lines = set(c["roles"].values())
        if c.get("verdict") == "error":
            out.append(c)
            continue
        if lines & (explained | taken):
            continue
        taken |= lines
        out.append(c)
    return _on_added(main + out)


def _on_added(claims: list[dict]) -> list[dict]:
    """The upper stage on the added engines' claims (wireskein._upper, as on the main engine's): protocols of others
    on an added engine's protocol (over = its name) read the claim as the analysis exports it, their readings in its
    result, their checks raising its score."""
    for c in claims:
        if "engine" not in c or c.get("verdict") == "error":
            continue
        r = {"protocol": c["protocol"], "roles": c["roles"], "params": c.get("params") or {},
             "score": float(c.get("score", 0)), "layers": c.get("layers") or c.get("result") or {}}
        up = _upper.finish(_upper.read(r, None, None))
        for name, m in up.user.items():
            c.setdefault("result", {})[name] = {k: v for k, v in m.items() if k != "score"}
        if up.user:
            c["score"] = round(up.score, 4)
    return claims
