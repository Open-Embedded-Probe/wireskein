"""Running stages x algorithms x datasets, and writing what was measured.

bench/results/ledger.jsonl (in git): one line per (run, stage, algorithm, dataset) with the summary and everything
needed to run it again: the commit (and whether the tree was dirty), the algorithm's source hash and parameters,
the dataset's text and a hash of the exact inputs. Lines are only ever added.
bench/results/runs/<run>.jsonl (not in git): every item's row, for looking into one result.
"""

from __future__ import annotations

import hashlib
import json
import os
import multiprocessing as mp
import platform
import subprocess
import sys
import time
import tomllib
import traceback
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

from . import cost, data, registry
from .stages import STAGES

BENCH = Path(__file__).resolve().parents[1]
# WSBENCH_RESULTS: somewhere else (a scratch ledger while an algorithm is being written)
RESULTS = Path(os.environ.get("WSBENCH_RESULTS") or BENCH / "results")
LEDGER = RESULTS / "ledger.jsonl"


def suites() -> dict:
    return tomllib.loads((BENCH / "suites.toml").read_text())["suite"]


def datasets_of(names: list[str]) -> list[tuple[str, str, str, str]]:
    """(suite, role, source, chain) for each dataset of the suites."""
    all_ = suites()
    out = []
    for n in names:
        if n not in all_:
            raise KeyError(f"no suite {n!r} (have: {', '.join(all_)})")
        s = all_[n]
        for src in s["sources"]:
            for chain in s.get("transforms", [""]):
                out.append((n, s.get("role", "tune"), src, chain))
    return out


def git_state() -> dict:
    def g(*a):
        return subprocess.run(["git", "-C", str(data.REPO), *a], capture_output=True, text=True).stdout.strip()
    # the ledger itself is written by the runs: not a change to what is measured
    dirty = [ln[3:] for ln in g("status", "--porcelain").splitlines() if not ln[3:].startswith("bench/results")]
    return {"commit": g("rev-parse", "HEAD"), "dirty": bool(dirty), "dirty_files": dirty[:20]}


# ---------------- worker ----------------

_EXTRA: list[str] = []


def _init(extra):
    global _EXTRA
    _EXTRA = extra
    registry.load(extra)
    cost.reference()                    # warm: the first one pays for numpy's first calls


def _work(job):
    path, source, chain, plan, memory = job
    t0 = time.perf_counter()
    try:
        case = data.load_case(Path(path), source, chain)
    except Exception as e:  # noqa: BLE001 - a case that cannot be made is reported, the run goes on
        return {"case": Path(path).name, "source": source, "chain": chain, "error": f"load: {type(e).__name__}: {e}"}
    out = {"case": case.id, "dataset": case.dataset, "input_sha": case.input_sha, "results": [],
           "load_s": time.perf_counter() - t0, "ref": cost.reference(),
           "edges": sum(len(c.edges) for c in case.cap.channels)}
    algos = registry.load(_EXTRA)
    for stage_name, names in plan:
        st = STAGES[stage_name]
        if not st.wants(case):
            continue
        for name in names:
            a = algos[(stage_name, name)]
            r = {"stage": stage_name, "algo": name}
            c1, t1 = time.process_time(), time.perf_counter()
            try:
                got, r["cpu"], r["seconds"], r["calls"] = cost.measure(lambda: st.run(a, case))
                if memory:
                    r["mem"] = cost.peak_memory(lambda: st.run(a, case))
                r["rows"] = st.score(got, case)          # not part of the cost
            except Exception as e:  # noqa: BLE001 - an algorithm's failure is a result
                r["cpu"], r["seconds"] = time.process_time() - c1, time.perf_counter() - t1
                r["error"] = f"{type(e).__name__}: {e}"
                r["trace"] = traceback.format_exc(limit=4)
                r["rows"] = []
            out["results"].append(r)
    return out


# ---------------- run ----------------

