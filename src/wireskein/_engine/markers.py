"""Marker lines -> a tree of time segments.

A dialect (decl/markers/<name>.toml) gives the line patterns and their action.
The default is the heading dialect of docs/workbench-model.ja.md:

  heading "#", "##", ... : the count of '#' is the level; closes open sections of
          that level or deeper and opens a new one; with no name it only closes

Other dialects (existing logs such as the I2CDeviceDB harness) name their levels,
outermost first, and use:

  begin   opens a segment at a level; closes open segments of that level or deeper
  end     closes the nearest open segment of that level (name/seq checked)
  point   a section: lasts until the next point of the same level, or the parent's end
  attr    attaches key -> value (JSON if it parses) to the innermost open segment

Segment times are in samples: a segment starts at the end of its marker line
and ends at the start of the closing marker line, so the marker traffic is
outside the segment. Lines that are not markers (PING, PONG, parameters, on
the marker channel or any other UART) are events with times (events()), so a
segment shows which command was passed where. Siblings with the same name are told apart by an index in the path
("step[1]"); the raw marker text is kept. Problems are recorded as issues,
never silently fixed: missing end, end without begin, level skip, name/seq
mismatch, seq gap, unparsed line.
"""

from __future__ import annotations

import json
import re
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parents[1] / "decl" / "markers"


@dataclass(eq=False)
class Segment:
    level: str | int                     # dialect level name, or heading depth
    name: str
    begin: int
    end: int | None = None
    seq: int | None = None
    kind: str = "block"                  # block (begin/end) / section (point, heading) / root
    raw: str = ""                        # the marker line as received
    attrs: dict = field(default_factory=dict)
    children: list = field(default_factory=list)
    issues: list = field(default_factory=list)
    parent: "Segment | None" = None

    def path(self) -> str:
        parts, s = [], self
        while s is not None and s.kind != "root":
            sib = [x for x in s.parent.children if x.name == s.name] if s.parent else [s]
            parts.append(s.name + (f"[{sib.index(s)}]" if len(sib) > 1 else ""))
            s = s.parent
        return "/".join(reversed(parts))

    def walk(self):
        yield self
        for c in self.children:
            yield from c.walk()


class _Depth(dict):
    """Heading levels are their depth (1, 2, ...), unbounded."""

    def __getitem__(self, k):
        return int(k)

    def __contains__(self, k):
        return isinstance(k, int)


def load_dialects() -> dict[str, dict]:
    out = {}
    for p in sorted(HERE.glob("*.toml")):
        d = tomllib.loads(p.read_text())
        for m in d["marker"]:
            m["_re"] = re.compile(m["pattern"])
        out[d["dialect"]["name"]] = d
    return out


def lines_with_times(node, rate: float) -> list[tuple[str, int, int]]:
    """(text, first sample, sample after the last character) per complete line of a UART claim."""
    it = node.output.items
    vals = np.asarray(it["value"], dtype=np.int64) & 0xFF
    st = np.asarray(it["start"], dtype=np.int64)
    char = int(round(node.params.get("L", 10) * rate / node.params["baud"]))
    out, a = [], 0
    for i in np.flatnonzero(vals == 0x0A).tolist():
        text = bytes(vals[a:i].tolist()).decode("latin-1").rstrip("\r")
        if a < i or text:
            out.append((text, int(st[a]) if a < len(st) else int(st[i]), int(st[i]) + char))
        a = i + 1
    return out


def _value(s: str):
    try:
        return json.loads(s)
    except ValueError:
        return s


