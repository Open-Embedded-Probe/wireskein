"""Exercise an evidence chain on saved signals; scores here are uncalibrated trials.

The aim is to find where hypotheses need separate coverage, validity, and upper
layer evidence. This is not an automatic protocol classifier.
"""

from __future__ import annotations

import argparse
import copy
import json
import re
import subprocess
import sys
from pathlib import Path

import numpy as np

from foundation_probe import channel_summary, uart_like_trial
from i2c_wire_audit import audit, load_sr
from probe import load_module


def active_channels(data: bytes, channels: dict[str, int]) -> list[str]:
    samples = np.frombuffer(data, dtype=np.uint8)
    return [name for name, bit in channels.items()
            if np.any(((samples[:-1] ^ samples[1:]) >> bit) & 1)]


def i2c_hypotheses(data: bytes, channels: dict[str, int], active: list[str]) -> list[dict]:
    out = []
    for clock in active:
        for line in active:
            if clock == line:
                continue
            evidence = audit(data, {"SCL": channels[clock], "SDA": channels[line]})
            starts = evidence["starts"]
            complete_nonempty = max(0, evidence["segments_checked"] - len(evidence["empty_spans"]))
            nonempty_fraction = min(1.0, complete_nonempty / starts) if starts else 0.0
            all_complete = bool(starts and evidence["unfinished_start"] is None
                                and not evidence["orphan_stops"])
            clock_coverage = (1 - evidence["unclaimed_clock_rises"] / evidence["clock_rises"]
                              if evidence["clock_rises"] else 0.0)
            # Deliberately simple ranking trial. A high number here is neither
            # calibrated confidence nor a claim of I2C conformance.
            rank_score = round(0.5 * nonempty_fraction + 0.3 * all_complete +
                               0.2 * clock_coverage, 4)
            out.append({"clock": clock, "data": line, "rank_score_trial": rank_score,
                        "evidence": {"starts": starts, "repeated": evidence["repeated_starts"],
                                     "stops": evidence["stops"],
                                     "empty_spans": len(evidence["empty_spans"]),
                                     "unfinished": evidence["unfinished_start"] is not None,
                                     "unclaimed_clock_rises": evidence["unclaimed_clock_rises"],
                                     "nonempty_fraction": round(nonempty_fraction, 4),
                                     "clock_coverage": round(clock_coverage, 4)}})
    return sorted(out, key=lambda row: (-row["rank_score_trial"], row["clock"], row["data"]))


def uart_trial(path: Path, data: bytes, rate: int, channels: dict[str, int], active: list[str]) -> list[dict]:
    samples = np.frombuffer(data, dtype=np.uint8)
    out = []
    for name in active:
        values = (samples >> channels[name]) & 1
        modal = channel_summary(values, rate)["modal_short_run_samples"]
        if modal is None:
            continue
        shape = uart_like_trial(values, modal)
        baud = round(rate / modal)
        cmd = ["sigrok-cli", "-i", str(path), "-P",
               f"uart:rx={name}:baudrate={baud}:format=hex", "--protocol-decoder-jsontrace"]
        result = subprocess.run(cmd, capture_output=True, text=True, check=True)
        events = json.loads(result.stdout)["traceEvents"]
        byte_names = [str(event["name"]) for event in events
                      if event.get("pid") == "uart-1" and event.get("tid") == "RX"
                      and event.get("ph") == "B" and re.fullmatch(r"[0-9A-Fa-f]{2}", str(event.get("name")))]
        raw = bytes(int(value, 16) for value in byte_names)
        marker_counts = {keyword: raw.count(keyword.encode()) for keyword in
                         ("CASE_BEGIN", "CASE_END", "PHASE", "INPUT", "RESULT")}
        out.append({"line": name, "bit_period_samples_trial": modal, "baud_trial": baud,
                    "shape": shape, "decoded_bytes": len(raw), "distinct_bytes": len(set(raw)),
                    "upper_marker_counts": marker_counts,
                    "upper_marker_pairing": bool(marker_counts["CASE_BEGIN"]
                                                 and marker_counts["CASE_BEGIN"] == marker_counts["CASE_END"])})
    return out