def run(stage_names: list[str], algos_by_stage: dict[str, list], dsets: list[tuple[str, str, str, str]],
        jobs: int, note: str, extra: list[str], label: str = "", memory: bool = False) -> str:
    git = git_state()
    load_before = os.getloadavg()[0]      # the others' load (a build beside): told with the costs
    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + (git["commit"][:7] or "nogit")
    plan = [(s, [a.name for a in algos_by_stage[s]]) for s in stage_names]
    work = []
    meta_ds = {}
    for suite, role, src, chain in dsets:
        ds = src + (f"+{chain}" if chain else "")
        meta_ds[ds] = (suite, role, src, chain)
        work += [(str(p), src, chain, plan, memory) for p in data.source_paths(src)]
    print(f"run {run_id}: {len(work)} cases, stages {stage_names}, "
          f"{sum(len(v) for _, v in plan)} algorithms", file=sys.stderr)
    RESULTS.mkdir(exist_ok=True)
    (RESULTS / "runs").mkdir(exist_ok=True)
    per = defaultdict(lambda: {"rows": [], "seconds": [], "errors": [], "cases": [], "cost": []})
    t0 = time.time()
    with (RESULTS / "runs" / f"{run_id}.jsonl").open("w") as fh, \
            mp.get_context("fork").Pool(jobs, _init, (extra,)) as pool:
        for k, out in enumerate(pool.imap_unordered(_work, work, chunksize=1), 1):
            fh.write(json.dumps(out, default=_json) + "\n")
            if "error" in out:
                print(f"  case {out['case']}: {out['error']}", file=sys.stderr)
                continue
            for r in out["results"]:
                p = per[(r["stage"], r["algo"], out["dataset"])]
                p["rows"] += r["rows"]
                p["seconds"].append(r["seconds"])
                p["cost"].append({"case": out["case"], "cpu": r["cpu"], "wall": r["seconds"], "edges": out["edges"],
                                  "ref": out["ref"], "calls": r.get("calls", 1), "mem": r.get("mem")})
                p["cases"].append((out["case"], out["input_sha"]))
                if "error" in r:
                    p["errors"].append(f"{out['case']}: {r['error']}")
            if k % 20 == 0 or k == len(work):
                print(f"  [{k}/{len(work)}] {time.time() - t0:.0f}s", file=sys.stderr)
    algos = registry.load(extra)
    env = {"python": platform.python_version(), "numpy": np.__version__, "wireskein": _wireskein_version(),
           # what ran, even on a dirty tree: the package's sources and the bench's own (stages, data, suites)
           "wireskein_src_sha": tree_sha(data.REPO / "src" / "wireskein", "*.py", "*.toml"),
           "bench_sha": tree_sha(BENCH / "wsbench", "*.py") + tree_sha(BENCH, "suites.toml")[:4],
           "cpus": os.cpu_count(), "jobs": jobs, "load_before": load_before, "load_after": os.getloadavg()[0]}
    lines = []
    mains = {(s, ds): {e["case"]: e["cpu"] for e in p["cost"]} for (s, n, ds), p in per.items()
             if algos[(s, n)].status == "main"}
    for (stage, name, ds), p in sorted(per.items()):
        a = algos[(stage, name)]
        suite, role, src, chain = meta_ds[ds]
        cases = sorted(p["cases"])
        rec = {"run": run_id, "label": label, "time": datetime.now(timezone.utc).isoformat(timespec="seconds"),
               "note": note, "stage": stage, "algo": name, "status": a.status, "params": a.params,
               "algo_path": _rel(a.path), "algo_sha": a.src_sha, "suite": suite, "role": role, "dataset": ds,
               "source": src, "transforms": chain, "cases": len(cases),
               "inputs_sha": hashlib.sha256(json.dumps(cases).encode()).hexdigest()[:16],
               "summary": STAGES[stage].summarize(p["rows"]),
               "seconds_median": float(np.median(p["seconds"])) if p["seconds"] else None,
               "cost": cost.summarize(p["cost"], None if a.status == "main" else mains.get((stage, ds))),
               "errors": len(p["errors"]), "first_errors": p["errors"][:3],
               "git": git, "env": env, "argv": sys.argv[1:]}
        lines.append(json.dumps(rec, default=_json))
    with LEDGER.open("a") as fh:
        for ln in lines:
            fh.write(ln + "\n")
    print(f"{len(lines)} results added to {_rel(str(LEDGER))} (items: {_rel(str(RESULTS / 'runs' / run_id))}.jsonl)",
          file=sys.stderr)
    return run_id


def tree_sha(root: Path, *globs: str) -> str:
    h = hashlib.sha256()
    for g in globs:
        for p in sorted(root.rglob(g)):
            h.update(str(p.relative_to(root)).encode() + b"\0" + p.read_bytes())
    return h.hexdigest()[:12]


def _wireskein_version() -> str:
    import wireskein
    return getattr(wireskein, "__version__", "?")


def _rel(p: str) -> str:
    try:
        return str(Path(p).resolve().relative_to(data.REPO))
    except ValueError:
        return p


def _json(o):
    if isinstance(o, (np.integer,)):
        return int(o)
    if isinstance(o, (np.floating,)):
        return float(o)
    if isinstance(o, (np.bool_,)):
        return bool(o)
    if isinstance(o, np.ndarray):
        return o.tolist()
    return str(o)
