"""Inspect signal primitives and marker roles before protocol specific interpretation.

This is an experiment on known fixtures, not a general protocol classifier.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from pathlib import Path

import numpy as np

from probe import load_module


def edges(values: np.ndarray) -> np.ndarray:
    return np.flatnonzero(np.diff(values.astype(np.int8))) + 1


def channel_summary(values: np.ndarray, rate: float) -> dict:
    change = edges(values)
    if not len(change):
        return {"edges": 0, "idle_level": int(values[0])}
    runs = np.diff(np.r_[0, change, len(values)])
    active_runs = runs[(runs >= 5) & (runs < rate * 100e-6)]
    modal = None
    if len(active_runs):
        widths, counts = np.unique(active_runs, return_counts=True)
        modal = int(widths[np.argmax(counts)])
    return {
        "edges": len(change),
        "idle_level": int(values[0]),
        "high_fraction": round(float(np.mean(values)), 6),
        "modal_short_run_samples": modal,
        "runs_1_to_3_samples": int(np.count_nonzero(runs <= 3)),
    }


def clock_gaps(values: np.ndarray, rate: float, steady: tuple[int, int] | None = None) -> dict:
    change = edges(values)
    rises = change[values[change] == 1]
    gap = np.diff(rises)
    local = gap[(gap >= 3) & (gap <= rate * 100e-6)]
    widths, counts = np.unique(local, return_counts=True)
    top = sorted(zip(widths, counts), key=lambda pair: (-pair[1], pair[0]))[:6]
    out = {
        "rising_edges": len(rises),
        "local_gaps": len(local),
        "top_local_gap_samples": [[int(a), int(b)] for a, b in top],
    }
    if steady is not None:
        selected = gap[(gap >= steady[0]) & (gap <= steady[1])]
        out["steady_window_samples"] = list(steady)
        out["steady_gaps"] = len(selected)
        out["steady_median_samples"] = float(np.median(selected))
        out["steady_p05_p95_samples"] = [float(np.percentile(selected, q)) for q in (5, 95)]
        out["steady_mad_samples"] = float(np.median(np.abs(selected - np.median(selected))))
        out["steady_frequency_hz"] = round(rate / np.median(selected), 3)
    return out


def level_widths(values: np.ndarray, rate: float, band: tuple[int, int]) -> dict:
    change = edges(values)
    bounds = np.r_[0, change, len(values)]
    widths = np.diff(bounds)
    levels = values[bounds[:-1]]
    out = {}
    for level, name in ((0, "low"), (1, "high")):
        chosen = widths[(levels == level) & (widths >= band[0]) & (widths <= band[1])]
        out[name] = {
            "runs": len(chosen),
            "mean_samples": round(float(np.mean(chosen)), 4) if len(chosen) else None,
            "p05_p95_samples": [float(np.percentile(chosen, q)) for q in (5, 95)] if len(chosen) else None,
            "inverse_mean_width_hz": round(rate / np.mean(chosen), 1) if len(chosen) else None,
        }
    return out


def data_edges_while_clock_high(clock: np.ndarray, data: np.ndarray) -> dict:
    change = edges(data)
    return {
        "data_edges": len(change),
        "while_clock_high": int(np.count_nonzero(clock[change])),
        "fraction": round(float(np.mean(clock[change])), 4) if len(change) else None,
    }


def uart_like_trial(values: np.ndarray, bit_samples: float) -> dict:
    """Deliberately weak 8N1 trial to expose false positives on clock signals."""
    change = edges(values)
    falling = change[values[change] == 0]
    previous = change[np.searchsorted(change, falling) - 1]
    starts = falling[(falling - previous) >= bit_samples * 0.8]
    sample_offsets = np.rint((np.arange(10) + 0.5) * bit_samples).astype(int)
    starts = starts[starts + sample_offsets[-1] < len(values)]
    sampled = values[starts[:, None] + sample_offsets]
    valid = sampled[(sampled[:, 0] == 0) & (sampled[:, 9] == 1)]
    octets = (valid[:, 1:9] * (1 << np.arange(8))).sum(axis=1).astype(int)
    counts = Counter(map(int, octets))
    return {
        "candidate_starts": len(starts),
        "start_stop_shape": len(valid),
        "start_stop_fraction": round(len(valid) / len(starts), 4) if len(starts) else None,
        "distinct_octets": len(counts),
        "top_octets": [[f"{byte:02x}", count] for byte, count in counts.most_common(3)],
    }


def inspect_i2c(path: Path, decoder, rvswd) -> dict:
    samples, rate = rvswd.load(str(path))
    lines = {name: ((samples >> bit) & 1) for bit, name in enumerate(("UART_TX", "SCL", "SDA"))}
    events = decoder.load_events(path)
    markers = decoder.parse_markers(events)
    transactions = decoder.parse_transactions(events)
    # The byte trial is intentionally not treated as an identification score.
    return {
        "source": str(path),
        "known_signal_roles": ["UART_TX", "SCL", "SDA"],
        "channels": {name: channel_summary(values, rate) for name, values in lines.items()},
        "scl_rising_gaps": clock_gaps(lines["SCL"], rate, (75, 85)),
        "scl_level_widths_38_to_42": level_widths(lines["SCL"], rate, (38, 42)),
        "uart_tx_short_runs_68_to_71": level_widths(lines["UART_TX"], rate, (68, 71)),
        "sda_edges_vs_scl": data_edges_while_clock_high(lines["SCL"], lines["SDA"]),
        "uart_tx_8n1_weak_trial": uart_like_trial(lines["UART_TX"], rate / 115200),
        "scl_8n1_false_trial": uart_like_trial(lines["SCL"], 40),
        "marker_kinds": dict(Counter(marker["kind"] for marker in markers)),
        "structured_marker_payloads_json_valid": all(
            isinstance(json.loads(marker["arg"]), dict)
            for marker in markers if marker["kind"] in ("INPUT", "RESULT")
        ),
        "marker_examples": {kind: next(marker["arg"] for marker in markers if marker["kind"] == kind)
                            for kind in dict.fromkeys(marker["kind"] for marker in markers)},
        "decoded_i2c_transactions": len(transactions),
    }


def inspect_wch(path: Path, rvswd, swio, kind: str) -> dict:
    samples, rate = rvswd.load(str(path))
    names = ("SWCLK", "SWDIO") if kind == "rvswd" else ("SWIO",)
    lines = {name: ((samples >> bit) & 1) for bit, name in enumerate(names)}
    result = {
        "source": str(path), "known_signal_roles": list(names),
        "channels": {name: channel_summary(values, rate) for name, values in lines.items()},
    }
    if kind == "rvswd":
        result["clock_rising_gaps"] = clock_gaps(lines["SWCLK"], rate)
        result["data_edges_vs_clock"] = data_edges_while_clock_high(lines["SWCLK"], lines["SWDIO"])
        frames, _ = rvswd.frames(str(path), k=1, k_frame=3)
        result["rvswd_frame_shapes"] = dict(Counter(
            "known_length" if rvswd.decode(frame[3]) else "unknown_length"
            for frame in frames
        ))
    else:
        result["pulse_rising_gaps"] = clock_gaps(lines["SWIO"], rate)
        frames, _ = swio.frames(str(path))
        result["swio_frame_shapes"] = dict(Counter(
            "known_length" if swio.decode(frame[1]) else "unknown_length"
            for frame in frames
        ))
    return result


def synthetic_spi() -> dict:
    """Two generated 8-bit SPI transfers to exercise clock/data/select features."""
    clock, mosi, miso, select = [], [], [], []
    for tx, rx in ((0xA5, 0x3C), (0x5A, 0xC3)):
        for _ in range(40):
            clock.append(0); mosi.append(0); miso.append(0); select.append(1)
        for bit in range(7, -1, -1):
            for pos in range(20):
                clock.append(int(pos >= 10))
                mosi.append((tx >> bit) & 1)
                miso.append((rx >> bit) & 1)
                select.append(0)
        for _ in range(40):
            clock.append(0); mosi.append(0); miso.append(0); select.append(1)
    lines = {name: np.array(values, dtype=np.uint8) for name, values in
             (("CLK", clock), ("MOSI", mosi), ("MISO", miso), ("CS", select))}
    return {
        "origin": "generated; 20 MHz sampling, 1 MHz clock, SPI mode 0, two transfers",
        "channels": {name: channel_summary(values, 20_000_000) for name, values in lines.items()},
        "clock_rising_gaps": clock_gaps(lines["CLK"], 20_000_000, (20, 20)),
        "mosi_edges_while_clock_high": data_edges_while_clock_high(lines["CLK"], lines["MOSI"]),
        "clock_rises_while_cs_low": int(np.count_nonzero(
            lines["CS"][edges(lines["CLK"])[lines["CLK"][edges(lines["CLK"])] == 1]] == 0)),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--i2c-root", type=Path, default=Path.home() / "dev" / "I2CDeviceDB")
    parser.add_argument("--wch-root", type=Path, default=Path.home() / "dev_wch" / "wch-protocols")
    args = parser.parse_args()
    decoder = load_module("foundation_i2c_decoder", args.i2c_root / "tools" / "decode.py")
    sys.path.insert(0, str(args.wch_root / "captures" / "tools"))
    import rvswd
    import swio

    fixture = args.wch_root / "captures" / "fixtures" / "wire-linke-p4-2026-09-25"
    result = {
        "i2c": [inspect_i2c(path, decoder, rvswd) for path in
                sorted((args.i2c_root / "captures" / "raw").glob("*.sr"))],
        "rvswd": inspect_wch(fixture / "v203-100mhz" / "target_info.sr", rvswd, swio, "rvswd"),
        "swio": inspect_wch(fixture / "v003" / "target_info.sr", rvswd, swio, "swio"),
        "spi_generated": synthetic_spi(),
    }
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
