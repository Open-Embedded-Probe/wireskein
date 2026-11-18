"""Staged engine: survey -> pin classes -> typed streams -> protocol plugins by
input type -> scores -> explanation packing -> verdicts.

Plugins declare the stream type they consume and never touch edges:

    frames.startstop  -> i2c, sync_unknown
    frames.select     -> spi, sync_unknown
    frames.gap        -> spi, sync_unknown
    chars             -> uart, lin, dmx512
    line              -> rgb_led, onewire, ir_nec (one line by pulse widths)
    bytes             -> lines, nmea, modbus_rtu, markers (upper layers, reused)

Undecidable choices inside a stage (sampling edge when data changes midway,
the delimiter family) are kept as sibling streams; plugins score each and the
packing picks. Outputs use the same shapes as the flat engine so the same
evaluation applies.
"""

from __future__ import annotations

import math
import time
from dataclasses import dataclass, replace
from functools import lru_cache

import numpy as np

from . import kernels as K
from . import methods
from . import plugins_uartlike as UL
from . import timing, typed
from .analyzers.pulse import NecPlugin, OneWirePlugin, RgbLedPlugin
from .analyzers.upper import Lines, MarkerGrammar, ModbusRtu, Nmea
from .model import Capture
from .scoring import DefaultScorer, degenerate
from .stack import Node, Stream, explanations
from .taxonomy import classify
from .declarative import load_all
from .riscv_dm import dm_node
from .rvswd import SwioPlugin


CLAIM_COST = 0.3  # a hypothesis must beat this per channel to be worth claiming


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
    bus: str | None = None  # the bus a target's claim is on, when several targets share its lines (shared_buses)


def channel_labels(nodes: list[Node]) -> dict[str, tuple]:
    """channel -> (analyzer, role, key params) under an explanation."""
    out = {}
    for n in nodes:
        key = ()
        if n.analyzer == "uart":
            key = (round(n.params["baud"] / 1000, 0), n.params["idle"])
        elif n.analyzer == "spi" and getattr(n, "members", None):
            for m, _ in n.members:                  # a bus of several targets: as its targets' claims label them
                out.update(channel_labels([m]))
            continue
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


def q(n: float, scale: float = 8.0) -> float:
    return 1.0 - math.exp(-n / scale)


@dataclass
class Ctx:
    cap: Capture
    tx: object          # Taxonomy
    sv: object          # Survey


# ---------------- sync plugins ----------------

class SyncUnknownPlugin:
    """Clock + data with a consistent frame length that no known plugin claims.
    Reported as a separate kind of result, never as a known protocol."""
    name = "sync_unknown"
    consumes = ("frames.startstop", "frames.select", "frames.gap")

    def run(self, c: Ctx, fr: typed.Frames, g):
        sb = fr.source
        lens = np.diff(fr.bounds, axis=1).ravel()
        lens = lens[lens > 2]
        if len(lens) < 4:
            return []
        vals, cnt = np.unique(lens, return_counts=True)
        order = np.argsort(cnt)[::-1]
        top = [(int(vals[i]), int(cnt[i])) for i in order[:4]]
        share = float(cnt[order[:2]].sum() / len(lens))
        clk = c.sv.clocks[sb.clock].clock_score
        pair = float(np.mean([c.sv.pairs[(sb.clock, d)].data_score for d in sb.data]))
        m = {"frame_bits": top, "share_top2": share, "frames": int(len(lens)), "clk": clk, "pair": pair,
             "delimiter": fr.delimiter}
        # "Unknown" means "not explained by the known framings": discount by how
        # well the same frames fit I2C (9n, +1) or SPI (8n) word lengths.
        # every known framing of the same bits counts, not only this delimiter's
        known = 0.0
        # all views of the same clock/data, with or without the select candidate
        views = [typed.frames_gap(sb), typed.frames_startstop(c.cap, sb).closed()]
        if sb.select:
            views.append(methods.get("select_frames", typed.frames_select)(c.cap, c.sv, sb))
        for v in views:
            ln = np.diff(v.bounds, axis=1).ravel()
            ln = ln[ln > 0]
            if len(ln) == 0:
                continue
            if v.delimiter == "startstop":
                fit, chance = float(np.mean(np.isin(ln % 9, (0, 1)))), 2 / 9
            else:
                fit, chance = float(np.mean(ln % 8 == 0)), 1 / 8
            known = max(known, max(0.0, (fit - chance) / (1 - chance)))  # above chance only
        m["known_fit"] = known
        score = 0.9 * share * q(len(lens), 4) * (0.5 + 0.5 * clk) * (1 - known)
        roles = {"clk": sb.clock, **({"cs": sb.select} if sb.select else {})}
        for i, d in enumerate(sb.data):
            roles[f"data{i}"] = d
        return [Node("sync_unknown", {"sample_edge": sb.sample_edge, "delimiter": fr.delimiter}, roles,
                     Stream("frames", fr), m, layer_score=score, total=score)]


# ---------------- async plugins (on Chars) ----------------

GRID_LOW, GRID_SPAN = 0.88, 0.04   # the share of in-character changes on the bit grid: 0.92 or more counts whole


def _on_grid(pin, blocks) -> float | None:
    """Of the changes inside the well-formed characters (changes closer than a quarter bit merged: a bounce is one),
    the share within a quarter bit of the character's start plus a whole number of bits."""
    from .survey import collapse
    e_all = np.asarray(pin.edges, np.int64)
    num = den = 0
    for (s0, s1, u), _, ch in blocks:
        if ch is None:
            continue
        e = collapse(e_all[(e_all >= s0) & (e_all < s1)], u / 4)
        st = np.sort(ch.start[ch.ok]).astype(np.int64)
        if not len(st) or not len(e):
            continue
        i = np.searchsorted(st, e, side="right") - 1
        ok = i >= 0
        d = (e[ok] - st[i[ok]]) / u
        d = d[d < ch.bits - 0.5]
        num += int((np.abs(d - np.round(d)) < 0.25).sum())
        den += len(d)
    return num / den if den else None


CLOCKS_A_BUS = 0.8      # a clock this clock-like, with a partner keeping to it as data this well, is no UART line


