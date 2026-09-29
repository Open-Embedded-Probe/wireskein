"""End-to-end analysis: features -> full hypothesis expansion -> scoring ->
explanation packing -> per-bus verdicts with margins."""

from __future__ import annotations

import time
from dataclasses import dataclass, field

import numpy as np

from . import features as feat
from .analyzers.i2c import I2c
from .analyzers.spi import Spi
from .analyzers.uart import Uart
from .analyzers.upper import Lines, MarkerGrammar, ModbusRtu, Nmea
from .model import Capture
from .scoring import DefaultScorer
from .stack import Context, Engine, Node, explanations

CLAIM_COST = 0.3  # a hypothesis must beat this per channel to be worth claiming


def default_analyzers():
    return [Uart(), I2c(), Spi(), Lines(), Nmea(), ModbusRtu(), MarkerGrammar()]


@dataclass
class Claim:
    protocol: str
    roles: dict
    params: dict
    total: float
    margin: float
    verdict: str            # confirmed / likely / ambiguous
    node: Node
    runner_up: Node | None


@dataclass
class Result:
    roots: list[Node]
    explanations: list
    claims: list[Claim]
    runs: int
    seconds: float
    features: dict
    probe_runs: int = 0
    abandoned: int = 0
    engine: object = None
    excluded: int = 0


def value(n: Node) -> float:
    return len(n.roles) * (n.total - CLAIM_COST)


def equivalent(a: Node, b: Node) -> bool:
    """Same conclusion for the purpose of a verdict: same analyzer and roles and
    the same decoded content (e.g. 8N1 vs 7N2 on 7-bit text, baud within 3%,
    or SPI bit order, which only upper layers can tell apart)."""
    if a.analyzer != b.analyzer or a.roles != b.roles:
        return False
    if a.output is None or b.output is None:  # abandoned in the probe stage
        return a.params == b.params
    if a.analyzer == "uart":
        if a.params["idle"] != b.params["idle"] or abs(a.params["baud"] / b.params["baud"] - 1) > 0.03:
            return False
        va, vb = a.output.items["value"], b.output.items["value"]
        return len(va) == len(vb) and bool(np.all((va & 0x7F) == (vb & 0x7F)))
    if a.analyzer == "spi":
        return a.params["sample_edge"] == b.params["sample_edge"] and a.params.get("cs_active") == b.params.get("cs_active")
    return True


def channel_labels(nodes: list[Node]) -> dict[str, tuple]:
    """channel -> (analyzer, role, key params) under an explanation."""
    out = {}
    for n in nodes:
        key = ()
        if n.analyzer == "uart":
            key = (round(n.params["baud"] / 1000, 0), n.params["idle"])
        elif n.analyzer == "spi":
            key = (n.params["sample_edge"],)
        for role, ch in n.roles.items():
            out[ch] = (n.analyzer, role, key)
    return out


def changed_channels(a: list[Node], b: list[Node]) -> int:
    la, lb = channel_labels(a), channel_labels(b)
    return sum(1 for ch in set(la) | set(lb) if la.get(ch) != lb.get(ch))


def verdict(total: float, margin: float) -> str:
    if total >= 0.85 and margin >= 0.15:
        return "confirmed"
    if total >= 0.6 and margin >= 0.05:
        return "likely"
    return "ambiguous"


def analyze(cap: Capture, scorer=None, analyzers=None, probe=None, exclude: bool = False) -> Result:
    t0 = time.perf_counter()
    fs = feat.features(cap)
    active = [n for n, f in fs.items() if not f.static]
    ctx = Context(cap, fs, active)
    exclusion = None
    if exclude:
        from .exclude import RULES, excluded_by
        from .survey import survey
        sv = survey(cap, fs)
        safe = [r for r in RULES if r.kind == "definitional"]
        exclusion = lambda name, params: excluded_by(name, params, sv, cap, safe)  # noqa: E731
    eng = Engine(analyzers or default_analyzers(), scorer or DefaultScorer(), probe=probe, exclusion=exclusion)
    roots = eng.expand(ctx)
    exps = explanations(roots, value)
    claims = []
    if exps:
        best_score, best = exps[0]
        for n in best:
            # Margin: how much worse is the best explanation in which this claim
            # (and anything equivalent to it) is not allowed, per channel it covers.
            alt = explanations(roots, value, top=1, exclude=lambda r, n=n: equivalent(r, n))
            alt_score, alt_set = alt[0] if alt else (0.0, [])
            margin = (best_score - alt_score) / max(1, changed_channels(best, alt_set))
            chans = set(n.roles.values())
            ru = max((r for r in alt_set if chans & set(r.roles.values())), key=lambda r: r.total, default=None)
            claims.append(Claim(n.analyzer, dict(n.roles), dict(n.params), n.total, margin,
                                verdict(n.total, margin), n, ru))
    return Result(roots, exps, claims, eng.runs, time.perf_counter() - t0, fs, eng.probe_runs, eng.abandoned, eng, eng.excluded)
