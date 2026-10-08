"""Hypothesis nodes and the packing of root hypotheses into explanations.

A plugin turns a typed stream into Nodes (a protocol with roles, parameters,
an output stream and raw metrics); the scorer turns metrics into scores; the
staged engine (staged.py) packs root hypotheses on disjoint channels into the
best explanations.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Iterable

import numpy as np


@dataclass
class Stream:
    kind: str                 # e.g. "bytes", "i2c.transactions", "lines"
    items: Any                # analyzer-defined payload (lists / arrays with sample ranges)
    meta: dict = field(default_factory=dict)


@dataclass(eq=False)  # identity: nodes link to parents and children
class Node:
    analyzer: str
    params: dict
    roles: dict[str, str]           # role -> channel (inherited from the base node)
    output: Stream | None
    metrics: dict[str, float]
    parent: "Node | None" = None
    children: list["Node"] = field(default_factory=list)
    layer_score: float = 0.0
    total: float = 0.0
    note: str = ""

    @property
    def depth(self) -> int:
        return 0 if self.parent is None else self.parent.depth + 1

    def label(self) -> str:
        p = ",".join(f"{k}={_fmt(v)}" for k, v in self.params.items())
        return f"{self.analyzer}({p})"

    def walk(self) -> Iterable["Node"]:
        yield self
        for c in self.children:
            yield from c.walk()


def _fmt(v):
    return f"{v:.6g}" if isinstance(v, float) else str(v)


# --- explanations: channel-disjoint sets of root hypotheses -----------------

def explanations(roots: list[Node], value, top: int = 5, per_channel_set: int = 3,
                 exclude=None) -> list[tuple[float, list[Node]]]:
    """Exhaustive set packing over root hypotheses. `value(node)` is the
    contribution of a node to an explanation. Returns best explanations, best first.

    To keep the search finite, only the best `per_channel_set` hypotheses per
    (analyzer, channel set) enter the packing — alternatives inside one channel
    set are compared separately via margins, not dropped from the report.
    """
    groups: dict[tuple, list[Node]] = {}
    for r in roots:
        if exclude and exclude(r):
            continue
        groups.setdefault((r.analyzer, tuple(sorted(r.roles.items()))), []).append(r)
    pool = []
    for g in groups.values():
        g.sort(key=lambda n: -n.total)
        pool.extend(g[:per_channel_set])
    pool = [n for n in pool if value(n) > 0]
    pool.sort(key=lambda n: -value(n))
    best: list[tuple[float, list[Node]]] = []

    # Branch and bound. A node "owns" the channels no other chosen node may use
    # . The bound is the best remaining value share per free channel.
    owned = [set(n.roles.values()) for n in pool]
    chans = sorted(set().union(*owned)) if owned else []
    ci = {c: k for k, c in enumerate(chans)}
    share = np.zeros((len(pool) + 1, len(chans)))
    for i in range(len(pool) - 1, -1, -1):
        share[i] = share[i + 1]
        v = value(pool[i]) / len(owned[i])
        for c in owned[i]:
            share[i, ci[c]] = max(share[i, ci[c]], v)

    def dfs(i: int, chosen: list[Node], used: set, score: float) -> None:
        if len(best) >= top:
            free = [ci[c] for c in chans if c not in used]
            if score + share[i, free].sum() <= best[-1][0] + 1e-12:
                return
        if i == len(pool):
            best.append((score, list(chosen)))
            best.sort(key=lambda x: -x[0])
            del best[top:]
            return
        n = pool[i]
        if not (owned[i] & used):
            chosen.append(n)
            dfs(i + 1, chosen, used | owned[i], score + value(n))
            chosen.pop()
        dfs(i + 1, chosen, used, score)

    dfs(0, [], set(), 0.0)
    # dedupe identical sets
    seen, out = set(), []
    for s, ns in best:
        k = frozenset(id(n) for n in ns)
        if k not in seen:
            seen.add(k)
            out.append((s, ns))
    return out[:top]
