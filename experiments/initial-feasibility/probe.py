"""Read-only preflight of existing captures and their saved interpretations.

This calls the original projects' decoders; it does not implement WireSkein.
Pass --i2c-root and --wch-root if the sibling projects live elsewhere.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import sys
import zipfile
from collections import Counter
from pathlib import Path


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def sample_rate(sr: Path) -> int:
    with zipfile.ZipFile(sr) as archive:
        metadata = archive.read("metadata").decode()
    match = re.search(r"samplerate=(\d+)(?:\s*(Hz|kHz|MHz|GHz))?", metadata, re.I)
    if not match:
        raise ValueError(f"samplerate missing in {sr}")
    multiplier = {"hz": 1, "khz": 1000, "mhz": 1000000, "ghz": 1000000000}
    return int(match.group(1)) * multiplier[(match.group(2) or "Hz").lower()]


def first_different_line(actual: str, expected: str) -> int | None:
    actual_lines, expected_lines = actual.splitlines(), expected.splitlines()
    for number, (left, right) in enumerate(zip(actual_lines, expected_lines), 1):
        if left != right:
            return number
    return min(len(actual_lines), len(expected_lines)) + 1 if len(actual_lines) != len(expected_lines) else None


def inspect_i2c(root: Path) -> list[dict]:
    sys.path.insert(0, str(root / "tools"))
    decode = load_module("i2c_existing_decode", root / "tools" / "decode.py")
    timing = load_module("timing", root / "tools" / "timing.py")
    observations = [(path, path.read_text()) for path in (root / "observations").glob("*/*.yaml")]
    out = []
    for sr in sorted((root / "captures" / "raw").glob("*.sr")):
        saved = root / "captures" / "decoded" / f"{sr.stem}.jsonl"
        if not saved.is_file():
            raise FileNotFoundError(saved)
        events = decode.load_events(sr)
        txns = decode.parse_transactions(events)
        markers = decode.parse_markers(events)
        decode.annotate(txns, markers)
        timing.clock_stretch_features(sr, markers, txns)
        records = decode.to_records(txns)
        rendered = "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in records)
        expected = saved.read_text()
        phases = sorted({str(row["phase"]) for row in records if row["phase"] is not None})
        linked_observations = [
            str(path) for path, body in observations
            if f"raw: captures/raw/{sr.name}" in body
            and f"decoded: captures/decoded/{saved.name}" in body
        ]
        out.append({
            "source": str(sr),
            "saved_result": str(saved),
            "observations": linked_observations,
            "sample_rate_hz": sample_rate(sr),
            "transactions": len(records),
            "markers": dict(sorted(Counter(marker["kind"] for marker in markers).items())),
            "operations": sorted({row["operation"] for row in records if row["operation"]}),
            "phases": phases,
            "first_i2c_start_in_capture_us": txns[0]["start_ts"] if txns else None,
            "byte_exact_redecode": rendered == expected,
            "first_different_line": first_different_line(rendered, expected) if rendered != expected else None,
            "content_hash": decode.content_hash(records),
            "content_hash_matches_filename": decode.content_hash(records) == sr.stem.rsplit("__", 1)[-1],
            "raw_bytes": sr.stat().st_size,
            "result_bytes": saved.stat().st_size,
        })
    return out


def inspect_wch(root: Path) -> list[dict]:
    rvswd = load_module("wch_existing_rvswd", root / "captures" / "tools" / "rvswd.py")
    fixture = root / "captures" / "fixtures" / "wire-linke-p4-2026-09-25"
    out = []
    for relative, k, k_frame in (
        ("l103/target_info", 3, None),
        ("v203/target_info", 3, None),
        ("v203-100mhz/target_info", 1, 3),
    ):
        sr = fixture / f"{relative}.sr"
        frames, rate = rvswd.frames(str(sr), k=k, k_frame=k_frame)
        decoded = [(start / rate * 1000, rvswd.decode(bits))
                   for start, _end, _edges, bits, status in frames if status.startswith("stop")]
        known = [(time, frame) for time, frame in decoded if frame is not None]
        unknown = [
            (start, end, len(edges), status)
            for start, end, edges, bits, status in frames
            if not status.startswith("stop") or rvswd.decode(bits) is None
        ]
        short = [frame for _time, frame in known if frame["kind"] in ("W", "R")]
        memory = rvswd.memory_log(known)
        saved_memory = fixture / f"{relative}.mem.txt"
        saved_rows = re.findall(
            r"^\s*\d+\.\d+\s+(MEMR|MEMW|REGR|REGW)\s+([0-9a-f]+)\s*=\s*([0-9a-f]+)",
            saved_memory.read_text(), re.M,
        ) if saved_memory.is_file() else None
        memory_rows = [
            (kind, f"{address:08x}" if address is not None else None,
             f"{value:08x}" if value is not None else None)
            for _time, kind, address, value in memory
        ]
        out.append({
            "source": str(sr),
            "usb_log": str(fixture / f"{relative}.ndjson"),
            "sample_rate_hz": int(rate),
            "filter_samples": k,
            "frame_filter_samples": k_frame or k,
            "frames": len(frames),
            "kinds": dict(sorted(Counter(frame["kind"] for _time, frame in known).items())),
            "uninterpreted_frames": len(unknown),
            "uninterpreted_examples": [
                {"sample_range": [int(start), int(end)], "time_ms": start / rate * 1000,
                 "rising_edges": count, "status": status}
                for start, end, count, status in unknown[:5]
            ],
            "short_parity_failures": sum(not (frame["hdr_ok"] and frame["data_ok"]) for frame in short),
            "memory_operations": len(memory),
            "saved_memory_operations": len(saved_rows) if saved_rows is not None else None,
            "saved_memory_matches": memory_rows == saved_rows if saved_rows is not None else None,
            "first_memory_operations": [
                {"time_ms": time, "kind": kind, "address": f"{addr:08x}" if addr is not None else None,
                 "value": f"{value:08x}" if value is not None else None}
                for time, kind, addr, value in memory[:3]
            ],
            "first_frame_sample_range": [int(frames[0][0]), int(frames[0][1])] if frames else None,
            "raw_bytes": sr.stat().st_size,
        })
    return out


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--i2c-root", type=Path, default=Path.home() / "dev" / "I2CDeviceDB")
    parser.add_argument("--wch-root", type=Path, default=Path.home() / "dev_wch" / "wch-protocols")
    args = parser.parse_args()
    result = {"i2c": inspect_i2c(args.i2c_root), "wch": inspect_wch(args.wch_root)}
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if not all(item["byte_exact_redecode"] and item["content_hash_matches_filename"]
               and len(item["observations"]) == 1 for item in result["i2c"]):
        raise SystemExit("At least one I2C re-decode differs from its saved result")


if __name__ == "__main__":
    main()
