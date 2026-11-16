"""How algorithms could be chosen between, from what a run already measured (its item rows; nothing is run again).

    uv run python bench/select.py --stage bit_unit --suite noise
    uv run python bench/select.py --stage e2e --suite noise --by noise
    uv run python bench/select.py --stage clock_period --suite rate --by rate

Strategies, each scored on the same items:
  fixed <algo>     one algorithm for everything
  oracle           the best algorithm per item, knowing the truth: the most any switching can reach
  confidence       per item, the algorithm that says it is most sure ("confidence" in its output)
  dispatch:<key>   per group, the algorithm best on that group; the groups' choices made on half of the cases and
                   scored on the other half (and the other way round), so it is not credited for fitting the very
                   cases it is scored on. Groups a switch could go by at run time (what the capture shows of
                   itself, wsbench/observe.py): rate, obs_noise (share of runs under a fifth of their line's unit), obs_os (fewest
                   samples per unit among the lines), obs (both). Marked (truth): protocol, label, dataset, noise -
                   known to the bench, not to a switch; an upper bound only

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

KEYS = ("rate", "obs_noise", "obs_os", "obs", "protocol", "label", "dataset", "noise")
TRUTH_KEYS = {"protocol", "label", "dataset", "noise"}


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
    from wsbench.observe import bucket_noise, bucket_os
    ob = out.get("obs") or {}
    bn, bo = bucket_noise(ob.get("short_rel", ob.get("short_share"))), bucket_os(ob.get("samples_bit"))
    return {"protocol": row.get("protocol") or "-", "label": row.get("label") or row.get("truth") or "-",
            "dataset": ds, "noise": noise, "rate": f"{out.get('rate', 0) / 1e6:g}M",
            "obs_noise": bn, "obs_os": bo, "obs": f"{bn} {bo}"}


def load(stage: str, suite: str, run: str | None):
    recs = runner.read_ledger()
    recs = [r for r in recs if r["stage"] == stage and r["suite"] == suite and (not run or r["run"].startswith(run))]
    if not recs:
        sys.exit(f"no results of {stage} on suite {suite} in {runner.LEDGER}")
    run_id = recs[-1]["run"]
    dsets = {r["dataset"] for r in recs if r["run"] == run_id}       # this suite's datasets (a run may hold several)
    paths = [p for p in sorted((runner.RESULTS / "runs").glob(f"{run_id}*.jsonl")) if not p.name.endswith(".ledger.jsonl")]
    if not paths:
        sys.exit(f"the items of run {run_id} are not in {runner.RESULTS / 'runs'} (merge.py --import them)")
    items = defaultdict(dict)       # (case, item) -> {algo: row}
    meta = {}
    for ln in (x for p in paths for x in p.read_text().splitlines()):
        out = json.loads(ln)
        if out.get("dataset") not in dsets:
            continue
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


def penalty(stage: str, picks: dict, items: dict) -> float:
    """False claims the picks make (e2e: false confirmed + a tenth of false likely)."""
    return sum(item_value(stage, items[k][a])[2] for k, a in picks.items())


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
    picks_of = {}

    def add(name, picks):
        picks_of[name] = picks
        res.append((name, score(st, picks, items)))
    for a in algos:
        add(f"fixed {a}", {k: a for k in items})
    best_fixed = max(v for _, v in res)

    def oracle_pick(k):
        return max(algos, key=lambda a: (item_value(st, items[k][a])[0], -item_value(st, items[k][a])[2]))
    add("oracle", {k: oracle_pick(k) for k in items})
    if all(items[k][a].get("confidence") is not None for k in items for a in algos):
        add("confidence", {k: max(algos, key=lambda a: items[k][a]["confidence"]) for k in items})
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
        add(f"dispatch:{key}" + (" (truth)" if key in TRUTH_KEYS else ""), picks)
    w = max(len(n) for n, _ in res) + 2
    pen = st == "e2e"
    print(f"\n{'strategy':<{w}}  score   vs best fixed" + ("   false (confirmed + likely/10)" if pen else ""))
    for n, v in res:
        print(f"{n:<{w}}  {v:.3f}  {v - best_fixed:+.3f}" + (f"   {penalty(st, picks_of[n], items):8.1f}" if pen else ""))
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
