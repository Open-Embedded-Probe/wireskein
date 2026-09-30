"""Decoding results as rows of labelled spans for a viewer (decode/annotations.json,
docs/wireskein-format.ja.md §5.3).

The viewer knows no protocol: it draws rows of (start, end, text). This turns
what `wireskein analyze` found into that form. For each protocol found, the
row is its highest decoded layer that has times (I2C transactions, UART
characters, RVSWD DMI accesses, ...), placed under its data line.

    from wireskein import annotate
    doc = annotate.build(load("c.wireskein"))
    annotate.save("c.wireskein", doc)       # decode/annotations.json; the viewer shows it without a server
"""

from __future__ import annotations

import json
from pathlib import Path

from . import __version__

FORMAT = "wireskein-annotations/0"
ENTRY = "decode/annotations.json"
MAX_ITEMS = 50_000                     # per row: beyond this the viewer would only draw noise
DATA_ROLES = ("sda", "data", "dio", "mosi", "miso", "rx", "tx")
CONTROL = {9: "\\t", 10: "\\n", 13: "\\r", 0: "\\0"}


def _text(protocol: str, it: dict) -> str:
    if "text" in it:
        return str(it["text"])
    if "addr" in it and "rw" in it:                             # a bus transaction
        s = f"{str(it['rw'])[:1].upper()} {int(it['addr']):#04x}"
        data = it.get("bytes") or ""
        if data:
            s += " " + " ".join(data[i:i + 2] for i in range(0, len(data), 2))
        if it.get("addr_ack") is False:
            s += " NACK"
        return s
    if "op" in it and ("reg" in it or "addr" in it):           # a register access
        s = f"{it['op']} {it.get('reg') or it.get('addr')}"
        if isinstance(it.get("data"), int):
            s += f" {it['data']:#010x}"
        return s
    if isinstance(it.get("value"), int):                       # a character or word
        v = it["value"]
        if protocol == "uart":
            if v == 32:
                return "\u2423"                                # open box: a space would draw nothing
            if 32 < v < 127:
                return chr(v)
            if v in CONTROL:
                return CONTROL[v]
        return f"{v:#04x}" if v < 256 else f"{v:#x}"
    rest = {k: v for k, v in it.items() if k not in ("s", "e")}
    return ", ".join(f"{k}={v}" for k, v in rest.items())[:60]


def _level(it: dict) -> str:
    if it.get("ok") is False or it.get("error"):
        return "error"
    acks = it.get("acks")
    if isinstance(acks, list) and str(it.get("rw", "")).startswith("r"):
        acks = acks[:-1]                                        # a read ends with the master's NACK: normal
    if it.get("addr_ack") is False or (isinstance(acks, list) and False in acks):
        return "warn"
    return "ok"


def rows_of(doc: dict) -> list[dict]:
    """Rows from an `analyze --mode all` document."""
    rows = []
    for claim in doc.get("claims", []):
        proto = claim["protocol"]
        layers = claim.get("layers", {})
        timed = [(name, v) for name, v in layers.items()
                 if isinstance(v, list) and v and isinstance(v[0], dict) and "s" in v[0]]
        if not timed:
            continue
        name, items = timed[-1]                                 # the highest layer with times
        unit = None
        blocks = layers.get("blocks")
        if isinstance(blocks, list) and blocks and "unit_samples" in blocks[0]:
            unit = float(blocks[0]["unit_samples"])             # a character's bit time: its span is 10 bits
        roles = claim.get("roles", {})
        near = next((roles[r] for r in DATA_ROLES if r in roles), next(iter(roles.values()), None))
        out = []
        for it in items[:MAX_ITEMS]:
            a = {"s": int(it["s"]), "text": _text(proto, it), "level": _level(it)}
            if "e" in it:
                a["e"] = int(it["e"])
            elif unit and proto == "uart":
                a["e"] = int(it["s"] + 10 * unit)
            a["detail"] = {k: v for k, v in it.items() if k not in ("s", "e")}
            out.append(a)
        row = {"name": f"{proto} {name}", "items": out}
        if near:
            row["near"] = near
        if len(items) > MAX_ITEMS:
            row["truncated"] = len(items)
        rows.append(row)
    return rows


def build(cap, hints: dict | None = None) -> dict:
    """Run the analysis on a capture and return the annotations document."""
    from ._engine import staged
    from ._engine.export import dumps, export
    res = staged.analyze(cap, hints)
    doc = json.loads(dumps(export(res, cap, mode="all")))
    return {"format": FORMAT, "source": f"wireskein {__version__} analyze", "rows": rows_of(doc)}


def save(path: str | Path, doc: dict) -> None:
    from . import fileformat
    fileformat.put(path, ENTRY, json.dumps(doc, ensure_ascii=False, separators=(",", ":")))


def load(path: str | Path) -> dict | None:
    from . import fileformat
    data = fileformat.get(path, ENTRY)
    if data is None:
        return None
    doc = json.loads(data)
    return doc if doc.get("format") == FORMAT else None
