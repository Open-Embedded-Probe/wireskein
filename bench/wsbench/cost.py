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
- memory (--memory): the peak of what Python and numpy allocate during the call (tracemalloc), in a call of its
  own after the timed ones (tracemalloc slows the call). It does not move with the load at all.

Workers run with one thread for numpy's libraries (run.py sets it before numpy loads), so one call is one CPU.
With hardware counters (native Linux) the instructions retired would be steadier still; WSL2 has none.
"""

from __future__ import annotations

import time
import tracemalloc

import numpy as np

MIN_CPU = 2e-3        # seconds: shorter calls are too coarse for ratios and slopes
BUDGET = 0.05         # CPU seconds: calls are repeated while all of them so far took less
REPS = 5


def measure(call):
    """(the call's result, least CPU seconds, least wall seconds, calls made)."""
    c, w = time.process_time(), time.perf_counter()
    got = call()
    cpu, wall = time.process_time() - c, time.perf_counter() - w
    total, n = cpu, 1
    while n < REPS and total < BUDGET:
        c, w = time.process_time(), time.perf_counter()
        call()
        dc, dw = time.process_time() - c, time.perf_counter() - w
        cpu, wall, total, n = min(cpu, dc), min(wall, dw), total + dc, n + 1
    return got, cpu, wall, n


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


def summarize(entries: list[dict], main: dict[str, float] | None) -> dict:
    """entries: one per case {"case", "cpu", "wall", "edges", "ref"}; main: the main algorithm's cpu per case."""
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
    ok = (cpu >= MIN_CPU) & (edges > 0)
    if ok.sum() >= 8 and np.ptp(np.log(edges[ok])) > np.log(4):      # sizes spread over 4x at least
        out["scaling"] = float(np.polyfit(np.log(edges[ok]), np.log(cpu[ok]), 1)[0])
    if main is not None:
        r = [e["cpu"] / main[e["case"]] for e in entries
             if main.get(e["case"], 0) >= MIN_CPU and e["cpu"] >= MIN_CPU / 10]
        if r:
            out["vs_main"] = float(np.exp(np.mean(np.log(r))))
            out["vs_main_cases"] = len(r)
    mem = [e["mem"] for e in entries if e.get("mem") is not None]
    if mem:
        out["mem_peak_kb_median"] = float(np.median(mem)) / 1024
        out["mem_peak_kb_max"] = float(np.max(mem)) / 1024
    return out
