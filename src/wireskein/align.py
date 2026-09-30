"""Aligning analog channels to the logic ticks (docs/wireskein-format.ja.md §5.1).

A probe's track start times are estimates: an ADC may start some hundred
microseconds off, and run a little fast or slow. When the same signal is on a
logic channel and on an analog channel (the same net wired to both, or a marker
pulse on every track), its edges tell the offset and the time scale, the way a
video editor lines up sound by its waveform:

    from wireskein import align
    cap = load("m.wireskein")
    a = align.find(cap, reference="SYNC", via="SYNC_A", threshold=1.65)
    print(a["channels"]["SYNC_A"])        # offset_ticks, scale, matched, residual_ticks, ...
    align.save("m.wireskein", a)          # as attach/alignment.json; the samples are not touched
    cap = align.apply(cap, a)             # the analog channels on the aligned time

Time t (ticks, as stored) becomes offset + scale * t. The logic ticks are the
reference and are not moved.
"""

from __future__ import annotations

import json
from fractions import Fraction
from pathlib import Path

import numpy as np

from ._engine import analog
from ._engine.model import AnalogTrace, Capture

FORMAT = "wireskein-alignment/0"
NAME = "alignment.json"                  # attach/alignment.json


def _uncertainty_ticks(cap: Capture, trace: AnalogTrace, tick: float) -> float | None:
    ns = [cap.meta.get("start_uncertainty_ns"), trace.acquisition.get("start_uncertainty_ns")]
    ns = [float(x) for x in ns if isinstance(x, (int, float))]
    return sum(ns) * 1e-9 * tick if ns else None


def _match(ref: np.ndarray, pol_ref: np.ndarray, t: np.ndarray, pol: np.ndarray, off: float, scale: float,
           tol) -> tuple[np.ndarray, np.ndarray]:
    """Pairs (index in t, index in ref): each edge of t, moved by the model, to the
    nearest reference edge of the same direction within tol (a number, or one per edge of t)."""
    tol = np.broadcast_to(np.asarray(tol, dtype=float), t.shape)
    out_t, out_r = [], []
    for p in (0, 1):
        r = ref[pol_ref == p]
        ri = np.flatnonzero(pol_ref == p)
        ti = np.flatnonzero(pol == p)
        if not len(r) or not len(ti):
            continue
        x = off + scale * t[ti]
        right = np.clip(np.searchsorted(r, x), 0, len(r) - 1)
        left = np.clip(right - 1, 0, len(r) - 1)
        j = np.where(np.abs(r[left] - x) <= np.abs(r[right] - x), left, right)
        ok = np.abs(r[j] - x) <= tol[ti]
        out_t.append(ti[ok])
        out_r.append(ri[j[ok]])
    if not out_t:
        return np.zeros(0, int), np.zeros(0, int)
    return np.concatenate(out_t), np.concatenate(out_r)