def first_clocked_bit_trial(data: bytes, clock_bit: int, data_bit: int) -> dict:
    """Show raw bit provenance before I2C byte/frame interpretation."""
    samples = np.frombuffer(data, dtype=np.uint8)
    clock = (samples >> clock_bit) & 1
    line = (samples >> data_bit) & 1
    changes = np.flatnonzero(line[1:] != line[:-1]) + 1
    events = changes[(clock[changes - 1] == 1) & (clock[changes] == 1)]
    starts = events[line[events] == 0]
    if not len(starts):
        return {"status": "no_start"}
    start = int(starts[0])
    next_events = events[events > start]
    end = int(next_events[0]) if len(next_events) else len(samples)
    rises = np.flatnonzero((clock[:-1] == 0) & (clock[1:] == 1)) + 1
    bit_positions = rises[(rises > start) & (rises < end)]
    bits = "".join(str(int(line[index])) for index in bit_positions)
    return {"status": "raw_clocked_bits", "sample_range": [start, end],
            "bit_count": len(bits), "first_32_bits": bits[:32],
            "first_bit_sample_positions": bit_positions[:16].tolist(),
            "note": "Rising-edge samples are a hypothesis, not yet validated I2C bytes"}


def i2c_upper(path: Path, root: Path) -> dict:
    sys.path.insert(0, str(root / "tools"))
    decoder = load_module("chain_i2c_decode", root / "tools" / "decode.py")
    extension = load_module("chain_sht30_extension", Path(__file__).with_name("sht30_crc_extension.py"))
    events = decoder.load_events(path)
    txns = decoder.parse_transactions(events)
    markers = decoder.parse_markers(events)
    decoder.annotate(txns, markers)
    records = decoder.to_records(txns)
    readings = [result for row in records if (result := extension.analyze(row)) is not None]
    crc_checks = 2 * len(readings)
    crc_pass = sum(r["temperature_crc_ok"] + r["humidity_crc_ok"] for r in readings)
    corrupt_byte_detected = None
    if readings:
        matched = copy.deepcopy(next(row for row in records if extension.analyze(row) is not None))
        original = int(matched["bytes"][0]["value"], 16)
        matched["bytes"][0]["value"] = f"0x{original ^ 1:02X}"
        changed = extension.analyze(matched)
        corrupt_byte_detected = changed is not None and changed["temperature_crc_ok"] is False
    return {"transactions": len(records), "markers": len(markers),
            "sht30_structural_matches": len(readings),
            "sht30_crc_checks": crc_checks,
            "sht30_crc_pass": crc_pass,
            "sht30_validation_ratio": crc_pass / crc_checks if crc_checks else None,
            "sht30_corrupt_byte_detected": corrupt_byte_detected,
            "sht30_source_rows": [r["source_row"] for r in readings]}


