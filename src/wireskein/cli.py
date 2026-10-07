"""The `wireskein` command.

    wireskein analyze CAPTURE [--mode final|all|select] [--select PATH ...]
                              [--hint JSON|@file.json] [--alternatives] [--out FILE]
    wireskein segments CAPTURE [--results] [--markers JSON]
    wireskein verify RUN_DIR [--junit FILE] [--json FILE]
    wireskein convert IN OUT            (WireSkein / .sr / fixture directory -> WireSkein, or .sr by the name)
    wireskein capture --source oep:PORT|sigrok:DRIVER --channels NAME=ID,... --rate 20M --samples 1M
                      [--trigger NAME:rise|fall|both|high|low] [--pretrigger N] [--note TEXT] -o OUT.wireskein
    wireskein info FILE.wireskein       channels, rates, metadata, attachments, notes
    wireskein gui [FILE | DIR] [--port N] [--no-browser]   the viewer in the browser (localhost only)
    wireskein note FILE.wireskein TEXT [--json] [--kind K]
    wireskein attach FILE.wireskein NAME [SRC | --text TEXT] [--replace]
    wireskein annotate FILE.wireskein [--save] [--hint JSON]   decoding results as rows for the viewer
    wireskein align FILE.wireskein --reference LOGIC --via ANALOG --threshold V|LOW,HIGH
                    [--max-offset 300us] [--apply-to A,B] [--save]   analog tracks onto the logic ticks
    wireskein align B.wireskein --to A.wireskein --reference A_LOGIC --via B_CHANNEL [--save]
                                                               another probe's capture onto A's ticks

CAPTURE is a WireSkein file (.wireskein), a sigrok .sr file or a fixture directory (corpus/fixtures/real/<id>).
Files are told apart by their content, not their name; an output is a .sr when its name ends in .sr.
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
import zipfile
import sys
import time
from pathlib import Path

from . import __version__
from ._engine import export, markers, staged


def load(path: Path):
    from .analyze import load as _load
    return _load(path)


def info(args) -> None:
    from . import fileformat
    head, chans = fileformat.read(args.file)
    tick = fileformat.tick_hz(head)
    print(f"{args.file}: {fileformat.FORMAT}, tick {float(tick):g} Hz ({tick}), {head['ticks']} ticks "
          f"({head['ticks'] / float(tick):.6g} s), id {head.get('id', '-')}")
    for c in chans:
        if isinstance(c, fileformat.AnalogChannel):
            conv = (f"raw {c.width}-bit" + (f" ({c.value_bits} valid)" if c.value_bits else "")
                    + (f", {c.unit} = (raw - {c.zero:g}) x {c.scale_nv:g} n{c.unit}" if c.zero is not None and c.scale_nv is not None
                       else ", no volt conversion")) if c.encoding == "analog" else f"float32 {c.unit}"
            print(f"  {c.name:12s} {c.n:>12d} samples  {c.encoding:10s} {float(c.rate_hz):g} Hz from tick "
                  f"{float(c.t0_ticks):g}  {conv}")
            if c.encoding == "analog" and c.value_bits and c.zero is not None and c.scale_nv is not None:
                top = (1 << c.value_bits) - 1
                vals = c.values()
                lo, hi = sum(v <= 0 for v in vals), sum(v >= top for v in vals)
                lo_v, hi_v = -c.zero * c.scale_nv * 1e-9, (top - c.zero) * c.scale_nv * 1e-9
                if c.scale_nv < 0:                    # an inverting frontend: code 0 is the high end
                    lo, hi, lo_v, hi_v = hi, lo, hi_v, lo_v
                print(f"  {'':12s} range {lo_v:.3f}..{hi_v:.3f} {c.unit}" + (
                    f": {lo} samples clipped at <= {lo_v:.3f}, {hi} at >= {hi_v:.3f} (not voltages)" if lo or hi else ""))
        elif isinstance(c, fileformat.IntervalChannel):
            what = "any tick at" if c.encoding == "interval-any" else "end level, and a change to"
            print(f"  {c.name:12s} {c.n:>12d} values   {c.encoding:14s} {c.step} ticks a value from tick {c.phase} "
                  f"({what} {'active-high' if c.active else 'active-low'})")
        else:
            rate = float(tick) / c.step
            print(f"  {c.name:12s} {c.n:>12d} samples  step {c.step:<4d} phase {c.phase:<4d} {rate:g} Hz")
        if c.acquisition:
            print(f"  {'':12s} acquisition: {json.dumps(c.acquisition, ensure_ascii=False)}")
    for c in fileformat.skipped(head):
        print(f"  {c['name']:12s} encoding {c['encoding']!r}: not read by this version")
    meta = head.get("meta") or {}
    if meta:
        print("meta: " + json.dumps(meta, ensure_ascii=False, default=str))
    for line in _meta_notes(meta, tick):
        print("  " + line)
    for name, data in fileformat.attachments(args.file).items():
        print(f"attach/{name}: {len(data)} bytes")
    for k, n in enumerate(fileformat.notes(args.file), 1):
        extra = {x: v for x, v in n.items() if x not in ("time", "content")}
        body = n["content"] if isinstance(n["content"], str) else json.dumps(n["content"], ensure_ascii=False)
        print(f"note {k} {n['time']}" + (f" {json.dumps(extra, ensure_ascii=False)}" if extra else "") + f": {body}")


def _meta_notes(meta: dict, tick) -> list[str]:
    """What the meta keys with a set meaning say about this capture (docs/wireskein-format.ja.md §3.3)."""
    out = []
    if meta.get("time_base_slipped"):
        out.append("time_base_slipped: the probe knows some samples were taken late (its sampling fell behind, "
                   "e.g. at its buffer limit), so times in this capture may be stretched there; verify still "
                   "decides, and a failed check says the probe reported a slip")
    if isinstance(meta.get("trigger_tick"), int):
        out.append(f"trigger_tick {meta['trigger_tick']}: the trigger at tick {meta['trigger_tick']} "
                   f"({meta['trigger_tick'] / float(tick) * 1e6:.3f} us from the start)")
    if isinstance(meta.get("start_uncertainty_ns"), (int, float)):
        out.append(f"start_ns is the probe clock of the first sample, +- {meta['start_uncertainty_ns'] / 1000:g} us")
    return out


def note_cmd(args) -> None:
    from . import fileformat
    content = json.loads(args.text) if args.json else args.text
    print(fileformat.note(args.file, content, **({"kind": args.kind} if args.kind else {})))


def attach_cmd(args) -> None:
    from . import fileformat
    if (args.src is None) == (args.text is None):
        sys.exit("give SRC (a file) or --text")
    data = args.src.read_bytes() if args.src else args.text
    fileformat.attach(args.file, args.name, data, replace=args.replace)


def _probe_errors() -> tuple:
    """The OEP client's errors (all under OepError: Rejected, Failed, Expired when a lease ran out, ...),
    when it is installed, so a capture that fails on the probe ends in one line."""
    try:
        from oep_client.message import OepError
    except ImportError:
        return ()
    return (OepError,)


def _thresholds(items: list[str] | None) -> dict:
    """["RX=1.65", "SDA=1.0,2.3"] -> {"RX": 1.65, "SDA": (1.0, 2.3)}."""
    out = {}
    for it in items or []:
        name, sep, v = it.partition("=")
        if not sep:
            raise ValueError(f"--threshold {it!r}: give NAME=V or NAME=LOW,HIGH")
        vals = [float(x) for x in v.split(",")]
        out[name] = vals[0] if len(vals) == 1 else tuple(vals)
    return out


def _load_for_decoding(path, args):
    """load(), with the analog channels named by --threshold read as logic too."""
    cap = load(path)
    th = _thresholds(getattr(args, "threshold", None))
    if th:
        from .analyze import as_logic
        cap = as_logic(cap, th)
    return cap


def _seconds(text: str) -> float:
    """"300us", "1.5ms", "2s", "0.001" -> seconds."""
    import re
    m = re.fullmatch(r"\s*([\d.]+)\s*(ns|us|µs|ms|s)?\s*", text)
    if not m:
        raise ValueError(f"not a time: {text!r}")
    return float(m.group(1)) * {"ns": 1e-9, "us": 1e-6, "µs": 1e-6, "ms": 1e-3, "s": 1.0, None: 1.0}[m.group(2)]


def annotate_cmd(args) -> None:
    from . import annotate
    from .analyze import load
    hints = json.loads(Path(args.hint[1:]).read_text() if args.hint.startswith("@") else args.hint) if args.hint else None
    doc = annotate.build(_load_for_decoding(args.file, args), hints)
    for r in doc["rows"]:
        bad = sum(i["level"] != "ok" for i in r["items"])
        print(f"{r['name']:24s} under {r.get('near', '-'):8s} {len(r['items']):6d} items" + (f", {bad} not ok" if bad else ""))
    if not doc["rows"]:
        print("nothing decoded")
    if args.save:
        annotate.save(args.file, doc)
        print(f"saved as {annotate.ENTRY}")


def align_cmd(args) -> None:
    from . import align
    from .analyze import load
    cap = load(args.file)
    th = [float(v) for v in args.threshold.split(",")] if args.threshold else None
    if args.to:                                               # onto another file's ticks (spec §5.1.1)
        ref = load(args.to)
        window = _seconds(args.max_offset) * ref.rate if args.max_offset else None
        e = align.between(ref, args.reference, cap, args.via, (th if len(th) > 1 else th[0]) if th else None,
                          window, args.max_ppm)
        print(f"{args.file} onto {Path(args.to).name}: tick 0 at {e['offset_us']:+.3f} us, clock "
              f"{e['scale_ppm']:+.2f} +- {e['scale_ppm_uncertainty']:.2f} ppm, {e['matched']}/{e['overlap_edges']} "
              f"edges matched, residual {e['residual_ticks'] / ref.rate * 1e6:.3f} us")
        if args.save:
            align.save_between(args.file, args.to, e)
            print(f"saved in attach/{align.NAME} (files: {Path(args.to).name})")
        return
    if th is None:
        raise ValueError("give --threshold (the analog channel is read as logic)")
    window = _seconds(args.max_offset) * cap.rate if args.max_offset else None
    apply_to = args.apply_to.split(",") if args.apply_to else None
    a = align.find(cap, args.reference, args.via, th if len(th) > 1 else th[0], window, apply_to)
    c = a["channels"][args.via]
    print(f"{args.via} against {args.reference}: start {c['start_shift_us']:+.3f} us, "
          f"scale {c['scale_ppm']:+.1f} +- {c['scale_ppm_uncertainty']:.1f} ppm, "
          f"{c['matched']}/{c['overlap_edges']} edges matched, residual {c['residual_ticks'] / cap.rate * 1e6:.3f} us")
    if c["overlap_edges"] < 0.9 * c["edges"]:
        print(f"note: {args.reference} has edges over {c['overlap_s'] * 1e3:.3g} ms only; {c['edges'] - c['overlap_edges']} "
              f"of the {c['edges']} edges of {args.via} lie outside it (the scale comes from that span)")
    print(f"applies to: {', '.join(a['channels'])}")
    if args.save:
        align.save(args.file, a)
        print(f"saved as attach/{align.NAME}")


def _count(cap) -> str:
    return (f"{len(cap.channels)} logic" + (f" + {len(cap.intervals)} interval" if cap.intervals else "")
            + (f" + {len(cap.analog)} analog" if cap.analog else "") + " channels")


def gui_cmd(args) -> None:
    from . import gui
    gui.main(args.path, args.port, not args.no_browser)


def capture_cmd(args) -> None:
    from . import sources
    logic = sources.parse_channels(args.channels or "")
    rate = args.rate or (args.analog_rate if not logic else None)        # analog only: its own rate / samples do
    samples = args.samples or (args.analog_samples if not logic else None)
    if not rate or not samples:
        need = "--rate and --samples" if logic else "--analog-rate (or --rate) and --analog-samples (or --samples)"
        raise ValueError(f"give {need}")
    req = sources.Request(
        channels=logic, rate=sources.parse_count(rate), samples=sources.parse_count(samples),
        trigger=sources.parse_trigger(args.trigger) if args.trigger else None,
        pretrigger=sources.parse_count(args.pretrigger) if args.pretrigger else None, timeout=args.timeout,
        analog=sources.parse_analog(args.analog) if args.analog else [],
        analog_rate=sources.parse_count(args.analog_rate) if args.analog_rate else None,
        analog_samples=sources.parse_count(args.analog_samples) if args.analog_samples else None,
        reduce=sources.parse_reduce(args.channels or ""))
    out = sources.capture(args.source, req, args.output)
    if args.note:
        from . import fileformat
        if fileformat.sniff(out) == "wireskein":
            fileformat.note(out, args.note)
    from .analyze import load
    cap = load(out)
    print(f"{out}: {_count(cap)}, {cap.n_samples} ticks at {cap.rate:g} Hz ({cap.duration:.6g} s)")
    _fewer(out, req)
    link = (cap.meta.get("probe") or {}).get("link")
    if link:
        print("link: " + _link_line(link))


def _link_line(link: dict) -> str:
    """How the probe's link went: the rate in force and the trials, the open and the read (meta.probe.link)."""
    parts = []
    if "rate" in link and not link.get("rate"):        # no serial speed here (USB, TCP): say why only
        parts.append(f"not raised ({link.get('why', 'no serial port')})")
    elif "rate" in link:
        tried = [f"{t['rate']} {t['result']}" for t in link.get("trials", []) if t["result"] != "committed"]
        parts.append(f"{link['rate']} baud" + (" (raised)" if link.get("raised") else " (the boot speed)")
                     + (f", tried {'; '.join(tried)}" if tried else "") + (f", {link['why']}" if link.get("why") else "")
                     + (f", skipped {', '.join(map(str, link['skipped']))} (failed before)" if link.get("skipped") else "")
                     + "".join(f", {d['from']} -> {d['to'] or 'the boot speed'}"
                               f" {'in its probation' if d['probation'] else 'in use'} ({d['why']})"
                               for d in link.get("step_downs", []))
                     + (f", stepped down while in use ({link['stepped_down']})"
                        if link.get("stepped_down") and not link.get("step_downs") else "")
                     + (f", retried {link['retried']} (all failed before)" if link.get("retried") else "")
                     + (", rates from what this port and probe did before" if link.get("remembered") else ""))
    parts.append(f"opened in {link['open_s']:.2f} s")
    kb = link["read_bytes"] / 1000
    rate = f" ({kb / link['read_s']:.1f} KB/s)" if link["read_s"] > 0 else ""
    parts.append(f"read {kb:.1f} KB in {link['read_s']:.2f} s{rate}")
    return ", ".join(parts)