def find(cap: Capture, reference: str, via: str, threshold, window_ticks: float | None = None,
         apply_to: list[str] | None = None, max_ppm: float = 5000) -> dict:
    """Offset and scale of the analog channel `via` against the logic channel
    `reference` (the same signal on both), read at `threshold` volts (one value
    or (low, high)). window_ticks: how far off the start may be (default: from
    the start uncertainties the probe reported, else a tenth of the capture).
    apply_to: the analog channels that get the result (default: all of them;
    channels from one ADC share its clock). max_ppm: how far the ADC's rate
    may be off. Raises ValueError when the edges do
    not tell one answer (too few, or a periodic signal whose period is shorter
    than the window: then give a smaller window, or use a marker pulse)."""
    tick_hz = cap.meta.get("tick_hz", cap.rate)
    tick = float(tick_hz)
    ref = cap.channel(reference)
    trace = next((a for a in cap.analog if a.name == via), None)
    if trace is None:
        raise ValueError(f"{via}: not an analog channel in this capture")
    per = float(Fraction(tick_hz) / trace.rate_hz)            # ticks per analog sample
    # the reference edge happened somewhere in the step before the tick it is stored at: take the middle
    r_t = ref.edges.astype(float) - ref.step / 2
    r_pol = (ref.initial ^ ((np.arange(len(ref.edges)) + 1) & 1)).astype(np.uint8)   # level after each edge
    _, a_t, a_pol = analog.crossings(trace, tick_hz, threshold)
    if len(r_t) < 2 or len(a_t) < 2:
        raise ValueError(f"too few edges to align: {reference} has {len(r_t)}, {via} has {len(a_t)}")
    if window_ticks is None:
        u = _uncertainty_ticks(cap, trace, tick)
        window_ticks = 3 * u + 4 * per if u else cap.n_samples / 10
    # 1. offset and scale together: for each scale on a grid fine enough that the drift over the edges used
    #    stays under half a bin, the most common difference between an analog edge (scaled) and a reference
    #    edge of the same direction within the window. The pair with the most agreeing edges wins.
    width = max(per, ref.step)
    use = np.unique(np.linspace(0, len(a_t) - 1, min(len(a_t), 512)).round().astype(int))
    t_use, p_use = a_t[use], a_pol[use]
    span = max(float(t_use.max() - t_use.min()), width)
    reach = window_ticks + max_ppm * 1e-6 * float(np.abs(t_use).max())
    pairs_t, pairs_r = [], []
    for p in (0, 1):
        r = r_t[r_pol == p]
        for t in t_use[p_use == p]:
            lo, hi = np.searchsorted(r, [t - reach, t + reach])
            pairs_r.append(r[lo:hi])
            pairs_t.append(np.full(hi - lo, t))
    pt, pr = np.concatenate(pairs_t), np.concatenate(pairs_r)
    if not len(pt):
        raise ValueError(f"no edge of {reference} within {window_ticks / tick * 1e6:.1f} us of an edge of {via}")
    step = width / (2 * span)
    t_first = float(a_t[0])
    scales = 1 + np.arange(-max_ppm * 1e-6, max_ppm * 1e-6 + step / 2, step)
    best = (0, 1.0, 0, None)                                  # (count, scale, bin, rival count)
    for sc in scales:
        d = pr - sc * pt
        ok = np.abs(d + (sc - 1) * t_first) <= window_ticks        # the corrected start stays in the window
        bins = np.round(d[ok] / width).astype(np.int64)
        if not len(bins):
            continue
        values, counts = np.unique(bins, return_counts=True)
        top = int(np.argmax(counts))
        if counts[top] > best[0]:
            far = np.abs(values - values[top]) > 2
            best = (int(counts[top]), float(sc), int(values[top]), int(counts[far].max()) if far.any() else 0)
    count, scale, b, rival = best
    if count < 2:
        raise ValueError(f"the edges of {via} and {reference} do not agree on any offset")
    if rival >= 0.8 * count:
        raise ValueError(f"ambiguous: another offset fits about as well ({rival} vs {count} edges; a periodic "
                         f"signal: give a smaller window, or align on a marker pulse)")
    off = b * width
    # 2. refine: match every edge with the model, fit offset and scale, and match again with the new model
    tol = 1.5 * width
    for _ in range(4):
        ti, ri = _match(r_t, r_pol, a_t, a_pol, off, scale, tol)
        if len(ti) < 2:
            break
        x, y = a_t[ti], r_t[ri]
        if x.max() - x.min() > 10 * width:
            scale, off = (float(v) for v in np.polyfit(x, y, 1))
        else:
            off = float(np.median(y - scale * x))
        resid = y - (off + scale * x)
        tol = max(1.5 * width, 4 * float(np.sqrt(np.mean(resid ** 2))))
    ti, ri = _match(r_t, r_pol, a_t, a_pol, off, scale, tol)
    if len(ti) < 2:
        raise ValueError(f"only {len(ti)} edges of {via} match {reference}")
    resid = r_t[ri] - (off + scale * a_t[ti])
    # the edges that could match: those that land where the reference has edges (a logic channel may
    # cover only part of the analog track, when the probe gave it fewer samples)
    moved = off + scale * a_t
    overlap = int(np.count_nonzero((moved >= r_t[0] - tol) & (moved <= r_t[-1] + tol)))
    x = a_t[ti]
    spread = float(np.sum((x - x.mean()) ** 2))
    dof = max(1, len(ti) - 2)
    scale_err = float(np.sqrt(np.sum(resid ** 2) / dof / spread)) if spread > 0 else float("inf")
    low, high = analog.thresholds(threshold)
    entry = {"offset_ticks": off, "scale": scale, "reference": reference, "via": via, "method": "edges",
             "matched": int(len(ti)), "edges": int(len(a_t)), "overlap_edges": overlap,
             "overlap_s": float(r_t[-1] - r_t[0]) / tick, "residual_ticks": float(np.sqrt(np.mean(resid ** 2))),
             "scale_ppm_uncertainty": scale_err * 1e6,
             "threshold_v": [low, high], "window_ticks": float(window_ticks),
             "offset_us": off / tick * 1e6, "scale_ppm": (scale - 1) * 1e6,
             "start_shift_us": (off + (scale - 1) * float(trace.t0_ticks)) / tick * 1e6}   # how far its start moved
    names = apply_to if apply_to is not None else [a.name for a in cap.analog]
    missing = [n for n in names if n not in {a.name for a in cap.analog}]
    if missing:
        raise ValueError(f"not analog channels here: {', '.join(missing)}")
    return {"format": FORMAT, "channels": {n: dict(entry) for n in names}}


