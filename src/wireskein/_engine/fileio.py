"""wireskein.fileformat files <-> the edge-list model (numpy side)."""

from __future__ import annotations

from pathlib import Path

import numpy as np

from .. import _fileformat as fileformat
from .model import AnalogTrace, Capture, Channel, EventTrace, edges_from_dense


def decode(c) -> Channel | EventTrace | AnalogTrace:
    """One stored channel as the edge-list model: bits and lossless events (step 1) as a Channel, events of a
    coarser step as an EventTrace, analog as an AnalogTrace."""
    if isinstance(c, fileformat.EventsChannel):
        ks, levels, counts = c.decode()
        ks = np.array(ks, np.int64)
        if c.step == 1:                          # lossless: each event is one change, at its tick
            init = c.start_level
            if len(ks) and ks[0] == 0:           # a change at the first tick: the Channel starts at the level after it
                init, ks = init ^ 1, ks[1:]
            return Channel(c.name, init, c.phase + ks, 1, c.phase, dict(c.acquisition))
        return EventTrace(c.name, c.step, c.phase, c.n, c.start_level, ks, np.array(levels, np.uint8),
                          np.array(counts, np.uint8), dict(c.acquisition))
    if isinstance(c, fileformat.AnalogChannel):
        dtype = np.float32 if c.encoding == "analog-f32" else {8: np.uint8, 16: "<u2", 32: "<u4"}[c.width]
        return AnalogTrace(c.name, np.frombuffer(c.data, dtype), c.rate_hz, c.t0_ticks, c.encoding, c.width,
                           c.value_bits, c.zero, c.scale_nv, c.unit, dict(c.acquisition))
    bits = np.unpackbits(np.frombuffer(c.bits, np.uint8), bitorder="little")[: c.n]
    init, edges = edges_from_dense(bits, c.step, c.phase)
    return Channel(c.name, init, edges, c.step, c.phase, dict(c.acquisition))


def view(head: dict, decoded: list, path: str | Path | None = None, extras: dict | None = None) -> Capture:
    """The decoders' Capture of one part: its decoded channels (an EventTrace also as a Channel of its step when no
    quantum hides a change), with what the capture says about itself in meta."""
    out, analog, events = [], [], []
    for d in decoded:
        if isinstance(d, EventTrace):
            events.append(d)
            ch = d.as_channel()                  # at most one change a quantum: the decoders read it at that step
            if ch is not None:
                out.append(ch)
        elif isinstance(d, AnalogTrace):
            analog.append(d)
        else:
            out.append(d)
    tick = fileformat.tick_hz(head)
    return Capture(float(tick), int(head["ticks"]), out,
                   meta={**head.get("meta", {}), **({"file": str(path)} if path is not None else {}), "tick_hz": tick,
                         "extras": extras if extras is not None else (fileformat.extras(path) if path is not None
                                                                      else {}),
                         "capture_id": head.get("id"), "complete": head.get("complete", True),
                         "lost": head.get("lost", []), "layout": head.get("layout", "timeline"),
                         "part": head.get("part", 0), "n_parts": head.get("n_parts", 1),
                         "skipped_channels": fileformat.skipped(head)}, analog=analog, events=events)


def load(path: str | Path, part: int | None = None) -> Capture:
    """One part of a WireSkein file (a capture of several parts needs `part`, wireskein-format §3.4). meta carries
    whether the capture is complete and what was lost (a loss makes the whole capture a failure), the layout, and
    which part of how many."""
    head, chans = fileformat.read(path, part=part)
    return view(head, [decode(c) for c in chans], path)


def load_parts(path: str | Path) -> list[Capture]:
    """Every part of a WireSkein file, in order (wireskein-format §3.4: analysis never crosses them)."""
    ex = fileformat.extras(path)                     # read once for every part
    return [view(head, [decode(c) for c in chans], path, ex) for head, chans in fileformat.read_parts(path)]


