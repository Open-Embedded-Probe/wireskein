"""Shards into the ledger, and results from elsewhere (a GitHub Actions run, another machine) into this one.

    uv run python bench/merge.py RUN_ID                 # the shards of RUN_ID in bench/results/runs -> the ledger
    uv run python bench/merge.py RUN_ID --lines OUT     # also its ledger lines alone in OUT (what CI hands back)
    uv run python bench/merge.py --import DIR           # a downloaded bench artifact: its items into results/runs,
                                                        # its ledger lines into the ledger (those already there left out)
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from wsbench import runner  # noqa: E402


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("run_id", nargs="?")
    ap.add_argument("--lines", default=None, help="also write the new ledger lines alone to this file")
    ap.add_argument("--import", dest="imp", default=None, help="a directory holding a run's items and ledger lines")
    args = ap.parse_args()
    runs = runner.RESULTS / "runs"
    if args.imp:
        src = Path(args.imp)
        runs.mkdir(parents=True, exist_ok=True)
        copied = 0
        for p in sorted(src.rglob("*")):
            if p.is_file() and p.name.endswith((".jsonl", ".meta.json")) and not p.name.endswith(".ledger.jsonl"):
                shutil.copy2(p, runs / p.name)
                copied += 1
        lines = [ln for p in sorted(src.rglob("*.ledger.jsonl")) for ln in p.read_text().splitlines() if ln.strip()]
        n = runner.append(lines)
        print(f"{copied} item files into {runs}; {n} of {len(lines)} ledger lines added to {runner.LEDGER}")
        return
    if not args.run_id:
        ap.error("RUN_ID or --import DIR")
    files = sorted(runs.glob(f"{args.run_id}.shard-*.jsonl"))
    if not files:
        sys.exit(f"no shards of {args.run_id} in {runs}")
    lines = runner.aggregate(args.run_id, files)
    n = runner.append(lines)
    if args.lines:
        Path(args.lines).write_text("".join(ln + "\n" for ln in lines))
    print(f"{len(files)} shards, {n} of {len(lines)} results added to {runner.LEDGER}")


if __name__ == "__main__":
    main()
