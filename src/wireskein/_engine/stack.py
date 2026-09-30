"""Layered analysis engine.

Analyzers consume a typed stream and produce another. The engine expands every
proposal of every analyzer that accepts a produced stream, recursively, with
no pruning. Scoring is kept out of analyzers: they only report raw metrics, and
a Scorer turns metrics into layer scores and combines them across layers. Both
analyzers and scorers are meant to be swappable (future plugin boundary).
"""

from __future__ import annotations

import itertools
import time
from dataclasses import dataclass, field
from typing import Any, Iterable, Protocol

import numpy as np

from . import kernels
from .features import ChannelFeatures
from .model import Capture, Channel


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


@dataclass
class Context:
    capture: Capture
    features: dict[str, ChannelFeatures]
    active: list[str]


class Analyzer(Protocol):
    name: str
    consumes: str   # "capture" for line-level decoders
    produces: str

    def propose(self, ctx: Context, parent: Node | None) -> Iterable[dict]: ...

    def run(self, ctx: Context, parent: Node | None, params: dict) -> tuple[dict[str, str], Stream | None, dict]: ...


class Scorer(Protocol):
    def layer(self, node: Node) -> float: ...

    def combine(self, node: Node) -> float: ...


@dataclass
class ProbeConfig:
    """Early abandonment: decode a few windows first and give up on the
    hypothesis only if every window scores below `threshold`. Windows are taken
    around the 10/50/90 % points of the activity on the hypothesis' own
    channels, so a bad start (mid-frame capture, junk before traffic) cannot
    kill a hypothesis on its own."""
    window_edges: int = 600
    positions: tuple = (0.1, 0.5, 0.9)
    threshold: float = 0.1
    min_edges: int = 4000   # below this, a full decode is cheap enough


def slice_capture(cap: Capture, s0: int, s1: int) -> Capture:
    chans = []
    for ch in cap.channels:
        lo, hi = np.searchsorted(ch.edges, [s0, s1], side="right")
        init = int(ch.initial ^ (lo & 1))
        chans.append(Channel(ch.name, init, ch.edges[lo:hi] - s0, ch.step, (ch.phase - s0) % ch.step))
    return Capture(cap.rate, int(s1 - s0), chans, meta={"slice": (int(s0), int(s1))})


class Engine:
    def __init__(self, analyzers: list[Analyzer], scorer: Scorer, max_depth: int = 4,
                 probe: ProbeConfig | None = None, exclusion=None):
        self.analyzers = analyzers
        self.scorer = scorer
        self.max_depth = max_depth
        self.probe = probe
        self.exclusion = exclusion  # callable(analyzer_name, params) -> list of rule names, or None
        self.excluded = 0
        self.runs = 0
        self.probe_runs = 0
        self.abandoned = 0
        self._windows: dict = {}
        # per analyzer: calls, wall seconds, kernel seconds, input elements, output elements
        self.stats: dict[str, list] = {}
        self.score_calls = 0
        self.score_seconds = 0.0

    def _call(self, a, ctx, parent, params, tag=""):
        k0, t0 = kernels.kernel_seconds(), time.perf_counter()
        roles, out, m = a.run(ctx, parent, params)
        st = self.stats.setdefault(a.name + tag, [0, 0.0, 0.0, 0, 0])
        st[0] += 1
        st[1] += time.perf_counter() - t0
        st[2] += kernels.kernel_seconds() - k0
        if parent is None and hasattr(a, "probe_channels"):
            st[3] += sum(len(ctx.capture.channel(c).edges) for c in a.probe_channels(params))
        elif parent is not None and parent.output is not None:
            st[3] += _count(parent.output.items)
        if out is not None:
            st[4] += _count(out.items)
        return roles, out, m

    def windows(self, ctx: Context, chans: tuple[str, ...]) -> list[Context]:
        if chans in self._windows:
            return self._windows[chans]
        cfg = self.probe
        e = np.unique(np.concatenate([ctx.capture.channel(c).edges for c in chans]))
        out = []
        if len(e) >= cfg.min_edges:
            half = cfg.window_edges // 2
            for q in cfg.positions:
                i = int(q * (len(e) - 1))
                lo, hi = max(0, i - half), min(len(e) - 1, i + half)
                out.append(Context(slice_capture(ctx.capture, int(e[lo]), int(e[hi]) + 1), ctx.features, ctx.active))
        self._windows[chans] = out
        return out

    def expand(self, ctx: Context) -> list[Node]:
        roots = []
        for a in self.analyzers:
            if a.consumes != "capture":
                continue
            for params in a.propose(ctx, None):
                node = self._run(a, ctx, None, params)
                if node is not None:
                    roots.append(node)
        for r in roots:
            self._score(r)
        return roots

    def _probe(self, a: Analyzer, ctx: Context, params: dict) -> Node | None:
        """Returns an abandoned node, or None to continue with the full decode."""
        wins = self.windows(ctx, tuple(a.probe_channels(params)))
        if not wins:
            return None
        best, best_m, best_roles = -1.0, None, None
        for w in wins:
            self.probe_runs += 1
            roles, _, m = self._call(a, w, None, params, tag=".probe")
            if roles is None:
                continue
            s = self.scorer.layer(Node(a.name, params, roles, None, dict(m)))
            if s > best:
                best, best_m, best_roles = s, m, roles
            if best >= self.probe.threshold:
                return None
        if best_roles is None:
            return None
        self.abandoned += 1
        return Node(a.name, params, best_roles, None, {**best_m, "_probe_score": best}, note="abandoned")

    def _run(self, a: Analyzer, ctx: Context, parent: Node | None, params: dict) -> Node | None:
        if parent is None and self.exclusion is not None:
            hit = self.exclusion(a.name, params)
            if hit:
                self.excluded += 1
                return Node(a.name, params, {}, None, {"_excluded": hit}, note="excluded")
        if parent is None and self.probe and hasattr(a, "probe_channels"):
            ab = self._probe(a, ctx, params)
            if ab is not None:
                return ab
        self.runs += 1
        roles, out, metrics = self._call(a, ctx, parent, params)
        if roles is None:
            return None
        node = Node(a.name, params, roles, out, metrics, parent)
        if out is not None and node.depth < self.max_depth:
            for b in self.analyzers:
                if b.consumes == out.kind:
                    for p in b.propose(ctx, node):
                        child = self._run(b, ctx, node, p)
                        if child is not None:
                            node.children.append(child)
        return node

    def _score(self, node: Node) -> None:
        for c in node.children:
            self._score(c)
        t0 = time.perf_counter()
        node.layer_score = self.scorer.layer(node)
        node.total = self.scorer.combine(node)
        self.score_calls += 1
        self.score_seconds += time.perf_counter() - t0


# --- explanations: channel-disjoint sets of root hypotheses -----------------

def shares_ok(a: Node, b: Node) -> bool:
    """Roots in one explanation must use disjoint channels."""
    return not (set(a.roles.values()) & set(b.roles.values()))


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


def _count(items) -> int:
    if isinstance(items, dict):
        v = items.get("value")
        return int(len(v)) if v is not None else 0
    try:
        return len(items)
    except TypeError:
        return 0


def pairs(names: list[str]):
    return itertools.permutations(names, 2)
