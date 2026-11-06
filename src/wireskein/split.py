"""Cutting a long capture into pieces the analysis reads one at a time (memory bounded by the piece, not the capture).

A capture of more changes than `max_edges` is cut; one of fewer is read whole, as always. From the start, the
changes up to `max_edges` are a window; inside it the cut goes where every line is quiet:

- each line's usual interval is the median of its intervals in the window; a line that never stops in the window
  (a free-running clock, a PWM, I2S's WS) is periodic and left out, since it never tells a frame's end;
- every other line is busy between two of its changes closer than `gap_min` times its usual interval; where no
  line is busy, the window is quiet;
- a quiet stretch after `search_from` of the window is a candidate, its length against the fastest line's usual
  interval its ratio. The longest is cut at its middle; others at least `similar` times as long too, each piece
  keeping `min_edges` changes or more.
- with a periodic line in the window, a quiet stretch of the other lines may well be inside a frame (data the
  same for a while): the cut is "data_quiet";
- with no candidate at all (every line periodic, or a stream that never pauses), the window is cut at its end
  ("forced"), read with the same overlap.

Every cut is read past by `overlap_edges` changes on both sides: even a quiet stretch may be inside a frame (a
protocol's turnaround, frames back to back with gaps no longer than it). The remainder after the last cut starts the
next window. A piece owns the ticks between its cuts: an item read twice in an overlap is kept by the piece whose own
ticks it starts in.

The values come from experiments and are all given from outside: `options("max_edges=4000000,gap_min=8")`,
`{"split": {...}}` in a decode spec, `--split` on the command line; `off` reads the capture whole however long.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

DEFAULTS = {"max_edges": 1_000_000, "search_from": 0.5, "gap_min": 4.0, "similar": 0.5, "min_edges": 50_000,
            "overlap_edges": 10_000, "noise_max": 1, "noise_share": 0.3, "workers": 1, "roles_from": 3,
            "reanalyze": 1}
LISTS = ("cuts", "watch", "ignore")


def options(spec=None) -> dict | None:
    """The split options from a dict, a "key=value,..." string, or None (the defaults); "off" / False: no split."""
    if spec is None or spec is True:
        return dict(DEFAULTS)
    if spec is False or spec == "off":
        return None
    if isinstance(spec, str):
        got = {}
        for kv in filter(None, (x.strip() for x in spec.split(","))):
            k, _, v = kv.partition("=")
            got[k.strip()] = v.strip()
        spec = got
    out = dict(DEFAULTS)
    for k, v in dict(spec).items():
        if k not in DEFAULTS and k not in LISTS:
            raise ValueError(f"split: unknown option {k!r} ({', '.join([*DEFAULTS, *LISTS])})")
        if k in LISTS:
            vals = v.split(";") if isinstance(v, str) else list(v)
            out[k] = [int(x) for x in vals] if k == "cuts" else [str(x) for x in vals]
        else:
            out[k] = type(DEFAULTS[k])(float(v)) if isinstance(DEFAULTS[k], int) else float(v)
    return out


@dataclass
class Piece:
    s: int              # its own ticks: [s, e)
    e: int
    a: int              # the ticks read: [a, b) (past its own by the overlap where a cut needs it)
    b: int
    edges: int
    cut: str            # how it ends: "quiet", "data_quiet", "forced", "given", "end"
    ratio: float | None = None

    def doc(self) -> dict:
        return {"s": self.s, "e": self.e, "edges": self.edges, "cut": self.cut,
                **({"ratio": round(self.ratio, 1)} if self.ratio is not None else {}),
                **({"read": [self.a, self.b]} if (self.a, self.b) != (self.s, self.e) else {})}


def _edges(cap) -> list[np.ndarray]:
    return [np.asarray(c.edges) for c in cap.channels]


def _count(es, s: int, t: int) -> int:
    return sum(int(np.searchsorted(e, t) - np.searchsorted(e, s)) for e in es)


def _tick_after(es, s: int, k: int, n: int) -> int:
    """The first tick t with k changes in [s, t] (n when there are not so many)."""
    if _count(es, s, n) < k:
        return n
    lo, hi = s, n
    while lo < hi:
        mid = (lo + hi) // 2
        if _count(es, s, mid + 1) >= k:
            hi = mid
        else:
            lo = mid + 1
    return lo + 1


def _tick_before(es, e: int, k: int) -> int:
    """The last tick t with k changes in [t, e) (0 when there are not so many)."""
    if _count(es, 0, e) < k:
        return 0
    lo, hi = 0, e
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if _count(es, mid, e) >= k:
            lo = mid
        else:
            hi = mid - 1
    return lo


def quiet(es, s: int, t: int, gap_min: float, noise_max: int = 1, noise_share: float = 0.3, names=None,
          noisy: dict | None = None):
    """The quiet stretches of [s, t): (starts, ends, fastest usual interval, a periodic line was left out).
    Pulses of `noise_max` ticks or shorter (a reflection ringing at an edge, a floating input trembling) do not make
    a line busy. A line with more than `noise_share` of its intervals that short is told (its name and the share
    put in `noisy`), not left out: whether it is to be ignored or removed is the user's to see."""
    from ._engine import kernels as K
    spans_s, spans_e, usual_all, periodic = [], [], [], False
    for k, e in enumerate(es):
        w = e[np.searchsorted(e, s):np.searchsorted(e, t)]
        if len(w) >= 3:
            share = float(np.mean(np.diff(w) <= noise_max))
            if share > noise_share and noisy is not None and names is not None:
                noisy[names[k]] = max(noisy.get(names[k], 0.0), round(share, 3))
            if noise_max >= 1:
                w = K.deglitch(w, noise_max + 1)
        if len(w) < 3:
            spans_s.append(w)                  # a line that changes rarely: busy at its changes only
            spans_e.append(w)
            continue
        d = np.diff(w)
        usual = float(np.median(d))
        thr = gap_min * usual
        if d.max() <= thr:
            periodic = True                     # never stops in the window (from its first change to its last)
            continue
        usual_all.append(usual)
        busy = d < thr
        spans_s.append(w[:-1][busy])
        spans_e.append(w[1:][busy])
        lone = np.ones(len(w), bool)            # a change with no close neighbour is busy at itself
        lone[:-1] &= ~busy
        lone[1:] &= ~busy
        spans_s.append(w[lone])
        spans_e.append(w[lone])
    if not spans_s:
        return np.zeros(0, np.int64), np.zeros(0, np.int64), None, periodic
    a = np.concatenate(spans_s).astype(np.int64)
    b = np.concatenate(spans_e).astype(np.int64)
    if not len(a):
        return np.array([s]), np.array([t]), None, periodic
    o = np.argsort(a, kind="stable")
    a, b = a[o], np.maximum.accumulate(b[o])
    gap = np.flatnonzero(a[1:] > b[:-1])
    qs, qe = b[gap], a[gap + 1]
    return qs, qe, (min(usual_all) if usual_all else None), periodic


