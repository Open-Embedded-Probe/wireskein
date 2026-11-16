"""Cases: a capture read from a source, transformed (noise, crop, decimate), with per-line truth.

A dataset is a source and a chain of transforms, both written as strings so that a ledger entry names exactly what
was measured:

    source      set:<name>[a:b]   a frozen set under eval/corpus/synth (cases a..b in file order)
                real              eval/corpus/real (the records with about a million edges left out)
                dir:<path>[a:b]   any directory of <id>.wireskein beside <id>.truth.json (the eval/ truth format)
                gen:<profile>[/<stress>][a:b][@<rate>]   eval/synth.py's scenarios of seeds a..b, sampled at <rate>
                                  (the same traffic whatever the rate: a base for decimate, sweeping the rate), or at
                                  the scenario's own
                i2cbench:h417_slow/stream[a:b]   real captures of tests/data/i2c_bench: I2C through weak
                                  pull-ups, 50 times "S, 0x13 write, NACK, P" each (what the device answered: the
                                  summaries beside them), truth made on reading; SCL / SDA renamed D0 / D1
    transforms  "spike:0.1+bounce:0.1"  applied left to right; "" for none
                spike / bounce / xtalk: noise in ticks of the capture (as wide whatever the rate)
                pspike:L:NS / pbounce:L:NS: noise in time (NS nanoseconds), the way a line shows it: wider in
                samples the faster it is taken, missed when shorter than a sample and no sample falls in it

Noise is seeded from the case id and the transform text (never Python's hash()), so a dataset is the same on every
run and machine. The clean capture (the same transforms without the noise) is kept beside the noisy one: stages
that score a cleaning compare against it.
"""

from __future__ import annotations

import hashlib
import json
import re
import zlib
from dataclasses import dataclass, field, replace
from pathlib import Path

import numpy as np

from wireskein import capture as _capture
from wireskein._engine.model import Capture

REPO = Path(__file__).resolve().parents[2]
SETS = REPO / "eval" / "corpus" / "synth"
REAL = REPO / "eval" / "corpus" / "real"

# truth labels of a line (stage lineclass)
LABELS = ("clock", "data", "select", "periodic", "random", "static")


@dataclass
class LineTruth:
    label: str
    period_s: float | None = None      # clock / periodic lines: one period (rising to rising)
    unit_s: float | None = None        # asynchronous lines: one bit
    variable: bool = False             # the rate changes inside the capture (no single value is right)
    nominal: bool = False              # the value is the nominal one (a real record), not the generator's
    protocol: str | None = None
    role: str | None = None


@dataclass
class Case:
    id: str                            # case id + dataset transforms
    dataset: str
    cap: Capture                       # what algorithms see
    clean: Capture | None              # the same without noise (None for real records)
    truth: dict                        # the eval/ truth.json (algorithms never see it)
    lines: dict[str, LineTruth] = field(default_factory=dict)
    input_sha: str = ""


# ---------------- sources ----------------

_RANGE = re.compile(r"^(?P<base>[^\[]+)(\[(?P<a>\d*):(?P<b>\d*)\])?$")


def _slice(paths: list[Path], a: str | None, b: str | None) -> list[Path]:
    return paths[int(a) if a else None:int(b) if b else None]


_GEN = re.compile(r"^gen:(?P<profile>[a-z]+)(/(?P<stress>[a-z]+))?\[(?P<a>\d+):(?P<b>\d+)\](@(?P<rate>[0-9.e+]+))?$")


def source_items(source: str) -> list[str]:
    """The cases of a source: file paths, i2cbench:<path>, or gen:<profile>:<stress>:<seed>:<rate>."""
    g = _GEN.match(source)
    if g:
        rate = float(g["rate"]) if g["rate"] else 0.0          # 0: the scenario's own rate
        return [f"gen:{g['profile']}:{g['stress'] or ''}:{s}:{rate}" for s in range(int(g["a"]), int(g["b"]))]
    m = _RANGE.match(source)
    if m and m["base"].startswith("i2cbench:"):
        d = I2C_BENCH / m["base"][len("i2cbench:"):]
        if not d.is_dir():
            raise FileNotFoundError(d)
        return [f"i2cbench:{p}" for p in _slice(sorted(d.rglob("*.wireskein")), m["a"], m["b"])]
    return [str(p) for p in source_paths(source)]


I2C_BENCH = REPO / "tests" / "data" / "i2c_bench"


