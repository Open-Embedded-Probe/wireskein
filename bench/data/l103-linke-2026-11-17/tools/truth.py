"""Truth for a run, taken outside WireSkein, and how the 160 MS/s reading agrees with it.

    uv run --project <wireskein> python tools/truth.py <run dir> ...     (writes <run>/truth.json, prints a line each)

ch32rv.ndjson holds every USB round trip with the LinkE. A DmiOp (out 81 08 06 <addr> <data:4> <op>, in
82 08 06 <addr> <data:4> <status>) is one DMI frame on the wire (wch-protocols' 2026-09-25 fixture README), so the
DmiOps in order are a subsequence of the wire's DMI frames: a write's address and value, a read's address and the
value the LinkE gave back. What the LinkE does by itself (attach, its memory commands) is not on USB; read.bin (what
ch32rv read) and the files written stay as truth for those.

truth.json: {"dmi_ops": [...], "read_bytes": hex, "agreement_160M": {...}}. reading.json: WireSkein's reading of the
capture (its version, fit, noise, clock speeds, and "dmi": [[op, "0xNN", data], ...], answered frames, a BURST as
its reads, as eval counts them). Not truth; at 160 MS/s, where it agrees with ch32rv (every DmiOp found in it in
order: agreement_160M.usable), its dmi is the truth of the same run decimated (data-requests §1), never of the
160 MS/s capture itself; a capture at another rate (rate/) is scored by the DmiOps and read.bin only."""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

SPEC = json.dumps({"buses": [{"protocol": "rvswd", "pins": {"clk": "SWCLK", "dio": "SWDIO"}}]})


def dmi_ops(ndjson: Path) -> list[dict]:
    rows = [json.loads(x) for x in ndjson.read_text().splitlines() if x.strip()]
    rows = [r for r in rows if "seq" in r and r.get("chan") == "cmd"]
    out, pending = [], None
    for r in rows:
        b = bytes.fromhex(r.get("data") or "")
        if r["dir"] == "out" and b[:3] == b"\x81\x08\x06" and len(b) == 9:
            pending = {"addr": b[3], "data": int.from_bytes(b[4:8], "big"), "op": b[8], "t_us": r["t_us"]}
        elif r["dir"] == "in" and b[:3] == b"\x82\x08\x06" and len(b) == 9 and pending is not None:
            op = {1: "R", 2: "W"}.get(pending["op"], f"op{pending['op']}")
            out.append({"op": op, "addr": pending["addr"],
                        "data": pending["data"] if op == "W" else int.from_bytes(b[4:8], "big"),
                        "status": b[8], "t_us": pending["t_us"]})
            pending = None
    return out


def wire_dmi(cap: Path) -> tuple[list[dict], dict]:
    """The wire's DMI frames as WireSkein reads them, and its reading's summary (noise, frames, clock speeds)."""
    p = subprocess.run(["wireskein", "decode", str(cap), "--spec", SPEC], capture_output=True, text=True, check=True)
    doc = json.loads(p.stdout)
    rows, reading = [], {"wireskein": doc.get("wireskein"), "parts": []}
    for part in doc.get("parts", [doc]):
        for b in part.get("buses", []):
            rows += b.get("dmi") or (b.get("result") or {}).get("dmi") or []
            reading["parts"].append({"fit": b.get("fit"), "reading": b.get("reading"),
                                     "clock_speeds": b.get("clock_speeds")})
    return rows, reading


def eval_rows(wire: list[dict]) -> list:
    """The wire's DMI as eval/evaluate.py counts a decoding: answered frames, a BURST as each of its reads."""
    rows = []
    for x in wire:
        if not x.get("answered", True):
            continue
        if x["op"] == "BURST" and isinstance(x["data"], list):
            rows += [["R", f"0x{x['addr']:02x}", v] for v in x["data"]]
        else:
            rows.append([x["op"], f"0x{x['addr']:02x}", x["data"]])
    return rows


def _norm_op(op) -> str:
    return {"read": "R", "write": "W", "r": "R", "w": "W"}.get(str(op).lower(), str(op).upper()[:1])


def match(ops: list[dict], wire: list[dict]) -> int:
    """How many DmiOps are found in order among the wire's frames (greedy)."""
    k = 0
    for o in ops:
        while k < len(wire):
            w = wire[k]; k += 1
            if _norm_op(w.get("op")) == o["op"] and w.get("addr") == o["addr"] and w.get("data") == o["data"]:
                o["_found"] = True
                break
        else:
            break
    return sum(1 for o in ops if o.pop("_found", False))


def main() -> None:
    for d in map(Path, sys.argv[1:]):
        ops = dmi_ops(d / "ch32rv.ndjson")
        truth = {"source": "ch32rv.ndjson (DmiOp), read.bin", "dmi_ops": ops}
        if (d / "read.bin").exists():
            truth["read_bytes"] = (d / "read.bin").read_bytes().hex()
        from wireskein import _fileformat
        num, den = _fileformat.read_header(d / "c0001.wireskein")["tick_hz"]
        at_160M = num == 160_000_000 * den
        wire, reading = wire_dmi(d / "c0001.wireskein")
        reading["tick_hz"] = [num, den]
        reading["dmi"] = eval_rows(wire)
        (d / "reading.json").write_text(json.dumps(reading, indent=1))   # WireSkein's reading: not truth
        n = match([dict(o) for o in ops], wire)
        truth["agreement_160M"] = {"dmi_ops": len(ops), "found_in_order": n, "wire_frames": len(wire),
                                   "wire_ok": sum(1 for w in wire if w.get("ok", True)),
                                   "wire_answered": sum(1 for w in wire if w.get("answered", True)),
                                   "usable": at_160M and n == len(ops) and len(wire) > 0}
        (d / "truth.json").write_text(json.dumps(truth, indent=1))
        print(f"{d}: DmiOps {n}/{len(ops)} found in order among {len(wire)} wire frames "
              f"({truth['agreement_160M']['wire_ok']} ok)", flush=True)


if __name__ == "__main__":
    main()
