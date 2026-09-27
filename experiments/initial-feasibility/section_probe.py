"""Try source anchors, marker hierarchy, and evidence replay on real I2C captures.

The section conversion is a temporary experiment for CASE_BEGIN/PHASE/CASE_END.
It does not define the project's final marker syntax or storage model.
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
import zipfile
from collections import Counter, defaultdict
from pathlib import Path

from probe import load_module, sample_rate


def sample_at(time_us: float, rate_hz: int) -> int:
    return round(time_us * rate_hz / 1_000_000)


def logic_samples(sr: Path) -> bytes:
    with zipfile.ZipFile(sr) as archive:
        names = sorted(
            (name for name in archive.namelist() if name.startswith("logic-1-")),
            key=lambda name: int(name.rsplit("-", 1)[1]),
        )
        metadata = archive.read("metadata").decode()
        if "unitsize=1" not in metadata:
            raise ValueError(f"Only 1-byte digital samples were tried: {sr}")
        return b"".join(archive.read(name) for name in names)


def verify_edges(txns: list[dict], samples: bytes, rate: int) -> dict:
    # The fixture's metadata names probe2=SCL and probe3=SDA.
    start_failures, stop_failures = [], []
    stop_count = 0
    for number, txn in enumerate(txns):
        start = sample_at(txn["start_ts"], rate)
        if not 0 < start < len(samples) or (
            (samples[start - 1] >> 1) & 1,
            (samples[start] >> 1) & 1,
            (samples[start - 1] >> 2) & 1,
            (samples[start] >> 2) & 1,
        ) != (1, 1, 1, 0):
            start_failures.append(number)
        if txn["stop"]:
            stop_count += 1
            end = sample_at(txn["end_ts"], rate)
            if not 0 < end < len(samples) or (
                (samples[end - 1] >> 1) & 1,
                (samples[end] >> 1) & 1,
                (samples[end - 1] >> 2) & 1,
                (samples[end] >> 2) & 1,
            ) != (1, 1, 0, 1):
                stop_failures.append(number)
    return {"starts_checked": len(txns), "stops_checked": stop_count,
            "start_failures": start_failures, "stop_failures": stop_failures}


def sections_from_markers(markers: list[dict]) -> list[dict]:
    """Treat CASE as level 1 and PHASE as level 2, only for this experiment."""
    total = Counter(m["arg"] for m in markers if m["kind"] == "CASE_BEGIN")
    case_numbers: Counter[str] = Counter()
    phase_numbers: defaultdict[str, Counter[str]] = defaultdict(Counter)
    sections: list[dict] = []
    active_case: dict | None = None
    active_phase: dict | None = None
    for marker in markers:
        kind, name, ts = marker["kind"], marker["arg"], marker["ts"]
        if kind == "CASE_BEGIN":
            if active_case is not None:
                raise ValueError("Nested or unclosed CASE in input")
            case_numbers[name] += 1
            label = f"{name}#{case_numbers[name]}" if total[name] > 1 else name
            active_case = {"path": label, "level": 1, "start_us": ts, "end_us": None,
                           "original_label": name}
            sections.append(active_case)
        elif kind == "PHASE":
            if active_case is None:
                raise ValueError("PHASE outside CASE")
            if active_phase is not None:
                active_phase["end_us"] = ts
            phase_numbers[active_case["path"]][name] += 1
            number = phase_numbers[active_case["path"]][name]
            phase_label = f"{name}#{number}" if number > 1 else name
            active_phase = {"path": f"{active_case['path']}/{phase_label}", "level": 2,
                            "start_us": ts, "end_us": None, "original_label": name,
                            "inherited_case": active_case["original_label"]}
            sections.append(active_phase)
        elif kind == "CASE_END":
            if active_case is None or name != active_case["original_label"]:
                raise ValueError(f"Unmatched CASE_END: {name}")
            if active_phase is not None:
                active_phase["end_us"] = ts
                active_phase = None
            active_case["end_us"] = ts
            active_case = None
    if active_case is not None:
        raise ValueError("Unclosed CASE in input")
    return sections


def selected_txns(txns: list[dict], section: dict) -> list[dict]:
    return [txn for txn in txns
            if section["start_us"] <= txn["start_ts"] < section["end_us"]]


def write_slice(source: Path, destination: Path, start: int, end: int, data: bytes) -> None:
    """Keep original sample values and metadata; shift time zero to the slice start."""
    with zipfile.ZipFile(source) as original, zipfile.ZipFile(destination, "w", zipfile.ZIP_DEFLATED) as output:
        for name in ("version", "metadata"):
            output.writestr(name, original.read(name))
        output.writestr("logic-1-1", data[start:end])


def meaningful_row(txn: dict) -> dict:
    return {key: txn[key] for key in ("addr", "rw", "addr_ack", "bytes", "stop", "operation", "phase")}


def replay_section(decode, timing, source: Path, section: dict, txns: list[dict], data: bytes, rate: int) -> dict:
    # Include the two preceding control bytes and enough idle for UART reacquisition.
    start = max(0, sample_at(section["start_us"] - 200, rate))
    end = min(len(data), sample_at(section["end_us"], rate))
    selected = selected_txns(txns, section)
    expected = [meaningful_row(row) for row in selected]
    expected_records = decode.to_records(selected)
    with tempfile.TemporaryDirectory() as directory:
        sliced_sr = Path(directory) / "section.sr"
        write_slice(source, sliced_sr, start, end, data)
        events = decode.load_events(sliced_sr)
        replayed = decode.parse_transactions(events)
        replay_markers = decode.parse_markers(events)
        decode.annotate(replayed, replay_markers)
        timing.clock_stretch_features(sliced_sr, replay_markers, replayed)
        actual = [meaningful_row(row) for row in replayed]
        actual_records = decode.to_records(replayed)
        marker_names = [(m["kind"], m["arg"]) for m in replay_markers]
        if section["level"] == 2:
            for row in replayed:
                if row["operation"] is None:
                    row["operation"] = section["inherited_case"]
        restored_records = decode.to_records(replayed)
    return {
        "section": section["path"], "sample_range": [start, end],
        "expected_transactions": len(expected), "replayed_transactions": len(actual),
        "same_meaning": expected == actual,
        "same_full_records": expected_records == actual_records,
        "same_with_inherited_context": expected_records == restored_records,
        "inherited_case": section.get("inherited_case"),
        "expected_first_context": {key: expected[0][key] for key in ("operation", "phase")} if expected else None,
        "replayed_first_context": {key: actual[0][key] for key in ("operation", "phase")} if actual else None,
        "clock_stretch_records": sum("clock_stretch" in row.get("timing", {}) for row in expected_records),
        "replayed_markers": marker_names,
    }


def inspect(root: Path) -> list[dict]:
    sys.path.insert(0, str(root / "tools"))
    decode = load_module("section_existing_decode", root / "tools" / "decode.py")
    timing = load_module("timing", root / "tools" / "timing.py")
    out = []
    for sr in sorted((root / "captures" / "raw").glob("*.sr")):
        rate = sample_rate(sr)
        data = logic_samples(sr)
        events = decode.load_events(sr)
        txns = decode.parse_transactions(events)
        markers = decode.parse_markers(events)
        decode.annotate(txns, markers)
        timing.clock_stretch_features(sr, markers, txns)
        sections = sections_from_markers(markers)
        counts = []
        for section in sections:
            chosen = selected_txns(txns, section)
            counts.append({"path": section["path"], "level": section["level"],
                           "transactions": len(chosen),
                           "sample_range": [sample_at(section["start_us"], rate),
                                            sample_at(section["end_us"], rate)]})
        cases = [section for section in sections if section["level"] == 1]
        child = next(section for section in sections if section["level"] == 2
                     and ("stretching-high" in section["path"] if sr.stem.startswith("sht30")
                          else "calibration" in section["path"]))
        boundaries = [s["start_us"] for s in sections] + [s["end_us"] for s in sections]
        crossing = [(i, b) for i, row in enumerate(txns) for b in boundaries
                    if row["start_ts"] < b < row["end_ts"]]
        out.append({
            "source": str(sr), "sample_rate_hz": rate, "samples": len(data),
            "edges": verify_edges(txns, data, rate),
            "cases": len(cases),
            "phases": sum(s["level"] == 2 for s in sections),
            "sections": counts,
            "transactions_crossing_marker_boundaries": crossing,
            "case_replays": [replay_section(decode, timing, sr, section, txns, data, rate)
                             for section in cases],
            "child_only_replay": replay_section(decode, timing, sr, child, txns, data, rate),
        })
    return out


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--i2c-root", type=Path, default=Path.home() / "dev" / "I2CDeviceDB")
    args = parser.parse_args()
    result = inspect(args.i2c_root)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if not all(not row["edges"]["start_failures"] and not row["edges"]["stop_failures"]
               and not row["transactions_crossing_marker_boundaries"]
               and all(replay["same_full_records"] for replay in row["case_replays"])
               and row["child_only_replay"]["same_with_inherited_context"]
               for row in result):
        raise SystemExit("Source anchors or section replay did not match")


if __name__ == "__main__":
    main()
