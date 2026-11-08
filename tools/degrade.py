"""Make a capture worse on purpose, for test data: a lower sample rate, glitches, a neighbouring line picked up,
ringing at edges, timing jitter. The result is a WireSkein file whose meta.degraded says what was done (each step
with its settings), from what (the source's file and sha256) and the random seed, so it can be made again exactly.
Not part of the package.

    uv run python tools/degrade.py IN OUT [--seed N] [--pins A,B]
        [--glitch LINE:PER_EDGE[:WIDTH]]          pulses of WIDTH ticks (1) at random times, PER_EDGE of the line's changes
        [--crosstalk FROM:TO:PROB[:WIDTH]]        a pulse on TO at each change of FROM, with probability PROB
        [--ring LINE:PROB[:PULSES[:WIDTH]]]       after each change of LINE (PROB), PULSES pulses (2) of WIDTH ticks (1)
        [--jitter LINE:SD]                        each change of LINE moved by a normal with SD ticks
        [--every N[:PHASE]]                       then sampled every N ticks (from PHASE; random by default)

The noise is added at the source's ticks first (as it is on the wire), then the lower rate samples it. Each option
may be given more than once; the steps run in the order given (--every last). Logic lines only (analog lines and
lines kept as changes at a coarser step are refused). Without --seed one is drawn and printed.
"""

from __future__ import annotations

import argparse
import hashlib
import secrets
import sys
from pathlib import Path

import numpy as np

from wireskein import __version__
from wireskein._engine import fileio
from wireskein._engine.model import Capture, Channel
from wireskein.decode import load, only


def toggle(edges: np.ndarray, at: np.ndarray) -> np.ndarray:
    """The line with a change added at each tick of `at` (two at one tick cancel): its edges, sorted."""
    both = np.concatenate([np.asarray(edges, np.int64), np.asarray(at, np.int64)])
    v, c = np.unique(both, return_counts=True)
    return v[c % 2 == 1]


def pulses(start: np.ndarray, width: int) -> np.ndarray:
    return np.concatenate([start, start + width])


def glitch(ch: Channel, n_ticks: int, rng, per_edge: float, width: int = 1) -> Channel:
    n = int(rng.poisson(per_edge * max(1, len(ch.edges))))
    at = rng.integers(1, max(2, n_ticks - width), n)
    return Channel(ch.name, ch.initial, toggle(ch.edges, pulses(at, width)), ch.step, ch.phase, ch.acquisition)


def crosstalk(src: Channel, dst: Channel, rng, prob: float, width: int = 1) -> Channel:
    e = np.asarray(src.edges, np.int64)
    at = e[rng.random(len(e)) < prob]
    return Channel(dst.name, dst.initial, toggle(dst.edges, pulses(at, width)), dst.step, dst.phase,
                   dst.acquisition)


def ring(ch: Channel, rng, prob: float, count: int = 2, width: int = 1) -> Channel:
    e = np.asarray(ch.edges, np.int64)
    at = e[rng.random(len(e)) < prob]
    add = [at + width * (2 * i + 1) for i in range(count)] + [at + width * (2 * i + 2) for i in range(count)]
    return Channel(ch.name, ch.initial, toggle(ch.edges, np.concatenate(add) if add else at[:0]), ch.step, ch.phase,
                   ch.acquisition)


def jitter(ch: Channel, rng, sd: float) -> Channel:
    e = np.asarray(ch.edges, np.int64)
    moved = np.maximum(1, e + np.round(rng.normal(0, sd, len(e))).astype(np.int64))
    return Channel(ch.name, ch.initial, toggle(np.zeros(0, np.int64), moved), ch.step, ch.phase, ch.acquisition)


