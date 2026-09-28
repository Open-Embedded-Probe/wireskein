"""Read-only I2C wire audit on saved .sr files, with in-memory fault probes.

This checks signal-level properties independently of the saved transaction decoder.
It is an experiment, not a general I2C validator or a WireSkein implementation.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import zipfile
from pathlib import Path

import numpy as np


def load_sr(path: Path) -> tuple[bytes, int, dict[str, int]]:
    with zipfile.ZipFile(path) as archive:
        meta = archive.read("metadata").decode()
        width = re.search(r"(?m)^unitsize=(\d+)\s*$", meta)
        if width is None or int(width.group(1)) != 1:
            raise ValueError(f"Only unitsize=1 has been validated: {path}")
        rate = re.search(r"(?m)^samplerate=(\d+)\s*(Hz|kHz|MHz|GHz)?\s*$", meta, re.I)
        if rate is None:
            raise ValueError(f"No sample rate: {path}")
        scale = {"hz": 1, "khz": 1000, "mhz": 1000000, "ghz": 1000000000}
        rate_hz = int(rate.group(1)) * scale[(rate.group(2) or "Hz").lower()]
        channels = {name: int(number) - 1 for number, name in re.findall(r"(?m)^probe(\d+)=(.+?)\s*$", meta)}
        if "SCL" not in channels or "SDA" not in channels:
            raise ValueError(f"SCL/SDA labels are required: {path}")
        chunks = sorted((name for name in archive.namelist() if re.fullmatch(r"logic-1-\d+", name)),
                        key=lambda name: int(name.rsplit("-", 1)[1]))
        if not chunks:
            raise ValueError(f"No logic samples: {path}")
        return b"".join(archive.read(name) for name in chunks), rate_hz, channels


def audit(data: bytes, channels: dict[str, int]) -> dict:
    samples = np.frombuffer(data, dtype=np.uint8)
    if len(samples) < 2:
        raise ValueError("At least two samples are needed")
    scl = (samples >> channels["SCL"]) & 1
    sda = (samples >> channels["SDA"]) & 1
    scl_edges = np.flatnonzero(scl[1:] != scl[:-1]) + 1
    sda_edges = np.flatnonzero(sda[1:] != sda[:-1]) + 1
    simultaneous = np.intersect1d(scl_edges, sda_edges)
    high = sda_edges[(scl[sda_edges - 1] == 1) & (scl[sda_edges] == 1)]
    rising_clock = scl_edges[scl[scl_edges] == 1]

    busy = False
    starts: list[int] = []
    repeated: list[int] = []
    stops: list[int] = []
    orphan_stops: list[int] = []
    empty_spans: list[list[int]] = []
    segments_checked = 0
    spans: list[tuple[int, int]] = []
    current_start: int | None = None
    segment_start: int | None = None
    for index in high:
        at = int(index)
        if sda[at] == 0:
            starts.append(at)
            if busy:
                repeated.append(at)
                assert segment_start is not None
                segments_checked += 1
                clock_count = int(np.searchsorted(rising_clock, at, side="left") -
                                  np.searchsorted(rising_clock, segment_start, side="right"))
                if clock_count == 0:
                    empty_spans.append([segment_start, at])
            else:
                busy = True
                current_start = at
            segment_start = at
        else:
            stops.append(at)
            if not busy:
                orphan_stops.append(at)
            else:
                assert current_start is not None and segment_start is not None
                spans.append((current_start, at))
                segments_checked += 1
                clock_count = int(np.searchsorted(rising_clock, at, side="left") -
                                  np.searchsorted(rising_clock, segment_start, side="right"))
                if clock_count == 0:
                    empty_spans.append([segment_start, at])
            busy = False
            current_start = None
            segment_start = None
    if busy:
        assert current_start is not None
        spans.append((current_start, len(samples)))

    # A clock outside every START..STOP span can be a bus-recovery pulse or a
    # capture/decoder problem. Report it; do not call it a protocol violation.
    claimed_clock = np.zeros(len(rising_clock), dtype=bool)
    for start, end in spans:
        lo = np.searchsorted(rising_clock, start, side="right")
        hi = np.searchsorted(rising_clock, end, side="left")
        claimed_clock[lo:hi] = True
    unclaimed_positions = rising_clock[~claimed_clock]
    tail = len(samples) - stops[-1] if stops else None
    return {
        "samples": len(samples),
        "first_levels": [int(scl[0]), int(sda[0])],
        "last_levels": [int(scl[-1]), int(sda[-1])],
        "starts": len(starts), "repeated_starts": len(repeated), "stops": len(stops),
        "segments_checked": segments_checked,
        "clock_rises": len(rising_clock),
        "orphan_stops": orphan_stops,
        "unfinished_start": current_start,
        "empty_spans": empty_spans,
        "simultaneous_scl_sda_edges": simultaneous[:10].tolist(),
        "simultaneous_scl_sda_edge_count": len(simultaneous),
        "unclaimed_clock_rises": len(unclaimed_positions),
        "first_unclaimed_clock_samples": unclaimed_positions[:10].tolist(),
        "tail_after_last_stop_samples": tail,
        "last_stop_sample": stops[-1] if stops else None,
    }


def fault_probes(data: bytes, channels: dict[str, int], baseline: dict) -> dict:
    if baseline["last_stop_sample"] is None:
        raise ValueError("A complete recorded transaction is needed for the fault probes")
    stop = baseline["last_stop_sample"]
    sda_bit = 1 << channels["SDA"]
    scl_bit = 1 << channels["SCL"]
    # Hold SDA low after the final transaction. Earlier payload remains intact.
    held = bytearray(data)
    for index in range(stop, len(held)):
        held[index] &= ~sda_bit
    held_result = audit(held, channels)

    # Add one clock pulse in the settled tail, without changing I2C payload.
    if len(data) - stop < 64:
        raise ValueError("At least 64 idle samples after STOP are needed")
    pulsed = bytearray(data)
    at = stop + 16
    for index in range(at, at + 16):
        pulsed[index] &= ~scl_bit
    pulse_result = audit(pulsed, channels)
    return {
        "held_sda_low": {key: held_result[key] for key in ("unfinished_start", "last_levels", "stops")},
        "clock_after_stop": {key: pulse_result[key] for key in ("unclaimed_clock_rises", "stops")},
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--i2c-root", type=Path, default=Path.home() / "dev" / "I2CDeviceDB")
    args = parser.parse_args()
    rows = []
    for path in sorted((args.i2c_root / "captures" / "raw").glob("*.sr")):
        data, rate, channels = load_sr(path)
        baseline = audit(data, channels)
        probes = fault_probes(data, channels, baseline)
        rows.append({"source": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                     "sample_rate_hz": rate, "channels": channels, "baseline": baseline,
                     "fault_probes": probes})
    if not rows:
        raise SystemExit("No .sr files found")
    print(json.dumps(rows, ensure_ascii=False, indent=2))
    if not all(row["baseline"]["unfinished_start"] is None
               and row["baseline"]["last_levels"] == [1, 1]
               and not row["baseline"]["orphan_stops"]
               and row["fault_probes"]["held_sda_low"]["unfinished_start"] is not None
               and row["fault_probes"]["held_sda_low"]["last_levels"] == [1, 0]
               and row["fault_probes"]["clock_after_stop"]["unclaimed_clock_rises"] >
                   row["baseline"]["unclaimed_clock_rises"] for row in rows):
        raise SystemExit("Wire audit did not distinguish the recorded and faulted waveforms")


if __name__ == "__main__":
    main()
