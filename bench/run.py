"""Measure algorithms: stages x algorithms x the datasets of suites; every result goes to bench/results/ledger.jsonl.

    uv run python bench/run.py --list
    uv run python bench/run.py --stage clock_period --suite noise --note "comb: candidates from the histogram"
    uv run python bench/run.py --stage all --suite smoke
    uv run python bench/run.py --stage e2e --algo wireskein --algo preclean --suite noise
    uv run python bench/run.py --stage all --suite noise --shard 0/8 --run-id ID   # one of 8 shards: merge.py ID
"""

from __future__ import annotations

import argparse
import multiprocessing as mp
import os
import sys
from pathlib import Path

# one thread for numpy's libraries in every worker: one call, one CPU (bench/wsbench/cost.py)
for _v in ("OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS"):
    os.environ.setdefault(_v, "1")

sys.path.insert(0, str(Path(__file__).resolve().parent))

from wsbench import registry, runner  # noqa: E402
from wsbench.stages import STAGES  # noqa: E402


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--stage", action="append", default=[], help=f"one of {', '.join(STAGES)}, or all")
    ap.add_argument("--suite", action="append", default=[], help="a suite of bench/suites.toml")
    ap.add_argument("--algo", action="append", default=[], help="by name (a variant's base name runs all variants)")
    ap.add_argument("--status", default="main,trial", help="when no --algo: which to run (main,trial,retired)")
    ap.add_argument("--algos-dir", action="append", default=[], help="more algorithms (also WSBENCH_ALGOS)")
    ap.add_argument("--note", default="", help="why this run: kept in the ledger")
    ap.add_argument("--label", default="", help="a short name for this run")
    ap.add_argument("-j", type=int, default=max(1, mp.cpu_count() - 2))
    ap.add_argument("--memory", action="store_true", help="also each call's peak allocation (a call more, traced)")
    ap.add_argument("--shard", default=None, help="I/N: every N-th case from the I-th; items only (merge.py adds them)")
    ap.add_argument("--run-id", default=None, help="the run's id (the same for every shard of it)")
    ap.add_argument("--list", action="store_true", help="stages, algorithms and suites")
    ap.add_argument("--dry-run", action="store_true", help="what would run")
    args = ap.parse_args()

    algos = registry.load(args.algos_dir)
    for w in registry.check(STAGES):
        print("warning:", w, file=sys.stderr)
    if args.list:
        for s, st in STAGES.items():
            print(f"{s}: {st.about}  [headline: {st.headline}]")
            for a in registry.pick(s, None, list(registry.STATUSES)):
                params = f" {a.params}" if a.params else ""
                print(f"  {a.status:7s} {a.name:28s}{params}  {a.about}" + (f"  (retired: {a.reason})" if a.reason else ""))
        print("suites:")
        for n, s in runner.suites().items():
            nds = len(s["sources"]) * len(s.get("transforms", [""]))
            print(f"  {n:16s} {s.get('role', 'tune'):8s} {nds:3d} datasets  {s.get('about', '')}")
        return
    stages = list(STAGES) if "all" in args.stage else args.stage
    if not stages or not args.suite:
        ap.error("--stage and --suite are needed (see --list)")
    for s in stages:
        if s not in STAGES:
            ap.error(f"no stage {s!r}")
    by_stage = {}
    for s in stages:
        names = [n for n in args.algo if any(a.stage == s and (a.name == n or a.name.startswith(n + "."))
                                               for a in algos.values())]
        if args.algo and not names:
            continue
        by_stage[s] = registry.pick(s, names or None, args.status.split(","))
    stages = [s for s in stages if by_stage.get(s)]
    if not stages:
        ap.error("no algorithm to run")
    dsets = runner.datasets_of(args.suite)
    if args.dry_run:
        for s in stages:
            print(s, [a.name for a in by_stage[s]])
        for d in dsets:
            print(" ", d)
        return
    shard = None
    if args.shard:
        i, n = (int(x) for x in args.shard.split("/"))
        if not 0 <= i < n:
            ap.error("--shard I/N with 0 <= I < N")
        if not args.run_id:
            ap.error("--shard needs --run-id (the same for every shard)")
        shard = (i, n)
    runner.run(stages, by_stage, dsets, args.j, args.note, args.algos_dir, args.label, args.memory, shard,
               args.run_id)


if __name__ == "__main__":
    main()
