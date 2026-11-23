"""The upper stage: what reads the buses' output rather than the lines - upper layers on byte streams (lines, NMEA,
Modbus RTU, marker dialects), a UART pair's exchanges (SCPI, protocols of others on a pair), device packs on I2C and
SPI, SD on SPI, RISC-V's debug module on RVSWD / SWIO, protocols of others on a bus (wireskein.plugins)
- apart from the engine that found the buses (bench/notes/2026-11-23-extension-spec.ja.md §1, §3.1, §3.3).

It reads a bus as the analysis exports it (a reading: protocol, roles, params, score and its layers, the lowest
ones: bytes, transactions, transfers, dmi ...), whichever engine read it, and never the engine's insides. What it gives
(Upper): the upper layers, the devices, the protocols of others' readings, and the score raised by the checks that
held (each child's support by the same formula: s + 0.5 (1 - s) · support · s). The engine chooses its claims before
this stage; their verdicts are made again after it from the raised score (the margin raised as much)."""
from __future__ import annotations

from dataclasses import dataclass, field
from functools import lru_cache

import numpy as np

from ._engine.analyzers.upper import Lines, MarkerGrammar, ModbusRtu, Nmea
from ._engine.scoring import DefaultScorer
from ._engine.stack import Node, Stream

UPPER = [Lines(), Nmea(), ModbusRtu(), MarkerGrammar()]
SHOWN = 0.3            # an upper reading at this score or more is shown (and a protocol of others' is the final one)


@dataclass
class Upper:
    """What the upper stage gives a reading."""
    score: float                                   # the reading's score, raised by its upper readings
    layers: dict = field(default_factory=dict)     # upper layers, as the analysis format has them
    devices: list = field(default_factory=list)    # device packs' matches (wireskein._engine.devices.Match)
    user: dict = field(default_factory=dict)       # protocols of others: name -> {messages, checks_*, score[, error]}
    evidence: dict = field(default_factory=dict)   # figures beside the reading's own ("_support")
    _users: list = field(default_factory=list)     # their scores, raised into score at the end (finish)


def _support(t: float, s: float, w: float | None = None) -> float:
    """A reading at t raised by a child of score s (weighed by w, else by t)."""
    return t + 0.5 * (1 - t) * s * (t if w is None else w)


# ---------------- the bus as the upper readings take it ----------------

def _bytes_node(r: dict, rate: float) -> Node | None:
    b = (r.get("layers") or {}).get("bytes")
    if r["protocol"] != "uart" or not b or rate is None:
        return None
    v = np.frombuffer(bytes.fromhex(b["hex"]), np.uint8).astype(np.int64)
    if b.get("bit8"):
        v = v | (np.frombuffer(b["bit8"].encode(), np.uint8).astype(np.int64) - 48) << 8
    p = r.get("params") or {}
    cs = rate / p["baud"] * (p.get("L") or 10)
    items = {"value": v, "start": np.asarray(b["s"], np.int64), "char_samples": cs}
    return Node("uart", dict(p), dict(r["roles"]), Stream("bytes", items, meta={"char_samples": cs}), {},
                layer_score=float(r["score"]), total=float(r["score"]))


def _transactions(r: dict) -> list[dict]:
    """I2C's transactions as the device packs read them (bytes as numbers, s / e as start / end)."""
    out = []
    for t in (r.get("layers") or {}).get("transactions", []):
        out.append({**t, "start": t.get("s"), "end": t.get("e"), "bytes": list(bytes.fromhex(t.get("bytes") or ""))})
    return out


def _spi_lines(r: dict) -> dict:
    """SPI's frames per data line: {pin: [bytes of each transfer]}."""
    tr = (r.get("layers") or {}).get("transfers", [])
    pins = [k for k in (tr[0] if tr else {}) if k not in ("s", "e")]
    return {p: [np.frombuffer(bytes.fromhex(x.get(p, "")), np.uint8).astype(np.int64) for x in tr] for p in pins}


# ---------------- one reading ----------------

