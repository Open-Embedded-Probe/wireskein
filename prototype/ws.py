"""Command-line front end of the prototype (staged engine).

    PYTHONPATH=. uv run python ws.py analyze CAPTURE [--mode final|all|select] [--select PATH ...]
                                          [--hint JSON|@file.json] [--alternatives] [--out FILE]

CAPTURE is a fixture directory (corpus/fixtures/real/<id>) or a sigrok .sr file.
Paths for --select: "<protocol>.<layer>" with wildcards, e.g. i2c.transactions,
uart.lines, spi.transfers, rvswd.dm, *.final, i2c.* .

Hints restrict what is tried; the result is still scored by the plugins' checks:
    --hint '{"protocols": ["i2c", "uart"]}'
    --hint '{"pins": {"D6": {"protocol": "uart", "baud": 115200}, "D3": {"protocol": "i2c", "role": "scl"}}}'
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

from wsproto import export, fixture, staged
from wsproto.srio import read_sr


def load(path: Path):
    if path.is_dir():
        return fixture.load_capture(path)
    return read_sr(path)


def main() -> None:
    ap = argparse.ArgumentParser(prog="ws")
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("analyze")
    a.add_argument("capture", type=Path)
    a.add_argument("--mode", choices=["final", "all", "select"], default="final")
    a.add_argument("--select", nargs="*", default=[])
    a.add_argument("--hint", default=None)
    a.add_argument("--alternatives", action="store_true")
    a.add_argument("--depth", choices=["transport", "frames", "protocol", "device"], default="protocol",
                   help="how far up the interpretation goes in final mode")
    a.add_argument("--declarative", action="store_true", help="use the TOML-declared I2C/SPI/RVSWD plugins")
    a.add_argument("--devices", nargs="*", default=None, metavar="PATTERN",
                   help='device packs to use, paths under decl/devices: "i2c/**" "i2c/sensirion/*" "!spi/**"')
    a.add_argument("--out", type=Path, default=None)
    a.add_argument("--window", nargs=2, type=float, metavar=("FROM_S", "TO_S"), default=None,
                   help="only items inside this time range (seconds)")
    args = ap.parse_args()

    hints = None
    if args.hint:
        hints = json.loads(Path(args.hint[1:]).read_text() if args.hint.startswith("@") else args.hint)
    if args.devices is not None:
        hints = (hints or {}) | {"devices": args.devices}
    cap = load(args.capture)
    t0 = time.perf_counter()
    staged.use_declarative(args.declarative)
    res = staged.analyze(cap, hints)
    mode = "select" if args.select else args.mode
    window = (int(args.window[0] * cap.rate), int(args.window[1] * cap.rate)) if args.window else None
    doc = export.export(res, cap, mode, args.select, args.alternatives, window, args.depth)
    doc["analysis_seconds"] = round(time.perf_counter() - t0, 3)
    doc["hints"] = hints
    text = export.dumps(doc)
    if args.out:
        args.out.write_text(text)
        print(f"{len(text)} bytes -> {args.out}", file=sys.stderr)
    else:
        print(text)


if __name__ == "__main__":
    main()
