"""Collect reference captures from sibling repositories into corpus/raw/.

corpus/raw/ is not tracked by Git. Each collected file is recorded in
corpus/raw/manifest.json with its origin, source repository commit and SHA-256,
so the set can be rebuilt or audited later. Source files are only read.

    python3 corpus/collect.py [--i2c-root DIR] [--wch-root DIR]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
RAW = HERE / "raw"

I2C_STEMS = [
    "qmp6988__qmp6988__characterize__100k__nominal__0283eb57c7a7c010",
    "qmp6988__qmp6988__characterize__100k__nominal__a6ec5b99888f5897",
    "sht30__sht30__characterize__100k__nominal__1b9dbfe9f8c22329",
    "sht30__sht30__characterize__100k__nominal__29d9633ff0bd82a0",
]
I2C_OBSERVATIONS = {
    "qmp6988": ["qmp6988-p0-100k-nominal-0283eb57c7a7c010", "qmp6988-p0-100k-nominal-a6ec5b99888f5897"],
    "sht30": ["sht30-p0-100k-nominal-1b9dbfe9f8c22329", "sht30-p0-100k-nominal-29d9633ff0bd82a0"],
}

# (directory under wire-linke-p4-2026-09-25, operation) — small but varied; the
# 76 MB monitor_sdi captures are deliberately left out.
WCH_LINKE = [
    ("l103", "target_info"),
    ("l103", "flash_pattern4k"),
    ("v203", "target_info"),
    ("v203", "flash_pattern4k"),
    ("v203-100mhz", "target_info"),
    ("v203-100mhz", "flash_pattern4k"),
    ("v203-160mhz", "flash_pattern4k"),
    ("v003", "target_info"),
    ("v003", "flash_pattern4k"),
]
WCH_SIDECARS = [".json", ".mem.txt", ".dmi.txt"]


def git_head(path: Path) -> dict:
    def run(*args: str) -> str:
        return subprocess.run(["git", "-C", str(path), *args], capture_output=True, text=True).stdout.strip()

    return {"commit": run("rev-parse", "HEAD"), "dirty": bool(run("status", "--porcelain"))}


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def copy(src: Path, dest_rel: str, origin: str, entries: list[dict]) -> None:
    dest = RAW / dest_rel
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dest)
    entries.append({"path": dest_rel, "origin": origin, "source": str(src), "sha256": sha256(dest), "bytes": dest.stat().st_size})


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--i2c-root", type=Path, default=Path.home() / "dev/I2CDeviceDB")
    ap.add_argument("--wch-root", type=Path, default=Path.home() / "dev_wch/wch-protocols")
    args = ap.parse_args()

    entries: list[dict] = []
    for stem in I2C_STEMS:
        copy(args.i2c_root / "captures/raw" / f"{stem}.sr", f"i2cdevicedb/{stem}.sr", "i2cdevicedb", entries)
        copy(args.i2c_root / "captures/decoded" / f"{stem}.jsonl", f"i2cdevicedb/{stem}.jsonl", "i2cdevicedb", entries)
    for chip, names in I2C_OBSERVATIONS.items():
        for name in names:
            copy(args.i2c_root / "observations" / chip / f"{name}.yaml", f"i2cdevicedb/observations/{name}.yaml", "i2cdevicedb", entries)

    linke = args.wch_root / "captures/fixtures/wire-linke-p4-2026-09-25"
    for target, op in WCH_LINKE:
        for suffix in [".sr", *WCH_SIDECARS]:
            src = linke / target / f"{op}{suffix}"
            if src.exists():
                copy(src, f"wch-linke/{target}/{op}{suffix}", "wch-protocols", entries)
    copy(linke / "README.ja.md", "wch-linke/README.ja.md", "wch-protocols", entries)

    manifest = {
        "sources": {
            "i2cdevicedb": {"root": str(args.i2c_root), **git_head(args.i2c_root)},
            "wch-protocols": {"root": str(args.wch_root), **git_head(args.wch_root)},
        },
        "files": entries,
    }
    (RAW / "manifest.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    total = sum(e["bytes"] for e in entries)
    print(f"collected {len(entries)} files, {total / 1e6:.1f} MB into {RAW}")


if __name__ == "__main__":
    main()