def _clocks_a_bus(sv, pin: str) -> bool:
    """The pin is a clock some other line keeps to as data (a bus's clock: RVSWD's SWCLK, SPI's SCK)."""
    ci = sv.clocks.get(pin)
    if ci is None or ci.clock_score < CLOCKS_A_BUS:
        return False
    return any(r.data_score >= CLOCKS_A_BUS for (k, o), r in sv.pairs.items() if k == pin and o != pin)


def _tight_windows(pin, idle: int, sv, n: int) -> float:
    """Of the pin's whole windows away from its idle level (read through each clock's gate: a bounce is one change),
    the largest share holding one of a clock's bursts snugly (the margins before and after it together no longer than
    the burst), over the clocks."""
    from .survey import GATE, collapse
    best = 0.0
    for k, ci in sv.clocks.items():
        if k == pin.name or not len(ci.bursts):
            continue
        view, vc = sv.gated.get(k, (None, ci))
        e = (np.asarray(view.channel(pin.name).edges, np.int64) if view is not None
             else collapse(np.asarray(pin.edges, np.int64), max(3.0, (ci.period or 0) / GATE)))
        if len(e) < 3:
            continue
        ci = vc                                                           # the clock's bursts as gated too
        after = pin.initial ^ ((np.arange(len(e)) + 1) & 1)          # the level after each change
        i = np.flatnonzero(after[:-1] != idle)                           # whole windows away from idle
        if len(i) < 2:
            continue
        ws, we = e[i], e[i + 1]
        # a burst's span starts a little before its first clock edge, and a select may change within a clock period
        # of it: a burst overlapping a window's ends by up to a period is inside; a window shorter than a period is a
        # bounce's leftover, no selection
        tol = float(ci.period or 0)
        real = (we - ws) >= tol
        ws, we = ws[real], we[real]
        if len(ws) < 2:
            continue
        bs, be = ci.bursts[:, 0].astype(np.int64), ci.bursts[:, 1].astype(np.int64)
        j = np.searchsorted(ws, bs + tol, side="right") - 1
        jj = np.maximum(j, 0)
        inside = (j >= 0) & (be - tol <= we[jj])
        snug = inside & (np.maximum(bs - ws[jj], 0) + np.maximum(we[jj] - be, 0) <= (be - bs))
        best = max(best, len(np.unique(j[snug])) / len(ws))
    return best


class UartPlugin:
    name = "uart"
    consumes = ("chars",)

    def run(self, c: Ctx, rb: typed.RateBlocks, blocks):
        vals, starts, ok_n, n, cover = [], [], 0, 0, []
        units = []
        pin = c.cap.channel(rb.pin)
        for (s0, s1, u), cands, ch in blocks:
            if ch is None:
                continue
            units.append((u, len(ch.ok)))
            ok_n += int(ch.ok.sum())
            n += len(ch.ok)
            vals.append(ch.values[ch.ok])
            starts.append(ch.start[ch.ok])
            # edges explained: inside a well-formed character
            win = np.stack([ch.start[ch.ok], ch.start[ch.ok] + ch.bits * u], 1).astype(np.int64)
            e = pin.edges[(pin.edges >= s0) & (pin.edges < s1)]
            if len(e):
                inside, _ = K.in_windows(win, e) if len(win) else (np.zeros(len(e), bool), None)
                cover.append((int(inside.sum()), len(e)))
        if n < 2:
            return []
        v = np.concatenate(vals)
        framing = ok_n / n
        coverage = sum(a for a, _ in cover) / max(1, sum(b for _, b in cover))
        f = c.sv.features[rb.pin]
        clock_like = max(f.scores.get("clock", 0), f.scores.get("clock_local", 0))
        u_main = max(units, key=lambda x: x[1])[0]
        m = {"framing_rate": framing, "coverage": coverage, "chars": ok_n, "blocks": len(units),
             "clock_like": clock_like, "L": blocks[0][2].bits if blocks[0][2] else None}
        # a pin that moves in step with some clock is synchronous data, not a UART line
        sync = max((r.data_score * c.sv.clocks[k].clock_score for (k, o), r in c.sv.pairs.items()
                    if o == rb.pin and k != rb.pin and k in c.sv.clocks), default=0.0)
        m["sync_with_clock"] = sync
        # a pin whose active windows each hold a clock's burst tightly is that bus's select, not a UART (a CS's low
        # windows read as characters); a UART's lows meet another bus's bursts only by chance, never snugly
        tight = _tight_windows(pin, rb.idle, c.sv, c.cap.n_samples)
        m["tight_windows"] = tight
        # characters framed by breaks the way LIN (sync 0x55, PID parity, checksum) or DMX512 (250 kbaud, start code
        # 0) frames them are that protocol's, not a plain UART's: weigh that evidence against this claim
        framed = 0.0
        for (s0, s1, u), cands, ch in blocks:
            if ch is None or len(ch.breaks) < 2:
                continue
            lk = UL.lin(ch)["checks"]
            if lk["frames"] >= 2:
                framed = max(framed, (lk["sync"] + lk["pid_parity"] + lk["checksum"]) / (3 * lk["frames"]))
            dk = UL.dmx512(ch)["checks"]
            if dk["packets"] >= 2 and abs(c.cap.rate / u / 250000 - 1) < 0.03:
                framed = max(framed, dk["start_code_0"] / dk["packets"])
        m["framed_by_breaks"] = framed
        # only what is above chance counts: a UART whose rate changes mid-stream breaks into framing errors, and a
        # few of its "frames" fit by luck (up to a third on the evaluation corpus; LIN and DMX512 lines fit wholly)
        framed = max(0.0, (framed - 0.5) / 0.5)
        # a bit needs about 3 samples to have a middle to read: under it a line's bounces or a clock's halves read
        # as bits as well as a UART's (on the bench every UART claimed under 2.2 samples a bit was none)
        m["samples_per_bit"] = u_main
        resolvable = float(np.clip(u_main - 2, 0, 1))
        # a UART's changes inside its characters sit on the bit grid (its start plus whole bits); a line that only
        # toggles now and then (a slow random line) gives framed characters too, its changes off the grid
        grid = _on_grid(pin, blocks)
        m["on_grid"] = grid
        gridded = float(np.clip((grid - GRID_LOW) / GRID_SPAN, 0, 1)) if grid is not None else 1.0
        score = (framing * coverage * q(ok_n) * degenerate(v & 0xFF) * (1 - 0.5 * clock_like) * (1 - sync)
                 * (1 - float(np.clip((tight - 0.5) / 0.3, 0, 1))) * (1 - framed) * resolvable * gridded)
        params = {"baud": c.cap.rate / u_main, "idle": rb.idle, "L": m["L"], "blocks": len(units)}
        items = {"value": v, "start": np.concatenate(starts), "char_samples": u_main * (m["L"] or 10)}
        return [Node("uart", params, {"data": rb.pin}, Stream("bytes", items, meta={"char_samples": u_main * (m["L"] or 10)}),
                     m, layer_score=score, total=score)]


