"""What an algorithm costs, measured so that a busy machine (a build beside the run) moves it as little as can be.

- cpu: the process's CPU time around the algorithm's call (time.process_time), not the wall clock: time spent
  waiting for a CPU the others hold is not counted. Scoring is left out.
- repeats: a call shorter than BUDGET is made again (up to REPS times, algorithms being deterministic) and the least
  CPU time kept, as timeit does: the least disturbed of the calls.
- ref: before each case the worker times a fixed piece of work (numpy sorts and a Python loop, the best of a few);
  `cost_ref` is CPU time over it, both taken under about the same load, so machines, clock speeds and a load that
  changes over the run compare.
- vs_main: on each case, the algorithm's CPU time over the main algorithm's, measured by the same worker one right
  after the other (the load is about the same for both); the geometric mean over the cases. The steadiest of the
  three, and what to decide on.
- scaling: the slope of log(cpu) against log(edges of the input) over the cases: 1 grows with the data, 2 with its
  square. Constant factors (a slower machine, a busy one) do not move it.
- instructions (native Linux with hardware counters: perfcount.py): what the call retired, user space; with it
  vs_main and scaling are also given on instructions (`vs_main_instr`, `scaling_instr`), which a busy machine does
  not move. The best measure where there is one.
- memory (--memory): the peak of what Python and numpy allocate during the call (tracemalloc), in a call of its
  own after the timed ones (tracemalloc slows the call). It does not move with the load at all.

Workers run with one thread for numpy's libraries (run.py sets it before numpy loads), so one call is one CPU.
"""

from __future__ import annotations

import time
import tracemalloc

import numpy as np

from . import perfcount

MIN_CPU = 2e-3        # seconds: shorter calls are too coarse for ratios and slopes
MIN_INSTR = 1e5       # instructions: below, the call's own overhead weighs
BUDGET = 0.05         # CPU seconds: calls are repeated while all of them so far took less
REPS = 5


def _once(call, ctr):
    if ctr:
        ctr.start()
    c, w = time.process_time(), time.perf_counter()
    got = call()
    cpu, wall = time.process_time() - c, time.perf_counter() - w
    return got, cpu, wall, (ctr.stop() if ctr else None)


def measure(call) -> tuple:
    """(the call's result, least CPU seconds, least wall seconds, least instructions or None, calls made)."""
    ctr = perfcount.counter()
    got, cpu, wall, ins = _once(call, ctr)
    total, n = cpu, 1
    while n < REPS and total < BUDGET:
        _, dc, dw, di = _once(call, ctr)
        cpu, wall, total, n = min(cpu, dc), min(wall, dw), total + dc, n + 1
        ins = min(ins, di) if ins is not None else None
    return got, cpu, wall, ins, n


def peak_memory(call) -> int:
    """Bytes: the peak of what was allocated during the call."""
    tracemalloc.start()
    try:
        call()
        return tracemalloc.get_traced_memory()[1]
    finally:
        tracemalloc.stop()


def reference(reps: int = 3) -> float:
    """CPU seconds of a fixed piece of work, the best of reps."""
    x = np.random.default_rng(0).integers(0, 1 << 30, 200_000)
    lst = x[:30_000].tolist()
    best = float("inf")
    for _ in range(reps):
        t = time.process_time()
        np.sort(x)
        np.unique(x % 4096)
        s = 0
        for v in lst:
            s += v & 7
        best = min(best, time.process_time() - t)
    return best


def _paired(entries: list[dict], main: dict[str, dict] | None, key: str, floor: float) -> tuple[float, int] | None:
    if main is None:
        return None
    r = [e[key] / main[e["case"]][key] for e in entries
         if e.get(key) is not None and (main.get(e["case"]) or {}).get(key) is not None
         and main[e["case"]][key] >= floor and e[key] >= floor / 10]
    return (float(np.exp(np.mean(np.log(r)))), len(r)) if r else None


def _slope(edges: np.ndarray, y: np.ndarray, floor: float) -> float | None:
    ok = (y >= floor) & (edges > 0)
    if ok.sum() >= 8 and np.ptp(np.log(edges[ok])) > np.log(4):      # sizes spread over 4x at least
        return float(np.polyfit(np.log(edges[ok]), np.log(y[ok]), 1)[0])
    return None


def summarize(entries: list[dict], main: dict[str, dict] | None) -> dict:
    """entries: one per case {"case", "cpu", "wall", "edges", "ref", "instr", "mem"}; main: the main algorithm's
    entries per case."""
    if not entries:
        return {}
    cpu = np.array([e["cpu"] for e in entries])
    ref = float(np.median([e["ref"] for e in entries]))
    edges = np.array([e["edges"] for e in entries], dtype=np.float64)
    refs = np.array([e["ref"] for e in entries])
    out = {"cpu_median_ms": 1000 * float(np.median(cpu)), "cpu_total_s": float(cpu.sum()),
           "wall_median_ms": 1000 * float(np.median([e["wall"] for e in entries])),
           "ref_ms": 1000 * ref, "cost_ref": float(np.median(cpu / np.maximum(refs, 1e-9))),
           "calls_median": float(np.median([e.get("calls", 1) for e in entries])),
           "us_per_kedge": float(np.median(1e6 * cpu / np.maximum(edges / 1000, 1e-3)))}
    s = _slope(edges, cpu, MIN_CPU)
    if s is not None:
        out["scaling"] = s
    p = _paired(entries, main, "cpu", MIN_CPU)
    if p:
        out["vs_main"], out["vs_main_cases"] = p
    ins = [e.get("instr") for e in entries]
    if all(x is not None for x in ins):
        ins = np.array(ins, dtype=np.float64)
        out["instr_median_M"] = float(np.median(ins)) / 1e6
        out["instr_per_edge"] = float(np.median(ins / np.maximum(edges, 1)))
        s = _slope(edges, ins, MIN_INSTR)
        if s is not None:
            out["scaling_instr"] = s
        p = _paired(entries, main, "instr", MIN_INSTR)
        if p:
            out["vs_main_instr"] = p[0]
    mem = [e["mem"] for e in entries if e.get("mem") is not None]
    if mem:
        out["mem_peak_kb_median"] = float(np.median(mem)) / 1024
        out["mem_peak_kb_max"] = float(np.max(mem)) / 1024
    return out
