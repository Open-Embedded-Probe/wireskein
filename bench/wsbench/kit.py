"""Helpers algorithms share (an algorithm file cannot import another one: they are loaded by path)."""

from __future__ import annotations

import numpy as np

from wireskein._engine import kernels as K


def rising(ch) -> np.ndarray:
    return np.asarray(ch.edges, np.int64)[K.edge_levels(ch.edges, ch.initial) == 1]


def runs(cap, ch) -> np.ndarray:
    """Lengths of the whole runs (the partial first and last left out), in ticks."""
    _, length, _ = ch.runs(cap.n_samples)
    return length[1:-1].astype(np.float64)


def comb_unit(iv: np.ndarray, max_mult: float, fractions=(1, 2, 3, 4), top: int = 10) -> tuple[float | None, float]:
    """The largest unit T that the intervals are integer multiples of (up to max_mult T), scored by a cosine comb:
    mean cos(2 pi iv / T) is 1 when every interval is a whole number of T and about 0 for intervals at random (a
    noise pulse splits an interval into two random parts). Candidates: the most common intervals and their integer
    fractions, each refined by least squares on the intervals near a multiple. Returns (T, score)."""
    iv = iv[iv > 0]
    if len(iv) < 8:
        return None, 0.0
    vals, cnt = np.unique(np.round(iv), return_counts=True)
    # the most common first, ties by the shorter interval: a stable sort, the same on every CPU (numpy's default
    # argsort orders ties by the CPU's SIMD path: GitHub's runners differ, and the same case scored differently)
    peaks = vals[np.argsort(-cnt, kind="stable")[:top]]
    cands = sorted({float(p / k) for p in peaks for k in fractions if p / k >= 1.5})
    scored = []
    for T in cands:
        sel = iv[iv <= max_mult * T * 1.05]
        if len(sel) < max(4, 0.2 * len(iv)):
            continue
        k = np.maximum(1, np.round(sel / T))
        near = np.abs(sel / T - k) < 0.25
        if near.sum() >= 4:
            T = float((sel[near] * k[near]).sum() / (k[near] ** 2).sum())
        # an interval shorter than half of T is no multiple of it (cos would call it one: ~0 T is near 0)
        c = np.where(sel >= 0.5 * T, np.cos(2 * np.pi * sel / T), 0.0)
        s = float(np.mean(c)) * min(1.0, len(sel) / len(iv) / 0.5)
        scored.append((T, s))
    if not scored:
        return None, 0.0
    best = max(s for _, s in scored)
    T, s = max(((T, s) for T, s in scored if s >= best - 0.05), key=lambda x: x[0])
    return T, s