class LinPlugin:
    name = "lin"
    consumes = ("chars",)

    def run(self, c: Ctx, rb, blocks):
        out = []
        for (s0, s1, u), cands, ch in blocks:
            if ch is None or len(ch.breaks) < 2:
                continue
            r = UL.lin(ch)
            ck = r["checks"]
            if ck["frames"] < 2:
                continue
            passed = (ck["sync"] + ck["pid_parity"] + ck["checksum"]) / (3 * ck["frames"])
            # frames of 0x55 alone are what a clock's bursts give as well: no evidence of LIN
            score = passed * q(ck["frames"], 3) * (1 - ck["alternating"] / ck["frames"])
            out.append(Node("lin", {"baud": c.cap.rate / u, "idle": rb.idle}, {"data": rb.pin},
                            Stream("lin.frames", r["frames"]), dict(ck), layer_score=score, total=score))
        return out[:1]


class DmxPlugin:
    name = "dmx512"
    consumes = ("chars",)

    def run(self, c: Ctx, rb, blocks):
        out = []
        for (s0, s1, u), cands, ch in blocks:
            if ch is None or len(ch.breaks) < 2:
                continue
            r = UL.dmx512(ch)
            ck = r["checks"]
            if ck["packets"] < 2:
                continue
            baud = c.cap.rate / u
            rate_ok = 1.0 if abs(baud / 250000 - 1) < 0.03 else 0.3
            score = (ck["start_code_0"] / ck["packets"]) * (ck["framing_ok"] / max(1, ck["chars"])) * rate_ok * q(ck["packets"], 2)
            out.append(Node("dmx512", {"baud": baud, "idle": rb.idle}, {"data": rb.pin},
                            Stream("dmx.packets", r["packets"]), dict(ck), layer_score=score, total=score))
        return out[:1]


# the plugins written in Python (state across frames, search, upper layers), then the frame-local protocols
# declared in decl/*.toml (docs/design.ja.md §3.2)
PLUGINS = ([SyncUnknownPlugin(), UartPlugin(), LinPlugin(), DmxPlugin(), SwioPlugin(), RgbLedPlugin(),
            OneWirePlugin(), NecPlugin()]
           + load_all(["i2c", "spi", "rvswd"]) + load_all(["swd", "can"]))
UPPER = [Lines(), Nmea(), ModbusRtu(), MarkerGrammar()]


def _upper(node: Node, scorer: DefaultScorer) -> None:
    """Upper layers on byte streams."""
    if node.output is None or node.output.kind != "bytes":
        return

    def expand(parent: Node):
        for a in UPPER:
            if a.consumes != parent.output.kind:
                continue
            for p in a.propose(None, parent):
                roles, out, m = a.run(None, parent, p)
                child = Node(a.name, p, roles, out, m, parent)
                parent.children.append(child)
                if out is not None:
                    expand(child)

    expand(node)

    def score(n: Node):
        for ch in n.children:
            score(ch)
            ch.layer_score = scorer.layer(ch)
            ch.total = scorer.combine(ch)

    score(node)
    support = max((ch.total for ch in node.children), default=0.0)
    node.metrics["_support"] = support
    node.total = node.layer_score + 0.5 * (1 - node.layer_score) * support * node.layer_score


def _slow_at(edges: np.ndarray, k: int, rises: np.ndarray) -> np.ndarray:
    """Per edge, whether the clock around it is slow enough to filter pulses of k (its period 4k or more, the median
    of the periods before and after: a clock's own glitches do not make it look fast)."""
    edges = np.asarray(edges, np.int64)
    allow = np.zeros(len(edges), bool)
    near = np.flatnonzero(np.diff(edges) < k) + 1
    if len(near):
        allow[near] = K.local_period(rises, edges[near], q=50) >= 4 * k
    return allow


GLITCH_SHARE = 0.6    # a clock's runs this short a share or less: a view with them taken out