def plan(cap, opts: dict | None, watch: list[str] | None = None, noisy: dict | None = None) -> list[Piece]:
    """The pieces a capture is read in (one when it has max_edges changes or fewer, or opts is None).
    The changes are counted on every line; the quiet is looked for on `watch` (opts["watch"], else the lines given,
    else all) but opts["ignore"]; lines that tremble are told in `noisy` (name: the share of its short intervals)."""
    es_all = _edges(cap)
    names_all = [c.name for c in cap.channels]
    given = [*((opts or {}).get("watch") or ()), *((opts or {}).get("ignore") or ())]
    unknown = [x for x in given if x not in names_all]
    if unknown:
        raise ValueError(f"split: no such channel: {', '.join(unknown)}")
    want = (opts or {}).get("watch") or [x for x in (watch or ()) if x in names_all] or names_all
    ignore = set((opts or {}).get("ignore") or ())
    names = [x for x in names_all if x in want and x not in ignore]
    wes = [es_all[names_all.index(x)] for x in names]
    es = es_all
    n = int(cap.n_samples)
    total = _count(es, 0, n)
    if opts is None or (total <= opts["max_edges"] and not opts.get("cuts")):
        return [Piece(0, n, 0, n, total, "end")]
    pieces: list[Piece] = []
    if opts.get("cuts"):                       # given from outside
        cuts = sorted(c for c in opts["cuts"] if 0 < c < n)
        bounds = [0, *cuts, n]
        for x, y in zip(bounds[:-1], bounds[1:]):
            pieces.append(Piece(x, y, x, y, _count(es, x, y), "given" if y < n else "end"))
        return pieces
    mx, mn = int(opts["max_edges"]), int(opts["min_edges"])
    s = 0
    while s < n:
        if _count(es, s, n) <= mx:
            pieces.append(Piece(s, n, s, n, _count(es, s, n), "end"))
            break
        t = _tick_after(es, s, mx, n)
        t0 = _tick_after(es, s, max(1, int(mx * opts["search_from"])), n)
        qs, qe, usual, periodic = quiet(wes, s, t, opts["gap_min"], int(opts["noise_max"]), opts["noise_share"],
                                        names, noisy)
        keep = (qe > t0) & (qs >= s)
        qs, qe = np.maximum(qs[keep], t0), qe[keep]
        kind = "data_quiet" if periodic else "quiet"
        if len(qs) and usual:
            ratio = (qe - qs) / usual
            best = float(ratio.max())
            pick = np.flatnonzero(ratio >= opts["similar"] * best)
            cuts, prev = [], s
            for i in pick.tolist():             # in time order, each piece min_edges or more
                c = int((qs[i] + qe[i]) // 2)
                if _count(es, prev, c) >= mn:
                    cuts.append((c, float(ratio[i])))
                    prev = c
            if not cuts:                        # at least one: the longest
                i = int(np.argmax(ratio))
                cuts = [(int((qs[i] + qe[i]) // 2), float(ratio[i]))]
            prev = s
            for c, r in cuts:
                pieces.append(Piece(prev, c, prev, c, _count(es, prev, c), kind, r))
                prev = c
            s = prev
        else:
            pieces.append(Piece(s, t, s, t, _count(es, s, t), "forced"))
            s = t
    # read past a cut that may be inside a frame
    ov = int(opts["overlap_edges"])
    for i, p in enumerate(pieces):
        if p.cut != "end":
            p.b = _tick_after(es, p.e, ov, n)
            if i + 1 < len(pieces):
                pieces[i + 1].a = _tick_before(es, p.e, ov)
    return pieces


def window(cap, a: int, b: int):
    """Ticks [a, b) of an engine capture as one of its own (tick 0 at a)."""
    from .verify import _window
    return _window(cap, a, b)


# ---- joining the pieces' results ----

def _shift_item(x: dict, off: int) -> dict:
    out = dict(x)
    for k in ("s", "e"):
        if isinstance(out.get(k), int):
            out[k] += off
    return out


def join_items(per_piece: list[tuple[Piece, list]], key: str = "s") -> list:
    """Lists of items (dicts with s) from each piece: shifted to the capture's ticks, each kept by the piece whose own
    ticks it starts in."""
    out = []
    for p, items in per_piece:
        for x in items:
            if not isinstance(x, dict) or not isinstance(x.get(key), int):
                out.append(x)
                continue
            y = _shift_item(x, p.a)
            if p.s <= y[key] < p.e:
                out.append(y)
    return out


def join_columns(per_piece: list[tuple[Piece, dict]]) -> dict:
    """A layer of parallel lists keyed by time (bits: "t"; pulses: "s"), joined the same way."""
    first = per_piece[0][1]
    tkey = "t" if "t" in first else "s"
    out = {k: v for k, v in first.items() if not isinstance(v, list)}
    cols = [k for k, v in first.items() if isinstance(v, list)]
    acc = {k: [] for k in cols}
    for p, lay in per_piece:
        t = np.asarray(lay.get(tkey, []), np.int64) + p.a
        keep = (t >= p.s) & (t < p.e)
        idx = np.flatnonzero(keep)
        for k in cols:
            v = lay.get(k, [])
            if k == tkey:
                acc[k].extend(t[idx].tolist())
            elif len(v) == len(t):
                acc[k].extend([v[i] for i in idx.tolist()])
            elif v and isinstance(v[0], list) and all(len(r) == len(t) for r in v):   # a row per data pin
                if not acc[k]:
                    acc[k] = [[] for _ in v]
                for r, row in zip(acc[k], v):
                    r.extend([row[i] for i in idx.tolist()])
            elif not acc[k]:
                acc[k] = v
    out.update(acc)
    return out


# ---- reading as the pieces go: a source of changes a block at a time ----

class Source:
    """The logic lines of one part, read a block at a time: names, ticks, rate, meta, the level of each before tick 0,
    and blocks() -> (the tick read up to, {name: its changes in the block}). Only the piece being read is held."""
    names: list[str]
    n: int
    rate: float
    meta: dict
    initial: dict

    def blocks(self):
        raise NotImplementedError


class CaptureSource(Source):
    """An engine capture already in memory, in blocks of ticks."""

    def __init__(self, cap, block: int = 1 << 24):
        self.cap, self.block = cap, block
        self.names = [c.name for c in cap.channels]
        self.n, self.rate, self.meta = int(cap.n_samples), cap.rate, cap.meta
        self.initial = {c.name: int(c.initial) for c in cap.channels}

    def blocks(self):
        es = {c.name: np.asarray(c.edges) for c in self.cap.channels}
        for t0 in range(0, self.n, self.block):
            t1 = min(self.n, t0 + self.block)
            yield t1, {k: e[np.searchsorted(e, t0):np.searchsorted(e, t1)] for k, e in es.items()}


class FileSource(Source):
    """One part of a WireSkein file: a line kept as bits decompressed and turned into changes a block at a time, a
    line kept as its changes (step 1) read as they are. None of it held but the block (and the changes lines)."""

    def __init__(self, path, part: int = 0, pins: list[str] | None = None, block: int = 1 << 24):
        from . import _fileformat as ff
        from ._engine import fileio
        raw = ff.read_header(path)
        head = ff.part_view(raw, part)
        self.path, self.block = path, block
        self.chans = [c for c in head["channels"] if c.get("encoding") in ("bits", "events")
                      and (not pins or c["name"] in pins)]
        if any(c["step"] != 1 or c.get("phase", 0) != 0 for c in self.chans):
            raise ValueError("a line of its own step: read whole")
        self.names = [c["name"] for c in self.chans]
        self.n = int(head["ticks"])
        self.rate = float(ff.tick_hz(head))
        self.meta = fileio.view(head, [], path).meta
        self.initial, self.events = {}, {}
        with zipfile.ZipFile(path) as z:
            for c in self.chans:
                if c["encoding"] == "events":       # changes already: small
                    ch = fileio.decode(ff.EventsChannel(c["name"], z.read(c["file"]), c["n"], 1, 0, c["start_level"],
                                                        c["events"], c.get("acquisition", {})))
                    self.initial[c["name"]], self.events[c["name"]] = int(ch.initial), np.asarray(ch.edges)
                else:
                    with z.open(c["file"]) as f:
                        b = f.read(1)
                    self.initial[c["name"]] = int(b[0] & 1) if b else 0

    def blocks(self):
        with zipfile.ZipFile(self.path) as z:
            files = {c["name"]: z.open(c["file"]) for c in self.chans if c["encoding"] == "bits"}
            last = dict(self.initial)
            try:
                for t0 in range(0, self.n, self.block):
                    t1 = min(self.n, t0 + self.block)
                    out = {}
                    for name in self.names:
                        if name in self.events:
                            e = self.events[name]
                            out[name] = e[np.searchsorted(e, t0):np.searchsorted(e, t1)]
                            continue
                        raw = files[name].read((t1 - t0) // 8 if t1 < self.n else -1)
                        lv = np.unpackbits(np.frombuffer(raw, np.uint8), bitorder="little")[: t1 - t0].astype(np.int8)
                        ch = np.flatnonzero(np.diff(lv)) + 1
                        if len(lv) and lv[0] != last[name]:
                            ch = np.concatenate(([0], ch))
                        if len(lv):
                            last[name] = int(lv[-1])
                        out[name] = ch.astype(np.int64) + t0
                    yield t1, out
            finally:
                for f in files.values():
                    f.close()


import zipfile  # noqa: E402  (FileSource)


def run(source: Source, opts: dict | None, watch: list[str] | None = None, noisy: dict | None = None):
    """The pieces of a source, each with the engine capture of the ticks it reads, as they are cut: (Piece, Capture).
    Changes are held from the current piece on only. With opts None, or a source of max_edges changes or fewer,
    one piece: the whole."""
    from ._engine.model import Capture, Channel
    names = source.names
    mx = int(opts["max_edges"]) if opts else None
    mn = int(opts["min_edges"]) if opts else 0
    ov = int(opts["overlap_edges"]) if opts else 0
    given = [*((opts or {}).get("watch") or ()), *((opts or {}).get("ignore") or ())]
    unknown = [x for x in given if x not in names]
    if unknown:
        raise ValueError(f"split: no such channel: {', '.join(unknown)}")
    want = (opts or {}).get("watch") or [x for x in (watch or ()) if x in names] or names
    ignore = set((opts or {}).get("ignore") or ())
    wnames = [x for x in names if x in want and x not in ignore]
    buf = {k: np.zeros(0, np.int64) for k in names}
    lv0 = dict(source.initial)                       # the level before the buffer's first tick
    start = 0                                        # the buffer's first tick
    upto, eof = 0, False
    it = source.blocks()

    def count(s, t):
        return _count(list(buf.values()), s, t)

    def more():
        nonlocal upto, eof
        try:
            upto, got = next(it)
        except StopIteration:
            eof = True
            upto = source.n
            return
        for k in names:
            buf[k] = np.concatenate((buf[k], got[k]))

    def level_at(k, a):                              # as Channel.level_at: a change at a is in force at a
        return int(lv0[k] ^ (int(np.searchsorted(buf[k], a, side="right")) & 1))

    def cap_of(a, b):
        chans = []
        for k in names:
            e = buf[k]
            w = e[np.searchsorted(e, a, side="right"):np.searchsorted(e, b)] - a
            chans.append(Channel(k, level_at(k, a), w))
        return Capture(source.rate, b - a, chans, meta=dict(source.meta))

    def drop_before(t):                              # keep from tick t on
        nonlocal start
        for k in names:
            e = buf[k]
            i = int(np.searchsorted(e, t, side="right"))
            lv0[k] ^= (i & 1)
            buf[k] = e[i:]
        start = t

    s, prev_cut = 0, None
    while True:
        while not eof and (mx is None or count(s, upto) < mx + ov):
            more()
        if mx is None or (eof and count(s, source.n) <= mx):
            p = Piece(s, source.n, s, source.n, count(s, source.n), "end")
            if prev_cut is not None:
                p.a = _tick_before_buf(buf, start, s, ov)
            yield p, cap_of(p.a, p.b)
            return
        es = [buf[k] for k in names]
        wes = [buf[k] for k in wnames]
        t = _tick_after(es, s, mx, upto)
        t0 = _tick_after(es, s, max(1, int(mx * opts["search_from"])), upto)
        qs, qe, usual, periodic = quiet(wes, s, t, opts["gap_min"], int(opts["noise_max"]), opts["noise_share"],
                                        wnames, noisy)
        keep = (qe > t0) & (qs >= s)
        qs, qe = np.maximum(qs[keep], t0), qe[keep]
        kind = "data_quiet" if periodic else "quiet"
        cuts = []
        if len(qs) and usual:
            ratio = (qe - qs) / usual
            best = float(ratio.max())
            last = s
            for i in np.flatnonzero(ratio >= opts["similar"] * best).tolist():
                c = int((qs[i] + qe[i]) // 2)
                if count(last, c) >= mn:
                    cuts.append((c, float(ratio[i]), kind))
                    last = c
            if not cuts:
                i = int(np.argmax(ratio))
                cuts = [(int((qs[i] + qe[i]) // 2), float(ratio[i]), kind)]
        else:
            cuts = [(t, None, "forced")]
        for c, r, k in cuts:
            p = Piece(s, c, s, c, count(s, c), k, r)
            if prev_cut is not None:                 # every cut read past both ways: a quiet may be in a frame
                p.a = _tick_before_buf(buf, start, s, ov)
            p.b = min(_tick_after(es, c, ov, upto), upto)
            yield p, cap_of(p.a, p.b)
            s, prev_cut = c, k
        drop_before(_tick_before_buf(buf, start, s, ov))


def _tick_before_buf(buf: dict, start: int, e: int, k: int) -> int:
    """The last tick t >= start with k changes in [t, e) of the buffer (start when there are not so many)."""
    es = list(buf.values())
    if _count(es, start, e) < k:
        return start
    lo, hi = start, e
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if _count(es, mid, e) >= k:
            lo = mid
        else:
            hi = mid - 1
    return lo


def mapped(fn, items, workers: int = 1):
    """fn over items (an iterator: the pieces as they are cut), in order, in `workers` processes: at most twice as
    many in flight, so memory stays a few pieces' (not the capture's)."""
    if workers <= 1:
        for x in items:
            yield x, fn(x)
        return
    import collections
    from concurrent.futures import ProcessPoolExecutor
    with ProcessPoolExecutor(workers) as ex:
        pending = collections.deque()
        for x in items:
            pending.append((x, ex.submit(fn, x)))
            if len(pending) >= 2 * workers:
                y, f = pending.popleft()
                yield y, f.result()
        while pending:
            y, f = pending.popleft()
            yield y, f.result()


def size_bin(edges: int) -> str:
    """A bin of sizes in powers of two: "1", "2", "3-4", "5-8", ..."""
    if edges <= 2:
        return str(max(1, edges))
    k = (edges - 1).bit_length()
    return f"{(1 << (k - 1)) + 1}-{1 << k}"


class Unread:
    """What was not read, summed up: how many stretches, how many changes, their sizes in bins, the largest (with
    where it is) - a long capture has many small ones (a line moving between frames) and a few that matter."""

    def __init__(self):
        self.count, self.edges, self.bins, self.largest = 0, 0, {}, None

    def add(self, items) -> None:
        for u in items:
            k = int(u.get("edges", 0))
            self.count += 1
            self.edges += k
            b = size_bin(k)
            self.bins[b] = self.bins.get(b, 0) + 1
            if self.largest is None or k > self.largest["edges"]:
                self.largest = {"s": u.get("s"), "e": u.get("e"), "edges": k}

    def doc(self) -> dict:
        order = sorted(self.bins, key=lambda b: int(b.split("-")[0]))
        return {"count": self.count, "edges": self.edges, "sizes": {b: self.bins[b] for b in order},
                **({"largest": self.largest} if self.largest else {})}


class SrSource(Source):
    """A logic-only sigrok session (.sr) read a block at a time (decompressed as it goes): any length, only the
    piece being read held."""

    def __init__(self, path, pins: list[str] | None = None):
        import configparser
        from ._engine import srio
        with zipfile.ZipFile(path) as z:
            cfg = configparser.ConfigParser()
            cfg.read_string(z.read("metadata").decode())
            dev = cfg["device 1"]
            if any(k.startswith("analog") and k[6:].isdigit() for k in dev):
                raise ValueError("analog channels: read whole")
            if srio.EXTRA in z.namelist():
                extra = json.loads(z.read(srio.EXTRA))
                if extra.get("analog") or any(int(c.get("step", 1)) != 1 for c in extra.get("channels", {}).values()):
                    raise ValueError("lines of their own step: read whole")
            self.unitsize = int(dev.get("unitsize", "1"))
            prefix = dev.get("capturefile", "logic-1")
            names = {int(k[5:]): v for k, v in dev.items() if k.startswith("probe") and k[5:].isdigit()}
            total = int(dev.get("total probes", self.unitsize * 8))
            self.chunks = sorted((n for n in z.namelist() if n == prefix or n.startswith(prefix + "-")),
                                 key=lambda n: int(n.rsplit("-", 1)[1]) if n != prefix else 0)
            bits = [b for b in (sorted(b - 1 for b in names) if names else range(min(total, self.unitsize * 8)))
                    if b < self.unitsize * 8]
            self.bits = {names.get(b + 1, f"bit{b}"): b for b in bits}
            if pins:
                miss = [p for p in pins if p not in self.bits]
                if miss:
                    raise ValueError(f"pins not in the capture: {', '.join(miss)}")
                self.bits = {p: self.bits[p] for p in pins}
            self.n = sum(z.getinfo(c).file_size for c in self.chunks) // self.unitsize
            self.rate = srio._parse_rate(dev["samplerate"])
            dtype = self._dtype()
            first = next(srio._blocks(z, self.chunks, self.unitsize, dtype, 0, 1), (0, np.zeros(1, dtype)))[1]
        self.path = path
        self.names = list(self.bits)
        self.initial = {k: int((int(first[0]) >> b) & 1) if len(first) else 0 for k, b in self.bits.items()}
        self.meta = {"file": str(path)}

    def _dtype(self):
        return {1: np.uint8, 2: np.uint16, 4: np.uint32}[self.unitsize]

    def blocks(self):
        from ._engine import srio
        last = dict(self.initial)
        with zipfile.ZipFile(self.path) as z:
            for first, data in srio._blocks(z, self.chunks, self.unitsize, self._dtype(), 0, None):
                out = {}
                for k, b in self.bits.items():
                    lv = ((data >> b) & 1).astype(np.int8)
                    ch = np.flatnonzero(np.diff(lv)) + 1
                    if len(lv) and lv[0] != last[k]:
                        ch = np.concatenate(([0], ch))
                    if len(lv):
                        last[k] = int(lv[-1])
                    out[k] = ch.astype(np.int64) + first
                yield first + len(data), out


import json  # noqa: E402  (SrSource)
