"""Additional read-only probes of filter sensitivity, known payloads, and cross-source clues."""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path


def decoded_rvswd(rvswd, path: Path, k: int, k_frame: int) -> tuple[list, list, float]:
    frames, rate = rvswd.frames(str(path), k=k, k_frame=k_frame)
    parsed = [rvswd.decode(bits) if status.startswith("stop") else None
              for _start, _end, _edges, bits, status in frames]
    return frames, parsed, rate


def frame_summary(frames: list, parsed: list, rate: float) -> dict:
    short = [frame for frame in parsed if frame and frame["kind"] in ("R", "W")]
    unknown = [
        {"sample_range": [int(frame[0]), int(frame[1])],
         "time_ms": frame[0] / rate * 1000, "edges": len(frame[2]), "end": frame[4]}
        for frame, result in zip(frames, parsed) if result is None
    ]
    return {
        "frames": len(frames), "recognized": len(frames) - len(unknown),
        "uninterpreted": len(unknown),
        "short_parity_failures": sum(not (frame["hdr_ok"] and frame["data_ok"]) for frame in short),
        "uninterpreted_examples": unknown[:5],
    }


def make_decimated_source(source: Path, destination: Path, samples, phase: int) -> None:
    with zipfile.ZipFile(source) as original:
        metadata = original.read("metadata").decode()
        if "samplerate=100000000 Hz" not in metadata or "unitsize=1" not in metadata:
            raise ValueError("Unexpected source metadata for the 100 MHz controlled comparison")
        with zipfile.ZipFile(destination, "w", zipfile.ZIP_DEFLATED) as output:
            output.writestr("version", original.read("version"))
            output.writestr("metadata", metadata.replace("samplerate=100000000 Hz", "samplerate=50000000 Hz"))
            output.writestr("logic-1-1", samples[phase::2].tobytes())


def pattern_words(payload: bytes) -> list[int]:
    return [int.from_bytes(payload[i:i + 4], "little") for i in range(0, len(payload), 4)]


def find_exact_run(haystack: list[int], needle: list[int]) -> int | None:
    return next((i for i in range(len(haystack) - len(needle) + 1)
                 if haystack[i:i + len(needle)] == needle), None)


def replay_unknown_evidence(rvswd, source: Path, frames: list, parsed: list) -> list[dict]:
    original = [(int(frame[0]), int(frame[1]), frame[3])
                for frame, result in zip(frames, parsed) if result is None]
    if not original:
        return []
    samples, _rate = rvswd.load(str(source))
    with zipfile.ZipFile(source) as archive:
        metadata, version = archive.read("metadata"), archive.read("version")
    out = []
    for guard in (10_000, 50_000):
        start = max(0, original[0][0] - guard)
        end = min(len(samples), original[-1][1] + guard)
        with tempfile.TemporaryDirectory() as directory:
            clipped = Path(directory) / "evidence.sr"
            with zipfile.ZipFile(clipped, "w", zipfile.ZIP_DEFLATED) as archive:
                archive.writestr("version", version)
                archive.writestr("metadata", metadata)
                archive.writestr("logic-1-1", samples[start:end].tobytes())
            clip_frames, clip_parsed, _rate = decoded_rvswd(rvswd, clipped, 1, 3)
        clipped_unknown = [(int(frame[0]) + start, int(frame[1]) + start, frame[3])
                           for frame, result in zip(clip_frames, clip_parsed) if result is None]
        out.append({"guard_samples": guard, "sample_range": [start, end],
                    "original_unknown_recovered": sum(item in clipped_unknown for item in original),
                    "original_unknown_total": len(original),
                    "extra_unknown_at_slice_boundary": len(clipped_unknown) - sum(item in clipped_unknown for item in original)})
    return out


