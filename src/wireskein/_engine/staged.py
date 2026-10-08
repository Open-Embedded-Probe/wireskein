"""Staged engine: survey -> pin classes -> typed streams -> protocol plugins by
input type -> scores -> explanation packing -> verdicts.

Plugins declare the stream type they consume and never touch edges:

    frames.startstop  -> i2c, sync_unknown
    frames.select     -> spi, sync_unknown
    frames.gap        -> spi, sync_unknown
    chars             -> uart, lin, dmx512
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
from . import plugins_uartlike as UL
from . import typed
from .analyzers.upper import Lines, MarkerGrammar, ModbusRtu, Nmea
from .model import Capture
from .scoring import DefaultScorer
from .stack import Node, Stream, explanations
from .taxonomy import classify
from .rvswd import RvswdPlugin, SwioPlugin, dm_node


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


def q(n: float, scale: float = 8.0) -> float:
    return 1.0 - math.exp(-n / scale)


def degenerate(values: np.ndarray) -> float:
    if len(values) < 4:
        return 1.0
    top = np.bincount(values.astype(np.int64)).max() / len(values)
    return 1.0 if top <= 0.5 else max(0.05, 1 - (top - 0.5) / 0.5)


@dataclass
class Ctx:
    cap: Capture
    tx: object          # Taxonomy
    sv: object          # Survey


# ---------------- sync plugins ----------------

class I2cPlugin:
    name = "i2c"
    consumes = ("frames.startstop",)
    open_tail = True    # takes the frame a capture ended inside (others get Frames.closed())

    def run(self, c: Ctx, fr: typed.Frames, g):
        sb = fr.source
        # I2C samples SDA on the SCL rising edge by definition
        if len(sb.data) != 1 or len(fr.bounds) == 0 or sb.sample_edge != "rise":
            return []
        # a frame the capture ended inside (no STOP) is reported but is no
        # evidence for I2C: it scores 0 alone and does not count with others
        closed = len(fr.bounds) - int(fr.open_tail)
        lens = np.diff(fr.bounds[:closed], axis=1).ravel()
        lens = lens[lens > 0]
        w = typed.words(fr, 9)
        txs = typed.i2c_from_words(fr, w)
        if len(lens) == 0 and not txs:
            return []
        mod9 = float(np.mean(np.isin(lens % 9, (0, 1)))) if len(lens) else 0.0
        acks = (w.values[w.frame_of < closed] & 1) == 0
        whole = [t for t in txs if t.get("complete", True)]
        addr_ack = float(np.mean([t["addr_ack"] for t in whole])) if whole else 0.0
        clk = c.sv.clocks[sb.clock].clock_score
        pair = c.sv.pairs[(sb.clock, sb.data[0])].data_score
        nbytes = sum(1 + len(t["bytes"]) for t in whole)
        m = {"mod9": mod9, "ack_rate": float(acks.mean()) if len(acks) else 0.0, "addr_ack": addr_ack,
             "transactions": len(whole), "bytes": nbytes, "clk": clk, "pair": pair, "edge": sb.sample_edge}
        # NACKs are valid I2C (polling a busy device), so the ACK rate only
        # weighs lightly; it matters for choosing between sibling sampling edges
        score = mod9 * (0.8 + 0.2 * m["ack_rate"]) * q(nbytes) * (0.5 + 0.5 * clk) * (0.5 + 0.5 * pair)
        return [Node("i2c", {"sample_edge": sb.sample_edge}, {"scl": sb.clock, "sda": sb.data[0]},
                     Stream("i2c.transactions", txs), m, layer_score=score, total=score)]


class SpiPlugin:
    name = "spi"
    consumes = ("frames.select", "frames.gap")

    def run(self, c: Ctx, fr: typed.Frames, g):
        sb = fr.source
        if len(fr.bounds) == 0:
            return []
        lens = np.diff(fr.bounds, axis=1).ravel()
        lens = lens[lens > 0]
        if len(lens) == 0:
            return []
        mod8 = float(np.mean(lens % 8 == 0))
        k = int(np.sum(lens % 8 == 0))
        lines, line_scores, allv = {}, [], []
        for row, d in enumerate(sb.data):
            w = typed.words(fr, 8, row)
            per = [w.values[w.frame_of == i] for i in range(len(fr.bounds))]
            lines[d] = per
            allv.append(w.values)
            line_scores.append(c.sv.pairs[(sb.clock, d)].data_score * degenerate(w.values))
        nbytes = int(sum(len(v) for v in allv))
        clk = c.sv.clocks[sb.clock].clock_score
        cs = 1.0
        if sb.select:
            # measure the select evidence on the view this stream came from (a
            # glitch-filtered sibling), not on the raw capture the survey saw
            from .survey import clock_info, pair_relation
            view = getattr(sb, "view", c.cap)
            if view is c.cap:
                cs = c.sv.pairs[(sb.clock, sb.select)].boundary
            else:
                ci = clock_info(view, c.sv.features[sb.clock])  # bursts of the filtered clock
                cs = pair_relation(view, ci, sb.select).boundary if ci else 0.0
        clk_lv = c.sv.features[sb.clock].idle_level or 0
        want = 1 if sb.sample_edge == "rise" else 0
        mode = clk_lv * 2 + (0 if (want == 1) != bool(clk_lv) else 1)
        m = {"mod8": mod8, "frames": len(lens), "bytes": nbytes, "clk": clk, "cs": cs,
             "line_score": float(np.mean(line_scores)), "mode": mode, "delimiter": fr.delimiter}
        score = mod8 * (1 - 0.125 ** k) * q(nbytes) * (0.5 + 0.5 * clk) * m["line_score"] * cs
        if fr.delimiter == "gap" and len(lens) <= 1:
            score *= 0.5
        roles = {"clk": sb.clock, **({"cs": sb.select} if sb.select else {})}
        for i, d in enumerate(sb.data):
            roles[f"data{i}"] = d
        first = allv[0] if allv else np.zeros(0, np.int64)
        items = {"lines": lines, "value": first, "start": np.zeros(len(first), np.int64)}
        params = {"sample_edge": sb.sample_edge, "bit_order": "msb", "cs_active": None, "delimiter": fr.delimiter}
        return [Node("spi", params, roles, Stream("bytes", items, meta={"framed": True}), m,
                     layer_score=score, total=score)]


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
            views.append(typed.frames_select(c.cap, c.sv, sb))
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
        score = framing * coverage * q(ok_n) * degenerate(v & 0xFF) * (1 - 0.5 * clock_like) * (1 - sync)
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
            score = passed * q(ck["frames"], 3)
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


PLUGINS = [I2cPlugin(), SpiPlugin(), SyncUnknownPlugin(), RvswdPlugin(), UartPlugin(), LinPlugin(), DmxPlugin(),
           SwioPlugin()]
_BASE_PLUGINS = list(PLUGINS)
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


def glitch_views(cap, sv, g):
    """The capture as is, plus a sibling view with short pulses (reflections)
    removed from the clock and data pins when the clock shows such pulses:
    runs of <= 4 samples that are rare compared with the clock's own half period."""
    yield cap, 0
    ch = cap.channel(g.clock)
    _, length, _ = ch.runs(cap.n_samples)
    inner = length[1:-1]
    if len(inner) < 16:
        return
    med = float(np.median(inner))
    # Short pulses relative to the clock's own runs (reflections last a few ns,
    # i.e. more samples at higher rates; single-sample spikes at any rate).
    step = max(x.step for x in cap.channels if x.name in {g.clock, *g.data, *([g.select] if g.select else [])})
    short = inner <= max(float(step), 0.25 * med)
    if med >= 3 and 0 < short.mean() < 0.2:
        # the filter width is undecidable here: fixed small widths and one from
        # the observed short pulses are siblings; plugins' checks pick. A width
        # never exceeds half the clock's typical run.
        k_rel = int(max(2 * step, min(0.3 * med, np.quantile(inner[short], 0.95) + step)))
        pins = {g.clock, *g.data, *([g.select] if g.select else [])}
        for k in sorted({2 * step, 3 * step, 5 * step, k_rel}):
            if k > 0.5 * med:
                continue
            chans = [replace(x, edges=K.deglitch(x.edges, k)) if x.name in pins else x for x in cap.channels]
            yield Capture(cap.rate, cap.n_samples, chans), k


