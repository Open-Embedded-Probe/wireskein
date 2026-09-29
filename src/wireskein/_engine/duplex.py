"""Pairing of async lines after single-line analysis (TX/RX), and plugins on
the resulting duplex message stream (SCPI).

A pair is scored from the two UART results only: same bit time and character
length, messages that alternate between the lines, and little overlap in time.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

import numpy as np

from .stack import Node, Stream


@dataclass
class Message:
    pin: str
    t0: int
    t1: int
    data: bytes


@dataclass
class Duplex:
    a: str
    b: str
    messages: list[Message]

    def nbytes(self) -> int:
        return sum(len(m.data) + 9 for m in self.messages)


def messages(node: Node, gap_chars: float = 3.0) -> list[Message]:
    it = node.output.items
    v, st = np.asarray(it["value"]) & 0xFF, np.asarray(it["start"])
    if len(v) == 0:
        return []
    cs = float(it.get("char_samples") or node.output.meta.get("char_samples") or 1)
    cut = np.flatnonzero(np.diff(st) > (gap_chars + 1) * cs) + 1
    out = []
    for seg_v, seg_t in zip(np.split(v, cut), np.split(st, cut)):
        out.append(Message(node.roles["data"], int(seg_t[0]), int(seg_t[-1] + cs), bytes(int(x) for x in seg_v)))
    return out


def pair(na: Node, nb: Node) -> tuple[Duplex, dict] | None:
    pa, pb = na.params, nb.params
    if abs(pa["baud"] / pb["baud"] - 1) > 0.03 or pa.get("L") != pb.get("L"):
        return None
    ma, mb = messages(na), messages(nb)
    if not ma or not mb or len(ma) + len(mb) < 3:
        return None
    msgs = sorted(ma + mb, key=lambda m: m.t0)
    busy = sum(m.t1 - m.t0 for m in msgs)
    # overlap of two sorted interval lists, two-pointer sweep
    overlap, i, j = 0, 0, 0
    while i < len(ma) and j < len(mb):
        overlap += max(0, min(ma[i].t1, mb[j].t1) - max(ma[i].t0, mb[j].t0))
        if ma[i].t1 < mb[j].t1:
            i += 1
        else:
            j += 1
    ov = overlap / max(1, busy)

    # Request/response shape: every message of the responding side comes right
    # after a message of the other side. The requesting side may send several
    # messages in a row (commands without a response), so plain alternation
    # would be the wrong measure.
    def follows(resp_pin):
        idx = [i for i, m in enumerate(msgs) if m.pin == resp_pin]
        ok = sum(1 for i in idx if i > 0 and msgs[i - 1].pin != resp_pin)
        return ok / max(1, len(idx)), len(idx)

    fa, na_ = follows(na.roles["data"])
    fb, nb_ = follows(nb.roles["data"])
    f, n_resp = max((fa, na_), (fb, nb_))
    m = {"follow": f, "responses": n_resp, "overlap": ov, "messages": len(msgs)}
    m["score"] = f * (1 - min(1.0, 2 * ov)) * (1 - 0.5 ** n_resp)
    return Duplex(na.roles["data"], nb.roles["data"], msgs), m


SCPI_LINE = re.compile(r"^(\*[A-Za-z]{3}\??|:?[A-Za-z]{3,}(:[A-Za-z0-9]+)*\??)(\s+\S.*)?$")


def scpi(d: Duplex, host: str) -> dict:
    """SCPI on a duplex stream with `host` as the controller side."""
    lines, answered, queries, grammar, total_host, stray = [], 0, 0, 0, 0, 0
    msgs = d.messages
    for i, m in enumerate(msgs):
        if m.pin != host:
            continue
        for cmd in m.data.decode("latin-1").split("\n"):
            cmd = cmd.strip()
            if not cmd:
                continue
            total_host += 1
            grammar += bool(SCPI_LINE.match(cmd))
            resp = None
            if cmd.endswith("?"):
                queries += 1
                nxt = msgs[i + 1] if i + 1 < len(msgs) else None
                if nxt is not None and nxt.pin != host:
                    answered += 1
                    resp = nxt.data.decode("latin-1").strip()
            lines.append([cmd, resp])
    # device messages that do not follow a query are stray
    for i, m in enumerate(msgs):
        if m.pin == host:
            continue
        prev = msgs[i - 1] if i > 0 else None
        if prev is None or prev.pin != host or not prev.data.decode("latin-1").strip().endswith("?"):
            stray += 1
    dev_msgs = sum(1 for m in msgs if m.pin != host)
    return {"exchanges": lines, "grammar": grammar / max(1, total_host), "answered": answered / max(1, queries),
            "queries": queries, "stray": stray / max(1, dev_msgs), "host": host}


def scpi_node(d: Duplex, pm: dict) -> Node | None:
    best = None
    for host in (d.a, d.b):
        r = scpi(d, host)
        if r["queries"] == 0:
            continue
        score = r["grammar"] * r["answered"] * (1 - r["stray"]) * (1 - 0.5 ** r["queries"])
        if best is None or score > best[0]:
            best = (score, r)
    if best is None:
        return None
    score, r = best
    dev = d.b if r["host"] == d.a else d.a
    return Node("scpi", {"host": r["host"]}, {"tx": r["host"], "rx": dev}, Stream("scpi", r["exchanges"]),
                {k: v for k, v in r.items() if k != "exchanges"}, layer_score=score, total=score)


def relations(roots: list[Node]) -> list[Node]:
    """Pair every two UART results; attach SCPI on top; feed support back."""
    uarts = [n for n in roots if n.analyzer == "uart" and n.output is not None and n.total > 0.3]
    rel = []
    for i in range(len(uarts)):
        for j in range(i + 1, len(uarts)):
            a, b = uarts[i], uarts[j]
            if a.roles["data"] == b.roles["data"]:
                continue
            got = pair(a, b)
            if got is None:
                continue
            d, m = got
            if m["score"] < 0.4:
                continue
            node = Node("uart_duplex", {"baud": a.params["baud"]}, {"a": d.a, "b": d.b}, Stream("duplex", d), m,
                        layer_score=m["score"], total=m["score"] * min(a.total, b.total))
            s = scpi_node(d, a.params)
            if s is not None:
                node.children.append(s)
                node.total = node.total + 0.5 * (1 - node.total) * s.total
                # support flows down to both lines
                for u in (a, b):
                    u.total = u.total + 0.5 * (1 - u.total) * s.total * node.layer_score
            rel.append(node)
    return rel
