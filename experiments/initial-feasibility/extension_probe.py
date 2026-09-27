"""Load an optional upper analyzer and keep its source-row provenance."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
from pathlib import Path

from probe import load_module


def measurement_values_from_observation(path: Path) -> list[tuple[int, int]]:
    """Read the two simple integer fields from measurement events in this fixture."""
    values: list[tuple[int, int]] = []
    temperature = humidity = None
    in_measurement = False
    for line in path.read_text().splitlines() + ["- type: end"]:
        if line.startswith("- type: "):
            if in_measurement and temperature is not None and humidity is not None:
                values.append((temperature, humidity))
            in_measurement = line == "- type: measurement"
            temperature = humidity = None
        elif in_measurement and line.startswith("  raw_temperature: "):
            temperature = int(line.rsplit(" ", 1)[-1])
        elif in_measurement and line.startswith("  raw_humidity: "):
            humidity = int(line.rsplit(" ", 1)[-1])
    return values


def inspect(root: Path, extension_path: Path) -> dict:
    extension = load_module("experimental_sht30_crc_extension", extension_path)
    captures = []
    for path in sorted((root / "captures" / "decoded").glob("sht30*.jsonl")):
        rows = [json.loads(line) for line in path.read_text().splitlines()]
        results = [out for row in rows if (out := extension.analyze(row)) is not None]
        observation_matches = list((root / "observations" / "sht30").glob(f"*{path.stem.rsplit('__', 1)[-1]}.yaml"))
        if len(observation_matches) != 1:
            raise ValueError(f"Expected one observation for {path.name}")
        expected_values = measurement_values_from_observation(observation_matches[0])
        altered = copy.deepcopy(next(row for row in rows if extension.analyze(row) is not None))
        original_byte = int(altered["bytes"][0]["value"], 16)
        altered["bytes"][0]["value"] = f"0x{original_byte ^ 1:02X}"
        changed_result = extension.analyze(altered)
        captures.append({
            "detail": str(path), "readings": len(results),
            "temperature_crc_valid": sum(item["temperature_crc_ok"] for item in results),
            "humidity_crc_valid": sum(item["humidity_crc_ok"] for item in results),
            "first_result": results[0],
            "observation": str(observation_matches[0]),
            "raw_values_match_observation": [
                (item["temperature_raw"], item["humidity_raw"]) for item in results
            ] == expected_values,
            "flipped_data_byte_detected": changed_result["temperature_crc_ok"] is False,
            "source_file_unchanged": path.read_text() == "".join(
                json.dumps(row, separators=(",", ":")) + "\n" for row in rows
            ),
        })
    return {"extension": str(extension_path),
            "extension_sha256": hashlib.sha256(extension_path.read_bytes()).hexdigest(),
            "captures": captures}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--i2c-root", type=Path, default=Path.home() / "dev" / "I2CDeviceDB")
    args = parser.parse_args()
    path = Path(__file__).with_name("sht30_crc_extension.py")
    result = inspect(args.i2c_root, path)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if not all(item["readings"] == 6 and item["temperature_crc_valid"] == 6
               and item["humidity_crc_valid"] == 6 and item["flipped_data_byte_detected"]
               and item["source_file_unchanged"] and item["raw_values_match_observation"]
               for item in result["captures"]):
        raise SystemExit("Extension result did not match known fixture expectations")


if __name__ == "__main__":
    main()
