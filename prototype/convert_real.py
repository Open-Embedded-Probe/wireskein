"""Convert corpus/raw/ captures into anonymized fixtures under corpus/fixtures/real/.

Ground truth comes from channel names, sidecar files and the source READMEs,
never from the analyzer under test.
"""

from __future__ import annotations

import hashlib
import json
import zlib
from pathlib import Path

from wsproto import fixture
from wsproto.srio import read_sr

ROOT = Path(__file__).resolve().parents[1]
RAW = ROOT / "corpus/raw"
OUT = ROOT / "corpus/fixtures/real"


def seed_of(name: str) -> int:
    return zlib.crc32(name.encode())


def i2c_transactions(jsonl: Path) -> list[dict]:
    out = []
    for line in jsonl.read_text().splitlines():
        t = json.loads(line)
        out.append({
            "addr": int(t["addr"], 16),
            "rw": t["rw"],
            "addr_ack": t["addr_ack"],
            "bytes": [int(b["value"], 16) for b in t["bytes"]],
        })
    return out


def convert(src: Path, fid: str, truth_for) -> None:
    cap = read_sr(src)
    anon, mapping = fixture.anonymize(cap, seed_of(fid))
    by_orig = {v: k for k, v in mapping.items()}
    truth = {
        "id": fid,
        "source": {"path": str(src.relative_to(ROOT)), "sha256": hashlib.sha256(src.read_bytes()).hexdigest()},
        "channels": {k: {"original": v, "static": len(anon.channel(k).edges) == 0} for k, v in mapping.items()},
        **truth_for(by_orig),
    }
    fixture.save(OUT / fid, anon, truth)
    size = sum(f.stat().st_size for f in (OUT / fid).iterdir())
    print(f"{fid}: {cap.n_samples} samples @ {cap.rate / 1e6:g} MHz, {sum(len(c.edges) for c in cap.channels)} edges, {size / 1e3:.0f} kB")


def main() -> None:
    for sr in sorted((RAW / "i2cdevicedb").glob("*.sr")):
        chip, _, _, speed, cond, h = sr.stem.split("__")
        fid = f"i2cdb-{chip}-{h[:6]}"
        tx = i2c_transactions(sr.with_suffix(".jsonl"))

        def truth(ch, tx=tx):
            return {
                "buses": [
                    {"protocol": "i2c", "roles": {"scl": ch["SCL"], "sda": ch["SDA"]},
                     "params": {"clock_hz": 100_000}, "expect": {"transactions": tx}},
                    {"protocol": "uart", "roles": {"data": ch["UART_TX"]},
                     "params": {"baud": 115_200, "data_bits": 8, "parity": "none", "stop_bits": 1, "idle": 1,
                                "bit_order": "lsb"}},
                ],
                "notes": "I2CDeviceDB characterize capture; UART carries CASE/PHASE marker lines (sigrok uart 115200 8N1).",
            }

        convert(sr, fid, truth)

    wch = RAW / "wch-linke"
    for sr in sorted(wch.glob("*/*.sr")):
        target = sr.parent.name
        fid = f"wch-{target}-{sr.stem.replace('_', '-')}"
        if target.startswith("v003"):
            def truth(ch):
                return {"buses": [{"protocol": "swio", "roles": {"dio": ch["SWIO"]}, "params": {}}],
                        "notes": "WCH single-wire SDI (SWIO); out of scope for UART/I2C/SPI, must not be confirmed as those."}
        else:
            def truth(ch):
                return {"buses": [{"protocol": "rvswd", "roles": {"clk": ch["SWCLK"], "dio": ch["SWDIO"]}, "params": {}}],
                        "notes": "WCH 2-wire RVSWD with reflection glitches on SWCLK; out of scope for UART/I2C/SPI."}
        convert(sr, fid, truth)


if __name__ == "__main__":
    main()
