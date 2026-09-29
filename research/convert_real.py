"""Convert corpus/raw/ captures into anonymized fixtures under corpus/fixtures/real/.

Ground truth comes from channel names, sidecar files and the source READMEs,
never from the analyzer under test.
"""

from __future__ import annotations

import hashlib
import json
import zlib
from pathlib import Path

from wireskein._engine import fixture
from wireskein._engine.srio import read_sr

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


def swio_reference(sr: Path) -> dict:
    """DMI transactions from the wch-protocols SWIO decoder, if that repository is available."""
    import sys
    tools = Path.home() / "dev_wch/wch-protocols/captures/tools"
    if not tools.exists():
        return {}
    sys.path.insert(0, str(tools))
    try:
        import swio as ref
    except Exception:
        return {}
    frames, _ = ref.frames(str(sr), 0)
    rows = [[d["kind"], d["addr"], d["data"]] for _, b, _ in frames if (d := ref.decode(b))]
    return {"dmi_addr": rows} if rows else {}


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
            expect = swio_reference(sr)

            def truth(ch, expect=expect):
                return {"buses": [{"protocol": "swio", "roles": {"dio": ch["SWIO"]}, "params": {},
                                   **({"expect": expect, "expect_source": "wch-protocols captures/tools/swio.py (fixed 500 ns / 4 us thresholds)"} if expect else {})}],
                        "notes": "WCH single-wire SDI (SWIO); out of scope for UART/I2C/SPI, must not be confirmed as those."}
        else:
            expect = {}
            dmi = sr.with_suffix(".dmi.txt")
            if dmi.exists():
                rows = []
                for line in dmi.read_text().splitlines():
                    f = line.split()
                    if len(f) >= 4 and f[1] in ("W", "R"):
                        rows.append([f[1], f[2], int(f[3], 16)])
                if rows:
                    expect["dmi"] = rows
            mem = sr.with_suffix(".mem.txt")
            if mem.exists():
                rows = [l.split()[1:5] for l in mem.read_text().splitlines() if l.strip() and not l.startswith("#")]
                if rows:
                    expect["mem"] = rows

            def truth(ch, expect=expect):
                return {"buses": [{"protocol": "rvswd", "roles": {"clk": ch["SWCLK"], "dio": ch["SWDIO"]}, "params": {},
                                   **({"expect": expect, "expect_source": "wch-protocols tools/dmi_decode.py (.dmi.txt / .mem.txt)"} if expect else {})}],
                        "notes": "WCH 2-wire RVSWD with reflection glitches on SWCLK; out of scope for UART/I2C/SPI."}
        convert(sr, fid, truth)


if __name__ == "__main__":
    main()