def _i2cbench(path: Path) -> tuple[Capture, dict]:
    """A capture of tests/data/i2c_bench/h417_slow: stream/ 50 transactions "S, 0x13 write, NACK, P" (the device
    answered NACK to each: summary-<tag>.json's dut_nack2), noise/<tag>/run<k> one of them; SCL 100 kHz (20 kHz for
    the clk20000 ones)."""
    cap = _capture.read(path).parts[0].view()
    names = {ch.name for ch in cap.channels}
    if names != {"SCL", "SDA"}:
        raise ValueError(f"{path}: channels {sorted(names)}, want SCL and SDA")
    flip = zlib.crc32(path.name.encode()) & 1          # which is D0, and the order: from the name, not the role
    new = {"SCL": "D1" if flip else "D0", "SDA": "D0" if flip else "D1"}
    chans = sorted((replace(ch, name=new[ch.name]) for ch in cap.channels), key=lambda c: c.name)
    one = path.parent.parent.name == "noise"                       # noise/<tag>/run<k>: one transaction
    tag = (path.parent.name if one else path.stem).split("-")[0]
    hz = 20e3 if tag == "clk20000" else 100e3
    rel = path.relative_to(I2C_BENCH)
    truth = {"id": "i2cbench-" + str(rel.with_suffix("")).replace("/", "-"),
             "source": {"path": str(path.relative_to(REPO))},
             "buses": [{"protocol": "i2c", "roles": {"scl": new["SCL"], "sda": new["SDA"]}, "params": {"clock_hz": hz},
                        "expect": {"compare_ack": True,
                                   "transactions": [{"addr": 0x13, "rw": "write", "bytes": [], "addr_ack": False}]
                                   * (1 if one else 50)}}],
             "decoys": [],
             "notes": "real capture; truth: what the controller was told to send and the device's answer (NACK)"}
    return Capture(cap.rate, cap.n_samples, chans), truth


def source_paths(source: str) -> list[Path]:
    m = _RANGE.match(source)
    if not m:
        raise ValueError(f"source {source!r}: set:<name>[a:b], real, dir:<path>[a:b] or gen:<profile>[a:b]@<rate>")
    base, a, b = m["base"], m["a"], m["b"]
    if base.startswith("set:"):
        d = SETS / base[4:]
    elif base == "real":
        d = REAL
    elif base.startswith("dir:"):
        d = Path(base[4:]).expanduser()
        d = d if d.is_absolute() else REPO / d
    else:
        raise ValueError(f"source {source!r}: set:<name>[a:b], real or dir:<path>[a:b]")
    if not d.is_dir():
        raise FileNotFoundError(d)
    paths = sorted(d.glob("*.wireskein"))
    if base == "real":       # the flash writes (about a million edges each) are for large runs only
        paths = [p for p in paths if not ("flash" in p.name and not p.name.startswith("i2cdb"))]
    return _slice(paths, a, b)


def read(ref: str) -> tuple[Capture, dict]:
    if ref.startswith("i2cbench:"):
        return _i2cbench(Path(ref[len("i2cbench:"):]))
    if ref.startswith("gen:"):
        import sys
        sys.path.insert(0, str(REPO / "eval"))
        import synth  # eval/synth.py
        _, profile, stress, seed, rate = ref.split(":")
        cap, truth = synth.scenario(int(seed), profile, stress or None, rate=float(rate) or None)
        if float(rate):
            truth["id"] += f"@{float(rate) / 1e6:g}M"
        return cap, truth
    path = Path(ref)
    truth = json.loads(path.with_suffix("").with_suffix(".truth.json").read_text())
    return _capture.read(path).parts[0].view(), truth


# ---------------- transforms ----------------

def _toggle(edges: np.ndarray, extra: np.ndarray) -> np.ndarray:
    """Add toggles: two on the same tick cancel."""
    u, c = np.unique(np.concatenate([np.asarray(edges, np.int64), np.asarray(extra, np.int64)]), return_counts=True)
    return u[c % 2 == 1]


def _rng(case_id: str, spec: str) -> np.random.Generator:
    return np.random.default_rng(zlib.crc32(f"{case_id}|{spec}".encode()))


def noise_spike(cap: Capture, lvl: float, rng) -> Capture:
    """1-tick pulses at random places, lvl per real edge of the channel."""
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        if len(e) == 0:
            out.append(ch)
            continue
        g = rng.integers(1, cap.n_samples - 2, rng.poisson(len(e) * lvl))
        out.append(replace(ch, edges=_toggle(e, np.concatenate([g, g + 1]))))
    return Capture(cap.rate, cap.n_samples, out)


