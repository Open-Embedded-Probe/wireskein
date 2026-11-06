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

FORMAT = "wireskein-annotations/1"
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
    if it.get("level") in ("ok", "warn", "error"):              # a protocol of one's own says it
        return it["level"]
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
        # a protocol of one's own (wireskein.protocols) is {"messages": [...]}: its messages with times are a row too
        timed += [(name, v["messages"]) for name, v in layers.items()
                  if isinstance(v, dict) and isinstance(v.get("messages"), list) and v["messages"]
                  and isinstance(v["messages"][0], dict) and "s" in v["messages"][0]]
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
        if claim.get("verdict"):
            row["verdict"] = claim["verdict"]                 # how sure the analysis is ("confirmed", "likely", ...)
        if isinstance(claim.get("score"), (int, float)):
            row["score"] = claim["score"]
        if near:
            row["near"] = near
        if len(items) > MAX_ITEMS:
            row["truncated"] = len(items)
        rows.append(row)
    # pairs read together (a command line and its answers): scpi's exchanges have no times; a protocol of one's
    # own on a UART pair has messages with them
    for rel in doc.get("relations", []):
        items = rel.get("messages")
        if not (isinstance(items, list) and items and isinstance(items[0], dict) and "s" in items[0]):
            continue
        out = []
        for it in items[:MAX_ITEMS]:
            a = {"s": int(it["s"]), "text": _text(rel["protocol"], it), "level": _level(it),
                 "detail": {k: v for k, v in it.items() if k not in ("s", "e")}}
            if "e" in it:
                a["e"] = int(it["e"])
            out.append(a)
        roles = rel.get("roles", {})
        row = {"name": f"{rel['protocol']} messages", "items": out, "near": next(iter(roles.values()), None)}
        if isinstance(rel.get("score"), (int, float)):
            row["score"] = rel["score"]
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


def build_file(path: str | Path, hints: dict | None = None, prepare=None, part: int | None = None) -> dict:
    """The annotations document of a file: each part decoded on its own (analysis never crosses parts,
    wireskein-format §3.4), its rows naming it (`part`, §5.3) when the file has several. part: that one only.
    prepare: a step from the loaded capture to the one decoded (e.g. analog read as logic)."""
    from . import _fileformat as fileformat
    from .decode import load
    prepare = prepare or (lambda cap: cap)
    from ._engine import fileio
    n = len(fileformat.read_header(path)["parts"]) if fileformat.sniff(path) == "wireskein" else 1
    if n == 1:
        return build(prepare(load(path)), hints)
    if part is not None and not 0 <= part < n:
        raise ValueError(f"part {part}: the file has {n} (0 .. {n - 1})")
    rows = []
    for cap in fileio.load_runs(path):        # parts that follow on without a gap decoded as one stretch
        span = cap.meta.get("parts") or [cap.meta.get("part", 0)] * 2
        if part is not None and not span[0] <= part <= span[1]:
            continue
        for r in build(prepare(cap), hints)["rows"]:
            rows += _per_part(r, cap.meta, part)
    return {"format": FORMAT, "source": f"wireskein {__version__} analyze", "rows": rows}


def _per_part(row: dict, meta: dict, only: int | None) -> list[dict]:
    """A row of a joined stretch as rows of the file's parts (§5.3: a row's ticks are its part's), each item in the
    part its start is in."""
    from ._engine.fileio import locate
    by: dict[int, list] = {}
    for it in row["items"]:
        k, s = locate(meta, it["s"])
        x = {**it, "s": s}
        if "e" in it:
            x["e"] = s + (it["e"] - it["s"])
        by.setdefault(k, []).append(x)
    if not by:
        by = {locate(meta, 0)[0]: []}
    return [{**row, "items": items, "part": k} for k, items in sorted(by.items()) if only is None or k == only]


def save(path: str | Path, doc: dict) -> None:
    from . import _fileformat as fileformat
    fileformat.put(path, ENTRY, json.dumps(doc, ensure_ascii=False, separators=(",", ":")))


def load(path: str | Path) -> dict | None:
    from . import _fileformat as fileformat
    data = fileformat.get(path, ENTRY)
    if data is None:
        return None
    doc = json.loads(data)
    if doc.get("format") != FORMAT:
        raise ValueError(f"{path}: annotations format {doc.get('format')!r}, this version reads {FORMAT!r}")
    n = len(fileformat.read_header(path)["parts"])
    bad = [r.get("part") for r in doc.get("rows", []) if not 0 <= r.get("part", 0) < n]
    if bad:
        raise ValueError(f"{path}: annotation rows of a part the file does not have (of {n}): {bad[:3]}")
    return doc
