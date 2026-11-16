"""A clock's rate burst by burst, neighbouring bursts of about the same period (under 1.5x apart) one block: as the
engine's unused typed.clock_rate_segments, on the line as captured (raw) or after a gate that follows the local
rate (gated: changes closer than g of the median of the 16 intervals around are one change - a gate as wide as one
for the slow part would erase a fast part's real clock). Asynchronous lines: typed.async_segments, on the gated line
too for gated."""

from dataclasses import replace

import numpy as np
from numpy.lib.stride_tricks import sliding_window_view

from wsbench import algorithm
from wireskein._engine import kernels as K
from wireskein._engine import typed
from wireskein._engine.model import Capture
from wireskein._engine.survey import survey


def local_scale(e: np.ndarray, half: int = 8) -> np.ndarray:
    """Per edge, the median of the intervals around it (2 x half + 1 of them)."""
    iv = np.diff(e).astype(float)
    if len(iv) == 0:
        return np.zeros(len(e))
    if len(iv) < 2 * half + 1:
        return np.full(len(e), float(np.median(iv)))
    pad = np.concatenate((np.full(half, np.nan), iv, np.full(half, np.nan)))
    m = np.nanmedian(sliding_window_view(pad, 2 * half + 1), axis=1)
    return np.concatenate(([m[0]], m))


def local_scale_lr(e: np.ndarray, half: int = 8) -> np.ndarray:
    """Per edge, the smaller of the medians of the `half` intervals before it and the `half` after: where the rate
    changes, the faster side's (as kernels.local_period), so that a gate there does not erase the fast part."""
    iv = np.diff(e).astype(float)
    if len(iv) < 2 * half:
        return np.full(len(e), float(np.median(iv)) if len(iv) else 0.0)
    pad = np.concatenate((np.full(half, np.nan), iv, np.full(half, np.nan)))
    win = sliding_window_view(pad, half)
    with np.errstate(all="ignore"):
        import warnings
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", RuntimeWarning)
            before = np.nanmedian(win[:len(iv) + 1], axis=1)           # intervals ending at edge i
            after = np.nanmedian(win[half:half + len(iv) + 1], axis=1)  # intervals starting at edge i
    return np.fmin(before, after)


def collapse_local(e: np.ndarray, gate: float, step: int = 1, lr: bool = False, floor_from: float = 0) -> np.ndarray:
    """floor_from: the 2-sample floor only where the local scale (a half) is that many samples or more (0: always) -
    taken that coarsely a real half can be 2 samples."""
    e = np.asarray(e, np.int64)
    if len(e) < 3:
        return e
    sc = local_scale_lr(e) if lr else local_scale(e)
    w = np.maximum(gate * sc, np.where(sc >= floor_from * step, 2 * step, 0))
    first = np.concatenate(([True], np.diff(e) >= w[1:]))
    cid = np.cumsum(first) - 1
    last = np.flatnonzero(np.concatenate((first[1:], [True])))
    return e[last[np.bincount(cid) % 2 == 1]]


def clock_blocks(e: np.ndarray, initial: int, ratio: float = 1.5) -> list:
    r = e[K.edge_levels(e, initial) == 1]
    if len(r) < 4:
        return []
    st = K.burst_split(r)
    en = np.concatenate((st[1:], [len(r)]))
    out = []
    for a, z in zip(st, en):
        if z - a < 2:
            continue
        m = float(np.median(np.diff(r[a:z])))
        if out and max(m, out[-1][2]) / min(m, out[-1][2]) < ratio:
            out[-1] = (out[-1][0], int(r[z - 1]) + 1, out[-1][2])
        else:
            out.append((int(r[a]), int(r[z - 1]) + 1, m))
    # each block runs to where the next begins, so that any time in between belongs to one
    return [(b[0] if i else 0, out[i + 1][0] if i + 1 < len(out) else 1 << 62, b[2]) for i, b in enumerate(out)]


def raw_rates(blocks: list, e: np.ndarray, initial: int) -> list:
    """Each block's period measured again on the line as captured: the median rising interval inside it, the short
    ones a noise pulse makes (under half the block's gated period) left out."""
    r = e[K.edge_levels(e, initial) == 1]
    out = []
    for a, z, u in blocks:
        iv = np.diff(r[(r >= a) & (r < z)]).astype(float)
        iv = iv[(iv >= 0.5 * u) & (iv <= 1.5 * u)]
        out.append((a, z, float(np.median(iv)) if len(iv) >= 3 else u))
    return out


@algorithm("segments", "bursts", status="trial", about="a clock burst by burst, similar ones one block; raw or gated locally",
           variants={"raw": {"gate": None}, "gated2": {"gate": 0.2},
                     # the 2-sample floor only where a half has 4 samples or more
                     "gated2_f4": {"gate": 0.2, "floor_from": 4}})
@algorithm("segments", "bursts", status="retired", about="a clock burst by burst, similar ones one block; raw or gated locally",
           variants={"gated": {"gate": 0.3}, "gated_raw": {"gate": 0.3, "raw_rate": True},
                     "gated_lr": {"gate": 0.3, "lr": True}, "gated2_lr": {"gate": 0.2, "lr": True},
                     "gated2_lr_f4": {"gate": 0.2, "lr": True, "floor_from": 4}},
           reason="2026-11-16, local (segments suite): a gate of 0.3 cuts more of a UART's spikes-free bits than 0.2 "
                  "and no more of a clock's; measuring each block's period again on the raw line, or gating by the "
                  "faster side's median where the rate changes, changed nothing: what erased fast frames was the "
                  "2-sample floor where a half is 2 samples (gated2_f4 lifts it)")
def bursts(cap, lines, gate, raw_rate=False, lr=False, floor_from=0):
    out = {}
    view = cap
    if gate:
        view = Capture(cap.rate, cap.n_samples, [replace(x, edges=collapse_local(x.edges, gate, x.step, lr, floor_from)) if x.name in lines
                                                 else x for x in cap.channels])
    sv = None
    for ln, kind in lines.items():
        ch = view.channel(ln)
        if kind == "async":
            sv = sv or survey(view)
            out[ln] = [tuple(x) for x in typed.async_segments(view, sv, ln)]
        else:
            out[ln] = clock_blocks(np.asarray(ch.edges, np.int64), ch.initial)
            if raw_rate:
                raw = cap.channel(ln)
                out[ln] = raw_rates(out[ln], np.asarray(raw.edges, np.int64), raw.initial)
    return out