def single_views(cap, sv, pin):
    """An async pin as is, plus a sibling with single-sample spikes removed when
    such spikes are rare and the bit time is long enough to keep real bits."""
    yield cap, 0
    f = sv.features[pin]
    if not f.units:
        return
    unit = f.units[0].samples
    ch = cap.channel(pin)
    _, length, _ = ch.runs(cap.n_samples)
    inner = length[1:-1]
    if len(inner) < 16 or unit < 4 * ch.step:
        return
    spikes = inner <= ch.step
    if 0 < spikes.mean() < 0.1:
        k = int(max(2 * ch.step, min(0.3 * unit, 3 * ch.step)))
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
PULSE_PROTOCOLS = {"swio"}
SYNC_ROLES = {"i2c": {"scl": "clock", "sda": "data"}, "spi": {"clk": "clock", "mosi": "data", "miso": "data", "cs": "select"},
              "rvswd": {"clk": "clock", "dio": "data"}}


def _hinted(hints: dict | None):
    """Normalize hints: allowed protocols, per-pin protocol/role/baud, excluded pins."""
    h = hints or {}
    allowed = set(h.get("protocols") or [])
    pins = h.get("pins") or {}
    for info in pins.values():
        if info.get("protocol"):
            allowed.add(info["protocol"]) if h.get("protocols") else None
    return allowed, pins, set(h.get("exclude_pins") or [])


