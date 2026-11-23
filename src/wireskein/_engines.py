"""Engines side by side (bench/notes/2026-11-23-extension-spec.ja.md §3.2): WireSkein's own engine reads the
buses it knows; engines added from outside (wireskein.plugins.register_engine) read protocols of their own, each to
claims in the analysis format. Their claims join the main engine's by one rule (merge): on lines the main engine left
unexplained (no claim, or only unknown-sync / ambiguous ones), or first where a hint names their protocol; between
added engines the higher score, then the name. Protocols of others that ride on an added engine's protocol
(over = its name) read its claims there (upper)."""
from __future__ import annotations

from ._engine import staged

SURE = ("confirmed", "likely")


def analyze(view, hints: dict | None = None):
    """The main engine's result, with the added engines' claims beside it (res.side)."""
    res = staged.analyze(view, hints)
    res.side = _side(view, hints)
    res.hints = hints
    return res


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
    return _upper(main + out)


def _upper(claims: list[dict]) -> list[dict]:
    """Protocols of others on an added engine's protocol (over = its name): each reads the claim as the analysis
    exports it; its checks raise the claim's score as on any bus (staged's formula)."""
    from . import plugins
    protos = plugins.protocols()
    for c in claims:
        if "engine" not in c:
            continue
        mine = [p for p in protos.values() if c["protocol"] in p.over]
        for p in mine:
            bus = {"roles": dict(c["roles"]), "tick_hz": None, **(c.get("layers") or c.get("result") or {})}
            try:
                got = p.decode(bus) or {}
            except Exception as e:  # noqa: BLE001
                got = {"messages": [], "checks_passed": 0, "checks_failed": 0, "error": f"{type(e).__name__}: {e}"}
            m = {"messages": list(got.get("messages", [])), "checks_passed": int(got.get("checks_passed", 0)),
                 "checks_failed": int(got.get("checks_failed", 0)), **({"error": got["error"]} if "error" in got else {})}
            s = plugins._score(m)
            c.setdefault("result", {})[p.name] = m
            t = float(c.get("score", 0))
            c["score"] = round(t + 0.5 * (1 - t) * s * t, 4)
    return claims
