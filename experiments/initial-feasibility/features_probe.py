"""Measure basic signal features before attempting any protocol hypothesis ranking."""

from __future__ import annotations

import argparse
import json
import re
import sys
import zipfile
from pathlib import Path

import numpy as np


def inspect_source(path: Path, rvswd) -> dict:
    samples, rate = rvswd.load(str(path))
    with zipfile.ZipFile(path) as archive:
        metadata = archive.read("metadata").decode()
    unit = int(re.search(r"unitsize=(\d+)", metadata).group(1))
    names = {int(index) - 1: name for index, name in re.findall(r"probe(\d+)=(.+)", metadata)}
    channels = []
    for bit in range(min(8 * unit, 8)):
        values = ((samples >> bit) & 1).astype(np.int8)
        changes = np.flatnonzero(np.diff(values)) + 1
        rises = changes[values[changes] == 1]
        gaps = np.diff(rises)
        local_gaps = gaps[gaps < rate * 100e-6]
        pulse_lengths = np.diff(np.concatenate(([0], changes, [len(values)])))
        channels.append({
            "bit": bit, "label": names.get(bit), "edges": len(changes),
            "high_fraction": round(float(np.mean(values)), 6),
            "activity_span_ms": round(float((changes[-1] - changes[0]) / rate * 1000), 3)
            if len(changes) else 0,
            "runs_up_to_3_samples": int(np.count_nonzero(pulse_lengths <= 3)),
            "local_rising_gap_samples_p10_p50_p90": [
                float(np.percentile(local_gaps, pct)) for pct in (10, 50, 90)
            ] if len(local_gaps) else None,
        })
    active = [channel for channel in channels if channel["edges"]]
    return {
        "source": str(path), "sample_rate_hz": int(rate),
        "active_channels": len(active), "static_channels": len(channels) - len(active),
        "highest_edge_channel": max(active, key=lambda channel: channel["edges"])["label"] if active else None,
        "channels": channels,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--i2c-root", type=Path, default=Path.home() / "dev" / "I2CDeviceDB")
    parser.add_argument("--wch-root", type=Path, default=Path.home() / "dev_wch" / "wch-protocols")
    args = parser.parse_args()
    sys.path.insert(0, str(args.wch_root / "captures" / "tools"))
    import rvswd
    wch = args.wch_root / "captures" / "fixtures" / "wire-linke-p4-2026-09-25"
    captures = [
        *sorted((args.i2c_root / "captures" / "raw").glob("*.sr")),
        wch / "l103" / "target_info.sr",
        wch / "v203-100mhz" / "target_info.sr",
        wch / "v003" / "target_info.sr",
    ]
    result = [inspect_source(path, rvswd) for path in captures]
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