def wch_chain(root: Path) -> list[dict]:
    tools = root / "captures" / "tools"
    sys.path.insert(0, str(tools))
    import rvswd
    import swio
    fixture = root / "captures" / "fixtures" / "wire-linke-p4-2026-09-25"
    out = []
    for relative, protocol, k, k_frame in (
        ("l103/target_info", "RVSWD", 3, None),
        ("v203/target_info", "RVSWD", 3, None),
        ("v203-100mhz/target_info", "RVSWD", 1, 3),
        ("v003/target_info", "SWIO", None, None),
    ):
        path = fixture / f"{relative}.sr"
        sample_values, sample_rate = rvswd.load(str(path))
        raw_data = sample_values.tobytes()
        roles = {f"D{bit}": bit for bit in range(2 if protocol == "RVSWD" else 1)}
        alternatives = i2c_hypotheses(raw_data, roles, active_channels(raw_data, roles))
        uart_shapes = []
        for name, bit in roles.items():
            values = (sample_values >> bit) & 1
            period = channel_summary(values, sample_rate)["modal_short_run_samples"]
            if period is not None:
                shape = uart_like_trial(values, period)
                uart_shapes.append({"line": name, "bit_period_samples_trial": period,
                                    "start_stop_fraction": shape["start_stop_fraction"],
                                    "top_octets": shape["top_octets"]})
        if protocol == "RVSWD":
            frames, rate = rvswd.frames(str(path), k=k, k_frame=k_frame)
            known = [(start / rate * 1000, decoded) for start, _end, _edges, bits, status in frames
                     if status.startswith("stop") and (decoded := rvswd.decode(bits)) is not None]
            short = [frame for _time, frame in known if frame["kind"] in ("R", "W")]
            memory = rvswd.memory_log(known)
            chip_id = next((value for _time, kind, addr, value in memory
                            if kind == "MEMR" and addr == 0x1FFFF704), None)
            unknown = [(int(start), int(end)) for start, end, _edges, bits, status in frames
                       if not status.startswith("stop") or rvswd.decode(bits) is None]
            out.append({"source": str(path), "candidate": protocol,
                        "alternative_i2c_pair_trials": alternatives,
                        "alternative_uart_shape_trials": uart_shapes,
                        "frame_shapes": {"recognized": len(known), "total": len(frames)},
                        "first_bit_candidate": {"sample_range": [int(frames[0][0]), int(frames[0][1])],
                                                "bit_count": len(frames[0][3]),
                                                "first_32_bits": frames[0][3][:32],
                                                "first_bit_sample_positions": frames[0][2][:16].tolist()},
                        "validation": {"short_parity_checked": len(short),
                                       "short_parity_failed": sum(not (f["hdr_ok"] and f["data_ok"])
                                                                  for f in short),
                                       "long_and_burst_parity_not_checked": True},
                        "upper": {"memory_operations": len(memory), "chip_id": chip_id},
                        "issues": {"uninterpreted_frames": len(unknown),
                                   "first_uninterpreted_sample_ranges": unknown[:3]}})
        else:
            frames, _rate = swio.frames(str(path))
            known = [decoded for _start, bits, _widths in frames
                     if (decoded := swio.decode(bits)) is not None]
            out.append({"source": str(path), "candidate": protocol,
                        "alternative_i2c_pair_trials": alternatives,
                        "alternative_uart_shape_trials": uart_shapes,
                        "frame_shapes": {"recognized": len(known), "total": len(frames)},
                        "first_bit_candidate": {"start_sample": int(frames[0][0]),
                                                "bit_count": len(frames[0][1]),
                                                "first_32_bits": frames[0][1][:32]},
                        "validation": {"parity_available": False},
                        "upper": {"memory_operations": len(swio.memory_log([(0, x) for x in known]))},
                        "issues": {"unclassified_pulse_groups": len(frames) - len(known)}})
    return out


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--i2c-root", type=Path, default=Path.home() / "dev" / "I2CDeviceDB")
    parser.add_argument("--wch-root", type=Path, default=Path.home() / "dev_wch" / "wch-protocols")
    args = parser.parse_args()
    i2c = []
    for path in sorted((args.i2c_root / "captures" / "raw").glob("*.sr")):
        data, rate, channels = load_sr(path)
        active = active_channels(data, channels)
        candidates = i2c_hypotheses(data, channels, active)
        anonymous = {f"D{bit}": bit for bit in sorted(channels.values())}
        anonymous_candidates = i2c_hypotheses(data, anonymous, active_channels(data, anonymous))
        chosen = candidates[0]
        chosen_audit = audit(data, {"SCL": channels[chosen["clock"]], "SDA": channels[chosen["data"]]})
        i2c.append({"source": str(path), "active_channels": active,
                    "i2c_candidates": candidates,
                    "anonymous_i2c_rank": [{"clock": row["clock"], "data": row["data"],
                                            "rank_score_trial": row["rank_score_trial"]}
                                           for row in anonymous_candidates[:3]],
                    "uart_candidates": uart_trial(path, data, rate, channels, active),
                    "selected_i2c_bit_trial": first_clocked_bit_trial(
                        data, channels[chosen["clock"]], channels[chosen["data"]]),
                    "selected_i2c_wire_issues": {
                        "unfinished": chosen_audit["unfinished_start"] is not None,
                        "unclaimed_clock_rises": chosen_audit["unclaimed_clock_rises"],
                        "first_unclaimed_clock_samples": chosen_audit["first_unclaimed_clock_samples"],
                        "final_levels": chosen_audit["last_levels"],
                    },
                    "selected_i2c_upper": i2c_upper(path, args.i2c_root)})
    result = {"i2c_uart": i2c, "wch": wch_chain(args.wch_root)}
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if not all(row["i2c_candidates"][0]["clock"] == "SCL"
               and row["i2c_candidates"][0]["data"] == "SDA"
               and row["anonymous_i2c_rank"][0]["clock"] == "D1"
               and row["anonymous_i2c_rank"][0]["data"] == "D2"
               and next(c for c in row["uart_candidates"] if c["line"] == "UART_TX")["upper_marker_pairing"]
               and not next(c for c in row["uart_candidates"] if c["line"] == "SCL")["upper_marker_pairing"]
               for row in i2c):
        raise SystemExit("Known I2C/UART candidates were not separated")


if __name__ == "__main__":
    main()
