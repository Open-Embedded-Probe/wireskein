"""Is a capture taken at a lower rate the same as the 160 MS/s one taken down to it? Per rate and operation of the
rate sweep, the real capture's reading against the reading of rate/160M/<op> resampled to that rate (bench's
resample: the probe's whole or fractional divider, samples on the 160 MHz grid).

    uv run --project <wireskein> python tools/rate_compare.py runs [--json out.json]

The two are different runs of the same ch32rv operation (the LinkE's polling may differ a little), so they are
compared on shares, not frame by frame: ch32rv's DMI operations found in order (ops, truth.json), read.bin's words
among the data0 reads (mem), frames answered / read, the clock's glitch share and SWDIO's ringing share."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from difflib import SequenceMatcher
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parents[2]))           # bench/: wsbench
import truth as T  # noqa: E402
from wsbench import data as D  # noqa: E402
from wireskein import capture as C  # noqa: E402
from wireskein.decode import save  # noqa: E402
from wireskein import _fileformat as F  # noqa: E402


def stats(tr: dict, rows: list, reading: dict) -> dict:
    got = [(op, int(a, 16), d) for op, a, d in rows]
    want = [(o["op"], o["addr"], o["data"]) for o in tr.get("dmi_ops", [])]
    k = n = 0
    for w in want:
        while k < len(got) and got[k] != w:
            k += 1
        if k == len(got):
            break
        n, k = n + 1, k + 1
    out = {"ops": round(n / len(want), 4) if want else None}
    if tr.get("read_bytes"):
        mem = bytes.fromhex(tr["read_bytes"])
        words = [int.from_bytes(mem[i:i + 4], "little") for i in range(0, len(mem) - 3, 4)]
        vals = [g[2] for g in got if g[0] == "R" and g[1] == 0x04]
        out["mem"] = round(sum(b.size for b in SequenceMatcher(None, words, vals, autojunk=False).get_matching_blocks())
                           / len(words), 4)
    r = reading or {}
    out.update(frames=r.get("frames", 0), answered=r.get("ok", 0), glitch=r.get("glitch_share") or 0,
               ringing=(r.get("ringing_share") or {}).get("SWDIO") or 0)
    return out


def main() -> None:
    root = Path(sys.argv[1]) / "rate"
    rows = []
    for d in sorted(root.glob("*/*/truth.json")):
        run = d.parent
        op, tag = run.name, run.parent.name
        if tag == "160M":
            continue
        base = root / "160M" / op
        tr = json.loads(d.read_text())
        rd = json.loads((run / "reading.json").read_text())
        num, den = rd["tick_hz"]
        real = stats(tr, rd["dmi"], (rd["parts"] or [{}])[0].get("reading"))
        cap = C.read(base / "c0001.wireskein").parts[0].view()
        syn = D.resample(cap, num / den)
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "s.wireskein"
            save(p, syn)
            wire, reading = T.wire_dmi(p)
        btr = json.loads((base / "truth.json").read_text())
        made = stats(btr, T.eval_rows(wire), (reading["parts"] or [{}])[0].get("reading"))
        whole = (160_000_000 * den) % num == 0
        rows.append({"rate": num / den, "whole": whole, "op": op, "real": real, "from_160M": made})
        print(f"{num / den / 1e6:9.4g} MHz {'whole' if whole else 'frac '} {op:18} real {real}  from160 {made}", flush=True)
    if "--json" in sys.argv:
        Path(sys.argv[sys.argv.index("--json") + 1]).write_text(json.dumps(rows, indent=1))


if __name__ == "__main__":
    main()
