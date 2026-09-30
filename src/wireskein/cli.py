"""The `wireskein` command.

    wireskein analyze CAPTURE [--mode final|all|select] [--select PATH ...]
                              [--hint JSON|@file.json] [--alternatives] [--out FILE]
    wireskein segments CAPTURE [--results] [--markers JSON]
    wireskein verify RUN_DIR [--junit FILE] [--json FILE]
    wireskein convert IN OUT            (.wsc / .sr / fixture directory -> .wsc / .sr)
    wireskein info FILE.wsc             channels, rates, metadata, attachments, notes
    wireskein note FILE.wsc TEXT [--json] [--kind K]
    wireskein attach FILE.wsc NAME [SRC | --text TEXT] [--replace]

CAPTURE is a .wsc capture, a sigrok .sr file or a fixture directory (corpus/fixtures/real/<id>).
Paths for --select: "<protocol>.<layer>" with wildcards, e.g. i2c.transactions,
uart.lines, spi.transfers, rvswd.dm, *.final, i2c.* .

Marker lines ("# test", "## step", "##" closes; see decl/markers/) split the
capture into a segment tree; --segment PATH on analyze restricts to one.

RUN_DIR is a run recorded with wireskein.runlog; verify exits 1 when a check fails.

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

from . import __version__
from ._engine import export, markers, staged


def load(path: Path):
    from .analyze import load as _load
    return _load(path)


def info(args) -> None:
    from . import wsc
    head, chans = wsc.read(args.file)
    tick = wsc.tick_hz(head)
    print(f"{args.file}: {head['format']}, tick {float(tick):g} Hz ({tick}), {head['ticks']} ticks "
          f"({head['ticks'] / float(tick):.6g} s)")
    for c in chans:
        rate = float(tick) / c.step
        print(f"  {c.name:12s} {c.n:>12d} samples  step {c.step:<4d} phase {c.phase:<4d} {rate:g} Hz")
    if head.get("meta"):
        print("meta: " + json.dumps(head["meta"], ensure_ascii=False, default=str))
    for name, data in wsc.attachments(args.file).items():
        print(f"attach/{name}: {len(data)} bytes")
    for k, n in enumerate(wsc.notes(args.file), 1):
        extra = {x: v for x, v in n.items() if x not in ("time", "content")}
        body = n["content"] if isinstance(n["content"], str) else json.dumps(n["content"], ensure_ascii=False)
        print(f"note {k} {n['time']}" + (f" {json.dumps(extra, ensure_ascii=False)}" if extra else "") + f": {body}")


def note_cmd(args) -> None:
    from . import wsc
    content = json.loads(args.text) if args.json else args.text
    print(wsc.note(args.file, content, **({"kind": args.kind} if args.kind else {})))


def attach_cmd(args) -> None:
    from . import wsc
    if (args.src is None) == (args.text is None):
        sys.exit("give SRC (a file) or --text")
    data = args.src.read_bytes() if args.src else args.text
    wsc.attach(args.file, args.name, data, replace=args.replace)


def convert(args) -> None:
    from .analyze import save
    cap = load(args.input)
    meta = {k: v for k, v in cap.meta.items() if k not in ("file", "source", "tick_hz", "unitsize", "fixture", "extras")}
    out = save(args.output, cap, **meta)
    slow = [f"{c.name}/{c.step}" for c in cap.channels if c.step != 1]
    print(f"{args.input} -> {out}: {len(cap.channels)} channels, {cap.n_samples} ticks at {cap.rate:g} Hz"
          + (f", decimated: {', '.join(slow)}" if slow else ""))


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
    from . import verify
    rep = verify.verify(args.run)
    if not args.log:
        rep.pop("log")
    for line in verify.lines(rep, ok=True):
        print(line)
    print(verify.summary_line(rep))
    if args.json:
        args.json.write_text(verify.dumps(rep))
    if args.junit:
        args.junit.write_text(verify.junit(rep))
    sys.exit(1 if rep["summary"]["ng"] else 0)


def main() -> None:
    ap = argparse.ArgumentParser(prog="wireskein")
    ap.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
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
    vf = sub.add_parser("verify", help="check a recorded run (wireskein.runlog) against its expectations")
    vf.add_argument("run", type=Path, help="run directory with run.json")
    vf.add_argument("--junit", type=Path, default=None, help="also write JUnit XML")
    vf.add_argument("--json", type=Path, default=None, help="write the full report (with measured values)")
    vf.add_argument("--log", action="store_true", help="include the host log (markers, commands, replies) in --json")
    cv = sub.add_parser("convert", help="convert a capture between formats (by extension: .wsc, .sr; a fixture directory as input)")
    cv.add_argument("input", type=Path)
    cv.add_argument("output", type=Path)
    inf = sub.add_parser("info", help="what a .wsc holds: channels, rates, metadata, attachments, notes")
    inf.add_argument("file", type=Path)
    nt = sub.add_parser("note", help="append a note to a .wsc (its log is append-only)")
    nt.add_argument("file", type=Path)
    nt.add_argument("text")
    nt.add_argument("--json", action="store_true", help="TEXT is JSON, stored as such")
    nt.add_argument("--kind", default=None, help="a label stored with the note (e.g. analysis, setup)")
    at = sub.add_parser("attach", help="store a free-form file in a .wsc as attach/NAME")
    at.add_argument("file", type=Path)
    at.add_argument("name")
    at.add_argument("src", type=Path, nargs="?", default=None)
    at.add_argument("--text", default=None)
    at.add_argument("--replace", action="store_true")
    args = ap.parse_args()
    files = {"info": info, "note": note_cmd, "attach": attach_cmd, "convert": convert}
    if args.cmd in files:
        try:
            return files[args.cmd](args)
        except (ValueError, FileExistsError, FileNotFoundError, KeyError) as e:     # a file this version cannot use
            sys.exit(f"wireskein {args.cmd}: {e}")
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