def read(r: dict, rate: float | None, hints: dict | None = None) -> Upper:
    """The upper stage on one reading; the protocols of others' support is left to finish (after the pairs). rate:
    the capture's ticks a second (None where the reading's engine does not say)."""
    up = Upper(float(r["score"]))
    proto = r["protocol"]
    node = _bytes_node(r, rate)
    if node is not None:
        _bytes_upper(node, up)
    if proto in ("i2c", "spi"):
        _devices(r, up, hints)
    # (an I2C bus's addresses are its own evidence, not a reading above it: the engine's, staged.i2c_addresses)
    if proto == "spi":
        sd = sd_spi(r)
        if sd is not None:
            score, cmds = sd
            up.layers["sd"] = [{"s": c["t"], "cmd": ("ACMD" if c["acmd"] else "CMD") + str(c["cmd"]), "arg": c["arg"],
                                "r1": c.get("r1"), **{k: c[k] for k in ("resp", "ncr", "crc_ok") if k in c},
                                **({"blocks": [{"s": b["t"], "token": b["token"], "len": b.get("len"),
                                                "crc_ok": b.get("crc_ok")} for b in c["blocks"]]} if "blocks" in c else {})}
                               for c in cmds]
            up.score = _support(float(r["score"]), score, float(r["score"]))
    elif proto in ("rvswd", "swio"):
        dm = riscv_dm(r)
        if dm is not None:
            score, log = dm
            up.layers["dm"] = [{**({"s": int(t0)} if t0 is not None else {}), **({"e": int(t1)} if t1 is not None else {}),
                                "op": k, "addr": ad, "value": v, **({"size": sz} if sz else {})}
                               for k, ad, v, sz, t0, t1 in log]
            up.score = _support(float(r["score"]), score, float(r["score"]))
    _protocols(r, up, rate)
    return up


def finish(up: Upper) -> Upper:
    """The protocols of others' support raised into the score (the last of the stage, after the pairs')."""
    for s in up._users:
        up.score = _support(up.score, s)
    up._users = []
    return up


def _bytes_upper(node: Node, up: Upper) -> None:
    """Upper layers on a byte stream: lines, NMEA, Modbus RTU, and markers on the lines."""
    scorer = DefaultScorer()

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

    def score(n: Node):
        for ch in n.children:
            score(ch)
            ch.layer_score = scorer.layer(ch)
            ch.total = scorer.combine(ch)

    expand(node)
    score(node)
    support = max((ch.total for ch in node.children), default=0.0)
    up.evidence["_support"] = support
    up.score = _support(node.layer_score, support, node.layer_score)
    for ch in node.children:
        if ch.output is None or ch.total < SHOWN:
            continue
        if ch.analyzer == "lines":
            up.layers["lines"] = list(ch.output.items)
        elif ch.analyzer in ("nmea", "modbus_rtu"):
            up.layers[ch.analyzer] = list(ch.output.items)


def _devices(r: dict, up: Upper, hints) -> None:
    """Device packs on I2C's transactions and SPI's transfers: only self-verified matches support the bus."""
    from ._engine import devices as dev
    packs, addr_db = _device_packs(tuple((hints or {}).get("devices") or ["**"]))
    try:
        up.devices = (dev.match_i2c(_transactions(r), packs, addr_db) if r["protocol"] == "i2c"
                      else dev.match_spi(_spi_lines(r), packs))
    except (KeyError, TypeError):
        up.devices = []
    if any(m.level == "identified" for m in up.devices):
        up.score = _support(up.score, up.score)


def sd_spi(r: dict):
    """SD commands in SPI mode on an SPI bus's transfers (periph.sd_spi), the data lines tried both ways round as MOSI
    and MISO: (score, commands) or None. The evidence is commands with their end bit answered by an R1 within NCR,
    the lines idle (0xFF) around them as SD's are."""
    from ._engine import periph
    lay = r.get("layers") or {}
    bits, frames, tr = lay.get("bits"), lay.get("frames"), lay.get("transfers")
    lines = _spi_lines(r)
    if bits is None or frames is None or tr is None or len(lines) != 2:
        return None
    t = np.asarray(bits["t"], np.int64)
    starts = np.searchsorted(t, [x["s"] for x in tr])           # each transfer's first bit
    best = None
    pins = list(lines)
    for mo_pin, mi_pin in ((pins[0], pins[1]), (pins[1], pins[0])):
        rows = []
        for i, (a, mo, mi) in enumerate(zip(starts.tolist(), lines[mo_pin], lines[mi_pin])):
            rows += [(int(t[a + 8 * k]), int(x) & 0xFF, int(y) & 0xFF, i) for k, (x, y) in enumerate(zip(mo, mi))]
        cmds = periph.sd_spi(rows)["commands"]
        good = [c for c in cmds if c.get("r1") is not None and c["end_bit"] and c["idle_ok"]]
        if len(good) < 2:
            continue
        crc = [c["crc_ok"] for c in good if c["cmd"] in (0, 8) and not c["acmd"]]
        score = len(good) / len(cmds) * (1 - 0.5 ** (len(good) / 2)) * (1.0 if all(crc) else 0.5)
        if best is None or score > best[0]:
            best = (score, cmds)
    return best


def riscv_dm(r: dict):
    """RISC-V's debug module on the DMI accesses (RVSWD, SWIO): (score, accesses) or None."""
    from ._engine.riscv_dm import NAMES, accesses
    tx = [{**x, "t": x.get("s")} for x in (r.get("layers") or {}).get("dmi", [])]
    if not tx:
        return None
    known = float(np.mean([x["addr"] in NAMES for x in tx if x["op"] != "BURST"] or [0]))
    log = accesses(tx)
    has_ctrl = any(x["addr"] == 0x10 for x in tx) and any(x["addr"] == 0x11 for x in tx)
    return known * (0.5 + 0.5 * has_ctrl), log


