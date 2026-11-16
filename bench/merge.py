"""Shards into the ledger, and results from elsewhere (another machine, a GitHub Actions artifact) into this one.

    uv run python bench/merge.py RUN_ID                 # the shards of RUN_ID in bench/results/runs -> the ledger
    uv run python bench/merge.py RUN_ID --out DIR       # also the run's record and ledger lines alone, in DIR
                                                        # (<run>.run.json, <run>.ledger.jsonl: what CI commits)
    uv run python bench/merge.py --import DIR           # a run's record and ledger lines from DIR into the ledger
                                                        # (those already there left out), its items into results/runs
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from wsbench import runner  # noqa: E402


def take(src: Path) -> str:
    runs = runner.RESULTS / "runs"
    runs.mkdir(parents=True, exist_ok=True)
    copied = 0
    for p in sorted(src.rglob("*")):
        if p.is_file() and p.name.endswith((".jsonl", ".meta.json")) and not p.name.endswith(".ledger.jsonl"):
            if p.resolve() != (runs / p.name).resolve():
                shutil.copy2(p, runs / p.name)
                copied += 1
    added = total = 0
    for rp in sorted(src.rglob("*.run.json")):
        run_rec = json.loads(rp.read_text())
        lp = rp.with_name(run_rec["run"] + ".ledger.jsonl")
        lines = [ln for ln in lp.read_text().splitlines() if ln.strip()] if lp.exists() else []
        added += runner.append(lines, run_rec)
        total += len(lines)
    return f"{copied} item files into {runs}; {added} of {total} ledger lines added to {runner.LEDGER}"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("run_id", nargs="?")
    ap.add_argument("--out", default=None, help="also write the run's record and ledger lines alone in this directory")
    ap.add_argument("--import", dest="imp", default=None, help="a directory holding runs' records and ledger lines")
    args = ap.parse_args()
    if args.imp:
        print(take(Path(args.imp)))
        return
    if not args.run_id:
        ap.error("RUN_ID or --import DIR")
    runs = runner.RESULTS / "runs"
    files = sorted(runs.glob(f"{args.run_id}.shard-*.jsonl"))
    if not files:
        sys.exit(f"no shards of {args.run_id} in {runs}")
    run_rec, lines = runner.aggregate(args.run_id, files)
    n = runner.append(lines, run_rec)
    if args.out:
        out = Path(args.out)
        out.mkdir(parents=True, exist_ok=True)
        (out / f"{args.run_id}.run.json").write_text(json.dumps(run_rec, default=runner._json) + "\n")
        (out / f"{args.run_id}.ledger.jsonl").write_text("".join(ln + "\n" for ln in lines))
    print(f"{len(files)} shards, {n} of {len(lines)} results added to {runner.LEDGER}")


if __name__ == "__main__":
    main()
