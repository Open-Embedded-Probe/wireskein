"""Which algorithms to leave out of a run, from a cost run before it (the scale suite, with a time limit): the
ledger's lines of that run, read with what cost.summarize already gives.

    uv run python bench/run.py --stage all --suite scale --timeout 120     # adds a run to the ledger
    uv run python bench/gate.py --out skip.json                                 # its last run
    uv run python bench/run.py --stage all --suite all --skip-from skip.json

Left out (`stage/algo`), never a stage's main algorithm:
- one that timed out on any case (the line's `timeouts`);
- one whose CPU time is `--ratio` times the main algorithm's on the same cases or more (`cost.vs_main`);
- one that grows with the data `--growth` faster than the main algorithm (`cost.scaling` against the main's: 2 is the
  square of the data; the cases spread over sizes for the slope to be measured).
skip.json: {"run", "skip": [...], "why": {...}}; the reasons also go to $GITHUB_STEP_SUMMARY when set."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from wsbench import runner  # noqa: E402


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--run", default=None, help="the cost run (default: the ledger's last)")
    ap.add_argument("--ratio", type=float, default=5.0)
    ap.add_argument("--growth", type=float, default=0.5)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    lines = [json.loads(ln) for ln in runner.LEDGER.read_text().splitlines() if ln.strip()]
    run = args.run or lines[-1]["run"]
    mine = [r for r in lines if r["run"] == run]
    main_scaling = {(r["stage"], r["dataset"]): (r.get("cost") or {}).get("scaling")
                    for r in mine if r["status"] == "main"}
    why: dict[str, str] = {}
    for r in mine:
        if r["status"] == "main":
            continue
        key, c = f"{r['stage']}/{r['algo']}", r.get("cost") or {}
        if r.get("timeouts"):
            why.setdefault(key, f"timed out on {r['timeouts']} of {r['cases']} cases of {r['dataset']}")
        elif (c.get("vs_main") or 0) >= args.ratio:
            why.setdefault(key, f"{c['vs_main']:.1f} times the main's CPU time on {r['dataset']}")
        else:
            base = main_scaling.get((r["stage"], r["dataset"]))
            if c.get("scaling") is not None and base is not None and c["scaling"] - base >= args.growth:
                why.setdefault(key, f"grows as data^{c['scaling']:.2f} against the main's ^{base:.2f} on {r['dataset']}")
    doc = {"run": run, "skip": sorted(why), "why": why}
    if args.out:
        Path(args.out).write_text(json.dumps(doc, indent=1))
    text = f"### gate ({run}): {len(why)} left out\n\n" + "".join(f"- {k}: {v}\n" for k, v in sorted(why.items()))
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as fh:
            fh.write(text + "\n")
    print(text)


if __name__ == "__main__":
    main()