def _hinted_groups(pin_hint: dict, sv, have: list) -> list:
    """A bus whose clock and data pins are all named by the hints is tried even
    when the survey did not group it (a few clocks only: one byte, a transfer cut
    short). Nothing more is assumed; the plugins score it like any other group."""
    from .taxonomy import Group
    keys = {(g.clock, g.data, g.select) for g in have if g.kind == "sync"}
    out = []
    for proto, roles in SYNC_ROLES.items():
        by = {}
        for pn, i in pin_hint.items():
            if i.get("protocol") == proto and i.get("role") in roles and pn in sv.active:
                by.setdefault(roles[i["role"]], []).append(pn)
        clk, data = by.get("clock", []), tuple(sorted(by.get("data", [])))
        if len(clk) != 1 or clk[0] not in sv.clocks or not data:
            continue
        sel = by.get("select", [None])[0]
        if (clk[0], data, sel) not in keys:
            out.append(Group("sync", clock=clk[0], data=data, select=sel))
    return out


@lru_cache(maxsize=8)
def _device_packs(select: tuple[str, ...]):
    """hints["devices"]: pack path patterns under decl/devices ("i2c/**", "!i2c/qst/**")."""
    from . import devices as dev
    return dev.load_packs(list(select)), dev.address_table()


def use_declarative(on: bool = True) -> None:
    """Swap the hand-written I2C / SPI / RVSWD plugins for the declarative ones (decl/*.toml)."""
    global PLUGINS
    from .declarative import load_all
    base = [p for p in _BASE_PLUGINS if not (on and p.name in ("i2c", "spi", "rvswd"))]
    PLUGINS = base + (load_all(["i2c", "spi", "rvswd"]) if on else []) + load_all(DECLARED_ONLY)


DECLARED_ONLY = ["swd", "can"]   # no hand-written counterpart; always loaded from decl/


