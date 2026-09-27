"""Reopen a capture and result from relocated ZIP and SQLite containers."""

from __future__ import annotations

import argparse
import hashlib
import json
import sqlite3
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

from probe import load_module


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def redecode(decode, timing, source: Path) -> bytes:
    events = decode.load_events(source)
    txns = decode.parse_transactions(events)
    markers = decode.parse_markers(events)
    decode.annotate(txns, markers)
    timing.clock_stretch_features(source, markers, txns)
    return "".join(json.dumps(record, separators=(",", ":")) + "\n"
                   for record in decode.to_records(txns)).encode()


def inspect(root: Path) -> dict:
    sys.path.insert(0, str(root / "tools"))
    decode = load_module("portable_existing_decode", root / "tools" / "decode.py")
    timing = load_module("timing", root / "tools" / "timing.py")
    original = sorted((root / "captures" / "raw").glob("sht30*.sr"))[0]
    saved = root / "captures" / "decoded" / f"{original.stem}.jsonl"
    source_bytes, result_bytes = original.read_bytes(), saved.read_bytes()
    revision = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
    sigrok_version = subprocess.check_output(["sigrok-cli", "--version"], text=True).splitlines()[0]
    manifest = {
        "format": "experimental-manifest-1", "source": "sources/capture.sr",
        "result": "results/transactions.jsonl",
        "sha256": {"source": sha256(source_bytes), "result": sha256(result_bytes)},
        "analysis": {"source_project": "I2CDeviceDB", "revision": revision,
                     "script": "tools/decode.py + tools/timing.py",
                     "sigrok_cli": sigrok_version},
    }
    manifest_bytes = json.dumps(manifest, ensure_ascii=False, separators=(",", ":")).encode()
    with tempfile.TemporaryDirectory() as directory:
        destination = Path(directory)
        archive = destination / "project.zip"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as output:
            output.writestr("project.json", manifest_bytes)
            output.writestr(manifest["source"], source_bytes)
            output.writestr(manifest["result"], result_bytes)
        with zipfile.ZipFile(archive) as reopened:
            reopened_manifest = json.loads(reopened.read("project.json"))
            zip_source = reopened.read(reopened_manifest["source"])
            zip_result = reopened.read(reopened_manifest["result"])
        relocated_source = destination / "relocated-from-zip.sr"
        relocated_source.write_bytes(zip_source)
        zip_ok = (sha256(zip_source) == manifest["sha256"]["source"]
                  and sha256(zip_result) == manifest["sha256"]["result"]
                  and redecode(decode, timing, relocated_source) == zip_result)

        database = destination / "project.sqlite"
        with sqlite3.connect(database) as connection:
            connection.execute("CREATE TABLE items (name TEXT PRIMARY KEY, payload BLOB NOT NULL)")
            connection.executemany("INSERT INTO items VALUES (?, ?)", [
                ("project.json", manifest_bytes),
                (manifest["source"], source_bytes),
                (manifest["result"], result_bytes),
            ])
        with sqlite3.connect(database) as connection:
            sqlite_manifest = json.loads(connection.execute(
                "SELECT payload FROM items WHERE name='project.json'"
            ).fetchone()[0])
            db_source = connection.execute("SELECT payload FROM items WHERE name=?",
                                           (sqlite_manifest["source"],)).fetchone()[0]
            db_result = connection.execute("SELECT payload FROM items WHERE name=?",
                                           (sqlite_manifest["result"],)).fetchone()[0]
        relocated_db_source = destination / "relocated-from-sqlite.sr"
        relocated_db_source.write_bytes(db_source)
        sqlite_ok = (sha256(db_source) == manifest["sha256"]["source"]
                     and sha256(db_result) == manifest["sha256"]["result"]
                     and redecode(decode, timing, relocated_db_source) == db_result)
        return {
            "input": str(original), "source_bytes": len(source_bytes),
            "result_bytes": len(result_bytes), "manifest_bytes": len(manifest_bytes),
            "zip_bytes": archive.stat().st_size,
            "sqlite_bytes": database.stat().st_size,
            "zip_reopen_sha_and_redecode_match": zip_ok,
            "sqlite_reopen_sha_and_redecode_match": sqlite_ok,
            "decoder_embedded": False,
        }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--i2c-root", type=Path, default=Path.home() / "dev" / "I2CDeviceDB")
    args = parser.parse_args()
    result = inspect(args.i2c_root)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if not result["zip_reopen_sha_and_redecode_match"] or not result["sqlite_reopen_sha_and_redecode_match"]:
        raise SystemExit("Relocated project data did not reproduce the saved result")


if __name__ == "__main__":
    main()
