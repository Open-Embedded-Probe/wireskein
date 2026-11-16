"""How sure "confirmed" is, and where its thresholds would have to be, from runs' item rows (nothing is run again).

    uv run python bench/calibrate.py --run RUN [--run RUN2 ...] [--algo NAME] [--suite S ...] [--target 0.99]

Each claim carries its total, its margin and whether it was right (e2e rows' claim_list). For thresholds (total >= t,
margin >= m) it gives the precision of what would be confirmed and how many right claims that keeps; the loosest
pair reaching the target precision, then that pair on each suite (a check suite shows whether it holds where it was
not chosen). The engine's own: total 0.85, margin 0.15 (staged.verdict).

Only claims that are not "unknown-sync" count. A claim's correctness is eval's: right protocol, lines and main
parameters.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from wsbench import runner  # noqa: E402

TOTALS = [0.6, 0.7, 0.75, 0.8, 0.85, 0.9, 0.95]
MARGINS = [0.05, 0.1, 0.15, 0.2, 0.3, 0.4]


def claims(run_ids: list[str], algo: str, suites: list[str] | None):
    want = None
    if suites:
        want = {d for s in suites for d in runner.suite_datasets(s)}
    out = defaultdict(list)          # dataset -> [(protocol, total, margin, correct)]
    for rid in run_ids:
        for p in sorted((runner.RESULTS / "runs").glob(f"{rid}*.jsonl")):
            if p.name.endswith(".ledger.jsonl"):
                continue
            for ln in p.read_text().splitlines():
                o = json.loads(ln)
                if want is not None and o.get("dataset") not in want:
                    continue
                for r in o.get("results", []):
                    if r["stage"] != "e2e" or r["algo"] != algo or not r["rows"]:
                        continue
                    for pr, v, t, m, ok in r["rows"][0].get("claim_list", []):
                        if v != "unknown-sync":
                            out[o["dataset"]].append((pr, t, m, ok))
    return out


def table(cl: list, t: float, m: float) -> tuple[int, int, int]:
    """(right confirmed, wrong confirmed, right claims in all) at thresholds t, m."""
    right = sum(1 for _, tt, mm, ok in cl if ok and tt >= t and mm >= m)
    wrong = sum(1 for _, tt, mm, ok in cl if not ok and tt >= t and mm >= m)
    return right, wrong, sum(1 for c in cl if c[3])


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--run", action="append", required=True, help="run ids (prefixes)")
    ap.add_argument("--algo", default="wireskein")
    ap.add_argument("--suite", action="append", default=[], help="choose on these suites' datasets (default: all)")
    ap.add_argument("--check", action="append", default=[], help="then show the choice on these suites")
    ap.add_argument("--target", type=float, default=0.99, help="precision wanted of confirmed")
    args = ap.parse_args()
    by_ds = claims(args.run, args.algo, args.suite or None)
    cl = [c for v in by_ds.values() for c in v]
    if not cl:
        sys.exit("no claim_list in these runs' items (runs before claim_list was kept?)")
    print(f"{args.algo}: {len(cl)} claims ({sum(c[3] for c in cl)} right) over {len(by_ds)} datasets")
    print(f"\nprecision of confirmed / share of right claims confirmed, by total (rows) and margin (columns)")
    print("        " + "".join(f"  m>={m:<11}" for m in MARGINS))
    best = None
    for t in TOTALS:
        cells = []
        for m in MARGINS:
            r, w, n = table(cl, t, m)
            p = r / (r + w) if r + w else float("nan")
            cells.append(f"  {p:.4f}/{r / n if n else 0:.2f}  ")
            if r + w and p >= args.target and (best is None or r > best[2]):
                best = (t, m, r, w)
        print(f"t>={t:<5}" + "".join(cells))
    r, w, n = table(cl, 0.85, 0.15)
    print(f"\nthe engine's (0.85, 0.15): precision {r / (r + w) if r + w else float('nan'):.4f}, confirms {r} of {n} right")
    if best:
        t, m, r, w = best
        print(f"loosest reaching {args.target}: total >= {t}, margin >= {m}: precision {r / (r + w):.4f}, confirms {r} "
              f"of {n} right ({w} wrong)")
        for s in args.check:
            c2 = [c for v in claims(args.run, args.algo, [s]).values() for c in v]
            for name, (tt, mm) in (("engine", (0.85, 0.15)), ("chosen", (t, m))):
                r2, w2, n2 = table(c2, tt, mm)
                print(f"  check {s:14s} {name:6s}: precision {r2 / (r2 + w2) if r2 + w2 else float('nan'):.4f}, "
                      f"confirms {r2} of {n2} right, {w2} wrong")
    else:
        print(f"no thresholds here reach {args.target}")


if __name__ == "__main__":
    main()
