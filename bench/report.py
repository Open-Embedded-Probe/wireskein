"""Tables from bench/results/ledger.jsonl: algorithms (rows) x datasets (columns), the latest result of each.

    uv run python bench/report.py                          # every stage, its headline metric
    uv run python bench/report.py --stage clean --metric precision
    uv run python bench/report.py --stage e2e --delta      # each cell against the main algorithm
    uv run python bench/report.py --suite noise-holdout
    uv run python bench/report.py --history clock_period/comb     # every run of one algorithm
    uv run python bench/report.py --run 20261116T...       # one run only
    uv run python bench/report.py --cost                   # score against cost (bench/wsbench/cost.py)
    uv run python bench/report.py --metric cost.vs_main    # any cost.* key as a table

Marks: * main, . trial, x retired; ! the algorithm's source changed since (or it is gone): run it again;
P (with --cost) on the Pareto front: no other algorithm both scores higher and costs less.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from wsbench import registry, runner  # noqa: E402
from wsbench.stages import STAGES  # noqa: E402

MARK = {"main": "*", "trial": ".", "retired": "x"}


def read() -> list[dict]:
    return runner.read_ledger()


def fmt(v) -> str:
    if v is None:
        return "-"
    if isinstance(v, bool):
        return "y" if v else "n"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return f"{v:.3f}"
    return str(v)


def value(r: dict, metric: str):
    """A summary key, or cost.<key> from the run's cost."""
    if metric.startswith("cost."):
        return (r.get("cost") or {}).get(metric[5:])
    return r["summary"].get(metric)


def table(recs: list[dict], stage: str, metric: str, delta: bool, algos) -> None:
    st = STAGES.get(stage)
    lower = (st.LOWER if st else ()) + tuple(f"cost.{k}" for k in COST_LOWER)
    latest = {}
    for r in recs:                                   # ledger order is time order: the last one wins
        latest[(r["algo"], r["dataset"])] = r
    names = sorted({a for a, _ in latest}, key=lambda a: (registry.STATUSES.index(
        next(r["status"] for (x, _), r in latest.items() if x == a)), a))
    dsets = list(dict.fromkeys(r["dataset"] for r in recs))
    try:                                             # in the order the suite lists them (clean, weak, strong)
        order = [s + (f"+{c}" if c else "") for _, _, s, c in runner.datasets_of([recs[-1]["suite"]])]
        dsets.sort(key=lambda d: order.index(d) if d in order else len(order))
    except KeyError:
        pass
    sources = {latest[k]["source"] for k in latest}
    head = [(d.split("+", 1)[1] if "+" in d else "clean") if len(sources) == 1 else d for d in dsets]
    better = "lower" if metric in lower else "higher"
    print(f"\n== {stage}: {metric} ({better} is better)" + (" - against main" if delta else "")
          + (f"  [{next(iter(sources))}]" if len(sources) == 1 else ""))
    main = next((a for a in names if latest.get((a, dsets[0]), {}).get("status") == "main"), None)
    w0 = max([len(n) for n in names] + [10]) + 4
    widths = [max(7, len(h)) for h in head]
    print(" " * w0 + "  ".join(h.rjust(w) for h, w in zip(head, widths)) + "    mean")
    for a in names:
        vals, cells = [], []
        for d, w in zip(dsets, widths):
            r = latest.get((a, d))
            v = value(r, metric) if r else None
            if delta and main and a != main and v is not None:
                m = value(latest[(main, d)], metric) if (main, d) in latest else None
                v = None if m is None else v - m
                cells.append(("-" if v is None else f"{v:+.3f}").rjust(w))
            else:
                cells.append(fmt(v).rjust(w))
            if isinstance(v, (int, float)) and not isinstance(v, bool):
                vals.append(v)
        any_r = next(latest[(a, d)] for d in dsets if (a, d) in latest)
        cur = algos.get((stage, a))
        stale = "!" if cur is None or cur.src_sha != any_r["algo_sha"] else " "
        mean = f"{sum(vals) / len(vals):+.3f}" if delta and a != main and vals else (
            f"{sum(vals) / len(vals):.3f}" if vals and len(vals) == len(dsets) else "")
        print(f"{MARK.get(any_r['status'], '?')}{stale} {a:<{w0 - 3}}" + "  ".join(cells) + f"  {mean:>6s}")
    errs = sum(r["errors"] for r in latest.values())
    if errs:
        print(f"   ({errs} algorithm errors: see first_errors in the ledger)")