def inspect_rvswd(root: Path, rvswd) -> dict:
    fixture = root / "captures" / "fixtures" / "wire-linke-p4-2026-09-25"
    source_50 = fixture / "v203" / "target_info.sr"
    source_100 = fixture / "v203-100mhz" / "target_info.sr"
    sweep = []
    short_periods = None
    for path, label in ((source_50, "50MHz real"), (source_100, "100MHz real")):
        for k, k_frame in ((1, 3), (2, 3), (3, 3), (4, 4)):
            frames, parsed, rate = decoded_rvswd(rvswd, path, k, k_frame)
            sweep.append({"input": label, "k": k, "k_frame": k_frame,
                          **frame_summary(frames, parsed, rate)})
            if label == "100MHz real" and k == 1:
                short_periods = [
                    (int(frame[0]) / rate * 1000,
                     statistics.median(int(right) - int(left)
                                       for left, right in zip(frame[2][:-1], frame[2][1:])) / rate * 1e6)
                    for frame, decoded in zip(frames, parsed)
                    if decoded and decoded["kind"] in ("R", "W") and len(frame[2]) > 2
                ]

    samples, rate = rvswd.load(str(source_100))
    if int(rate) != 100_000_000:
        raise ValueError(f"Unexpected rate: {rate}")
    decimated = []
    for phase in (0, 1):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "decimated.sr"
            make_decimated_source(source_100, path, samples, phase)
            for k, k_frame in ((1, 3), (2, 3), (3, 3)):
                frames, parsed, rate = decoded_rvswd(rvswd, path, k, k_frame)
                decimated.append({"phase": phase, "k": k, "k_frame": k_frame,
                                  **frame_summary(frames, parsed, rate)})

    pattern = pattern_words((fixture / "tools" / "pattern-4k.bin").read_bytes())
    flash_path = fixture / "v203-100mhz" / "flash_pattern4k.sr"
    flash_frames, flash_parsed, flash_rate = decoded_rvswd(rvswd, flash_path, 1, 3)
    writes = [frame["data"] for frame in flash_parsed
              if frame and frame["kind"] == "W" and frame["addr"] == 4]
    readback: list[int] = []
    unmatched_bursts = 0
    for index, frame in enumerate(flash_parsed):
        if not frame or frame["kind"] != "BURST":
            continue
        following = next((item for item in flash_parsed[index + 1:index + 5]
                          if item and item["kind"] == "R" and item["addr"] == 4), None)
        if following is None:
            unmatched_bursts += 1
        else:
            readback.extend(frame["words"] + [following["data"]])

    metadata_160 = json.loads((fixture / "v203-160mhz" / "flash_pattern4k.json").read_text())
    evidence = replay_unknown_evidence(rvswd, flash_path, flash_frames, flash_parsed)
    flash_summary = frame_summary(flash_frames, flash_parsed, flash_rate)
    brief = {
        "source": flash_path.name,
        "decoder": "rvswd.py", "k": 1, "k_frame": 3,
        "frames": flash_summary["frames"], "recognized": flash_summary["recognized"],
        "uninterpreted": flash_summary["uninterpreted_examples"],
        "write_pattern_exact": find_exact_run(writes, pattern) is not None,
        "readback_pattern_exact": readback == pattern,
    }
    detail_text = subprocess.run(
        [sys.executable, str(root / "captures" / "tools" / "rvswd.py"), str(flash_path),
         "--k", "1", "--k-frame", "3"],
        check=True, capture_output=True,
    ).stdout
    return {
        "filter_sweep": sweep,
        "short_frame_periods_100mhz": {
            "frames": len(short_periods),
            "below_0_5us": sum(period < 0.5 for _time, period in short_periods),
            "fast_time_range_ms": [
                min(time for time, period in short_periods if period < 0.5),
                max(time for time, period in short_periods if period < 0.5),
            ],
            "fast_median_period_us": statistics.median(
                period for _time, period in short_periods if period < 0.5
            ),
        },
        "decimated_from_same_100mhz_capture": decimated,
        "known_4k_rvswd": {
            "source": str(flash_path), "frame_summary": flash_summary,
            "pattern_words": len(pattern), "write_run_start": find_exact_run(writes, pattern),
            "readback_words": len(readback), "readback_exact": readback == pattern,
            "unmatched_bursts": unmatched_bursts,
            "unknown_evidence_replay": evidence,
            "detail_text_bytes": len(detail_text),
            "brief_json_bytes": len(json.dumps(brief, separators=(",", ":")).encode()),
            "brief": brief,
        },
        "160mhz_recording": {"source": str(fixture / "v203-160mhz" / "flash_pattern4k.sr"),
                             "gap_marked": metadata_160.get("gap_marked")},
    }