def _protocols(r: dict, up: Upper, rate: float) -> None:
    """Protocols of others on the bus (declared in a definitions directory: wireskein.plugins): each given the bus as the analysis exports
    it (with the upper layers read so far); its checks, as Modbus RTU's, support the bus."""
    from . import plugins as P
    mine = [p for p in P.protocols().values() if r["protocol"] in p.over]
    if not mine or r["protocol"] == "uart_pair":
        return
    for p in mine:
        bus = {"roles": dict(r["roles"]), "tick_hz": None if rate is None else float(rate), **(r.get("layers") or {}),
               **up.layers}
        try:
            got = p.decode(bus) or {}
        except Exception as e:  # noqa: BLE001 - another's protocol: its failure is shown, the analysis goes on
            got = {"messages": [], "checks_passed": 0, "checks_failed": 0, "error": f"{type(e).__name__}: {e}"}
        m = {"messages": list(got.get("messages", [])), "checks_passed": int(got.get("checks_passed", 0)),
             "checks_failed": int(got.get("checks_failed", 0)), **({"error": got["error"]} if "error" in got else {})}
        s = P._score(m)
        up.user[p.name] = {**m, "score": s}
        up._users.append(s)
        if s >= SHOWN:
            up.layers[p.name] = {"messages": m["messages"], "checks_passed": m["checks_passed"],
                                 "checks_failed": m["checks_failed"]}


def final_user(up: Upper | None) -> str | None:
    """The protocol of others whose checks held best: the bus's final reading."""
    shown = [(v["score"], k) for k, v in (up.user if up else {}).items() if v["score"] >= SHOWN]
    return max(shown, key=lambda x: x[0])[1] if shown else None


# ---------------- the pairs, and the stage over an engine's claims ----------------

def pairs(readings: list[tuple[dict, Upper]], rate: float) -> list[dict]:
    """Every two UART readings paired (wireskein._engine.duplex): SCPI and the protocols of others on the pair, their
    support flowing down to both lines. -> the relations, as the analysis format has them (with their scores; the
    export shows those at SHOWN or more)."""
    from ._engine import duplex
    uarts = []
    for r, up in readings:
        n = _bytes_node(r, rate)
        if n is not None and up.score > 0.3:
            n.total = up.score
            uarts.append((n, up))
    rel = []
    for i in range(len(uarts)):
        for j in range(i + 1, len(uarts)):
            (a, ua), (b, ub) = uarts[i], uarts[j]
            if a.roles["data"] == b.roles["data"]:
                continue
            got = duplex.pair(a, b)
            if got is None:
                continue
            d, m = got
            if m["score"] < 0.4:
                continue
            node = Node("uart_duplex", {"baud": a.params["baud"]}, {"a": d.a, "b": d.b}, Stream("duplex", d), m,
                        layer_score=m["score"], total=m["score"] * min(ua.score, ub.score))
            users = duplex.user_protocols(d, node)
            for u in users:                       # their support first, then SCPI's (shown SCPI first)
                for x in (ua, ub):
                    x.score = _support(x.score, u.total, node.layer_score)
            s = duplex.scpi_node(d, a.params)
            if s is not None:
                for x in (ua, ub):
                    x.score = _support(x.score, s.total, node.layer_score)
                rel.append({"protocol": "scpi", "roles": s.roles, "score": round(float(s.total), 4),
                            "exchanges": s.output.items})
            rel += [{"protocol": u.analyzer, "roles": u.roles, "score": round(float(u.total), 4),
                     "messages": u.output.items, "user": True,
                     **{k: u.metrics[k] for k in ("checks_passed", "checks_failed")}} for u in users]
    return rel


def run(readings: list[dict], rate: float, hints: dict | None = None) -> tuple[list[Upper], list[dict]]:
    """The stage over an engine's claims (readings): each one's Upper, and the relations of the UART pairs."""
    ups = [read(r, rate, hints) for r in readings]
    rels = pairs(list(zip(readings, ups)), rate)
    return [finish(u) for u in ups], rels


def _device_packs(select: tuple[str, ...]):
    """hints["devices"]: pack path patterns under decl/devices ("i2c/**", "!i2c/qst/**"), and under devices/ of
    each definitions directory added from outside."""
    from . import plugins as P
    extra = tuple(str(d / "devices") for d in P.definitions() if (d / "devices").is_dir())
    return _device_packs_at(select, extra)


@lru_cache(maxsize=8)
def _device_packs_at(select: tuple[str, ...], extra: tuple[str, ...]):
    from ._engine import devices as dev
    from pathlib import Path
    sets = [dev.load_packs(list(select))] + [dev.load_packs(list(select), Path(r)) for r in extra]
    return (sets[0] if len(sets) == 1 else dev.PackSets(sets)), dev.address_table()