def noise_bounce(cap: Capture, lvl: float, rng) -> Capture:
    """At a share lvl of the real edges, a pulse back to the old level 1-3 ticks wide within 4 ticks after the edge
    (and a second one at half of those): a slow or ringing edge crossing the threshold more than once."""
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        if len(e) == 0:
            out.append(ch)
            continue
        sel = e[rng.random(len(e)) < lvl]
        parts = []
        for keep in (1.0, 0.5):
            s = sel[rng.random(len(sel)) < keep]
            a = s + rng.integers(1, 5, len(s))
            parts += [a, a + rng.integers(1, 4, len(s))]
        extra = np.concatenate(parts)
        out.append(replace(ch, edges=_toggle(e, extra[extra < cap.n_samples])))
    return Capture(cap.rate, cap.n_samples, out)


def noise_xtalk(cap: Capture, lvl: float, rng) -> Capture:
    """At a share lvl of the next channel's edges, a 1-tick pulse on this one (coupling between neighbours)."""
    chans = cap.channels
    out = []
    for i, ch in enumerate(chans):
        e = np.asarray(ch.edges, np.int64)
        if len(e) == 0 or len(chans) < 2:
            out.append(ch)
            continue
        ne = np.asarray(chans[(i + 1) % len(chans)].edges, np.int64)
        g = ne[rng.random(len(ne)) < lvl] + 1
        g = g[g < cap.n_samples - 1]
        out.append(replace(ch, edges=_toggle(e, np.concatenate([g, g + 1]))))
    return Capture(cap.rate, cap.n_samples, out)


def _pulses(e0: np.ndarray, width: np.ndarray) -> np.ndarray:
    """Toggles for pulses from e0 (ticks, real-valued) width ticks long, as samples on whole ticks see them: a level
    is seen from the first tick at or after its start, so a pulse no tick falls in is not seen (both toggles on one
    tick cancel)."""
    return np.concatenate([np.ceil(e0), np.ceil(e0 + width)]).astype(np.int64)


def noise_pspike(cap: Capture, lvl: float, ns: float, rng) -> Capture:
    """Pulses ns nanoseconds wide at random places, lvl per real edge of the channel."""
    w = ns * 1e-9 * float(cap.rate)
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        if len(e) == 0:
            out.append(ch)
            continue
        t0 = rng.uniform(1, cap.n_samples - 2 - w, rng.poisson(len(e) * lvl))
        extra = _pulses(t0, np.full(len(t0), w))
        out.append(replace(ch, edges=_toggle(e, extra[(extra > 0) & (extra < cap.n_samples)])))
    return Capture(cap.rate, cap.n_samples, out)


def noise_pbounce(cap: Capture, lvl: float, ns: float, rng) -> Capture:
    """At a share lvl of the real edges, within ns nanoseconds after it, a pulse back to the old level 10-40 % of ns
    wide, and at half of those a second one: a slow or ringing edge crossing the threshold more than once."""
    span = ns * 1e-9 * float(cap.rate)
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        if len(e) == 0:
            out.append(ch)
            continue
        sel = e[rng.random(len(e)) < lvl].astype(np.float64)
        parts = []
        for keep, at in ((1.0, (0.0, 0.4)), (0.5, (0.5, 0.8))):
            s = sel[rng.random(len(sel)) < keep]
            parts.append(_pulses(s + span * rng.uniform(*at, len(s)), span * rng.uniform(0.1, 0.4, len(s))))
        extra = np.concatenate(parts)
        out.append(replace(ch, edges=_toggle(e, extra[(extra > 0) & (extra < cap.n_samples)])))
    return Capture(cap.rate, cap.n_samples, out)


def crop(cap: Capture, a: float, b: float) -> Capture:
    """The part from a to b (fractions of the length)."""
    s0, s1 = int(a * cap.n_samples), int(b * cap.n_samples)
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        initial = int(ch.initial ^ (int(np.sum(e <= s0)) & 1))
        out.append(replace(ch, initial=initial, edges=e[(e > s0) & (e < s1)] - s0))
    return Capture(cap.rate, s1 - s0, out)