def glitch_views(cap, sv, g, memo: dict | None = None):
    """The capture as is, plus a sibling view with short pulses (reflections)
    removed from the clock and data pins when the clock shows such pulses:
    runs of <= 4 samples that are rare compared with the clock's own half period.
    memo: one analysis' store (a clock's groups share its runs and each pin's filtered edges)."""
    memo = {} if memo is None else memo
    yield cap, 0
    ch = cap.channel(g.clock)
    _, length, _ = ch.runs(cap.n_samples)
    inner = length[1:-1]
    if len(inner) < 16:
        return
    step = max(x.step for x in cap.channels if x.name in {g.clock, *g.data, *([g.select] if g.select else [])})
    # the clock's typical run, also with its pulses under BOUNCE_TICKS taken out: a bounce makes two short runs, so
    # with bounces at half the edges most runs are short and their median is a bounce's
    key = ("med", g.clock, step)
    if key not in memo:
        med = float(np.median(inner))
        clean = np.diff(np.asarray(K.deglitch(ch.edges, BOUNCE_TICKS * step), np.int64))
        if len(clean) >= 16:
            med = max(med, float(np.median(clean)))
        memo[key] = med
    med = memo[key]
    # Short pulses relative to the clock's own runs (reflections last a few ns,
    # i.e. more samples at higher rates; single-sample spikes at any rate).
    short = inner <= max(float(step), 0.25 * med)
    # (bounces at half the edges make a third of the runs short: still a view; the widths stay under half a run)
    if med >= 3 and 0 < short.mean() < GLITCH_SHARE:
        # the filter width is undecidable here: fixed small widths and one from
        # the observed short pulses are siblings; plugins' checks pick. A width
        # never exceeds half the clock's typical run.
        k_rel = int(max(2 * step, min(0.3 * med, np.quantile(inner[short], 0.95) + step)))
        pins = {g.clock, *g.data, *([g.select] if g.select else [])}
        for k in sorted({2 * step, 3 * step, 5 * step, k_rel}):
            if k > 0.5 * med:
                continue
            # only where the clock around is slow enough for k (its period 4k or more): a capture whose clock
            # changes speed (a LinkE connecting slowly, then reading fast) keeps its fast clock's short highs. The
            # period from the clock with its pulses of k taken out: bounces at half its edges split its periods,
            # and the median of them was a bounce's
            if ("rises", g.clock, k) not in memo:
                ce = K.deglitch(ch.edges, k)
                memo[("rises", g.clock, k)] = ce[K.edge_levels(ce, ch.initial) == 1]
            rises = memo[("rises", g.clock, k)]

            def filtered(x):
                kx = ("pin", g.clock, x.name, k)
                if kx not in memo:
                    memo[kx] = K.deglitch(x.edges, k, _slow_at(x.edges, k, rises))
                return memo[kx]
            chans = [replace(x, edges=filtered(x)) if x.name in pins else x for x in cap.channels]
            yield Capture(cap.rate, cap.n_samples, chans), k


BOUNCE_TICKS = 4    # an async line's pulses under this many ticks are bounces, where its runs are much longer


def single_views(cap, sv, pin):
    """An async pin as is, plus a sibling with single-sample spikes removed when
    such spikes are rare and the bit time is long enough to keep real bits."""
    yield cap, 0
    ch = cap.channel(pin)
    _, length, _ = ch.runs(cap.n_samples)
    inner = length[1:-1]
    f = sv.features[pin]
    ks = set()
    if f.units and len(inner) >= 16 and f.units[0].samples >= 4 * ch.step:
        unit = f.units[0].samples
        spikes = inner <= ch.step
        if 0 < spikes.mean() < 0.1:
            ks.add(int(max(2 * ch.step, min(0.3 * unit, 3 * ch.step))))
    # bounces at the edges (pulses back of a tick to three): with many of them the bit time estimated from the line
    # is a bounce's, so the view is made on its own: the pulses under 4 ticks taken out, where the line left keeps
    # its runs (the shortest tenth) twice that long or more
    kb = BOUNCE_TICKS * ch.step
    if len(inner) >= 16 and kb not in ks and np.any(inner < kb):
        e = K.deglitch(ch.edges, kb)
        left = np.diff(np.asarray(e, np.int64))
        if len(left) >= 16 and np.quantile(left, 0.1) >= 2 * kb:
            ks = {kb}           # it takes out the spikes too (one view, not two: a long capture reads each)
    for k in sorted(ks):
        chans = [replace(x, edges=K.deglitch(x.edges, k)) if x.name == pin else x for x in cap.channels]
        yield Capture(cap.rate, cap.n_samples, chans), k


def i2c_device_node(parent: Node) -> Node | None:
    """Upper layer over I2C transactions: devices on the bus. A real bus talks to
    few addresses, repeatedly, and register-map devices show "write register
    pointer, then read" pairs to the same address."""
    txs = [t for t in parent.output.items if t.get("addr") is not None]
    if len(txs) < 3:
        return None
    addrs = [t["addr"] for t in txs]
    uniq, cnt = np.unique(addrs, return_counts=True)
    top2 = float(np.sort(cnt)[::-1][:2].sum() / len(addrs))
    acked = float(np.mean([t.get("addr_ack", False) for t in txs]))
    pairs = sum(1 for a, b in zip(txs, txs[1:]) if a["rw"] == "write" and b["rw"] == "read" and a["addr"] == b["addr"])
    reads = sum(1 for t in txs if t["rw"] == "read")
    m = {"devices": int(len(uniq)), "top2_share": top2, "addr_ack": acked, "ptr_then_read": pairs, "reads": reads}
    # repeated addresses that are acknowledged at least sometimes
    score = top2 * (1 - 0.5 ** (len(txs) / 3)) * (1.0 if acked > 0 else 0.3)
    return Node("i2c_devices", {}, parent.roles, Stream("i2c.devices", {int(a): int(c) for a, c in zip(uniq, cnt)}), m,
                parent, layer_score=score, total=score)


def _external_plugins() -> list:
    """Declarative protocols from definition directories added from outside (wireskein.protocols.add_definitions,
    the entry point group wireskein.definitions); a name of WireSkein's own is refused."""
    from .. import protocols as P
    from .declarative import load_dir
    out = []
    own = {p.name for p in PLUGINS}
    for d in P.definitions():
        for p in load_dir(d):
            if p.name in own:
                raise ValueError(f"{d / (p.name + '.toml')}: {p.name!r} is a protocol of WireSkein's own")
            out.append(p)
    return out


def _protocols(roots: list[Node]) -> None:
    """Protocols of others on the buses decoded (wireskein.protocols): each given the bus as the analysis exports
    it; its checks, as Modbus RTU's, support the bus."""
    from .. import protocols as P
    from . import export
    protos = P.registered()
    if not protos:
        return
    for n in roots:
        if n.output is None or n.analyzer == "uart_pair":
            continue
        mine = [p for p in protos.values() if n.analyzer in p.over]
        if not mine:
            continue
        bus = {"roles": dict(n.roles), "tick_hz": None, **export.layers_of(n)}
        view = getattr(n.layers.get("bits") or n.layers.get("blocks"), "view", None) if getattr(n, "layers", None) \
            else None
        if view is not None:
            bus["tick_hz"] = float(view.rate)
        for p in mine:
            try:
                got = p.decode(bus) or {}
            except Exception as e:  # noqa: BLE001 - another's protocol: its failure is shown, the analysis goes on
                got = {"messages": [], "checks_passed": 0, "checks_failed": 0, "error": f"{type(e).__name__}: {e}"}
            m = {"checks_passed": int(got.get("checks_passed", 0)), "checks_failed": int(got.get("checks_failed", 0)),
                 **({"error": got["error"]} if "error" in got else {})}
            s = P.score(m)
            child = Node(p.name, {}, n.roles, Stream(p.name, list(got.get("messages", []))), m, n,
                         layer_score=s, total=s)
            child.user = True
            n.children.append(child)
            n.total = n.total + 0.5 * (1 - n.total) * s * n.total


