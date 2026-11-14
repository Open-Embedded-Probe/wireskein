"""How a check reads at a given number of samples in T, the shortest length it looks at: synthetic captures (known
truth, gen.py) taken at k samples per T, at a random phase of the sample grid, through the checks a test uses
(wireskein.verify). Not part of the package; it is what docs/capturing.md §2's rates rest on.

    uv run python eval/rates.py [--trials N] [--out work/rates.json]

Rows:
- decode: i2c / spi / uart with their expected bytes, k samples in T, the clock's shorter half or a bit; clean, and
  with each edge moved by a normal of SD T/20 (a clock's own jitter, skew between lines). I2C twice: SDA changing
  mid-low (hold T/2) and soon after SCL falls (hold T/10, as many controllers do; clean only, a jitter near the hold
  would put the lines' changes in the other order).
- i2c_noise: I2C at 100 kHz with three pulses of 20 ns on each line, anywhere (a reflection, a neighbour picked up),
  read through the check's spike filter (spike_max_s 50 ns), from 2 to 200 MS/s: a slow capture sees a pulse now
  and then, as a whole sample, and cannot tell it from the signal; a fast one sees each, as short as it is.
- width: a pulse of T measured from its edges (the error, in T).
- spike: a pulse of width T on SDA while SCL is low, as the i2c check's bus_glitch notice (spike_max_s = 1.5 T): how
  often it is told, and the width it is told with (in T).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

from wireskein._engine import gen
from wireskein.runlog import i2c, spi, uart
from wireskein.verify import CHECKS

KS = [2, 2.5, 3, 4, 5, 6, 8, 10, 15, 20]
KS_SPIKE = [0.25, 0.5, 1, 1.5, 2, 3, 5, 10]


NOISE: tuple | None = None    # set by main: (lines, pulses a capture, width in s): pulses of that width on the wire
SPIKE_RATES = [2e6, 5e6, 10e6, 20e6, 40e6, 50e6, 100e6, 200e6]


def _cap(waves, rate, end, rng, jitter_s=0.0, T=None):
    phase = float(rng.uniform(0, 1 / rate))            # where the sample grid falls on the wire
    if NOISE and T:
        lines, count, width_s = NOISE
        for n in lines:
            if n in waves:
                w = waves[n]
                at = rng.uniform(20e-6, end, count)
                w.times = sorted(w.times + list(at) + list(at + width_s))
    return gen.quantize(waves, rate, end + 20e-6, rng, jitter_s=jitter_s, t_start=-phase)


def _run(x, cap):
    ok, got, why = CHECKS[x["kind"]](cap, x)
    return ok is True, got


def case_i2c(k, rng, jitter, data_at=0.5):
    f = 100e3
    T = 0.5 / f                                        # gen.i2c: SCL high and low each half a period
    data = [int(b) for b in rng.integers(0, 256, 4)]
    scl, sda = gen.Wave(1), gen.Wave(1)
    tx = [{"addr": 0x42, "rw": "write", "bytes": data}, {"addr": 0x42, "rw": "read", "bytes": data[:2]}]
    end = gen.i2c(scl, sda, 20e-6, tx, f, data_at=data_at)
    cap = _cap({"SCL": scl, "SDA": sda}, k / T, end, rng, jitter * T, T)
    return _run(i2c("SCL", "SDA", [{k2: v for k2, v in t.items()} for t in tx]), cap)[0]


def case_spi(k, rng, jitter):
    f = 1e6
    T = 0.5 / f
    tx = bytes(int(b) for b in rng.integers(0, 256, 4))
    clk, mosi, cs = gen.Wave(0), gen.Wave(0), gen.Wave(1)
    end = gen.spi(clk, mosi, None, cs, 5e-6, [(tx, b"")], f, mode=0)
    cap = _cap({"CLK": clk, "MOSI": mosi, "CS": cs}, k / T, end, rng, jitter * T, T)
    return _run(spi("CLK", "MOSI", cs="CS", mode=0, mosi_bytes=tx.hex()), cap)[0]


def case_uart(k, rng, jitter):
    baud = 115200
    T = 1 / baud
    data = bytes(int(b) for b in rng.integers(0, 256, 6))
    w = gen.Wave(1)
    end = gen.uart(w, 12 / baud, data, baud, gap_bits=1.0)   # a character's idle first: a start bit is sure
    cap = _cap({"TX": w}, k / T, end, rng, jitter * T, T)
    return _run(uart("TX", baud, data.hex()), cap)[0]


def case_i2c_noise(rate, rng):
    f = 100e3
    data = [int(b) for b in rng.integers(0, 256, 4)]
    scl, sda = gen.Wave(1), gen.Wave(1)
    tx = [{"addr": 0x42, "rw": "write", "bytes": data}, {"addr": 0x42, "rw": "read", "bytes": data[:2]}]
    end = gen.i2c(scl, sda, 20e-6, tx, f, data_at=0.1)
    cap = _cap({"SCL": scl, "SDA": sda}, rate, end, rng, 0.0, 1.0)
    return _run(i2c("SCL", "SDA", tx, spike_max_s=50e-9), cap)[0]


def width_error(k, rng):
    """A pulse of T: its measured width minus T, in T."""
    T = 1e-6 * float(rng.uniform(1, 1.2))             # not a whole number of samples
    rate = k / 1e-6
    t0 = 5e-6 + float(rng.uniform(0, 1 / rate))
    s = np.round(np.array([t0, t0 + T]) * rate)
    return float((s[1] - s[0]) / rate - T) / T


def spike_told(k, rng):
    """A pulse of T on SDA while SCL is low, the i2c check's spike_max_s at 1.5 T: told as bus_glitch, and with what
    width (in T)."""
    f = 100e3
    T = 50e-9
    rate = k / T
    scl, sda = gen.Wave(1), gen.Wave(1)
    end = gen.i2c(scl, sda, 20e-6, [{"addr": 0x42, "rw": "write", "bytes": [0x00]}], f)
    # a spike in the second half of the address's first low of SCL (gen.i2c changes SDA a quarter bit in)
    low0 = scl.times[1]
    at = low0 + 0.6 / f / 4
    sda.times = sorted(sda.times + [at, at + T])
    cap = _cap({"SCL": scl, "SDA": sda}, rate, end, rng)
    x = i2c("SCL", "SDA", spike_max_s=1.5 * T)
    _, got = _run(x, cap)
    g = [n for n in got.get("notices", []) if n["kind"] == "bus_glitch"]
    return (len(g) > 0, g[0]["width_ticks"] / k if g else None)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--trials", type=int, default=100)
    ap.add_argument("--out", type=Path, default=Path(__file__).parent / "work" / "rates.json")
    a = ap.parse_args(argv)
    rows = []
    cases = (("i2c hold T/2", case_i2c), ("i2c hold T/10", lambda k, r, j: case_i2c(k, r, j, 0.1)),
             ("spi", case_spi), ("uart", case_uart))
    global NOISE
    for name, fn in cases:
        # no jitter where it is near the hold itself: the lines would change in the other order, a START or STOP
        for cond in ("clean", "jitter T/20") if "T/10" not in name else ("clean",):
            jitter = 0.05 if cond.startswith("jitter") else 0.0
            for k in KS:
                rng = np.random.default_rng(int(k * 100) + len(cond))
                ok = sum(fn(k, rng, jitter) for _ in range(a.trials))
                rows.append({"row": "decode", "protocol": name, "condition": cond, "k": k, "ok": ok / a.trials})
                print(f"decode {name:13s} {cond:11s} k {k:>4}: {ok}/{a.trials}", file=sys.stderr)
    # I2C at 100 kHz with three pulses of 20 ns on each line (a reflection, a neighbour picked up), read through the
    # i2c check's spike filter (spike_max_s 50 ns): rates from 2 to 200 MS/s, k in T = spike_max_s
    for rate in SPIKE_RATES:
        rng = np.random.default_rng(int(rate / 1e5))
        NOISE = (("SCL", "SDA"), 3, 20e-9)
        ok = sum(case_i2c_noise(rate, rng) for _ in range(a.trials))
        rows.append({"row": "i2c_noise", "rate_hz": rate, "k_spike": rate * 50e-9, "ok": ok / a.trials})
        print(f"i2c 100 kHz, 20 ns pulses, {rate / 1e6:>5.0f} MS/s (k {rate * 50e-9:.2f} of 50 ns): "
              f"{ok}/{a.trials}", file=sys.stderr)
    NOISE = None
    for k in KS:
        rng = np.random.default_rng(int(k * 100))
        e = [width_error(k, rng) for _ in range(1000)]
        rows.append({"row": "width", "k": k, "err_max_T": round(max(abs(x) for x in e), 3)})
        print(f"width k {k:>4}: worst error {max(abs(x) for x in e):.3f} T", file=sys.stderr)
    for k in KS_SPIKE:
        rng = np.random.default_rng(int(k * 100))
        r = [spike_told(k, rng) for _ in range(a.trials)]
        told = sum(1 for t, _ in r if t)
        w = [x for t, x in r if t]
        rows.append({"row": "spike", "k": k, "told": told / a.trials,
                     "width_T": [min(w), max(w)] if w else None})
        print(f"spike k {k:>4}: told {told}/{a.trials}, width {w and (round(min(w), 2), round(max(w), 2))} T",
              file=sys.stderr)
    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text(json.dumps(rows, indent=1))


if __name__ == "__main__":
    main()
