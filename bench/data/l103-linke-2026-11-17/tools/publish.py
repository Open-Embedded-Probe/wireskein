"""The bench's copy of runs taken by capture.py: each capture cut to its operation, the truth beside it.

    uv run --project <wireskein> python tools/publish.py <raw runs dir> <bench runs dir> [run ...]

A raw run holds a stream of several seconds, the operation in a part of it. The copy keeps the ticks from 20 ms
before ch32rv started to 100 ms after it ended (from take.json's host times and the capture's clock record), written
by `wireskein convert --from --to` (meta.excerpt says where; start_ns is the excerpt's own), with run.json, take.json,
truth.json, ch32rv.ndjson / .json / .stderr and read.bin as they are. reading.json is made again on the excerpt
(tools/truth.py), and the excerpt must read the same DMI as the whole capture did: a run that does not is left out
and listed. The raw runs stay where they are (kept outside the repository)."""
from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import capture  # noqa: E402
import truth  # noqa: E402

BEFORE_S, AFTER_S = 0.02, 0.10
COPY = ("run.json", "take.json", "truth.json", "ch32rv.ndjson", "ch32rv.json", "ch32rv.stderr", "read.bin")


def publish(raw: Path, out: Path) -> str:
    take = json.loads((raw / "take.json").read_text())
    start, span = capture.first_sample_host_ns(raw / "c0001.wireskein")
    a = max(0.0, (take["t0_host_ns"] - start) / 1e9 - BEFORE_S)
    b = min(span, (take["t1_host_ns"] - start) / 1e9 + AFTER_S)
    tmp = out.with_name(out.name + ".tmp")
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir(parents=True)
    subprocess.run(["wireskein", "convert", "--from", f"{a:.6f}", "--to", f"{b:.6f}", str(raw / "c0001.wireskein"),
                    str(tmp / "c0001.wireskein")], check=True, capture_output=True)
    for f in COPY:
        if (raw / f).exists():
            shutil.copy2(raw / f, tmp / f)
    whole = json.loads((raw / "reading.json").read_text())
    wire, reading = truth.wire_dmi(tmp / "c0001.wireskein")
    reading["tick_hz"] = whole["tick_hz"]
    reading["dmi"] = truth.eval_rows(wire)
    if reading["dmi"] != whole["dmi"]:
        shutil.rmtree(tmp)
        return f"left out: the excerpt reads {len(reading['dmi'])} DMI rows, the whole {len(whole['dmi'])}"
    (tmp / "reading.json").write_text(json.dumps(reading, indent=1))
    t = json.loads((tmp / "take.json").read_text())
    t["excerpt_s"] = [round(a, 6), round(b, 6)]
    (tmp / "take.json").write_text(json.dumps(t, indent=1))
    shutil.rmtree(out, ignore_errors=True)
    tmp.rename(out)
    return f"{(raw / 'c0001.wireskein').stat().st_size / 1e6:.2f} -> {(out / 'c0001.wireskein').stat().st_size / 1e6:.3f} MB"


def main() -> None:
    raw_root, out_root = Path(sys.argv[1]), Path(sys.argv[2])
    runs = [raw_root / r for r in sys.argv[3:]] or [p.parent for p in sorted(raw_root.rglob("truth.json"))]
    for raw in runs:
        rel = raw.relative_to(raw_root)
        out = out_root / rel
        out.parent.mkdir(parents=True, exist_ok=True)
        print(f"{rel}: {publish(raw, out)}", flush=True)


if __name__ == "__main__":
    main()