def sd_spi_node(parent: Node) -> Node | None:
    """Upper layer over SPI transfers: SD commands in SPI mode (periph.sd_spi), with the data pins tried both ways
    round as MOSI and MISO. The evidence is commands with their end bit answered by an R1 within NCR, the lines
    idle (0xFF) around them as SD's are."""
    from . import periph
    lay = getattr(parent, "layers", None) or {}
    fr, sb = lay.get("frames"), lay.get("bits")
    lines = parent.output.items.get("lines", {}) if isinstance(parent.output.items, dict) else {}
    if fr is None or sb is None or len(lines) != 2:
        return None
    best = None
    pins = list(lines)
    for mo_pin, mi_pin in ((pins[0], pins[1]), (pins[1], pins[0])):
        rows = []
        for i, ((a, _), mo, mi) in enumerate(zip(fr.bounds.tolist(), lines[mo_pin], lines[mi_pin])):
            rows += [(int(sb.t[a + 8 * k]), int(x) & 0xFF, int(y) & 0xFF, i) for k, (x, y) in enumerate(zip(mo, mi))]
        cmds = periph.sd_spi(rows)["commands"]
        good = [c for c in cmds if c.get("r1") is not None and c["end_bit"] and c["idle_ok"]]
        if len(good) < 2:
            continue
        crc = [c["crc_ok"] for c in good if c["cmd"] in (0, 8) and not c["acmd"]]
        score = len(good) / len(cmds) * (1 - 0.5 ** (len(good) / 2)) * (1.0 if all(crc) else 0.5)
        if best is None or score > best[0]:
            best = (score, mo_pin, mi_pin, cmds)
    if best is None:
        return None
    score, mo_pin, mi_pin, cmds = best
    m = {"commands": len(cmds), "answered": sum(c.get("r1") is not None for c in cmds), "mosi": mo_pin,
         "miso": mi_pin}
    return Node("sd_spi", {}, {**parent.roles, "mosi": mo_pin, "miso": mi_pin}, Stream("sd.commands", cmds), m,
                parent, layer_score=score, total=score)


def value(n: Node) -> float:
    return len(n.roles) * (n.total - CLAIM_COST)


def equivalent(a: Node, b: Node) -> bool:
    if a.analyzer != b.analyzer or a.roles != b.roles:
        return False
    if a.analyzer == "uart":
        return a.params["idle"] == b.params["idle"] and abs(a.params["baud"] / b.params["baud"] - 1) <= 0.03
    if a.analyzer in ("spi", "i2c", "sync_unknown", "rvswd"):
        return a.params.get("sample_edge") == b.params.get("sample_edge")
    return True


@dataclass
class StagedResult:
    roots: list[Node]
    explanations: list
    claims: list[Claim]
    seconds: dict
    features: dict
    taxonomy: object
    runs: int = 0


SYNC_PROTOCOLS = {"i2c", "spi", "rvswd", "swd", "sync_unknown"}
ASYNC_PROTOCOLS = {"uart", "lin", "dmx512", "can"}
PULSE_PROTOCOLS = {"swio", "rgb_led", "onewire", "ir_nec"}
SYNC_ROLES = {"i2c": {"scl": "clock", "sda": "data"}, "spi": {"clk": "clock", "mosi": "data", "miso": "data", "cs": "select"},
              "rvswd": {"clk": "clock", "dio": "data"}}


SHARED_COVER = 0.95    # the targets' selections together hold this share of the clock's bursts or more
SHARED_MAX = 8         # select lines on one clock tried in sets for one bus of targets (2^n sets)


