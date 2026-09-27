"""Synthetic slow analog readings aligned to real I2C marker intervals.

Only the marker order/boundaries come from a real capture. The time expansion,
clock offset/drift, uncertainty, and analog values are generated assumptions.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

from probe import load_module
from section_probe import sections_from_markers


SCALE = 6000.0  # Expand ~0.25 s of the capture to ~25 minutes.
OFFSET_S = 3.0
CLOCK_SCALE = 1.0002
UNCERTAINTY_S = 2.0


def case_containing(time_s: float, cases: list[dict]) -> str | None:
    return next((case["path"] for case in cases
                 if case["start_s"] <= time_s < case["end_s"]), None)


def possible_cases(time_s: float, cases: list[dict], uncertainty: float) -> list[str | None]:
    left, right = time_s - uncertainty, time_s + uncertainty
    possibilities: list[str | None] = [
        case["path"] for case in cases
        if left < case["end_s"] and right >= case["start_s"]
    ]
    if not any(case["start_s"] <= left and right < case["end_s"] for case in cases):
        possibilities.append(None)  # Interval also touches unmarked time.
    return possibilities


def inspect(root: Path) -> dict:
    sys.path.insert(0, str(root / "tools"))
    decode = load_module("mixed_existing_decode", root / "tools" / "decode.py")
    source = sorted((root / "captures" / "raw").glob("sht30*.sr"))[0]
    markers = decode.parse_markers(decode.load_events(source))
    real_cases = [section for section in sections_from_markers(markers) if section["level"] == 1]
    origin = real_cases[0]["start_us"]
    cases = [
        {"path": case["path"],
         "start_s": (case["start_us"] - origin) / 1_000_000 * SCALE,
         "end_s": (case["end_us"] - origin) / 1_000_000 * SCALE}
        for case in real_cases
    ]
    end_s = cases[-1]["end_s"]
    generated_times = [float(t) for t in range(0, int(end_s), 300)]
    for case in cases:
        generated_times.append((case["start_s"] + case["end_s"]) / 2)
        generated_times += [case["start_s"] - 1, case["start_s"] + 1,
                            case["end_s"] - 1, case["end_s"] + 1]
    generated_times = sorted(set(t for t in generated_times if 0 <= t <= end_s + 2))
    readings = []
    for index, true_s in enumerate(generated_times):
        local_s = (true_s - OFFSET_S) / CLOCK_SCALE
        corrected_s = local_s * CLOCK_SCALE + OFFSET_S
        possible = possible_cases(corrected_s, cases, UNCERTAINTY_S)
        confident = possible[0] if len(possible) == 1 else None
        readings.append({
            "source_time_s": round(local_s, 6),
            "actual_time_s_for_experiment_only": round(true_s, 6),
            "voltage_v": round(3.0 + 0.02 * math.sin(index / 3), 4),
            "current_a": round(0.1 + 0.005 * math.cos(index / 4), 4),
            "temperature_c": round(25 + 0.1 * math.sin(index / 5), 4),
            "actual_case": case_containing(true_s, cases),
            "possible_cases": possible,
            "confident_case": confident,
            "naive_case_without_alignment": case_containing(local_s, cases),
        })
    confident = [row for row in readings if row["confident_case"] is not None]
    boundary = [row for row in readings if len(row["possible_cases"]) != 1]
    return {
        "fixture_origin": str(source),
        "generated": True,
        "time_expansion": SCALE,
        "source_clock_transform": {"offset_s": OFFSET_S, "scale": CLOCK_SCALE,
                                   "uncertainty_s": UNCERTAINTY_S},
        "cases": cases,
        "readings": readings,
        "reading_count": len(readings),
        "confident_readings": len(confident),
        "ambiguous_readings": len(boundary),
        "wrong_confident_assignments": sum(
            row["confident_case"] != row["actual_case"] for row in confident
        ),
        "naive_disagreements_with_true_case": sum(
            row["naive_case_without_alignment"] != row["actual_case"] for row in readings
        ),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--i2c-root", type=Path, default=Path.home() / "dev" / "I2CDeviceDB")
    args = parser.parse_args()
    result = inspect(args.i2c_root)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if result["wrong_confident_assignments"]:
        raise SystemExit("A confidently assigned synthetic reading has the wrong marker section")


if __name__ == "__main__":
    main()