def build(lines: list[tuple[str, int, int]], dialect: dict, n_samples: int) -> Segment:
    heading = dialect["dialect"].get("kind") == "heading"
    levels = dialect["dialect"].get("levels", [])
    prefix = dialect["dialect"].get("prefix")
    rank = {lv: i for i, lv in enumerate(levels)}
    if heading:
        rank = _Depth()
    root = Segment("capture", "", 0, n_samples, kind="root")
    stack = [root]
    last_seq: dict[str, int] = {}

    def close(seg, t, issue=None):
        seg.end = t
        if issue:
            seg.issues.append(issue)

    def close_from(depth_rank, t, why):
        # close open segments at depth_rank or deeper (sections silently)
        while len(stack) > 1 and rank[stack[-1].level] >= depth_rank:
            s = stack.pop()
            close(s, t, None if s.kind == "section" else why)

    for text, t0, t1 in lines:
        m = next(((mk, mk["_re"].match(text)) for mk in dialect["marker"] if mk["_re"].match(text)), None)
        if m is None:
            if prefix and text.startswith(prefix):
                stack[-1].issues.append({"kind": "unparsed", "at": t0, "text": text})
            continue
        mk, g = m
        gd = g.groupdict()
        act = mk["action"]
        if act == "heading":
            depth = len(gd["hashes"])
            name = gd.get("name", "")
            if name:
                close_from(depth, t0, None)
                parent = stack[-1]
                pd = parent.level if parent.kind != "root" else 0
                s = Segment(depth, name, t1, kind="section", raw=text, parent=parent)
                if depth > pd + 1:
                    s.issues.append({"kind": "level_skip", "from": pd, "to": depth})
                parent.children.append(s)
                stack.append(s)
            elif any(x.kind != "root" and x.level == depth for x in stack):
                close_from(depth, t0, None)
            else:
                stack[-1].issues.append({"kind": "end_without_begin", "at": t0, "text": text})
            continue
        if act == "attr":
            key = gd.get("key") or mk["key"]
            v = _value(gd.get("value", ""))
            stack[-1].attrs.setdefault(key, []).append(v)
            continue
        level = gd.get("level") or mk.get("level")
        if level not in rank:
            stack[-1].issues.append({"kind": "unknown_level", "at": t0, "text": text})
            continue
        seq = int(gd["seq"]) if gd.get("seq") else None
        name = gd.get("name", "")
        if act in ("begin", "point"):
            close_from(rank[level], t0, {"kind": "implicit_end", "at": t0, "by": text})
            parent = stack[-1]
            s = Segment(level, name, t1, seq=seq, kind="block" if act == "begin" else "section", raw=text, parent=parent)
            if seq is not None:
                if level in last_seq and seq != last_seq[level] + 1:
                    s.issues.append({"kind": "seq_gap", "expected": last_seq[level] + 1, "got": seq})
                last_seq[level] = seq
            parent.children.append(s)
            stack.append(s)
        elif act == "end":
            idx = next((i for i in range(len(stack) - 1, 0, -1)
                        if stack[i].level == level and stack[i].kind == "block"), None)
            if idx is None:
                stack[-1].issues.append({"kind": "end_without_begin", "at": t0, "text": text})
                continue
            target = stack[idx]
            while len(stack) - 1 > idx:
                s = stack.pop()
                close(s, t0, None if s.kind == "section" else {"kind": "unterminated", "closed_by": text})
            stack.pop()
            mism = (seq is not None and target.seq is not None and seq != target.seq) or (
                name and target.name and name != target.name)
            close(target, t0, {"kind": "end_mismatch", "text": text} if mism else None)
    while len(stack) > 1:
        s = stack.pop()
        close(s, n_samples, None if s.kind == "section" else {"kind": "unterminated", "closed_by": "capture end"})
    return root


def find(res, cap, dialects: dict | None = None, hint: dict | None = None):
    """The marker stream: the UART claim + dialect whose lines match best.
    hint: {"pin": "D6", "dialect": "wiretest"} narrows the search."""
    dialects = dialects or load_dialects()
    best = None
    for c in res.claims:
        if c.protocol != "uart" or c.node.output is None:
            continue
        if hint and hint.get("pin") and c.roles.get("data") != hint["pin"]:
            continue
        lines = lines_with_times(c.node, cap.rate)
        if not lines:
            continue
        for name, d in dialects.items():
            if hint and hint.get("dialect") and name != hint["dialect"]:
                continue
            hits = sum(any(mk["_re"].match(t) for mk in d["marker"]) for t, _, _ in lines)
            if hits >= 2 and (best is None or hits > best[0]):
                best = (hits, c, name, lines)
    if best is None:
        return None
    hits, c, name, lines = best
    return {"pin": c.roles["data"], "dialect": name, "lines": len(lines), "markers": hits,
            "tree": build(lines, dialects[name], cap.n_samples)}


def activity(cap, seg: Segment, exclude: set[str]) -> dict:
    """Per segment: which pins moved, levels at both ends, quiet time at head and tail."""
    b, e = seg.begin, seg.end if seg.end is not None else cap.n_samples
    moved, first, last, levels, periodic = {}, None, None, {}, {}
    for ch in cap.channels:
        if ch.name in exclude:
            continue
        ed = np.asarray(ch.edges)
        i0, i1 = np.searchsorted(ed, b), np.searchsorted(ed, e)
        lv = lambda t: int(ch.initial ^ (np.searchsorted(ed, t, side="right") & 1))  # noqa: E731
        levels[ch.name] = [lv(b), lv(max(b, e - 1))]
        if i1 > i0:
            moved[ch.name] = int(i1 - i0)
            first = ed[i0] if first is None else min(first, ed[i0])
            last = ed[i1 - 1] if last is None else max(last, ed[i1 - 1])
            p = _periodic(ed[i0:i1], ch.initial ^ (i0 & 1), cap.rate, ch.step)
            if p:
                periodic[ch.name] = p
    return {"begin": int(b), "end": int(e), "moved": moved, "levels": levels,
            "quiet_head_s": (float(first - b) / cap.rate) if first is not None else None,
            "quiet_tail_s": (float(e - last) / cap.rate) if last is not None else None,
            "changed": sorted(k for k, (x, y) in levels.items() if x != y), "periodic": periodic}