def shared_buses(roots: list[Node], sv) -> list[Node]:
    """One clocked bus with a target on each select line (SPI with a CS per device: CLK and the data lines shared).
    Each select's own reading counts the other targets' transfers, which fall in its idle level, against it
    (select_boundary); together, selections that never overlap and hold the clock's bursts between them are the
    bus. A hypothesis of the whole (roles: the shared lines and every select) enters the packing; its targets'
    claims come out of it (`members`: (node, score with the others' transfers not counted against it))."""
    by: dict[tuple, dict[str, Node]] = {}
    for n in roots:
        if n.analyzer != "spi" or n.output is None or "cs" not in n.roles or "frames" not in getattr(n, "layers", {}):
            continue
        data = tuple(sorted(v for k, v in n.roles.items() if k.startswith("data")))
        key = (n.roles["clk"], data)
        cs = n.roles["cs"]
        if cs not in by.setdefault(key, {}) or n.total > by[key][cs].total:
            by[key][cs] = n
    out = []
    for (clk, data), per in by.items():
        if len(per) < 2 or clk not in sv.clocks:
            continue
        ci = sv.clocks[clk]
        real = ci.bits >= 2
        mid = ((ci.bursts[real, 0] + ci.bursts[real, 1]) // 2) if real.any() else np.zeros(0, np.int64)
        if not len(mid):
            continue

        def spans(n):
            fr = n.layers["frames"]
            t = np.asarray(fr.source.t, np.int64)
            b = np.asarray(fr.bounds, np.int64).reshape(-1, 2)
            b = b[b[:, 1] > b[:, 0]]
            return np.stack([t[b[:, 0]], t[b[:, 1] - 1]], 1) if len(b) else np.zeros((0, 2), np.int64)

        def adjusted(n):
            sb = n.metrics.get("select_boundary")
            return float(min(0.99, n.total / sb)) if sb else float(n.total)
        cands = [(n, spans(n), adjusted(n)) for n in sorted(per.values(), key=lambda n: -adjusted(n))]
        cands = [x for x in cands if len(x[1])][:SHARED_MAX]

        def holds(sp):
            sp = sp[np.argsort(sp[:, 0])]
            k = np.searchsorted(sp[:, 0], mid, side="right") - 1
            return (k >= 0) & (mid <= sp[np.maximum(k, 0), 1])
        held_by = [holds(sp) for _, sp, _ in cands]

        def apart(a, b):
            return not any(((a[:, 0] <= e) & (a[:, 1] >= s)).any() for s, e in b)
        # every set of targets whose selections never overlap: the one holding the most bursts, then scoring best
        # (a decoy select-like line taken first would keep the real ones out)
        best: list = [(-1, -1.0), ()]

        def dfs(i, chosen, held):
            if i == len(cands):
                if len(chosen) >= 2:
                    key = (int(held.sum()), sum(cands[j][2] for j in chosen))
                    if key > best[0]:
                        best[:] = [key, tuple(chosen)]
                return
            dfs(i + 1, chosen, held)
            if all(apart(cands[j][1], cands[i][1]) for j in chosen):
                dfs(i + 1, chosen + [i], held | held_by[i])
        dfs(0, [], np.zeros(len(mid), bool))
        if not best[1]:
            continue
        cover = best[0][0] / len(mid)
        if cover < SHARED_COVER:
            continue
        chosen = [(cands[j][0], cands[j][2]) for j in best[1]]
        w = np.array([max(1, n.metrics.get("frames", 1)) for n, _ in chosen], float)
        total = float(np.dot(w, [a for _, a in chosen]) / w.sum()) * cover
        roles = {"clk": clk, **{f"data{i}": d for i, d in enumerate(data)},
                 **{("cs" if i == 0 else f"cs{i}"): n.roles["cs"] for i, (n, _) in enumerate(chosen)}}
        bus = Node("spi", {"bus": f"spi:{clk}"}, roles, None, {"targets": len(chosen), "cover": cover},
                   layer_score=total, total=total)
        bus.members = chosen
        out.append(bus)
    return out


def _hinted(hints: dict | None):
    """Normalize hints: allowed protocols, per-pin protocol/role/baud, excluded pins."""
    h = hints or {}
    allowed = set(h.get("protocols") or [])
    pins = h.get("pins") or {}
    for info in pins.values():
        if info.get("protocol"):
            allowed.add(info["protocol"]) if h.get("protocols") else None
    return allowed, pins, set(h.get("exclude_pins") or [])


def _hinted_groups(pin_hint: dict, sv, have: list, buses: list | None = None) -> list:
    """A bus whose clock and data pins are all named by the hints is tried even
    when the survey did not group it (a few clocks only: one byte, a transfer cut
    short). Nothing more is assumed; the plugins score it like any other group.
    buses: the hinted buses as given ({protocol, pins}), each tried as its own group: buses sharing lines (one SPI
    bus, a target on each CS) are not merged by pin (a pin's hint names one role only)."""
    from .taxonomy import Group
    keys = {(g.clock, g.data, g.select) for g in have if g.kind == "sync"}
    out = []
    given = [(b["protocol"], {p: {"protocol": b["protocol"], "role": r} for r, p in b["pins"].items()})
             for b in buses or [] if b.get("protocol") in SYNC_ROLES]
    if not given:
        given = [(proto, pin_hint) for proto in SYNC_ROLES]
    for proto, hint in given:
        roles = SYNC_ROLES[proto]
        by = {}
        for pn, i in hint.items():
            if i.get("protocol") == proto and i.get("role") in roles and pn in sv.active:
                by.setdefault(roles[i["role"]], []).append(pn)
        clk, data = by.get("clock", []), tuple(sorted(by.get("data", [])))
        if len(clk) != 1 or clk[0] not in sv.clocks or not data:
            continue
        sel = by.get("select", [None])[0]
        if (clk[0], data, sel) not in keys:
            keys.add((clk[0], data, sel))
            out.append(Group("sync", clock=clk[0], data=data, select=sel))
    return out


def _device_packs(select: tuple[str, ...]):
    """hints["devices"]: pack path patterns under decl/devices ("i2c/**", "!i2c/qst/**"), and under devices/ of
    each definitions directory added from outside."""
    from .. import protocols as P
    extra = tuple(str(d / "devices") for d in P.definitions() if (d / "devices").is_dir())
    return _device_packs_at(select, extra)


@lru_cache(maxsize=8)
def _device_packs_at(select: tuple[str, ...], extra: tuple[str, ...]):
    from . import devices as dev
    from pathlib import Path
    sets = [dev.load_packs(list(select))] + [dev.load_packs(list(select), Path(r)) for r in extra]
    return (sets[0] if len(sets) == 1 else dev.PackSets(sets)), dev.address_table()


def analyze(cap: Capture, hints: dict | None = None) -> StagedResult:
    """hints (all optional): {"protocols": [...], "pins": {pin: {"protocol", "role", "baud"}},
    "buses": [{"protocol", "pins": {role: pin}}], "exclude_pins": [...], "devices": ["i2c/**", ...]} restrict what is tried; nothing is decided by a hint alone,
    the plugins' checks still score every hypothesis."""
    t = {}
    allowed, pin_hint, excluded = _hinted(hints)
    if excluded:
        cap = Capture(cap.rate, cap.n_samples, [x for x in cap.channels if x.name not in excluded])
    pre = methods.get("preclean", None)       # none of the engine's own: the lines as captured
    if pre is not None:
        cap = pre(cap)
    ext = _external_plugins()
    # a protocol added from outside runs where its input is made: on clock and data, or on one NRZ line
    SYNC = SYNC_PROTOCOLS | {p.name for p in ext if any(k.startswith("frames") for k in p.consumes)}
    ASYNC = ASYNC_PROTOCOLS | {p.name for p in ext if set(p.consumes) & {"nrzbits", "chars"}}
    t0 = time.perf_counter()
    from .survey import survey as _survey
    tx = classify(cap, _survey(cap, asyncs=not allowed or bool(allowed & ASYNC)))
    t["classify"] = time.perf_counter() - t0
    c = Ctx(cap, tx, tx.survey)
    by_type: dict[str, list] = {}
    for p in PLUGINS + ext:
        if allowed and p.name not in allowed:
            continue
        for k in p.consumes:
            by_type.setdefault(k, []).append(p)
    async_pins = {pn for pn, i in pin_hint.items() if i.get("protocol") in ASYNC | PULSE_PROTOCOLS}
    sync_pins = {pn for pn, i in pin_hint.items() if i.get("protocol") in SYNC}

    def group_ok(g) -> bool:
        pins = {g.clock, *g.data, *([g.select] if g.select else [])}
        if pins & async_pins:
            return False
        for pn, i in pin_hint.items():
            role = SYNC_ROLES.get(i.get("protocol", ""), {}).get(i.get("role", ""))
            if role == "clock" and pn != g.clock and pn in pins:
                return False
            if role == "clock" and g.clock in pin_hint and SYNC_ROLES.get(pin_hint[g.clock].get("protocol", ""), {}).get(pin_hint[g.clock].get("role", "")) != "clock":
                return False
            if role == "select" and pn in pins and pn != g.select:
                return False
        return True
    roots: list[Node] = []
    runs = 0
    t0 = time.perf_counter()
    seen = set()
    gv_memo: dict = {}          # the glitch views' shared work, this analysis only
    run_sync = not allowed or bool(allowed & SYNC)
    for g in [*tx.groups, *_hinted_groups(pin_hint, c.sv, tx.groups, (hints or {}).get("buses"))]:
        if g.kind != "sync" or not run_sync or not group_ok(g):
            continue
        key = (g.clock, g.data, g.select)
        if key in seen:
            continue
        seen.add(key)
        views = list(glitch_views(cap, c.sv, g, gv_memo))
        # I2C reads the lines as an I2C input sees them (its spike filter, its hysteresis: timing.i2c_input), the
        # only reading such a device makes of them; the other plugins the views above
        i2c_view, i2c_only = None, False
        if not g.select and len(g.data) == 1 and any(p.name == "i2c" for p in by_type.get("frames.startstop", [])):
            hint = pin_hint.get(g.clock, {})
            spike = float(hint.get("spike_max_s") or timing.I2C_SPIKE_S)       # a check's spike_max_s
            iv, changed = timing.i2c_input(cap, g.clock, g.data[0], spike, freq_hz=hint.get("freq_hz"))
            if changed:
                i2c_view = iv
                views.append((iv, "i2c_input"))
            # at a rate that shows t_SP (2 samples or more in it) the input view is the whole reading: no view with
            # short pulses dropped by width (it would drop clocks clipped short, and restored ones); slower, the
            # views as for any clocked bus
            i2c_only = spike * float(cap.rate) >= 2
        # the edges the lines say in any view (a view with its short pulses left out may say the other), and the one
        # the spec (or a long capture's first piece) gives: every view read on each, so that the same reading is
        # there whatever the view (a piece of a long capture reads as the whole does)
        edges = []
        for view_cap, _ in views:
            edges += [e for e in typed.sample_edge_for(view_cap, g.clock, g.data) if e not in edges]
        want = pin_hint.get(g.clock, {}).get("sample_edge")
        if want in ("rise", "fall") and want not in edges:
            edges.append(want)
        # a clock given as a protocol (a check's or a spec's pins) is read on that protocol's own edge too, whatever
        # the lines suggest: a device that moves SDA well before SCL's rise looks like data on the falling edge
        given = pin_hint.get(g.clock, {}).get("protocol")
        for p in (p for ps in by_type.values() for p in ps if p.name == given):
            own = getattr(p, "edge", None)
            if own in ("rise", "fall") and own not in edges:
                edges.append(own)
        for view_cap, k in views:
            for edge in edges:
                sb = typed.sync_bits(view_cap, c.sv, g.clock, g.data, g.select, edge)
                sb.deglitched = k
                sb.view = view_cap
                streams = []
                if g.select:
                    streams.append(("frames.select", methods.get("select_frames", typed.frames_select)(view_cap, c.sv, sb)))
                else:
                    # gap threshold is undecidable here (a slow clock with short idle
                    # gaps between frames): both views are siblings, deduplicated
                    g3, g18 = typed.frames_gap(sb), typed.frames_gap(sb, 1.8)
                    streams.append(("frames.gap", g3))
                    if len(g18.bounds) != len(g3.bounds):
                        streams.append(("frames.gap", g18))
                    streams.append(("frames.startstop", typed.frames_startstop(view_cap, sb)))
                for kind, fr in streams:
                    for p in by_type.get(kind, []):
                        # I2C reads its input view (timing.i2c_input) or, when that changed nothing, the lines as
                        # they are: never a view with short pulses dropped by width, which would drop clocks clipped
                        # short and restored ones; the other plugins every view but I2C's
                        if p.name == "i2c" and i2c_view is not None and k != "i2c_input":
                            continue
                        if p.name == "i2c" and i2c_only and k != ("i2c_input" if i2c_view is not None else 0):
                            continue
                        if p.name != "i2c" and k == "i2c_input":
                            continue
                        runs += 1
                        got = p.run(c, fr if getattr(p, "open_tail", False) else fr.closed(), g)
                        for n in got:
                            # the engine keeps the typed inputs (references) for
                            # consumers that want intermediate layers (GUI)
                            n.layers = {"bits": fr.source, "frames": fr}
                            roots.append(n)
    t["sync"] = time.perf_counter() - t0
    t0 = time.perf_counter()
    run_async = not allowed or bool(allowed & ASYNC)
    run_pulse = not allowed or bool(allowed & PULSE_PROTOCOLS)
    for g in tx.groups:
        if g.kind != "single" or g.pin in sync_pins:
            continue
        pc = tx.pins[g.pin]
        # the pulse-width protocols read any lone line (an LED chain or a carrier looks like a clock to the survey)
        # but a bus's clock (another line keeps to it as data) is none of them, unless a hint names it
        if (run_pulse and pin_hint.get(g.pin, {}).get("protocol") not in ASYNC
                and (pin_hint.get(g.pin, {}).get("protocol") or not _clocks_a_bus(c.sv, g.pin))):
            for p in by_type.get("line", []):
                runs += 1
                roots += p.run(c, cap.channel(g.pin), g)
        if not ({"data", "sparse"} & set(pc.candidates)) and g.pin not in async_pins:
            continue
        hinted = pin_hint.get(g.pin, {})
        if hinted.get("protocol") in ASYNC and not run_pulse:
            pass
        want_pulse = run_pulse and hinted.get("protocol") not in ASYNC
        ps = typed.pulse_symbols(cap, c.sv, g.pin) if want_pulse and ("pulse" in pc.candidates or "data" in pc.candidates) else None
        if ps is not None:
            for p in by_type.get("pulses", []):
                runs += 1
                for n in p.run(c, ps, g):
                    n.layers = {"pulses": ps}
                    roots.append(n)
        if not run_async or hinted.get("protocol") in PULSE_PROTOCOLS:
            continue
        if not hinted.get("protocol") and _clocks_a_bus(c.sv, g.pin):
            continue            # a clock other lines keep to as data: a bus's clock, not a UART line (not tried)
        for view_cap, k in single_views(cap, c.sv, g.pin):
            if hinted.get("baud"):
                # a given baud rate replaces rate segmentation and unit estimation
                rb = typed.RateBlocks(g.pin, c.sv.features[g.pin].idle_level, [(0, cap.n_samples, cap.rate / hinted["baud"])])
            else:
                rb = typed.rate_blocks(view_cap, c.sv, g.pin)
            if not rb.blocks:
                continue
            if by_type.get("nrzbits"):
                nb = typed.nrz_bits(view_cap, c.sv, rb)
                for p in by_type["nrzbits"] if nb is not None else []:
                    runs += 1
                    for n in p.run(Ctx(view_cap, c.tx, c.sv), nb, g):
                        n.params["deglitch"] = k
                        n.layers = {"blocks": rb, "bits": nb}
                        roots.append(n)
            blocks = typed.block_chars(view_cap, c.sv, rb)
            for p in by_type.get("chars", []):
                runs += 1
                got = p.run(Ctx(view_cap, c.tx, c.sv), rb, blocks)
                for n in got:
                    n.params["deglitch"] = k
                    n.layers = {"blocks": rb, "chars": blocks}
                    roots.append(n)
    t["async"] = time.perf_counter() - t0
    t0 = time.perf_counter()
    scorer = DefaultScorer()
    for n in roots:
        _upper(n, scorer)
    from .duplex import relations
    rels = relations(roots)
    # device packs on top of transactions/transfers: only self-verified matches support the bus
    from . import devices as dev
    packs, addr_db = _device_packs(tuple((hints or {}).get("devices") or ["**"]))
    for n in roots:
        if n.output is None or n.analyzer not in ("i2c", "spi"):
            continue
        try:
            n.devices = (dev.match_i2c(n.output.items, packs, addr_db) if n.analyzer == "i2c"
                         else dev.match_spi(n.output.items["lines"], packs))
        except (KeyError, TypeError):
            n.devices = []
        if any(m.level == "identified" for m in n.devices):
            n.total = n.total + 0.5 * (1 - n.total) * n.total
    for n in roots:
        if n.analyzer == "i2c" and n.output is not None:
            d = i2c_device_node(n)
            if d is not None:
                n.children.append(d)
                n.total = n.layer_score + 0.5 * (1 - n.layer_score) * d.total * n.layer_score
        if n.analyzer == "spi" and n.output is not None:
            d = sd_spi_node(n)
            if d is not None:
                n.children.append(d)
                n.total = n.layer_score + 0.5 * (1 - n.layer_score) * d.total * n.layer_score
        if n.analyzer in ("rvswd", "swio") and n.output is not None:
            d = dm_node(n)
            if d is not None:
                n.children.append(d)
                n.total = n.layer_score + 0.5 * (1 - n.layer_score) * d.total * n.layer_score
    _protocols(roots)
    t["upper"] = time.perf_counter() - t0
    t0 = time.perf_counter()
    # unknown-sync only explains what no known protocol explains better: a
    # free-running clock "sampling" a known bus's pins must not win by pin count
    known = [n for n in roots if n.analyzer != "sync_unknown" and n.output is not None]
    roots_pack = [n for n in roots if n.analyzer != "sync_unknown" or not any(
        k.total >= n.total and set(k.roles.values()) & set(n.roles.values()) for k in known)]
    roots_pack += shared_buses(roots_pack, tx.survey)
    exps = explanations(roots_pack, value)
    claims = []
    if exps:
        best_score, best = exps[0]
        for n in best:
            if getattr(n, "members", None):
                # one bus of several targets: a claim per target, on the bus's lines and its own select
                alt = explanations(roots_pack, value, top=1, exclude=lambda r, n=n: r is n)
                alt_score, alt_set = alt[0] if alt else (0.0, [])
                margin = (best_score - alt_score) / max(1, changed_channels(best, alt_set))
                ru = max((r for r in alt_set if set(n.roles.values()) & set(r.roles.values())),
                         key=lambda r: r.total, default=None)
                for m, adj in n.members:
                    v = methods.get("verdict", verdict)(adj, margin)
                    claims.append(Claim(m.analyzer, dict(m.roles), dict(m.params), adj, margin, v, m, ru,
                                        bus=n.params["bus"]))
                continue
            alt = explanations(roots_pack, value, top=1, exclude=lambda r, n=n: equivalent(r, n))
            alt_score, alt_set = alt[0] if alt else (0.0, [])
            margin = (best_score - alt_score) / max(1, changed_channels(best, alt_set))
            chans = set(n.roles.values())
            ru = max((r for r in alt_set if chans & set(r.roles.values())), key=lambda r: r.total, default=None)
            v = methods.get("verdict", verdict)(n.total, margin)
            if n.analyzer == "sync_unknown":
                v = "unknown-sync"
            if (v == "confirmed" and n.analyzer == "spi" and "cs" not in n.roles
                    and n.roles.get("clk") in tx.survey.select_ambiguous):
                v = "likely"            # selects are there but which lines are open: the bus without them is not sure
            claims.append(Claim(n.analyzer, dict(n.roles), dict(n.params), n.total, margin, v, n, ru))
    t["pack"] = time.perf_counter() - t0
    res = StagedResult(roots, exps, claims, t, tx.survey.features, tx, runs)
    res.relations = rels
    return res
