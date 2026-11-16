"""Running stages x algorithms x datasets, and writing what was measured.

bench/results/ledger.jsonl (in git): one line per (run, stage, algorithm, dataset): the summary, the cost, the
algorithm's source hash and parameters, the dataset's text and a hash of the exact inputs.
bench/results/runs.jsonl (in git): one line per run: what is the same for all its results (the commit and whether the
tree was dirty, the machine and versions, the note, the command). read_ledger() gives the two joined.
Lines of both are only ever added.
bench/results/runs/<run>[.shard-i-of-n].jsonl and .meta.json (not in git): every item's row and how the run was
made, for looking into one result and for merging shards (merge.py).
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
RUNS = RESULTS / "runs.jsonl"
RUN_KEYS = ("label", "time", "note", "git", "env", "argv")     # kept once per run in RUNS


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
        case = data.load_case(path, source, chain)
    except Exception as e:  # noqa: BLE001 - a case that cannot be made is reported, the run goes on
        return {"case": str(path).rsplit("/", 1)[-1], "source": source, "chain": chain, "error": f"load: {type(e).__name__}: {e}"}
    out = {"case": case.id, "dataset": case.dataset, "input_sha": case.input_sha, "results": [],
           "load_s": time.perf_counter() - t0, "ref": cost.reference(),
           "edges": sum(len(c.edges) for c in case.cap.channels), "rate": float(case.cap.rate)}
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
                got, r["cpu"], r["seconds"], r["instr"], r["calls"] = cost.measure(lambda: st.run(a, case))
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

def plan_work(stage_names: list[str], algos_by_stage: dict[str, list], dsets: list[tuple[str, str, str, str]],
              memory: bool):
    """(plan, the cases to run in a fixed order, each dataset's suite / role / source / chain)."""
    plan = [(s, [a.name for a in algos_by_stage[s]]) for s in stage_names]
    work, meta_ds = [], {}
    for suite, role, src, chain in dsets:
        ds = src + (f"+{chain}" if chain else "")
        meta_ds[ds] = (suite, role, src, chain)
        work += [(ref, src, chain, plan, memory) for ref in data.source_items(src)]
    return plan, work, meta_ds


def new_run_id(git: dict) -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + (git["commit"][:7] or "nogit")


def machine() -> dict:
    model = ""
    try:
        for ln in Path("/proc/cpuinfo").read_text().splitlines():
            if ln.startswith("model name"):
                model = ln.split(":", 1)[1].strip()
                break
    except OSError:
        model = platform.processor()
    from . import perfcount
    return {"cpu_model": model or platform.machine(), "cpus": os.cpu_count(), "instructions": perfcount.why(),
            "ci": os.environ.get("GITHUB_RUN_ID") and {
                "run_id": os.environ.get("GITHUB_RUN_ID"), "runner": os.environ.get("RUNNER_NAME"),
                "url": f"{os.environ.get('GITHUB_SERVER_URL')}/{os.environ.get('GITHUB_REPOSITORY')}/actions/runs/"
                       f"{os.environ.get('GITHUB_RUN_ID')}"} or None}


def run(stage_names: list[str], algos_by_stage: dict[str, list], dsets: list[tuple[str, str, str, str]],
        jobs: int, note: str, extra: list[str], label: str = "", memory: bool = False,
        shard: tuple[int, int] | None = None, run_id: str | None = None) -> str:
    """Run the cases (of one shard: every n-th from the i-th, all algorithms of a case in the same worker, so that
    against-main costs pair up) and write their items; without a shard, add the results to the ledger too (with
    shards, merge.py does once all are in)."""
    git = git_state()
    run_id = run_id or new_run_id(git)
    plan, work, meta_ds = plan_work(stage_names, algos_by_stage, dsets, memory)
    total = len(work)
    if shard:
        i, n = shard
        work = work[i::n]
    stem = run_id + (f".shard-{shard[0]}-of-{shard[1]}" if shard else "")
    print(f"run {stem}: {len(work)} of {total} cases, stages {stage_names}, "
          f"{sum(len(v) for _, v in plan)} algorithms", file=sys.stderr)
    runs_dir = RESULTS / "runs"
    runs_dir.mkdir(parents=True, exist_ok=True)
    algos = registry.load(extra)
    meta = {"run": run_id, "shard": list(shard) if shard else None, "label": label, "note": note,
            "argv": sys.argv[1:], "git": git, "plan": plan, "datasets": meta_ds, "cases_total": total,
            "algos": {f"{s}/{n}": {"status": algos[(s, n)].status, "params": algos[(s, n)].params,
                                   "path": _rel(algos[(s, n)].path), "sha": algos[(s, n)].src_sha}
                      for s, names in plan for n in names},
            "env": {"python": platform.python_version(), "numpy": np.__version__,
                    "wireskein": _wireskein_version(),
                    # what ran, even on a dirty tree: the package's sources and the bench's own (stages, data, suites)
                    "wireskein_src_sha": tree_sha(data.REPO / "src" / "wireskein", "*.py", "*.toml"),
                    "bench_sha": tree_sha(BENCH / "wsbench", "*.py") + tree_sha(BENCH, "suites.toml")[:4],
                    **machine(), "jobs": jobs, "load_before": os.getloadavg()[0]}}
    t0 = time.time()
    with (runs_dir / f"{stem}.jsonl").open("w") as fh, \
            mp.get_context("fork").Pool(jobs, _init, (extra,)) as pool:
        for k, out in enumerate(pool.imap_unordered(_work, work, chunksize=1), 1):
            fh.write(json.dumps(out, default=_json) + "\n")
            if "error" in out:
                print(f"  case {out['case']}: {out['error']}", file=sys.stderr)
            if k % 20 == 0 or k == len(work):
                print(f"  [{k}/{len(work)}] {time.time() - t0:.0f}s", file=sys.stderr)
    meta["env"]["load_after"] = os.getloadavg()[0]
    meta["seconds"] = time.time() - t0
    (runs_dir / f"{stem}.meta.json").write_text(json.dumps(meta, indent=1, default=_json))
    if not shard:
        run_rec, lines = aggregate(run_id, [runs_dir / f"{stem}.jsonl"])
        append(lines, run_rec)
        print(f"{len(lines)} results added to {_rel(str(LEDGER))} (items: {_rel(str(runs_dir / stem))}.jsonl)",
              file=sys.stderr)
    return run_id


def aggregate(run_id: str, item_files: list[Path]) -> tuple[dict, list[str]]:
    """(the run's record, its ledger lines) from its item files (one, or every shard's), each with its .meta.json."""
    metas = [json.loads(f.with_suffix(".meta.json").read_text()) for f in item_files]
    m0 = metas[0]
    for m in metas[1:]:
        for k in ("run", "git", "plan", "algos", "datasets"):
            if json.dumps(m[k], sort_keys=True) != json.dumps(m0[k], sort_keys=True):
                raise ValueError(f"run {run_id}: the shards differ in {k}")
    if m0["shard"]:
        n = m0["shard"][1]
        got = sorted(m["shard"][0] for m in metas)
        if got != list(range(n)):
            raise ValueError(f"run {run_id}: shards {got} of {n}")
    per = defaultdict(lambda: {"rows": [], "seconds": [], "errors": [], "cases": [], "cost": []})
    for f, m in zip(item_files, metas):
        for ln in f.read_text().splitlines():
            out = json.loads(ln)
            if "error" in out:
                continue
            for r in out["results"]:
                p = per[(r["stage"], r["algo"], out["dataset"])]
                p["rows"] += r["rows"]
                p["seconds"].append(r["seconds"])
                p["cost"].append({"case": out["case"], "cpu": r["cpu"], "wall": r["seconds"], "edges": out["edges"],
                                  "ref": out["ref"], "calls": r.get("calls", 1), "mem": r.get("mem"),
                                  "instr": r.get("instr")})
                p["cases"].append((out["case"], out["input_sha"]))
                if "error" in r:
                    p["errors"].append(f"{out['case']}: {r['error']}")
    envs = [m["env"] for m in metas]
    env = dict(envs[0])
    for k in ("cpu_model", "instructions", "runner"):
        vals = sorted({str(e.get(k)) for e in envs})
        if len(vals) > 1:
            env[k] = vals
    env["load_before"] = max(e["load_before"] for e in envs)
    env["load_after"] = max(e.get("load_after", 0) for e in envs)
    env["shards"] = len(metas)
    if env.get("ci"):
        env["ci"] = {k: v for k, v in env["ci"].items() if k != "runner"}
    lines = []
    mains = {(s, ds): {e["case"]: e for e in p["cost"]} for (s, n, ds), p in per.items()
             if m0["algos"][f"{s}/{n}"]["status"] == "main"}
    for (stage, name, ds), p in sorted(per.items()):
        a = m0["algos"][f"{stage}/{name}"]
        suite, role, src, chain = m0["datasets"][ds]
        cases = sorted(map(tuple, p["cases"]))
        rec = {"run": run_id, "stage": stage, "algo": name, "status": a["status"], "params": a["params"],
               "algo_path": a["path"], "algo_sha": a["sha"], "suite": suite, "role": role, "dataset": ds,
               "source": src, "transforms": chain, "cases": len(cases),
               "inputs_sha": hashlib.sha256(json.dumps(cases).encode()).hexdigest()[:16],
               # rounded: scores to 6 significant digits, costs to 4 (they vary more than that run to run)
               "summary": _round(STAGES[stage].summarize(p["rows"]), 6),
               "seconds_median": _round(float(np.median(p["seconds"])), 4) if p["seconds"] else None,
               "cost": _round(cost.summarize(p["cost"], None if a["status"] == "main" else mains.get((stage, ds))), 4),
               "errors": len(p["errors"]), "first_errors": p["errors"][:3]}
        lines.append(json.dumps(rec, default=_json))
    run_rec = {"run": run_id, "label": m0["label"], "time": datetime.now(timezone.utc).isoformat(timespec="seconds"),
               "note": m0["note"], "git": m0["git"], "env": env, "argv": m0["argv"], "plan": m0["plan"],
               "cases": m0["cases_total"], "results": len(lines),
               "seconds": max(m.get("seconds", 0) for m in metas)}
    return run_rec, lines


def read_ledger() -> list[dict]:
    """The ledger's lines, each with its run's record (runs.jsonl) joined in, in the order they were added."""
    runs = {}
    if RUNS.exists():
        for ln in RUNS.read_text().splitlines():
            if ln.strip():
                r = json.loads(ln)
                runs[r["run"]] = r
    out = []
    if LEDGER.exists():
        for ln in LEDGER.read_text().splitlines():
            if ln.strip():
                r = json.loads(ln)
                run = runs.get(r["run"], {})
                out.append({**{k: run.get(k) for k in RUN_KEYS}, **r})
    return out


def append(lines: list[str], run_rec: dict | None = None) -> int:
    """Add the run's record (once) and its lines to the ledger, leaving out lines already there (the same run, stage,
    algorithm and dataset)."""
    LEDGER.parent.mkdir(parents=True, exist_ok=True)
    if run_rec is not None:
        known = {json.loads(ln)["run"] for ln in RUNS.read_text().splitlines() if ln.strip()} if RUNS.exists() else set()
        if run_rec["run"] not in known:
            with RUNS.open("a") as fh:
                fh.write(json.dumps(run_rec, default=_json) + "\n")
    have = set()
    if LEDGER.exists():
        for ln in LEDGER.read_text().splitlines():
            if ln.strip():
                r = json.loads(ln)
                have.add((r["run"], r["stage"], r["algo"], r["dataset"]))
    n = 0
    with LEDGER.open("a") as fh:
        for ln in lines:
            r = json.loads(ln)
            if (r["run"], r["stage"], r["algo"], r["dataset"]) in have:
                continue
            fh.write(ln + "\n")
            n += 1
    return n


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


def _round(o, digits: int):
    if isinstance(o, (float, np.floating)):
        return float(f"{float(o):.{digits}g}")
    if isinstance(o, dict):
        return {k: _round(v, digits) for k, v in o.items()}
    if isinstance(o, (list, tuple)):
        return [_round(v, digits) for v in o]
    return o


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
