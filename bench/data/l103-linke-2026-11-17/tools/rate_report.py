"""The rate sweep (runs/rate/<MHz>/<op>) as a table: per rate, how noisy the lines look and how much is read.

    python3 tools/rate_report.py runs [--json out.json]

From truth.json (ch32rv's DMI operations and read.bin: truth) and reading.json (WireSkein's reading of that capture:
what is measured). Columns: the rate actually taken, whole or fractional divider of 160 MHz, then per operation the
share of ch32rv's DMI operations found in order (ops), the share of read.bin's words among the data0 reads (mem),
frames read / answered, the clock's glitch share and SWDIO's ringing share."""
from __future__ import annotations

import json
import sys
from difflib import SequenceMatcher
from pathlib import Path


def got_rows(reading_dmi: list) -> list:
    return [tuple(r) for r in reading_dmi]


def score(run: Path) -> dict:
    t = json.loads((run / "truth.json").read_text())
    rd = json.loads((run / "reading.json").read_text())
    num, den = rd["tick_hz"]
    got = [(op, int(a, 16), d if not isinstance(d, list) else tuple(d)) for op, a, d in rd.get("dmi", [])]
    want = [(o["op"], o["addr"], o["data"]) for o in t.get("dmi_ops", [])]
    k = n = 0
    for w in want:
        while k < len(got) and got[k] != w:
            k += 1
        if k == len(got):
            break
        n, k = n + 1, k + 1
    out = {"rate": num / den, "whole": (160_000_000 * den) % num == 0, "ops": n / len(want) if want else None}
    if t.get("read_bytes"):
        mem = bytes.fromhex(t["read_bytes"])
        words = [int.from_bytes(mem[i:i + 4], "little") for i in range(0, len(mem) - 3, 4)]
        vals = [g[2] for g in got if g[0] == "R" and g[1] == 0x04 and isinstance(g[2], int)]
        out["mem"] = sum(b.size for b in SequenceMatcher(None, words, vals, autojunk=False).get_matching_blocks()) / len(words)
    p = (rd["parts"] or [{}])[0]
    r = p.get("reading") or {}
    out.update(frames=r.get("frames", 0), answered=r.get("ok", 0), glitch=r.get("glitch_share"),
               ringing=(r.get("ringing_share") or {}).get("SWDIO"), verdict=(p.get("fit") or {}).get("verdict"))
    return out


def main() -> None:
    root = Path(sys.argv[1]) / "rate"
    rows = {}
    for t in sorted(root.rglob("truth.json")):
        run = t.parent
        rows.setdefault(run.parent.name, {})[run.name] = score(run)
    ops = sorted({o for v in rows.values() for o in v})
    order = sorted(rows, key=lambda r: -next(iter(rows[r].values()))["rate"])
    print("| rate | divider | " + " | ".join(ops) + " |")
    print("|---|---|" + "---|" * len(ops))
    for r in order:
        first = next(iter(rows[r].values()))
        cells = []
        for o in ops:
            s = rows[r].get(o)
            if not s:
                cells.append("")
                continue
            c = f"ops {s['ops']:.2f}" if s["ops"] is not None else "ops -"
            if "mem" in s:
                c += f", mem {s['mem']:.2f}"
            c += f", {s['answered']}/{s['frames']} fr, gl {s['glitch'] or 0:.3f}, ring {s['ringing'] or 0:.3f}"
            cells.append(c)
        print(f"| {first['rate'] / 1e6:.4g} MHz | {'whole' if first['whole'] else 'frac'} | " + " | ".join(cells) + " |")
    if "--json" in sys.argv:
        Path(sys.argv[sys.argv.index("--json") + 1]).write_text(json.dumps(rows, indent=1))


if __name__ == "__main__":
    main()
