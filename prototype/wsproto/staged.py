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
from dataclasses import dataclass

import numpy as np

from . import kernels as K
from . import plugins_uartlike as UL
from . import typed
from .analyzers.upper import Lines, MarkerGrammar, ModbusRtu, Nmea
from .model import Capture
from .pipeline import CLAIM_COST, Claim, changed_channels, verdict
from .scoring import DefaultScorer
from .stack import Node, Stream, explanations
from .taxonomy import classify
from .rvswd import RvswdPlugin, SwioPlugin, dm_node
from .model import Channel


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

    def run(self, c: Ctx, fr: typed.Frames, g):
        sb = fr.source
        # I2C samples SDA on the SCL rising edge by definition
        if len(sb.data) != 1 or len(fr.bounds) == 0 or sb.sample_edge != "rise":
            return []
        lens = np.diff(fr.bounds, axis=1).ravel()
        lens = lens[lens > 0]
        if len(lens) == 0:
            return []
        mod9 = float(np.mean(np.isin(lens % 9, (0, 1))))
        w = typed.words(fr, 9)
        acks = (w.values & 1) == 0
        txs = typed.i2c_from_words(fr, w)
        addr_ack = float(np.mean([t["addr_ack"] for t in txs])) if txs else 0.0
        clk = c.sv.clocks[sb.clock].clock_score
        pair = c.sv.pairs[(sb.clock, sb.data[0])].data_score
        nbytes = sum(1 + len(t["bytes"]) for t in txs)
        m = {"mod9": mod9, "ack_rate": float(acks.mean()) if len(acks) else 0.0, "addr_ack": addr_ack,
             "transactions": len(txs), "bytes": nbytes, "clk": clk, "pair": pair, "edge": sb.sample_edge}
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
        views = [typed.frames_gap(sb), typed.frames_startstop(c.cap, sb)]
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


# Optional recorder for the plugin-boundary experiment (jsplugin_bench.py):
# a list that receives (plugin, stream fields, python result, seconds).
RECORDER = None


def _record(name, fields, nodes, dt):
    if RECORDER is None:
        return
    RECORDER.append((name, fields, [(n.layer_score, n.output.items if n.output else None) for n in nodes], dt))


PLUGINS = [I2cPlugin(), SpiPlugin(), SyncUnknownPlugin(), RvswdPlugin(), UartPlugin(), LinPlugin(), DmxPlugin(),
           SwioPlugin()]
UPPER = [Lines(), Nmea(), ModbusRtu(), MarkerGrammar()]


def _upper(node: Node, scorer: DefaultScorer) -> None:
    """Upper layers on byte streams (reused from the flat engine)."""
    if node.output is None or node.output.kind != "bytes":
        return
    if RECORDER is not None and node.analyzer == "uart":
        nm = Nmea()
        t_p = time.perf_counter()
        roles, out, m = nm.run(None, node, {})
        dt = time.perf_counter() - t_p
        tmp = Node("nmea", {}, roles, out, m)
        tmp.layer_score = scorer._checked(m)
        _record("nmea", {"values": np.asarray(node.output.items["value"]) & 0xFF, "roles": dict(node.roles)}, [tmp], dt)

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
    short = inner <= max(1.0, 0.25 * med)
    if med >= 3 and 0 < short.mean() < 0.2:
        # the filter width is undecidable here: fixed small widths and one from
        # the observed short pulses are siblings; plugins' checks pick. A width
        # never exceeds half the clock's typical run.
        k_rel = int(max(2, min(0.3 * med, np.quantile(inner[short], 0.95) + 1)))
        pins = {g.clock, *g.data, *([g.select] if g.select else [])}
        for k in sorted({2, 3, 5, k_rel}):
            if k > 0.5 * med:
                continue
            chans = [Channel(x.name, x.initial, K.deglitch(x.edges, k)) if x.name in pins else x for x in cap.channels]
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
    if len(inner) < 16 or unit < 4:
        return
    spikes = inner <= 1
    if 0 < spikes.mean() < 0.1:
        k = int(max(2, min(0.3 * unit, 3)))
        chans = [type(x)(x.name, x.initial, K.deglitch(x.edges, k)) if x.name == pin else x for x in cap.channels]
        yield Capture(cap.rate, cap.n_samples, chans), k


def i2c_device_node(parent: Node) -> Node | None:
    """Upper layer over I2C transactions: devices on the bus. A real bus talks to
    few addresses, repeatedly, and register-map devices show "write register
    pointer, then read" pairs to the same address."""
    txs = [t for t in parent.output.items if "addr" in t]
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


SYNC_PROTOCOLS = {"i2c", "spi", "rvswd", "sync_unknown"}
ASYNC_PROTOCOLS = {"uart", "lin", "dmx512"}
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


def analyze(cap: Capture, hints: dict | None = None) -> StagedResult:
    """hints (all optional): {"protocols": [...], "pins": {pin: {"protocol", "role", "baud"}},
    "exclude_pins": [...]} restrict what is tried; nothing is decided by a hint alone,
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
    for g in tx.groups:
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
                        t_p = time.perf_counter()
                        got = p.run(c, fr, g)
                        if RECORDER is not None and p.name == "i2c" and kind == "frames.startstop":
                            _record("i2c", {"bits": sb.bits[0] if len(sb.data) == 1 else sb.bits, "t": sb.t,
                                            "bounds": fr.bounds.ravel(), "sample_edge": sb.sample_edge,
                                            "n_data": len(sb.data), "clock": sb.clock, "data": list(sb.data),
                                            "clk_score": c.sv.clocks[sb.clock].clock_score,
                                            "pair_score": c.sv.pairs[(sb.clock, sb.data[0])].data_score},
                                    got, time.perf_counter() - t_p)
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
            blocks = typed.block_chars(view_cap, c.sv, rb)
            for p in by_type.get("chars", []):
                runs += 1
                t_p = time.perf_counter()
                got = p.run(Ctx(view_cap, c.tx, c.sv), rb, blocks)
                if RECORDER is not None and p.name in ("lin", "dmx512"):
                    for (s0, s1, u), cands, ch in blocks:
                        if ch is not None:
                            _record(p.name, {"values": ch.values, "ok": ch.ok.astype(np.uint8), "start": ch.start,
                                             "breaks": ch.breaks, "pin": rb.pin, "baud": cap.rate / u, "idle": rb.idle},
                                    got, (time.perf_counter() - t_p) / max(1, len(blocks)))
                            break
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
    exps = explanations(roots, value)
    claims = []
    if exps:
        best_score, best = exps[0]
        for n in best:
            alt = explanations(roots, value, top=1, exclude=lambda r, n=n: equivalent(r, n))
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