def _periodic(ed: np.ndarray, level_before: int, rate: float, step: int = 1) -> dict | None:
    """Frequency / duty of a pin that toggles regularly inside the segment
    (PWM, tone, clock outputs): rising-to-rising periods, high time per period.
    None when there are too few periods or they do not agree (not a steady signal)."""
    rises = ed[0::2] if level_before == 0 else ed[1::2]
    falls = ed[1::2] if level_before == 0 else ed[0::2]
    if len(rises) < 4:
        return None
    per = np.diff(rises).astype(np.float64)
    med = float(np.median(per))
    if med <= 0 or float(np.mean(np.abs(per - med) <= 0.05 * med)) < 0.9:
        return None
    j = np.searchsorted(falls, rises[:-1])
    ok = j < len(falls)
    high = (falls[j[ok]] - rises[:-1][ok]).astype(np.float64)
    high = high[high < per[ok]]
    return {"freq_hz": rate * (len(per)) / float(rises[-1] - rises[0]), "duty": float(np.median(high / med)) if len(high) else None,
            "periods": int(len(per)), "period_spread": float(np.std(per) / med),
            "resolution": step / med}      # one sample of the channel, as a share of the period


def select(tree: Segment, pattern: str) -> list[Segment]:
    """Segments whose path matches a glob ("session/pwm/*", "*/1kHz-25[1]").
    "[" is literal: it is the sibling index, not a character class."""
    import fnmatch
    pat = pattern.replace("[", "[[]")
    return [s for s in tree.walk() if s.kind != "root" and fnmatch.fnmatchcase(s.path(), pat)]


def events(res, cap, found: dict, dialects: dict | None = None) -> list[dict]:
    """Every text line of every UART claim, on one time line: marker lines of the
    marker channel are kind "marker", everything else (commands, PING/PONG,
    parameters, responses on other lines) is kind "event"."""
    dialects = dialects or load_dialects()
    d = dialects[found["dialect"]]
    out = []
    for c in res.claims:
        if c.protocol != "uart" or c.node.output is None:
            continue
        pin = c.roles["data"]
        for text, t0, t1 in lines_with_times(c.node, cap.rate):
            is_marker = pin == found["pin"] and any(mk["_re"].match(text) for mk in d["marker"])
            out.append({"s": t0, "e": t1, "pin": pin, "kind": "marker" if is_marker else "event", "text": text})
    out.sort(key=lambda x: x["s"])
    return out


def describe(res, cap, found: dict, results: bool = False, depth: str = "protocol", with_events: bool = False) -> dict:
    """The segment tree as plain data: times, raw marker, issues, events, pin
    activity (marker pin excluded) and, per claim, what was decoded inside.
    A segment lists the events directly in it; a child's marker line lies
    between the children, so it shows up in the parent as an event of kind
    "marker" (the parent sees its children's markers as events)."""
    from . import export
    marker_pin = found["pin"]
    evs = events(res, cap, found)

    def ev_out(x):
        return {"s": x["s"] / cap.rate, "pin": x["pin"], "kind": x["kind"], "text": x["text"]}

    def node(s: Segment) -> dict:
        b, e = s.begin, s.end if s.end is not None else cap.n_samples
        d = {"path": s.path(), "name": s.name, "level": s.level, "raw": s.raw,
             "begin_s": b / cap.rate, "end_s": e / cap.rate}
        if s.attrs:
            d["attrs"] = s.attrs
        if s.issues:
            d["issues"] = s.issues
        kids = [(ch.begin, ch.end if ch.end is not None else cap.n_samples) for ch in s.children]
        direct = [ev_out(x) for x in evs if b <= x["s"] < e and not any(k0 <= x["s"] < k1 for k0, k1 in kids)]
        if direct and with_events:
            d["events"] = direct
        d["activity"] = activity(cap, s, {marker_pin})
        doc = export.export(res, cap, "final", window=(b, e), depth=depth)
        inside = []
        for c in doc["claims"]:
            if c["protocol"] == "uart":
                continue          # text lines are the events above
            r = c.get("result", {})
            n = sum(len(v) if isinstance(v, list) else len(v.get("s", [])) if isinstance(v, dict) else 0 for v in r.values())
            if n:
                inside.append({"protocol": c["protocol"], "roles": c["roles"], "items": n,
                               **({"result": r} if results else {})})
        d["decoded"] = inside
        d["children"] = [node(ch) for ch in s.children]
        return d

    root = found["tree"]
    top = [(ch.begin, ch.end if ch.end is not None else cap.n_samples) for ch in root.children]
    doc = {"marker": {k: v for k, v in found.items() if k != "tree"}, "root_issues": root.issues,
           "segments": [node(ch) for ch in root.children]}
    if with_events:
        doc["events_outside"] = [ev_out(x) for x in evs if not any(k0 <= x["s"] < k1 for k0, k1 in top)]
    return doc


def events_in(res, cap, found: dict, window: tuple[int, int]) -> list[dict]:
    """All events and marker lines inside a window (a selected segment sees its
    children's markers as events)."""
    return [{"s": x["s"] / cap.rate, "pin": x["pin"], "kind": x["kind"], "text": x["text"]}
            for x in events(res, cap, found) if window[0] <= x["s"] < window[1]]
