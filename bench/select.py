"""How algorithms could be chosen between, from what a run already measured (its item rows; nothing is run again).

    uv run python bench/select.py --stage bit_unit --suite noise
    uv run python bench/select.py --stage e2e --suite noise --by noise
    uv run python bench/select.py --stage clock_period --suite rate --by rate

Strategies, each scored on the same items:
  fixed <algo>     one algorithm for everything
  oracle           the best algorithm per item, knowing the truth: the most any switching can reach
  confidence       per item, the algorithm that says it is most sure ("confidence" in its output)
  dispatch:<key>   per group (protocol, line class, noise, rate ...), the algorithm best on that group; the groups'
                   choices made on half of the cases and scored on the other half (and the other way round), so it
                   is not credited for fitting the very cases it is scored on

If oracle is far above the best fixed algorithm and dispatch or confidence get close to it, switching is worth
building; if not, one algorithm is enough.
"""

from __future__ import annotations

import argparse
import json
import sys
import zlib
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from wsbench import runner  # noqa: E402

KEYS = ("protocol", "label", "dataset", "noise", "rate")


def item_value(stage: str, row: dict) -> tuple[float, float, float]:
    """(value, weight, penalty): value higher is better; penalty breaks ties for the oracle (false claims)."""
    if stage in ("clock_period", "bit_unit"):
        return float(row["hit3"]), 1.0, 0.0
    if stage == "lineclass":
        return float(row["correct"]), 1.0, 0.0
    if stage == "clean":
        d = 2 * row["tp_rel"] + row["fp_rel"] + row["fn_rel"]
        return (2 * row["tp_rel"] / d if d else 1.0), 1.0, 0.0
    if stage == "e2e":
        return float(row["ok"]), float(row["buses"]), float(row["false_confirmed"] + 0.1 * row["false_likely"])
    raise KeyError(stage)


def groups(row: dict, out: dict) -> dict:
    ds = out["dataset"]
    chain = ds.split("+", 1)[1] if "+" in ds else ""
    noise = "+".join(t for t in chain.split("+") if t and not t.startswith(("decimate", "crop"))) or "none"
    return {"protocol": row.get("protocol") or "-", "label": row.get("label") or row.get("truth") or "-",
            "dataset": ds, "noise": noise, "rate": f"{out.get('rate', 0) / 1e6:g}M"}


def load(stage: str, suite: str, run: str | None):
    recs = [json.loads(ln) for ln in runner.LEDGER.read_text().splitlines() if ln.strip()]
    recs = [r for r in recs if r["stage"] == stage and r["suite"] == suite and (not run or r["run"].startswith(run))]
    if not recs:
        sys.exit(f"no results of {stage} on suite {suite} in {runner.LEDGER}")
    run_id = recs[-1]["run"]
    paths = [p for p in sorted((runner.RESULTS / "runs").glob(f"{run_id}*.jsonl")) if not p.name.endswith(".ledger.jsonl")]
    if not paths:
        sys.exit(f"the items of run {run_id} are not in {runner.RESULTS / 'runs'} (merge.py --import them)")
    items = defaultdict(dict)       # (case, item) -> {algo: row}
    meta = {}
    for ln in (x for p in paths for x in p.read_text().splitlines()):
        out = json.loads(ln)
        for r in out.get("results", []):
            if r["stage"] != stage:
                continue
            for row in r["rows"]:
                k = (out["case"], row["item"])
                items[k][r["algo"]] = row
                meta[k] = groups(row, out)
    algos = sorted({a for v in items.values() for a in v})
    full = {k: v for k, v in items.items() if len(v) == len(algos)}
    return run_id, algos, full, meta


def score(stage: str, picks: dict, items: dict) -> float:
    """picks: item -> algo."""
    num = den = 0.0
    for k, a in picks.items():
        v, w, _ = item_value(stage, items[k][a])
        num, den = num + v, den + w
    return num / den if den else float("nan")


def fold(case: str) -> int:
    return zlib.crc32(case.encode()) & 1


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--stage", required=True)
    ap.add_argument("--suite", required=True)
    ap.add_argument("--run", default=None, help="a run id (prefix); default the latest with this stage and suite")
    ap.add_argument("--by", default=None, help=f"also a table per group: one of {', '.join(KEYS)}")
    args = ap.parse_args()
    run_id, algos, items, meta = load(args.stage, args.suite, args.run)
    st = args.stage
    print(f"{st} on {args.suite}, run {run_id}: {len(items)} items, {len(algos)} algorithms")
    res = []
    for a in algos:
        res.append((f"fixed {a}", score(st, {k: a for k in items}, items)))
    best_fixed = max(v for _, v in res)

    def oracle_pick(k):
        return max(algos, key=lambda a: (item_value(st, items[k][a])[0], -item_value(st, items[k][a])[2]))
    res.append(("oracle", score(st, {k: oracle_pick(k) for k in items}, items)))
    if all(items[k][a].get("confidence") is not None for k in items for a in algos):
        res.append(("confidence", score(st, {k: max(algos, key=lambda a: items[k][a]["confidence"]) for k in items},
                                         items)))
    for key in KEYS:
        picks = {}
        for f in (0, 1):
            train = [k for k in items if fold(k[0]) != f]
            choice = {}
            for g in {meta[k][key] for k in train}:
                ks = [k for k in train if meta[k][key] == g]
                choice[g] = max(algos, key=lambda a: score(st, {k: a for k in ks}, items))
            overall = max(algos, key=lambda a: score(st, {k: a for k in train}, items))
            for k in items:
                if fold(k[0]) == f:
                    picks[k] = choice.get(meta[k][key], overall)
        res.append((f"dispatch:{key}", score(st, picks, items)))
    w = max(len(n) for n, _ in res) + 2
    print(f"\n{'strategy':<{w}}  score   vs best fixed")
    for n, v in res:
        print(f"{n:<{w}}  {v:.3f}  {v - best_fixed:+.3f}")
    if args.by:
        print(f"\nby {args.by} (* the best in its row):")
        gs = sorted({meta[k][args.by] for k in items})
        wg = max([len(g) for g in gs] + [8]) + 2
        print(f"{'':<{wg}}{'n':>6s}  " + "  ".join(f"{a[:14]:>14s}" for a in algos))
        for g in gs:
            ks = [k for k in items if meta[k][args.by] == g]
            vals = [score(st, {k: a for k in ks}, items) for a in algos]
            top = max(vals)
            print(f"{g:<{wg}}{len(ks):>6d}  " + "  ".join(
                f"{('*' if v == top and vals.count(top) < len(vals) else ' ') + f'{v:.3f}':>14s}" for v in vals))


if __name__ == "__main__":
    main()