def every(cap: Capture, n: int, phase: int) -> Capture:
    """The capture sampled every n ticks from `phase`: sample k is the level at tick phase + k * n; a change between
    two samples is seen at the next (several there: their parity)."""
    k_end = (cap.n_samples - phase + n - 1) // n
    chans = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        init = int(ch.level_at(np.array([phase]))[0])
        e = e[e > phase]
        k = (e - phase + n - 1) // n                  # the first sample at or after each change
        k = k[k < k_end]
        v, c = np.unique(k, return_counts=True)
        chans.append(Channel(ch.name, init, v[c % 2 == 1], 1, 0, dict(ch.acquisition)))
    tick = cap.meta.get("tick_hz", cap.rate)
    meta = {**cap.meta, "tick_hz": tick / n}
    return Capture(float(cap.rate) / n, int(k_end), chans, meta)


def _fields(text: str, conv: tuple, defaults: tuple = ()) -> list:
    """NAME:VALUE:... -> the fields converted, the missing ones from `defaults` (for the last ones)."""
    parts = text.split(":")
    need = len(conv) - len(defaults)
    if not need <= len(parts) <= len(conv):
        raise SystemExit(f"{text!r}: {need} to {len(conv)} fields")
    parts += [None] * (len(conv) - len(parts))
    return [c(p) if p is not None else defaults[i - need] for i, (c, p) in enumerate(zip(conv, parts))]


class _Step(argparse.Action):
    """Keeps the noise steps in the order given."""

    def __call__(self, parser, ns, value, option_string=None):
        ns.steps = [*getattr(ns, "steps", []), (self.dest, value)]


def main(argv=None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("input", type=Path)
    ap.add_argument("output", type=Path)
    ap.add_argument("--seed", type=int, default=None)
    ap.add_argument("--pins", default=None, help="only these lines (A,B)")
    for k in ("glitch", "crosstalk", "ring", "jitter"):
        ap.add_argument(f"--{k}", action=_Step)
    ap.add_argument("--every", default=None)
    args = ap.parse_args(argv)
    seed = args.seed if args.seed is not None else secrets.randbelow(2**32)
    rng = np.random.default_rng(seed)
    cap = only(load(args.input), args.pins.split(",") if args.pins else None)
    if cap.analog or cap.events or any(c.step != 1 for c in cap.channels):
        raise SystemExit("logic lines of every sample only (leave the others out with --pins)")
    chans = {c.name: c for c in cap.channels}
    ops = []
    for kind, text in getattr(args, "steps", []):
        if kind == "glitch":
            line, per_edge, width = _fields(text, (str, float, int), (1,))
            chans[line] = glitch(chans[line], cap.n_samples, rng, per_edge, width)
            ops.append({"op": "glitch", "line": line, "per_edge": per_edge, "width": width})
        elif kind == "crosstalk":
            src, dst, prob, width = _fields(text, (str, str, float, int), (1,))
            chans[dst] = crosstalk(chans[src], chans[dst], rng, prob, width)
            ops.append({"op": "crosstalk", "from": src, "to": dst, "prob": prob, "width": width})
        elif kind == "ring":
            line, prob, count, width = _fields(text, (str, float, int, int), (2, 1))
            chans[line] = ring(chans[line], rng, prob, count, width)
            ops.append({"op": "ring", "line": line, "prob": prob, "pulses": count, "width": width})
        elif kind == "jitter":
            line, sd = _fields(text, (str, float))
            chans[line] = jitter(chans[line], rng, sd)
            ops.append({"op": "jitter", "line": line, "sd_ticks": sd})
    out = Capture(cap.rate, cap.n_samples, [chans[c.name] for c in cap.channels], dict(cap.meta))
    if args.every:
        n, phase = _fields(args.every, (int, int), (None,))
        phase = int(rng.integers(0, n)) if phase is None else phase
        out = every(out, n, phase)
        ops.append({"op": "every", "n": n, "phase": phase})
    src = Path(args.input)
    out.meta["extras"] = {}
    for k in ("file", "capture_id", "n_parts", "part"):
        out.meta.pop(k, None)
    fileio.save(args.output, out, degraded={
        "tool": "tools/degrade.py", "wireskein": __version__, "seed": seed, "ops": ops,
        "source": {"file": src.name, "sha256": hashlib.sha256(src.read_bytes()).hexdigest()}})
    print(f"seed {seed}: {len(ops)} steps -> {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