def _edges(cap: Capture, name: str, threshold) -> tuple[np.ndarray, np.ndarray, float]:
    """(times in the capture's ticks, level after each, resolution in ticks) of a channel's edges: a logic
    channel's (the middle of the step each happened in), or an analog one read at `threshold`."""
    try:
        ch = cap.channel(name)
    except KeyError:
        ch = None
    if ch is not None:
        pol = (ch.initial ^ ((np.arange(len(ch.edges)) + 1) & 1)).astype(np.uint8)
        return ch.edges.astype(float) - ch.step / 2, pol, float(ch.step)
    trace = next((a for a in cap.analog if a.name == name), None)
    if trace is None:
        raise ValueError(f"{name}: no such channel")
    if threshold is None:
        raise ValueError(f"{name} is analog: give a threshold")
    tick_hz = cap.meta.get("tick_hz", cap.rate)
    _, t, pol = analog.crossings(trace, tick_hz, threshold)
    return t, pol.astype(np.uint8), float(Fraction(tick_hz) / trace.rate_hz)


def between(ref_cap: Capture, reference: str, cap: Capture, via: str, threshold=None,
            window_ticks: float | None = None, max_ppm: float = 200) -> dict:
    """How this capture's ticks map onto another capture's (wireskein-format §5.1.1): the
    same signal on `reference` (a logic channel of ref_cap) and `via` (a channel of cap;
    analog needs a threshold). Returns the entry for alignment.json["files"] (without
    capture_sha256). The start may be off by anything within window_ticks (reference
    ticks; default: the reference capture's length); the two clocks by max_ppm.
    Raises ValueError when the edges do not tell one answer."""
    r_t, r_pol, r_w = _edges(ref_cap, reference, None)
    t, pol, w = _edges(cap, via, threshold)
    if len(r_t) < 2 or len(t) < 2:
        raise ValueError(f"too few edges: {reference} has {len(r_t)}, {via} has {len(t)}")
    tick_ref, tick = float(ref_cap.meta.get("tick_hz", ref_cap.rate)), float(cap.meta.get("tick_hz", cap.rate))
    s0 = tick_ref / tick                                       # reference ticks per tick here
    fine = max(r_w, w * s0)
    if window_ticks is None:
        window_ticks = float(ref_cap.n_samples)
    # 1. a coarse offset from the first edges: the most common difference to a reference edge of the same
    #    direction, in bins wide enough that the clocks cannot drift out of one over these edges
    k = max(8, min(64, int(20_000_000 // max(1, len(r_t)))))
    head, hpol = t[:k], pol[:k]
    span = float(head[-1] - head[0]) * s0
    width = max(fine, 4 * max_ppm * 1e-6 * span)
    diffs = []
    for x, p in zip(head, hpol):
        r = r_t[r_pol == p]
        d = r - s0 * x
        diffs.append(d[np.abs(d) <= window_ticks])
    d = np.concatenate(diffs)
    if not len(d):
        raise ValueError(f"no edges of {reference} within the window")
    bins = np.round(d / width).astype(np.int64)
    values, counts = np.unique(bins, return_counts=True)
    order = np.argsort(counts)[::-1]
    best = values[order[0]]
    rival = next((values[i] for i in order[1:] if abs(values[i] - best) > 2), None)
    if counts[order[0]] < 2 or (rival is not None and counts[values == rival][0] >= 0.8 * counts[order[0]]):
        raise ValueError(f"ambiguous: the first {len(head)} edges of {via} fit several offsets (a periodic signal: "
                         f"give a smaller window, or align on an irregular marker pulse)")
    off, scale = float(np.median(d[np.abs(bins - best) <= 1])), s0
    # 2. refine over more and more of the edges; the tolerance grows with how uncertain the extrapolation is
    order_t = np.argsort(t)
    t, pol = t[order_t], pol[order_t]
    se, rms, center = max_ppm * 1e-6 * s0, width / 4, float(t[0])
    for frac in (0.02, 0.1, 0.3, 1.0):
        n = max(16, int(len(t) * frac))
        tt, pp = t[:n], pol[:n]
        tol = np.maximum(1.5 * fine, 4 * rms) + 3 * se * np.abs(tt - center)
        ti, ri = _match(r_t, r_pol, tt, pp, off, scale, tol)
        if len(ti) < 3:
            continue
        x, y = tt[ti], r_t[ri]
        if float(x.max() - x.min()) * s0 > 10 * fine:
            scale, off = (float(v) for v in np.polyfit(x, y, 1))
            resid = y - (off + scale * x)
            rms = float(np.sqrt(np.mean(resid ** 2)))
            spread = float(np.sum((x - x.mean()) ** 2))
            se = float(np.sqrt(np.sum(resid ** 2) / max(1, len(x) - 2) / spread)) if spread > 0 else se
            center = float(x.mean())
        else:
            off = float(np.median(y - scale * x))
    tol = np.maximum(1.5 * fine, 4 * rms) + 3 * se * np.abs(t - center)
    ti, ri = _match(r_t, r_pol, t, pol, off, scale, tol)
    if len(ti) < 3:
        raise ValueError(f"only {len(ti)} edges of {via} match {reference}")
    resid = r_t[ri] - (off + scale * t[ti])
    moved = off + scale * t
    overlap = int(np.count_nonzero((moved >= r_t[0] - fine) & (moved <= r_t[-1] + fine)))
    return {"offset_ticks": off, "scale": scale, "reference": reference, "via": via, "method": "edges",
            "matched": int(len(ti)), "edges": int(len(t)), "overlap_edges": overlap,
            "residual_ticks": float(np.sqrt(np.mean(resid ** 2))),
            "scale_ppm": (scale / s0 - 1) * 1e6, "scale_ppm_uncertainty": se / s0 * 1e6,
            "offset_us": off / tick_ref * 1e6, **({"threshold_v": list(analog.thresholds(threshold))} if threshold else {})}


def capture_sha256(path: str | Path) -> str:
    """The identity of a WireSkein file for alignment.json["files"]: SHA-256 of its capture.json."""
    import hashlib
    from . import fileformat
    data = fileformat.get(path, "capture.json")
    if data is None:
        raise ValueError(f"{path}: holds no capture")
    return hashlib.sha256(data).hexdigest()


def save_between(path: str | Path, reference_path: str | Path, entry: dict) -> dict:
    """Add (or replace) the alignment of `path` to `reference_path` in path's alignment.json."""
    doc = load(path) or {"format": FORMAT, "channels": {}}
    doc.setdefault("files", {})[Path(reference_path).name] = {"capture_sha256": capture_sha256(reference_path), **entry}
    save(path, doc)
    return doc


def apply(cap: Capture, alignment: dict) -> Capture:
    """The capture with its analog channels on the aligned time (a new Capture;
    the logic channels and `cap` are not changed)."""
    if alignment.get("format") != FORMAT:
        raise ValueError(f"alignment format {alignment.get('format')!r}, this version reads {FORMAT!r}")
    out = []
    for a in cap.analog:
        c = alignment["channels"].get(a.name)
        if c is None:
            out.append(a)
            continue
        scale = Fraction(c["scale"]).limit_denominator(10**12)
        t0 = Fraction(c["offset_ticks"]).limit_denominator(10**6) + scale * a.t0_ticks
        out.append(AnalogTrace(a.name, a.values, a.rate_hz / scale, t0, a.encoding, a.width, a.value_bits, a.zero,
                               a.scale_nv, a.unit, {**a.acquisition, "aligned": True}))
    return Capture(cap.rate, cap.n_samples, cap.channels, cap.meta, out)


def save(path: str | Path, alignment: dict) -> None:
    """Store it in a WireSkein file as attach/alignment.json (replacing an older one; the alignments to
    other files it held are kept unless `alignment` has its own "files")."""
    from . import fileformat
    old = load(path) if Path(path).exists() else None
    if old and old.get("files") and "files" not in alignment:
        alignment = {**alignment, "files": old["files"]}
    fileformat.attach(path, NAME, json.dumps(alignment, indent=1), replace=True)


def load(path: str | Path) -> dict | None:
    """The alignment stored in a WireSkein file, or None."""
    from . import fileformat
    data = fileformat.attachments(path).get(NAME)
    return json.loads(data) if data else None