def load_runs(path: str | Path) -> list[Capture]:
    """The parts of a file, each run of timeline parts that follow on from one another without a gap joined into one
    (a stream cut where its sample grid restarts, not where anything was lost: wireskein-format §3.4). A run is joined
    when its parts hold the same logic lines at the same steps, none of them hiding changes in a quantum (events kept
    at a step whose quanta each hold at most one change are their edges); otherwise its parts stay apart. A joined
    run's meta is its first part's, with `parts` [first, last] and `part_starts`."""
    head = fileformat.read_header(path)
    caps = load_parts(path)
    if head.get("layout", "timeline") != "timeline" or len(caps) < 2:
        return caps
    t0 = [p.get("tick0", 0) for p in head["parts"]]
    out, run = [], [0]
    for k in range(1, len(caps)):
        # follows on: no gap, or the previous part's last quantum running past this part's start by less than a
        # quantum (a sample grid restarted there: format 1's events end on whole quanta)
        over = t0[k - 1] + caps[k - 1].n_samples - t0[k]
        if 0 <= over < max([c.step for c in caps[k - 1].channels] + [1]) and (over == 0 or caps[k - 1].events):
            run.append(k)
        else:
            out.extend(_join(caps, run, t0))
            run = [k]
    out.extend(_join(caps, run, t0))
    return out


def _join(caps: list[Capture], run: list[int], t0: list[int]) -> list[Capture]:
    parts = [caps[k] for k in run]
    names = [c.name for c in parts[0].channels]
    steps = [c.step for c in parts[0].channels]
    # joined when every part holds the same logic lines at the same steps, and a line kept as changes hides none
    # (it is then a line of its step's edges as well; one that hides some stays part by part)
    if len(run) == 1 or any(p.analog or any(t.hidden for t in p.events) or [c.name for c in p.channels] != names
                            or [c.step for c in p.channels] != steps for p in parts):
        return parts
    chans = []
    for j, name in enumerate(names):
        first = parts[0].channels[j]
        edges = first.edges
        for k, p in zip(run[1:], parts[1:]):
            c, off = p.channels[j], t0[k] - t0[run[0]]
            edges = edges[edges < off]                    # what the previous part's last quantum ran past this one
            level = first.initial ^ (len(edges) & 1)
            if c.initial != level:
                edges = np.append(edges, off)             # the line changed at the parts' boundary
            edges = np.concatenate([edges, c.edges + off])
        chans.append(Channel(name, first.initial, edges.astype(np.int64), first.step, first.phase,
                             dict(first.acquisition)))
    n = t0[run[-1]] - t0[run[0]] + parts[-1].n_samples
    starts = [[k, t0[k] - t0[run[0]]] for k in run]          # where each part begins in the joined ticks
    return [Capture(parts[0].rate, n, chans, {**parts[0].meta, "parts": [run[0], run[-1]], "part_starts": starts})]


def locate(meta: dict, tick: int) -> tuple[int, int]:
    """A tick of a capture that load_runs may have joined, as (the file's part, the tick in that part)."""
    starts = meta.get("part_starts")
    if not starts:
        return int(meta.get("part", 0)), int(tick)
    k, off = starts[0]
    for kk, o in starts:
        if tick >= o:
            k, off = kk, o
    return int(k), int(tick - off)


