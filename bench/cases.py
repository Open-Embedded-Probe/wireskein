"""A run's items (bench/results/runs/<run>*.jsonl, an Actions run's taken in by merge.py --import) case by case: the
slow ones, what changed against another run, and one case run again here to measure or profile it.

    uv run python bench/cases.py slowest [--run R] [--stage e2e] [--algo wireskein] [-n 15]
    uv run python bench/cases.py diff --run R --base R0             # lines whose scores or times moved
    uv run python bench/cases.py replay CASE [--run R] [--stage e2e] [--algo wireskein] [--profile]

Choosing among many algorithms is the Actions run's (bench/run.py on every suite); making one algorithm faster or
fixing it is done here, on the cases the run says are slow or wrong: `slowest` names them, `replay` runs one of them
with the bench's own case (the same transforms), the algorithm's seconds and its output's summary, and with
--profile where the time goes. Heavy cases take GBs: run it with a memory limit (systemd-run --user --scope -p
MemoryMax=4G ...). The default run is the latest one with items here."""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "eval"))

from wsbench import data, registry, runner  # noqa: E402
from wsbench.stages import STAGES  # noqa: E402

RUNS = runner.RESULTS / "runs"


def _run_ids() -> list[str]:
    ids = {p.name.split(".shard-")[0].removesuffix(".jsonl") for p in RUNS.glob("*.jsonl")}
    return sorted(ids)


def items(run: str | None) -> tuple[str, list[dict]]:
    ids = _run_ids()
    if not ids:
        sys.exit(f"no items in {RUNS} (an Actions run's: merge.py --import <its artifact>)")
    rid = next((i for i in reversed(ids) if i.startswith(run)), None) if run else ids[-1]
    if rid is None:
        sys.exit(f"no run {run!r} here (have: {', '.join(ids[-5:])})")
    out = []
    for p in sorted(RUNS.glob(f"{rid}*.jsonl")):
        out += [json.loads(ln) for ln in p.read_text().splitlines() if ln.strip()]
    return rid, [d for d in out if "results" in d]


def slowest(args) -> None:
    rid, rows = items(args.run)
    got = []
    for d in rows:
        for r in d["results"]:
            if (args.stage and r["stage"] != args.stage) or (args.algo and r["algo"] != args.algo):
                continue
            got.append((r.get("seconds") or 0, r["stage"], r["algo"], d["case"], d.get("edges"), r.get("error", "")))
    print(f"run {rid}: the slowest of {len(got)}")
    for s, st, al, case, edges, err in sorted(got, reverse=True)[:args.n]:
        print(f"  {s:8.1f} s  {st}/{al:28s} {case}  ({edges} changes){'  ' + err if err else ''}")


def diff(args) -> None:
    lines = [json.loads(ln) for ln in runner.LEDGER.read_text().splitlines() if ln.strip()]
    def of(prefix):
        rid = next((r["run"] for r in reversed(lines) if r["run"].startswith(prefix)), None)
        if rid is None:
            sys.exit(f"no run {prefix!r} in the ledger")
        return rid, {(r["stage"], r["algo"], r["dataset"]): r for r in lines if r["run"] == rid}
    (ra, a), (rb, b) = of(args.base), of(args.run)
    print(f"{rb} against {ra}")
    moved = slower = 0
    for k in sorted(set(a) & set(b)):
        sa, sb = a[k].get("summary") or {}, b[k].get("summary") or {}
        ch = [(m, sa[m], sb[m]) for m in sorted(set(sa) & set(sb))
              if isinstance(sa[m], (int, float)) and isinstance(sb[m], (int, float)) and abs(sa[m] - sb[m]) > args.tol]
        if ch:
            moved += 1
            print(f"  score {k[0]}/{k[1]} {k[2]}: " + ", ".join(f"{m} {x:g} -> {y:g}" for m, x, y in ch[:4]))
        ta, tb = a[k].get("seconds_median"), b[k].get("seconds_median")
        if ta and tb and (tb > 2 * ta or tb < ta / 2) and max(ta, tb) > 0.5:
            slower += 1
            print(f"  time  {k[0]}/{k[1]} {k[2]}: median {ta:g} -> {tb:g} s")
    gone, new = sorted(set(a) - set(b)), sorted(set(b) - set(a))
    print(f"{moved} lines' scores moved, {slower} times halved or doubled; {len(gone)} lines not run again, "
          f"{len(new)} new")


def _case(rid: str, rows: list[dict], case_id: str):
    d = next((d for d in rows if d["case"] == case_id), None)
    if d is None:
        sys.exit(f"no case {case_id!r} in run {rid}")
    if d.get("from"):
        ref, src, chain = d["from"]
        kind, sep, rest = ref.partition(":")
        ref = f"{kind}:{data.REPO / rest}" if sep and kind in ("linke", "ebdd", "wchfix", "i2cbench") \
            else (ref if ref.startswith("gen:") else str(data.REPO / ref))
        return d, data.load_case(ref, src, chain)
    # items from before a case named where it came from: look for it in its dataset
    src, _, chain = d["dataset"].partition("+")
    for ref in data.source_items(src):
        c = data.load_case(ref, src, chain)
        if c.id == case_id:
            return d, c
    sys.exit(f"{case_id}: not found again in {d['dataset']}")


def replay(args) -> None:
    rid, rows = items(args.run)
    d, case = _case(rid, rows, args.case)
    registry.load([])
    st = STAGES[args.stage]
    algo = registry.get(args.stage, args.algo)
    was = next((r for r in d["results"] if r["stage"] == args.stage and r["algo"] == args.algo), {})
    print(f"{case.id}: {sum(len(c.edges) for c in case.cap.channels)} changes at {case.cap.rate / 1e6:g} MS/s; "
          f"the run: {was.get('seconds', 0):.1f} s{' ' + was['error'] if was.get('error') else ''}")
    prof = None
    if args.profile:
        import cProfile
        prof = cProfile.Profile()
        prof.enable()
    t = time.perf_counter()
    out = st.run(algo, case)
    s = time.perf_counter() - t
    if prof:
        prof.disable()
    rows_now = st.score(out, case)
    print(f"here: {s:.1f} s")
    print("scored:", json.dumps(rows_now, default=str)[:600])
    if prof:
        import pstats
        pstats.Stats(prof).sort_stats("cumulative").print_stats(r"src/wireskein|bench/", args.top)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("slowest")
    p.add_argument("--run")
    p.add_argument("--stage")
    p.add_argument("--algo")
    p.add_argument("-n", type=int, default=15)
    p = sub.add_parser("diff")
    p.add_argument("--run", required=True)
    p.add_argument("--base", required=True)
    p.add_argument("--tol", type=float, default=1e-6)
    p = sub.add_parser("replay")
    p.add_argument("case")
    p.add_argument("--run")
    p.add_argument("--stage", default="e2e")
    p.add_argument("--algo", default="wireskein")
    p.add_argument("--profile", action="store_true")
    p.add_argument("--top", type=int, default=30)
    args = ap.parse_args()
    {"slowest": slowest, "diff": diff, "replay": replay}[args.cmd](args)


if __name__ == "__main__":
    main()