def decimate(cap: Capture, k: int) -> Capture:
    """Sampled k times slower (every k-th tick)."""
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64) // k
        u, c = np.unique(e, return_counts=True)
        e = u[c % 2 == 1]
        initial = int(ch.initial ^ (int(np.sum(e == 0)) & 1))
        out.append(replace(ch, initial=initial, edges=e[e > 0], step=max(1, ch.step // k), phase=ch.phase // k))
    return Capture(cap.rate / k, cap.n_samples // k, out)


NOISE = {"spike": noise_spike, "bounce": noise_bounce, "xtalk": noise_xtalk}
TIMED_NOISE = {"pspike": noise_pspike, "pbounce": noise_pbounce}


def apply(cap: Capture, clean: Capture | None, chain: str, case_id: str) -> tuple[Capture, Capture | None]:
    for spec in [s for s in chain.split("+") if s]:
        kind, *args = spec.split(":")
        if kind in NOISE:
            cap = NOISE[kind](cap, float(args[0]), _rng(case_id, spec))
        elif kind in TIMED_NOISE:
            cap = TIMED_NOISE[kind](cap, float(args[0]), float(args[1]), _rng(case_id, spec))
        elif kind == "crop":
            cap = crop(cap, float(args[0]), float(args[1]))
            clean = crop(clean, float(args[0]), float(args[1])) if clean else None
        elif kind == "decimate":
            cap = decimate(cap, int(args[0]))
            clean = decimate(clean, int(args[0])) if clean else None
        else:
            raise ValueError(f"transform {spec!r}: spike:L, bounce:L, xtalk:L, pspike:L:NS, pbounce:L:NS, crop:A:B "
                             "or decimate:K")
    return cap, clean


# ---------------- truth per line ----------------

ASYNC = {"uart": ("data",), "lin": ("data",), "dmx512": ("data",), "scpi": ("rx", "tx"), "can": ("data",)}
CLOCK = {"i2c": "scl", "spi": "clk", "rvswd": "clk", "swd": "swclk"}


def line_truth(truth: dict, cap: Capture, clean: Capture) -> dict[str, LineTruth]:
    real = "generator" not in truth
    freqhop = (truth.get("generator") or {}).get("stress") == "freqhop"
    out: dict[str, LineTruth] = {}
    for b in truth.get("buses", []):
        pr, pm = b["protocol"], b.get("params", {})
        for role, ch in b["roles"].items():
            if pr in ASYNC and role in ASYNC[pr]:
                baud = pm.get("baud") or pm.get("bitrate")
                out[ch] = LineTruth("data", unit_s=1 / baud if baud else None, variable="baud_segments" in pm,
                                    nominal=real, protocol=pr, role=role)
            elif CLOCK.get(pr) == role:
                hz = pm.get("clock_hz")
                out[ch] = LineTruth("clock", period_s=1 / hz if hz else None, variable=freqhop, nominal=real,
                                    protocol=pr, role=role)
            elif role == "cs":
                out[ch] = LineTruth("select", protocol=pr, role=role)
            else:
                out[ch] = LineTruth("data", protocol=pr, role=role)
    for d in truth.get("decoys") or []:
        k = d["kind"]
        if k in ("clock", "burst_clock"):
            out[d["channel"]] = LineTruth("clock", period_s=1 / d["freq"], role="decoy")
        elif k == "pwm":
            out[d["channel"]] = LineTruth("periodic", period_s=1 / d["freq"], role="decoy")
        elif k == "random":
            out[d["channel"]] = LineTruth("random", role="decoy")
    for ch in cap.channels:
        ref = clean.channel(ch.name) if clean else ch
        if len(ref.edges) < 2:       # nothing of it in this part (a crop): there is no rate to find
            out[ch.name] = LineTruth("static", role=out[ch.name].role if ch.name in out else None)
        out.setdefault(ch.name, LineTruth("static"))
    return out


def input_sha(cap: Capture) -> str:
    h = hashlib.sha256(f"{cap.rate}|{cap.n_samples}".encode())
    for ch in cap.channels:
        h.update(f"|{ch.name}|{ch.initial}|{ch.step}|".encode())
        h.update(np.asarray(ch.edges, np.int64).tobytes())
    return h.hexdigest()[:16]


def load_case(ref: str, source: str, chain: str) -> Case:
    cap, truth = read(ref)
    # a generated case is its own clean reference, unless its frozen set already holds noise
    gen = truth.get("generator")
    clean = cap if gen and gen.get("stress") != "glitch" else None
    cap, clean = apply(cap, clean, chain, truth["id"])
    dataset = source + (f"+{chain}" if chain else "")
    return Case(truth["id"] + (f"+{chain}" if chain else ""), dataset, cap, clean, truth,
                line_truth(truth, cap, clean or cap), input_sha(cap))