def to_channel(ch: Channel, n_ticks: int) -> fileformat.Channel:
    """The samples a channel really has (at phase + k * step) back into a fileformat.Channel."""
    n = max(0, (n_ticks - ch.phase + ch.step - 1) // ch.step)
    ticks = ch.phase + np.arange(n, dtype=np.int64) * ch.step
    levels = ch.level_at(ticks).astype(np.uint8)
    return fileformat.Channel(ch.name, np.packbits(levels, bitorder="little").tobytes(), n, ch.step, ch.phase,
                       dict(ch.acquisition))


def to_analog(a: AnalogTrace) -> fileformat.AnalogChannel:
    if a.encoding == "analog-f32":
        data = np.asarray(a.values, "<f4").tobytes()
    else:
        data = np.asarray(a.values, {8: np.uint8, 16: "<u2", 32: "<u4"}[a.width]).tobytes()
    return fileformat.AnalogChannel(a.name, data, len(a.values), a.rate_hz, a.t0_ticks, a.encoding, a.width, a.value_bits,
                             a.zero, a.scale_nv, a.unit, dict(a.acquisition))


def to_events(t: EventTrace) -> fileformat.EventsChannel:
    return fileformat.events(t.name, t.quanta.tolist(), t.levels.tolist(), t.counts.tolist(), t.n, t.step, t.phase,
                             t.start_level, **t.acquisition)


def to_logic(ch: Channel, n_ticks: int) -> fileformat.Channel | fileformat.EventsChannel:
    """A Channel for the file: a step-1 line as its changes when that is smaller than its bits (lossless either
    way); a slower one as its samples."""
    if ch.step == 1:
        n = max(0, n_ticks - ch.phase)
        ks = ch.edges[(ch.edges > ch.phase) & (ch.edges < ch.phase + n)] - ch.phase
        if len(ks) * 3 >= n // 8:              # an event is 2-3 bytes, a sample 1 bit: bits are not larger
            return to_channel(ch, n_ticks)
        start = int(ch.level_at(np.array([ch.phase]))[0]) if n else ch.initial
        lv = start ^ (np.arange(1, len(ks) + 1) & 1)
        return fileformat.events(ch.name, ks.tolist(), lv.tolist(), [1] * len(ks), n, 1, ch.phase, start,
                                 **ch.acquisition)
    return to_channel(ch, n_ticks)


def refuse_if_hidden(cap: Capture, dest) -> None:
    """A .sr or VCD has no form for a line whose quanta hide changes (wireskein-format §4.4); one with at most a
    change a quantum is written as samples at its step."""
    hid = [t for t in cap.events if t.hidden]
    if hid:
        names = ", ".join(f"{t.name} ({t.hidden} quanta of {t.step} ticks hold more than one change)" for t in hid)
        raise ValueError(f"not writing {dest}: it has no form for lines kept as changes whose quanta hide changes: "
                         f"{names} (keep it as a WireSkein file)")


def refuse_if_skipped(cap: Capture, dest) -> None:
    """Writing a capture read with skipped channels would drop them (wireskein-format §3.2)."""
    sk = cap.meta.get("skipped_channels")
    if sk:
        names = ", ".join(f"{c['name']} ({c['encoding']!r})" for c in sk)
        raise ValueError(f"not writing {dest}: it would drop the channels this version does not read: {names}")


def save(path: str | Path, cap: Capture, **meta) -> Path:
    """meta: the capture file's metadata; attachments and notes carried in
    cap.meta["extras"] (as read from a .wireskein or .sr) are copied as they are."""
    import zipfile
    refuse_if_skipped(cap, path)
    if cap.meta.get("n_parts", 1) > 1:
        raise ValueError(f"not writing {path}: the capture has {cap.meta['n_parts']} parts and this is one of them "
                         f"(the others would be dropped)")
    tick = cap.meta.get("tick_hz", cap.rate)
    meta.pop("capture_id", None)
    for k in ("complete", "lost", "layout", "part", "n_parts"):
        meta.pop(k, None)
    kept = {t.name for t in cap.events}          # their Channel view is not written; the events are
    path = fileformat.write(path, tick, [to_logic(c, cap.n_samples) for c in cap.channels if c.name not in kept]
                     + [to_events(t) for t in cap.events] + [to_analog(a) for a in cap.analog],
                     capture_id=cap.meta.get("capture_id"), lost=cap.meta.get("lost") or None,
                     **meta)
    if cap.meta.get("extras"):
        with zipfile.ZipFile(path, "a", zipfile.ZIP_DEFLATED) as z:
            for name, data in cap.meta["extras"].items():
                z.writestr(name, data)
    return path