def _fewer(out, req) -> None:
    """Say when the probe gave fewer samples than asked (its buffer limit): easy to miss otherwise."""
    from . import fileformat
    if fileformat.sniff(out) != "wireskein":
        return
    _, chans = fileformat.read(out)
    for c in chans:
        if isinstance(c, fileformat.AnalogChannel):
            if req.analog_samples and c.n < req.analog_samples:
                print(f"note: {c.name}: {c.n} samples of the {req.analog_samples} asked (the probe's limit)")
        elif c.step == 1 and isinstance(c, fileformat.Channel):
            if req.samples and c.n < req.samples:
                print(f"note: {c.name}: {c.n} samples of the {req.samples} asked (the probe's limit)")
        elif req.samples and c.end + c.step <= req.samples:          # a reduced channel: its values cover fewer samples
            print(f"note: {c.name}: {c.n} values cover {c.end} of the {req.samples} samples asked (the probe's limit)")


def convert(args) -> None:
    from .analyze import save
    cap = load(args.input)
    meta = {k: v for k, v in cap.meta.items()
            if k not in ("file", "sr_file", "vcd_file", "tick_hz", "unitsize", "fixture", "extras", "skipped_channels",
                         "vcd_not_read")}
    out = save(args.output, cap, **meta)
    slow = [f"{c.name}/{c.step}" for c in cap.channels if c.step != 1]
    print(f"{args.input} -> {out}: {_count(cap)}, {cap.n_samples} ticks at {cap.rate:g} Hz"
          + (f", decimated: {', '.join(slow)}" if slow else ""))
    for c in cap.meta.get("vcd_not_read", []):
        print(f"note: {c['name']} not read ({c['encoding']})")
    if Path(out).suffix == ".vcd" and cap.meta.get("extras"):
        print(f"note: a VCD has no place for attachments and notes: {len(cap.meta['extras'])} entries not carried")


