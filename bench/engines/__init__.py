"""Engines of other directions than WireSkein's own, kept side by side to learn from each other's weak points
(bench/notes/2026-11-22-structure-experiments.ja.md): each maps a capture (a part's changes view) to claims in the
analysis format (protocol, roles, verdict, score, margin, params, evidence, layers), judged by the bench as the main
engine is (bench/algos/e2e/engines.py registers them as trials).

- light:    readers on the lines as captured, candidates by each protocol's least conditions, the best set of claims
- views:    light, read on a fixed family of views (as captured, pulses of a sample out, bounces merged), best per
            candidate
- declarative: protocols as tables (protocols.toml) read by one interpreter; candidates, noise handling and output
            from the declarations alone
"""
from __future__ import annotations

import itertools

from . import readers as R

SURE, SURE_MARGIN, LIKELY = 0.85, 0.15, 0.6


def candidates(cap):
    """Every assignment the protocols' least conditions allow, read: [(protocol, roles, fit, params, layers,
    evidence)]. UART: a line of 20 changes or more; I2C: SCL changes no less than SDA, both idle high; SPI: a select
    changes far less than the clock, a data line no more than half again as often."""
    n, rate = cap.n_samples, cap.rate
    act = [c for c in cap.channels if len(c.edges) >= 4]
    out = []
    for c in act:
        if len(c.edges) >= 20:
            r = R.read_uart(c, n, rate)
            if r:
                out.append(("uart", {"data": c.name}, r[0], r[1], r[2], {}))
    for a, b in itertools.permutations(act, 2):
        if len(a.edges) < len(b.edges) or a.initial != 1 or b.initial != 1:
            continue
        r = R.read_i2c(a, b, n)
        if r:
            out.append(("i2c", {"scl": a.name, "sda": b.name}, r[0], r[1], r[2], {}))
    for clk in act:
        others = [c for c in act if c is not clk]
        for cs in [None] + others:
            if cs is not None and len(cs.edges) * 8 > len(clk.edges):
                continue
            ds = [d for d in others if d is not cs and len(d.edges) <= 1.5 * len(clk.edges)]
            single = []
            for d in ds:
                r = R.read_spi(clk, [d], cs, n)
                if r and r[0] >= LIKELY:
                    single.append((r[0], d))
            if not single:
                continue
            single.sort(key=lambda x: -x[0])
            datas = [d for _, d in single[:2]]
            r = R.read_spi(clk, datas, cs, n)
            if r:
                roles = {"clk": clk.name, **{f"data{i}": d.name for i, d in enumerate(datas)},
                         **({"cs": cs.name} if cs is not None else {})}
                out.append(("spi", roles, r[0], r[1], r[2], r[3]))
    return out


def claims_of(cands):
    """The best set of claims using each line once, greedily by fit times the lines a claim explains; each claim's
    margin over the best rival sharing a line, its verdict from the two."""
    order = sorted(cands, key=lambda c: (-c[2] * len(set(c[1].values())), -c[2]))
    used, out = set(), []
    for proto, roles, fit, params, layers, ev in order:
        if fit < LIKELY:
            continue
        lines = set(roles.values())
        if lines & used:
            continue
        used |= lines
        # rivals: other readings of these lines; the same protocol on some of the same lines (the bus read without its
        # select, or with fewer data lines) is this reading less complete, no rival
        rivals = [c for c in cands if set(c[1].values()) & lines and (c[0], c[1]) != (proto, roles)
                  and not (c[0] == proto and set(c[1].values()) <= lines)]
        ru = max(rivals, key=lambda c: c[2], default=None)
        margin = fit - (ru[2] if ru else 0.0)
        verdict = "confirmed" if fit >= SURE and margin >= SURE_MARGIN else "likely" if margin >= 0.05 else "ambiguous"
        out.append({"protocol": proto, "roles": roles, "verdict": verdict, "score": round(float(fit), 4),
                    "margin": round(float(margin), 4), "params": params, "evidence": ev, "layers": layers,
                    **({"runner_up": {"protocol": ru[0], "roles": ru[1], "score": round(float(ru[2]), 4)}} if ru else {})})
    return out


def light(cap):
    return claims_of(candidates(cap))


def _views(cap):
    from dataclasses import replace
    from wireskein._engine import views as V
    yield "raw", cap
    yield "despike", replace(cap, channels=[replace(c, edges=V.pulses_out(c.edges, 2 * c.step)) for c in cap.channels])
    yield "debounce", replace(cap, channels=[replace(c, edges=V.bounces_merged(c.edges, 5 * c.step))
                                             for c in cap.channels])


def views(cap):
    best = {}
    for name, v in _views(cap):
        for c in candidates(v):
            k = (c[0], tuple(sorted(c[1].items())))
            if k not in best or c[2] > best[k][2]:
                best[k] = (*c[:3], {**c[3], "view": name}, *c[4:])
    return claims_of(list(best.values()))


def _declarative(cap):
    import importlib
    return importlib.import_module(__name__ + ".declarative").engine(cap)


ENGINES = {"light": light, "views": views, "declarative": _declarative}