def inspect_swio(root: Path, swio) -> dict:
    fixture = root / "captures" / "fixtures" / "wire-linke-p4-2026-09-25"
    path = fixture / "v003" / "flash_pattern4k.sr"
    frames, _rate = swio.frames(str(path))
    decoded = [swio.decode(bits) for _sample, bits, _widths in frames]
    pattern = pattern_words((fixture / "tools" / "pattern-4k.bin").read_bytes())
    writes = [frame["data"] for frame in decoded if frame and frame["kind"] == "W" and frame["addr"] == 4]
    reads = [frame["data"] for frame in decoded if frame and frame["kind"] == "R" and frame["addr"] == 4]
    block = pattern[:256]
    write_blocks = [i for i in range(len(writes) - len(block) + 1)
                    if writes[i:i + len(block)] == block]
    return {
        "source": str(path), "frames": len(frames),
        "recognized": sum(frame is not None for frame in decoded),
        "uninterpreted": sum(frame is None for frame in decoded),
        "write_1k_block_starts": write_blocks,
        "readback_4k_start": find_exact_run(reads, pattern),
    }


def inspect_usb_and_wire(root: Path, rvswd) -> list[dict]:
    fixture = root / "captures" / "fixtures" / "wire-linke-p4-2026-09-25"
    out = []
    for relative, k, k_frame in (("l103", 3, 3), ("v203-100mhz", 1, 3)):
        sr = fixture / relative / "target_info.sr"
        frames, parsed, rate = decoded_rvswd(rvswd, sr, k, k_frame)
        known = [(start / rate * 1000, frame) for (start, *_), frame in zip(frames, parsed) if frame]
        accesses = rvswd.memory_log(known)
        chip_reads = [value for _time, operation, address, value in accesses
                      if operation == "MEMR" and address == 0x1FFFF704]
        usb = [json.loads(line) for line in (fixture / relative / "target_info.ndjson").read_text().splitlines()]
        attach_responses = [row for row in usb if row.get("seq") == 7]
        if len(attach_responses) != 1:
            raise ValueError(f"Missing single attach response: {relative}")
        attach_bytes = bytes.fromhex(attach_responses[0]["data"])
        chip_from_usb = int.from_bytes(attach_bytes[-4:], "big")
        out.append({"source": str(sr), "wire_chip_reads": [f"{value:08x}" for value in chip_reads],
                    "usb_attach_chip_id": f"{chip_from_usb:08x}",
                    "value_matches": chip_from_usb in chip_reads,
                    "common_clock_or_marker": False})
    return out


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--wch-root", type=Path, default=Path.home() / "dev_wch" / "wch-protocols")
    args = parser.parse_args()
    sys.path.insert(0, str(args.wch_root / "captures" / "tools"))
    import rvswd
    import swio

    result = {
        "rvswd": inspect_rvswd(args.wch_root, rvswd),
        "swio": inspect_swio(args.wch_root, swio),
        "cross_source": inspect_usb_and_wire(args.wch_root, rvswd),
    }
    print(json.dumps(result, ensure_ascii=False, indent=2))
    known = result["rvswd"]["known_4k_rvswd"]
    if known["write_run_start"] is None or not known["readback_exact"]:
        raise SystemExit("RVSWD data did not match the independently known 4 KiB pattern")
    if len(result["swio"]["write_1k_block_starts"]) < 4 or result["swio"]["readback_4k_start"] is None:
        raise SystemExit("SWIO data did not match the independently known 4 KiB pattern")
    if not all(item["value_matches"] for item in result["cross_source"]):
        raise SystemExit("USB and wire chip IDs did not match")


if __name__ == "__main__":
    main()
