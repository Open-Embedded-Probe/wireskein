"""Command-line front end of the prototype (staged engine).

    PYTHONPATH=. uv run python ws.py analyze CAPTURE [--mode final|all|select] [--select PATH ...]
                                          [--hint JSON|@file.json] [--alternatives] [--out FILE]

CAPTURE is a fixture directory (corpus/fixtures/real/<id>) or a sigrok .sr file.
Paths for --select: "<protocol>.<layer>" with wildcards, e.g. i2c.transactions,
uart.lines, spi.transfers, rvswd.dm, *.final, i2c.* .

    PYTHONPATH=. uv run python ws.py segments CAPTURE [--results] [--markers JSON]
    PYTHONPATH=. uv run python ws.py verify RUN_DIR [--junit FILE] [--json FILE]

Marker lines ("# test", "## step", "##" closes; see decl/markers/) split the
capture into a segment tree; --segment PATH on analyze restricts to one.

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

from wsproto import export, fixture, markers, staged
from wsproto.srio import read_sr


def load(path: Path):
    if path.is_dir():
        return fixture.load_capture(path)
    return read_sr(path)


def segments(args) -> None:
    hints = json.loads(Path(args.hint[1:]).read_text() if args.hint and args.hint.startswith("@") else args.hint) if args.hint else None
    cap = load(args.capture)
    staged.use_declarative(args.declarative)
    res = staged.analyze(cap, hints)
    found = markers.find(res, cap, hint=json.loads(args.markers) if args.markers else None)
    if found is None:
        sys.exit("no marker stream found (give --markers)")
    text = export.dumps(markers.describe(res, cap, found, results=args.results, with_events=args.events))
    if args.out:
        args.out.write_text(text)
        print(f"{len(text)} bytes -> {args.out}", file=sys.stderr)
    else:
        print(text)


def verify_cmd(args) -> None:
    from wsproto import verify
    rep = verify.verify(args.run)
    if not args.log:
        rep.pop("log")
    for r in rep["results"]:
        mark = {True: "OK", False: "NG", None: "--"}[r["ok"]]
        print(f"{mark}  {r['path']}  {r['check']}  {r['capture'] or ''}  {r['reason']}")
    s = rep["summary"]
    print(f"{s['ok']} ok, {s['ng']} ng, {s['unchecked']} unchecked ({s['segments']} segments, {s['captures']} captures)")
    if args.json:
        args.json.write_text(export.dumps(rep))
    if args.junit:
        args.junit.write_text(verify.junit(rep))
    sys.exit(1 if s["ng"] else 0)


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
    a.add_argument("--segment", default=None, metavar="PATH",
                   help='only items inside a marker segment, e.g. "session/pwm/1kHz-25[1]" (glob; first match)')
    a.add_argument("--markers", default=None, metavar="JSON",
                   help='marker source: {"pin": "D6", "dialect": "heading"} (default: found automatically)')
    a.add_argument("--events", action="store_true",
                   help="with --segment: include text lines as events (always included in --mode all)")
    sg = sub.add_parser("segments", help="the marker segment tree with per-segment activity and decoded items")
    sg.add_argument("capture", type=Path)
    sg.add_argument("--hint", default=None)
    sg.add_argument("--markers", default=None, metavar="JSON")
    sg.add_argument("--results", action="store_true", help="include the decoded items of each segment")
    sg.add_argument("--events", action="store_true", help="include text lines (commands, responses, child markers) as events")
    sg.add_argument("--declarative", action="store_true")
    sg.add_argument("--out", type=Path, default=None)
    vf = sub.add_parser("verify", help="check a recorded run (wsproto/runlog.py) against its expectations")
    vf.add_argument("run", type=Path, help="run directory with run.json")
    vf.add_argument("--junit", type=Path, default=None, help="also write JUnit XML")
    vf.add_argument("--json", type=Path, default=None, help="write the full report (with measured values)")
    vf.add_argument("--log", action="store_true", help="include the host log (markers, commands, replies) in --json")
    args = ap.parse_args()
    if args.cmd == "segments":
        return segments(args)
    if args.cmd == "verify":
        return verify_cmd(args)

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
    if args.segment:
        found = markers.find(res, cap, hint=json.loads(args.markers) if args.markers else None)
        if found is None:
            sys.exit("no marker stream found (give --markers)")
        hit = markers.select(found["tree"], args.segment)
        if not hit:
            sys.exit(f"no segment matches {args.segment!r}; see: ws.py segments CAPTURE")
        window = (hit[0].begin, hit[0].end)
        seg_events = markers.events_in(res, cap, found, window)
    else:
        seg_events = None
    doc = export.export(res, cap, mode, args.select, args.alternatives, window, args.depth)
    if seg_events is not None:
        doc["segment"] = {"path": hit[0].path(), "raw": hit[0].raw, "issues": hit[0].issues}
        if args.events or mode == "all":
            doc["events"] = seg_events
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