def analyze(cap: Capture, hints: dict | None = None) -> StagedResult:
    """hints (all optional): {"protocols": [...], "pins": {pin: {"protocol", "role", "baud"}},
    "exclude_pins": [...], "devices": ["i2c/**", ...]} restrict what is tried; nothing is decided by a hint alone,
    the plugins' checks still score every hypothesis."""
    t = {}
    allowed, pin_hint, excluded = _hinted(hints)
    if excluded:
        cap = Capture(cap.rate, cap.n_samples, [x for x in cap.channels if x.name not in excluded])
    t0 = time.perf_counter()
    tx = classify(cap)
    t["classify"] = time.perf_counter() - t0
    c = Ctx(cap, tx, tx.survey)
    by_type: dict[str, list] = {}
    for p in PLUGINS:
        if allowed and p.name not in allowed:
            continue
        for k in p.consumes:
            by_type.setdefault(k, []).append(p)
    async_pins = {pn for pn, i in pin_hint.items() if i.get("protocol") in ASYNC_PROTOCOLS | PULSE_PROTOCOLS}
    sync_pins = {pn for pn, i in pin_hint.items() if i.get("protocol") in SYNC_PROTOCOLS}

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
    run_sync = not allowed or bool(allowed & SYNC_PROTOCOLS)
    for g in [*tx.groups, *_hinted_groups(pin_hint, c.sv, tx.groups)]:
        if g.kind != "sync" or not run_sync or not group_ok(g):
            continue
        key = (g.clock, g.data, g.select)
        if key in seen:
            continue
        seen.add(key)
        for view_cap, k in glitch_views(cap, c.sv, g):
            for edge in typed.sample_edge_for(view_cap, g.clock, g.data):
                sb = typed.sync_bits(view_cap, c.sv, g.clock, g.data, g.select, edge)
                sb.deglitched = k
                sb.view = view_cap
                streams = []
                if g.select:
                    streams.append(("frames.select", typed.frames_select(view_cap, c.sv, sb)))
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
                        runs += 1
                        got = p.run(c, fr if getattr(p, "open_tail", False) else fr.closed(), g)
                        for n in got:
                            # the engine keeps the typed inputs (references) for
                            # consumers that want intermediate layers (GUI)
                            n.layers = {"bits": fr.source, "frames": fr}
                            roots.append(n)
    t["sync"] = time.perf_counter() - t0
    t0 = time.perf_counter()
    run_async = not allowed or bool(allowed & ASYNC_PROTOCOLS)
    run_pulse = not allowed or bool(allowed & PULSE_PROTOCOLS)
    for g in tx.groups:
        if g.kind != "single" or g.pin in sync_pins:
            continue
        pc = tx.pins[g.pin]
        if not ({"data", "sparse"} & set(pc.candidates)) and g.pin not in async_pins:
            continue
        hinted = pin_hint.get(g.pin, {})
        if hinted.get("protocol") in ASYNC_PROTOCOLS and not run_pulse:
            pass
        want_pulse = run_pulse and hinted.get("protocol") not in ASYNC_PROTOCOLS
        ps = typed.pulse_symbols(cap, c.sv, g.pin) if want_pulse and ("pulse" in pc.candidates or "data" in pc.candidates) else None
        if ps is not None:
            for p in by_type.get("pulses", []):
                runs += 1
                for n in p.run(c, ps, g):
                    n.layers = {"pulses": ps}
                    roots.append(n)
        if not run_async or hinted.get("protocol") in PULSE_PROTOCOLS:
            continue
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
        if n.analyzer in ("rvswd", "swio") and n.output is not None:
            d = dm_node(n)
            if d is not None:
                n.children.append(d)
                n.total = n.layer_score + 0.5 * (1 - n.layer_score) * d.total * n.layer_score
    t["upper"] = time.perf_counter() - t0
    t0 = time.perf_counter()
    # unknown-sync only explains what no known protocol explains better: a
    # free-running clock "sampling" a known bus's pins must not win by pin count
    known = [n for n in roots if n.analyzer != "sync_unknown" and n.output is not None]
    roots_pack = [n for n in roots if n.analyzer != "sync_unknown" or not any(
        k.total >= n.total and set(k.roles.values()) & set(n.roles.values()) for k in known)]
    exps = explanations(roots_pack, value)
    claims = []
    if exps:
        best_score, best = exps[0]
        for n in best:
            alt = explanations(roots_pack, value, top=1, exclude=lambda r, n=n: equivalent(r, n))
            alt_score, alt_set = alt[0] if alt else (0.0, [])
            margin = (best_score - alt_score) / max(1, changed_channels(best, alt_set))
            chans = set(n.roles.values())
            ru = max((r for r in alt_set if chans & set(r.roles.values())), key=lambda r: r.total, default=None)
            v = verdict(n.total, margin)
            if n.analyzer == "sync_unknown":
                v = "unknown-sync"
            claims.append(Claim(n.analyzer, dict(n.roles), dict(n.params), n.total, margin, v, n, ru))
    t["pack"] = time.perf_counter() - t0
    res = StagedResult(roots, exps, claims, t, tx.survey.features, tx, runs)
    res.relations = rels
    return res

use_declarative(False)   # installs the declared-only plugins (swd, can) by default