COST_LOWER = ("cpu_median_ms", "cpu_total_s", "wall_median_ms", "cost_ref", "us_per_kedge", "vs_main", "scaling")


def _med(xs):
    xs = sorted(x for x in xs if x is not None)
    return xs[len(xs) // 2] if xs else None


def cost_table(recs: list[dict], stage: str, metric: str) -> None:
    """Per algorithm: its score (mean over the datasets) beside what it costs (the median over the datasets)."""
    latest = {}
    for r in recs:
        latest[(r["algo"], r["dataset"])] = r
    dsets = list(dict.fromkeys(d for _, d in latest))
    rows = []
    for a in dict.fromkeys(x for x, _ in latest):
        rs = [latest[(a, d)] for d in dsets if (a, d) in latest]
        sc = [value(r, metric) for r in rs]
        sc = [x for x in sc if isinstance(x, (int, float))]
        c = [r.get("cost") or {} for r in rs]
        rows.append({"algo": a, "status": rs[0]["status"], "n": len(rs), "score": sum(sc) / len(sc) if sc else None,
                     "cpu": _med([x.get("cpu_median_ms") for x in c]), "ref": _med([x.get("cost_ref") for x in c]),
                     "kedge": _med([x.get("us_per_kedge") for x in c]), "vs": _med([x.get("vs_main") for x in c]),
                     "scaling": _med([x.get("scaling") for x in c]), "mem": _med([x.get("mem_peak_kb_median") for x in c]),
                     "minstr": _med([x.get("instr_median_M") for x in c]), "vs_i": _med([x.get("vs_main_instr") for x in c]),
                     "sc_i": _med([x.get("scaling_instr") for x in c])})
    instr = any(r["minstr"] is not None for r in rows)
    hi = metric not in (STAGES[stage].LOWER if stage in STAGES else ())
    def dominates(o, r):    # o scores at least as well and costs no more, and is strictly better in one of them
        if o is r or None in (o["score"], o["c"]):
            return False
        so, sr = (o["score"], r["score"]) if hi else (-o["score"], -r["score"])
        return so >= sr and o["c"] <= r["c"] and (so > sr or o["c"] < r["c"])

    for r in rows:          # Pareto: no algorithm dominates it (cost: instructions where counted, else reference units)
        r["c"] = r["minstr"] if instr else r["ref"]
    for r in rows:
        r["P"] = None not in (r["score"], r["c"]) and not any(dominates(o, r) for o in rows)
    rows.sort(key=lambda r: (registry.STATUSES.index(r["status"]), r["algo"]))
    print(f"\n== {stage}: {metric} (mean over {len(dsets)} datasets) against cost (median over them)")
    w = max([len(r["algo"]) for r in rows] + [10]) + 2
    print(" " * (w + 3) + f"{metric:>9s}  {'cpu ms':>8s}  {'ref':>7s}  {'us/kedge':>9s}  {'vs main':>8s}  {'scaling':>7s}  {'mem kB':>8s}"
          + (f"  {'M instr':>9s}  {'vs main':>8s}  {'scaling':>7s}" if instr else ""))
    for r in rows:
        f = lambda v, p=3: "-" if v is None else f"{v:.{p}f}"  # noqa: E731
        print(f"{MARK[r['status']]}{'P' if r['P'] else ' '} {r['algo']:<{w}}{f(r['score']):>9s}  {f(r['cpu'], 2):>8s}  "
              f"{f(r['ref'], 2):>7s}  {f(r['kedge'], 1):>9s}  {('x' + f(r['vs'], 2)) if r['vs'] else '-':>8s}  "
              f"{f(r['scaling'], 2):>7s}  {f(r['mem'], 0):>8s}"
              + (f"  {f(r['minstr'], 2):>9s}  {('x' + f(r['vs_i'], 2)) if r['vs_i'] else '-':>8s}  {f(r['sc_i'], 2):>7s}"
                 if instr else ""))
    print("   cpu ms: CPU time per case; ref: in units of the reference work (machines compare); vs main: CPU time "
          "against main's on the same cases;\n   scaling: how CPU time grows with the edges (1 linear, 2 square); "
          "mem kB: peak allocation (--memory)" + ("; M instr, vs main, scaling: the same on instructions retired "
          "(hardware counter), which the load does not move" if instr else ""))
    busy = [r for r in latest.values() if r["env"] and (r["env"].get("load_before") or 0) > 0.25 * (r["env"].get("cpus") or 1)]
    if busy:
        worst = max(busy, key=lambda r: r["env"]["load_before"])
        print(f"   note: {len(busy)} of these results ran on a busy machine (load {worst['env']['load_before']:.1f} "
              f"of {worst['env']['cpus']} CPUs before run {worst['run']}): trust vs main and scaling, not cpu ms")


def history(recs: list[dict], key: str) -> None:
    stage, algo = key.split("/", 1)
    rows = [r for r in recs if r["stage"] == stage and (r["algo"] == algo or r["algo"].startswith(algo + "."))]
    st = STAGES[stage]
    for r in rows:
        dirty = "+dirty" if (r["git"] or {}).get("dirty") else ""
        print(f"{r['run']}  {r['algo']:20s} {r['algo_sha']}  {(r['git'] or {}).get('commit', '?')[:7]}{dirty:6s} {r['dataset']:40s} "
              f"{st.headline}={fmt(r['summary'].get(st.headline))}  {r['note']}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--stage", action="append", default=[])
    ap.add_argument("--suite", action="append", default=[])
    ap.add_argument("--metric", default=None, help="a summary key (default: the stage's headline)")
    ap.add_argument("--delta", action="store_true", help="cells against the main algorithm")
    ap.add_argument("--cost", action="store_true", help="score against cost, per algorithm")
    ap.add_argument("--run", default=None, help="results of one run only (a prefix of its id)")
    ap.add_argument("--history", default=None, help="STAGE/ALGO: every result of one algorithm")
    ap.add_argument("--algos-dir", action="append", default=[])
    args = ap.parse_args()
    recs = read()
    if not recs:
        print("the ledger is empty: run bench/run.py first")
        return
    if args.history:
        history(recs, args.history)
        return
    algos = registry.load(args.algos_dir)
    if args.run:
        recs = [r for r in recs if r["run"].startswith(args.run)]
    for stage in args.stage or list(dict.fromkeys(r["stage"] for r in recs)):
        rs = [r for r in recs if r["stage"] == stage]
        names = args.suite or list(dict.fromkeys(r["suite"] for r in rs))
        for suite in names:
            try:                                     # by the suite's definition: a shared dataset shows in each
                want = set(runner.suite_datasets(suite))
            except KeyError:
                want = {r["dataset"] for r in rs if r["suite"] == suite}
            sub = [{**r, "suite": suite} for r in rs if r["dataset"] in want]
            if not sub:
                continue
            print(f"\n[suite {suite}, {sub[-1]['role']}]", end="")
            if args.cost:
                cost_table(sub, stage, args.metric or STAGES[stage].headline)
            else:
                table(sub, stage, args.metric or STAGES[stage].headline, args.delta, algos)


if __name__ == "__main__":
    main()
