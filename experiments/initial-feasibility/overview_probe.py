"""Measure brief outputs and reproducible detail selection on the I2C fixtures."""

from __future__ import annotations

import argparse
import difflib
import json
import sys
from pathlib import Path

from probe import load_module, sample_rate
from section_probe import sections_from_markers, selected_txns, sample_at


def structural_rows(rows: list[dict]) -> list[tuple]:
    # Deliberately mask only observed byte values. ACK, direction, count, and order remain.
    return [
        (row["operation"], row["phase"], row["addr"], row["rw"], row["addr_ack"],
         tuple(("*", byte["ack"]) for byte in row["bytes"]), row["stop"])
        for row in rows
    ]


def inspect(root: Path) -> dict:
    sys.path.insert(0, str(root / "tools"))
    decode = load_module("overview_existing_decode", root / "tools" / "decode.py")
    captures = []
    stored: dict[str, list[dict]] = {}
    for source in sorted((root / "captures" / "raw").glob("*.sr")):
        detail = root / "captures" / "decoded" / f"{source.stem}.jsonl"
        rows = [json.loads(line) for line in detail.read_text().splitlines()]
        stored[source.stem] = rows
        events = decode.load_events(source)
        txns = decode.parse_transactions(events)
        markers = decode.parse_markers(events)
        decode.annotate(txns, markers)
        sections = sections_from_markers(markers)
        rate = sample_rate(source)
        overview = []
        for section in sections:
            if section["level"] != 1:
                continue
            matching = selected_txns(txns, section)
            indices = [txns.index(row) for row in matching]
            first = indices[0] if indices else None
            last = indices[-1] + 1 if indices else None
            if indices and indices != list(range(first, last)):
                raise ValueError(f"Noncontiguous record indices in {source.name}: {section['path']}")
            if any(rows[i]["operation"] != section["original_label"] for i in indices):
                raise ValueError(f"Saved result disagrees with marker scope: {source.name}")
            overview.append({
                "section": section["path"],
                "sample_range": [sample_at(section["start_us"], rate),
                                 sample_at(section["end_us"], rate)],
                "record_range": [first, last],
                "records": len(indices),
                "data_bytes": sum(len(rows[i]["bytes"]) for i in indices),
                "address_nacks": sum(rows[i]["addr_ack"] is False for i in indices),
            })
        view = {"source": source.name, "detail": detail.name, "overview": overview}
        encoded = json.dumps(view, ensure_ascii=False, separators=(",", ":")).encode()
        captures.append({
            "source": source.name, "detail": detail.name,
            "detail_bytes": detail.stat().st_size, "overview_bytes": len(encoded),
            "record_count": len(rows), "section_count": len(overview),
            "records_addressed": sum(item["records"] for item in overview),
            "all_records_addressed_once": sorted(
                i for item in overview for i in range(*item["record_range"])
            ) == list(range(len(rows))),
            "overview": overview,
        })
    comparisons = []
    for prefix in ("qmp6988", "sht30"):
        names = sorted(name for name in stored if name.startswith(prefix))
        left, right = (structural_rows(stored[name]) for name in names)
        matcher = difflib.SequenceMatcher(a=left, b=right, autojunk=False)
        edits = [{"kind": kind, "left_rows": [i, j], "right_rows": [k, l]}
                 for kind, i, j, k, l in matcher.get_opcodes() if kind != "equal"]
        comparisons.append({"kind": prefix, "left": names[0], "right": names[1],
                            "same_after_value_mask": left == right,
                            "left_rows": len(left), "right_rows": len(right),
                            "edits": edits})
    return {"captures": captures, "comparisons": comparisons}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--i2c-root", type=Path, default=Path.home() / "dev" / "I2CDeviceDB")
    args = parser.parse_args()
    result = inspect(args.i2c_root)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if not all(row["all_records_addressed_once"] for row in result["captures"]):
        raise SystemExit("An overview omitted or duplicated detailed records")


if __name__ == "__main__":
    main()
