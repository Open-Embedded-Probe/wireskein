"""Make the LinkE copies' readings again with this WireSkein: reading.json (the 160 MS/s reading, the truth of the same
run taken down where it agrees with ch32rv) and truth.json's agreement_160M. The truth itself (ch32rv's DmiOps and
read.bin, from outside WireSkein) is not touched. SOURCE.json's SHA-256 of the files rewritten follow.

    uv run python bench/data/refresh_linke.py l103-linke-2026-11-17 v003-linke-2026-11-17 ...

Run after a change to how RVSWD / SWIO are read, so that the bench scores a decimated run against the reading as it
reads now (a reading kept from before a fix would score the fix as wrong)."""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPECS = {frozenset({"SWCLK", "SWDIO"}): {"protocol": "rvswd", "pins": {"clk": "SWCLK", "dio": "SWDIO"}},
         frozenset({"SWIO"}): {"protocol": "swio", "pins": {"dio": "SWIO"}}}


def reading(cap: Path) -> tuple[list, dict]:
    from wireskein import _fileformat
    head = _fileformat.read_header(cap)
    names = frozenset(c["name"] for c in head["parts"][0]["channels"])
    spec = json.dumps({"buses": [SPECS[names]]})
    p = subprocess.run([sys.executable, "-m", "wireskein", "decode", str(cap), "--spec", spec],
                       capture_output=True, text=True, check=True)
    doc = json.loads(p.stdout)
    rows, out = [], {"wireskein": doc.get("wireskein"), "parts": [], "tick_hz": head["tick_hz"]}
    for part in doc.get("parts", [doc]):
        for b in part.get("buses", []):
            rows += b.get("dmi") or (b.get("result") or {}).get("dmi") or []
            out["parts"].append({"protocol": b.get("protocol"), "decoded": b.get("decoded"), "fit": b.get("fit"),
                                 "reading": b.get("reading"), "clock_speeds": b.get("clock_speeds")})
    rows.sort(key=lambda x: x.get("s", 0))
    dmi = []
    for x in rows:                       # as eval counts a decoding: answered frames, a BURST as each of its reads
        if not x.get("answered", True):
            continue
        if x["op"] == "BURST" and isinstance(x["data"], list):
            dmi += [["R", f"0x{x['addr']:02x}", v] for v in x["data"]]
        else:
            dmi.append([x["op"], f"0x{x['addr']:02x}", x["data"]])
    out["dmi"] = dmi
    return rows, out


def found(ops: list, rows: list) -> int:
    k = n = 0
    for o in ops:
        while k < len(rows):
            w = rows[k]
            k += 1
            if str(w.get("op")).upper()[:1] == o["op"] and w.get("addr") == o["addr"] and w.get("data") == o["data"]:
                n += 1
                break
        else:
            break
    return n


def main() -> None:
    for name in sys.argv[1:]:
        base = HERE / name
        src = json.loads((base / "SOURCE.json").read_text())
        for t in sorted((base / "runs").rglob("truth.json")):
            run = t.parent
            truth = json.loads(t.read_text())
            rows, rd = reading(run / "c0001.wireskein")
            num, den = rd["tick_hz"]
            n = found(truth["dmi_ops"], rows)
            truth["agreement_160M"] = {"dmi_ops": len(truth["dmi_ops"]), "found_in_order": n, "wire_frames": len(rows),
                                       "wire_ok": sum(1 for w in rows if w.get("ok", True)),
                                       "wire_answered": sum(1 for w in rows if w.get("answered", True)),
                                       "usable": num == 160_000_000 * den and n == len(truth["dmi_ops"]) and len(rows) > 0}
            t.write_text(json.dumps(truth, indent=1))
            (run / "reading.json").write_text(json.dumps(rd, indent=1))
            for f in ("truth.json", "reading.json"):
                rel = str((run / f).relative_to(base))
                if rel in src["files"]:
                    src["files"][rel]["sha256"] = hashlib.sha256((run / f).read_bytes()).hexdigest()
                    if f == "truth.json":
                        src["files"][rel]["remade"] = "agreement_160M with this WireSkein (bench/data/refresh_linke.py)"
            print(f"{name}/{run.relative_to(base)}: {n}/{len(truth['dmi_ops'])}", flush=True)
        (base / "SOURCE.json").write_text(json.dumps(src, indent=1) + "\n")


if __name__ == "__main__":
    main()
