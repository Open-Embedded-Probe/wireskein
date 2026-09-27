"""Experimental upper-layer analyzer for six-byte SHT30 I2C readings.

The CRC parameters are taken from I2CDeviceDB/profiles/sht30.yaml. This small
module deliberately consumes decoded records without changing the source decoder.
"""

from __future__ import annotations


def crc8(data: bytes) -> int:
    value = 0xFF
    for byte in data:
        value ^= byte
        for _ in range(8):
            value = ((value << 1) ^ 0x31) & 0xFF if value & 0x80 else (value << 1) & 0xFF
    return value


def analyze(record: dict) -> dict | None:
    if record["addr"] != "0x44" or record["rw"] != "read" or len(record["bytes"]) != 6:
        return None
    values = bytes(int(item["value"], 16) for item in record["bytes"])
    return {
        "source_row": record["i"],
        "operation": record["operation"],
        "phase": record["phase"],
        "temperature_raw": int.from_bytes(values[:2], "big"),
        "humidity_raw": int.from_bytes(values[3:5], "big"),
        "temperature_crc_ok": crc8(values[:2]) == values[2],
        "humidity_crc_ok": crc8(values[3:5]) == values[5],
    }
