"""The upper stage: what reads the buses' output rather than the lines - upper layers on byte streams (lines, NMEA,
Modbus RTU, marker dialects), a UART pair's exchanges (SCPI, protocols of others on a pair), device packs on I2C and
SPI, devices on I2C, SD on SPI, RISC-V's debug module on RVSWD / SWIO, protocols of others on a bus (wireskein.plugins)
- apart from the engine that found the buses (bench/notes/2026-11-23-extension-spec.ja.md §1, §3.1). Each reading's
checks that hold support the bus beneath it (§3.3).

The engine hands its readings over (run): each with its protocol (analyzer), roles, output (its kind and items),
layers, children and total; this stage adds children and raises totals."""
from __future__ import annotations

from functools import lru_cache

import numpy as np

from ._engine.analyzers.upper import Lines, MarkerGrammar, ModbusRtu, Nmea
from ._engine.riscv_dm import dm_node
from ._engine.scoring import DefaultScorer
from ._engine.stack import Node, Stream


def run(roots: list, hints: dict | None = None) -> list:
    """The stage over an engine's readings (roots): -> the relations (a UART pair's)."""
    scorer = DefaultScorer()
    for n in roots:
        _upper(n, scorer)
    from ._engine.duplex import relations
    rels = relations(roots)
    # device packs on top of transactions/transfers: only self-verified matches support the bus
    from ._engine import devices as dev
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

    return rels


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

def _protocols(roots: list[Node]) -> None:
    """Protocols of others on the buses decoded (wireskein.plugins.register_protocol): each given the bus as the analysis exports
    it; its checks, as Modbus RTU's, support the bus."""
    from . import plugins as P
    from ._engine import export
    protos = P.protocols()
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
            s = P._score(m)
            child = Node(p.name, {}, n.roles, Stream(p.name, list(got.get("messages", []))), m, n,
                         layer_score=s, total=s)
            child.user = True
            n.children.append(child)
            n.total = n.total + 0.5 * (1 - n.total) * s * n.total

def sd_spi_node(parent: Node) -> Node | None:
    """Upper layer over SPI transfers: SD commands in SPI mode (periph.sd_spi), with the data pins tried both ways
    round as MOSI and MISO. The evidence is commands with their end bit answered by an R1 within NCR, the lines
    idle (0xFF) around them as SD's are."""
    from ._engine import periph
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