def segments(args) -> None:
    hints = json.loads(Path(args.hint[1:]).read_text() if args.hint and args.hint.startswith("@") else args.hint) if args.hint else None
    cap = _load_for_decoding(args.capture, args)
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
        args.junit.write_text(verify.junit(rep, args.allow_unchecked))
    sys.exit(1 if verify.failed(rep, args.allow_unchecked) else 0)


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
    a.add_argument("--threshold", action="append", metavar="NAME=V|NAME=LOW,HIGH",
                   help="also decode this analog channel, read as logic at V (repeatable)")
    a.add_argument("--window", nargs=2, type=_seconds, metavar=("FROM", "TO"), default=None,
                   help="only items inside this time range (e.g. 1ms 5ms; a plain number is seconds)")
    a.add_argument("--segment", default=None, metavar="PATH",
                   help='only items inside a marker segment, e.g. "session/pwm/1kHz-25[1]" (glob; first match)')
    a.add_argument("--markers", default=None, metavar="JSON",
                   help='marker source: {"pin": "D6", "dialect": "heading"} (default: found automatically)')
    a.add_argument("--events", action="store_true",
                   help="with --segment: include text lines as events (always included in --mode all)")
    sg = sub.add_parser("segments", help="the marker segment tree with per-segment activity and decoded items")
    sg.add_argument("capture", type=Path)
    sg.add_argument("--hint", default=None)
    sg.add_argument("--threshold", action="append", metavar="NAME=V|NAME=LOW,HIGH",
                    help="also decode this analog channel, read as logic at V (repeatable)")
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
    vf.add_argument("--allow-unchecked", action="store_true",
                    help="checks that could not be made (a pin not captured, ...) do not fail the run")
    cv = sub.add_parser("convert", help="convert a capture between formats (read by content; written as .sr when OUT ends in .sr, else WireSkein)")
    cv.add_argument("input", type=Path)
    cv.add_argument("output", type=Path)
    cp = sub.add_parser("capture", help="capture logic channels from a device into a .wireskein (sources: oep, sigrok)")
    cp.add_argument("--source", required=True, help="oep:<serial port | tcp://HOST:PORT | usb | usb:UNIT_ID | usb:VID:PID>[?fast=0 | ?fast=RATE,RATE] or sigrok:<driver> "
                         "(a UART probe's link is raised for reading back, 1.5M / 921600 / 500000 first that "
                         "works; fast=0 keeps the boot speed)")
    cp.add_argument("--channels", default=None, help='logic: "NAME=ID,..." (ID: the probe channel number / sigrok channel) or "ID,..."; '
                         'a probe with multirate (OEP) can keep a channel at fewer values: NAME=ID/4 (every 4th '
                         'sample), NAME=ID/4+1 (from sample 1), NAME=ID:any-low/32 (per 32 samples, low at any?), '
                         'NAME=ID:latch-high/8 (per 8: the last level, and whether it went high)')
    cp.add_argument("--analog", default=None, help='analog: "NAME=ID[@FRONTEND],..." (FRONTEND: the input range number, OEP)')
    cp.add_argument("--analog-rate", default=None, help="analog samples per second (default: --rate)")
    cp.add_argument("--analog-samples", default=None, help="default: as long as the logic capture")
    cp.add_argument("--rate", default=None, help="logic samples per second, e.g. 20M, 500k (analog too unless --analog-rate)")
    cp.add_argument("--samples", default=None, help="logic samples, e.g. 1M, 200000 (analog only: --analog-samples does)")
    cp.add_argument("--trigger", default=None, help="NAME:rise|fall|both|high|low (default: start at once)")
    cp.add_argument("--pretrigger", default=None, help="samples kept before the trigger")
    cp.add_argument("--timeout", type=float, default=10.0, help="seconds to wait for the capture")
    cp.add_argument("--note", default=None, help="a note stored with the capture")
    cp.add_argument("-o", "--output", type=Path, required=True, help="OUT.wireskein (or OUT.sr)")
    gp = sub.add_parser("gui", help="show captures in the browser (a local server, 127.0.0.1 only)")
    gp.add_argument("path", type=Path, nargs="?", default=Path("."), help="a WireSkein or .sr file, or a directory (default: .)")
    gp.add_argument("--port", type=int, default=0, help="default: any free port")
    gp.add_argument("--no-browser", action="store_true", help="only print the URL")
    inf = sub.add_parser("info", help="what a WireSkein file holds: channels, rates, metadata, attachments, notes")
    inf.add_argument("file", type=Path)
    nt = sub.add_parser("note", help="append a note to a WireSkein file (its log is append-only)")
    nt.add_argument("file", type=Path)
    nt.add_argument("text")
    nt.add_argument("--json", action="store_true", help="TEXT is JSON, stored as such")
    nt.add_argument("--kind", default=None, help="a label stored with the note (e.g. analysis, setup)")
    at = sub.add_parser("attach", help="store a free-form file in a WireSkein file as attach/NAME")
    at.add_argument("file", type=Path)
    at.add_argument("name")
    at.add_argument("src", type=Path, nargs="?", default=None)
    at.add_argument("--text", default=None)
    at.add_argument("--replace", action="store_true")
    an = sub.add_parser("annotate", help="decoding results as rows for the viewer (decode/annotations.json)")
    an.add_argument("file", type=Path)
    an.add_argument("--save", action="store_true", help="store them in the file")
    an.add_argument("--hint", default=None, help="as for analyze: JSON or @file.json")
    an.add_argument("--threshold", action="append", metavar="NAME=V|NAME=LOW,HIGH",
                    help="also decode this analog channel, read as logic at V (repeatable)")
    al = sub.add_parser("align", help="align analog tracks to the logic ticks by a signal on both (attach/alignment.json)")
    al.add_argument("file", type=Path)
    al.add_argument("--to", default=None, help="another WireSkein file to align this one onto (its --reference)")
    al.add_argument("--reference", required=True, help="the logic channel (of --to, when given)")
    al.add_argument("--via", required=True, help="this file's channel with the same signal")
    al.add_argument("--threshold", default=None, help="volts for an analog --via: V, or LOW,HIGH for hysteresis")
    al.add_argument("--max-ppm", type=float, default=200, help="with --to: how far the two clocks may differ")
    al.add_argument("--max-offset", default=None,
                    help="how far off the start may be (e.g. 300us; a plain number is seconds; default from the probe)")
    al.add_argument("--apply-to", default=None, help="analog channels that get the result (default: all)")
    al.add_argument("--save", action="store_true", help="store it in the file as attach/alignment.json")
    args = ap.parse_args()
    files = {"info": info, "note": note_cmd, "attach": attach_cmd, "convert": convert, "capture": capture_cmd,
             "gui": gui_cmd, "align": align_cmd, "annotate": annotate_cmd}
    if args.cmd in files:
        try:
            return files[args.cmd](args)
        except (ValueError, OSError, KeyError, RuntimeError, TimeoutError,
                zipfile.BadZipFile, *_probe_errors()) as e:
            what = "" if type(e) in (ValueError, FileNotFoundError, zipfile.BadZipFile) or isinstance(e, zipfile.BadZipFile) \
                else f"{type(e).__name__}: "
            sys.exit(f"wireskein {args.cmd}: {what}{e}")
    if args.cmd == "segments":
        try:
            return segments(args)
        except (ValueError, FileNotFoundError, zipfile.BadZipFile) as e:
            sys.exit(f"wireskein segments: {e}")
    if args.cmd == "verify":
        return verify_cmd(args)

    hints = None
    if args.hint:
        hints = json.loads(Path(args.hint[1:]).read_text() if args.hint.startswith("@") else args.hint)
    if args.devices is not None:
        hints = (hints or {}) | {"devices": args.devices}
    try:
        cap = _load_for_decoding(args.capture, args)
    except (ValueError, FileNotFoundError, zipfile.BadZipFile) as e:
        sys.exit(f"wireskein analyze: {e}")
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
            sys.exit(f"no segment matches {args.segment!r}; see: wireskein segments CAPTURE")
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
