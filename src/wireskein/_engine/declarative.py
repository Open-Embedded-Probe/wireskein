"""Declarative framing plugins (docs/design.ja.md §3.2).

A TOML file describes how the bits of a frame become fields, which frame
lengths are valid and which fields are checked; the interpreter turns that into
a plugin with the same interface as the hand-written ones in staged.py.

Two shapes are supported:

  word-based    [word] bits/order/fields, [frame] length rule, first-word fields, rest
                (I2C: 9-bit words with ACK; SPI: 8-bit words per data line)
  layout-based  [[layout]] fixed-length frames with named bit fields and parity checks
                (RVSWD 53/54-clock frames)
  sequence      [[sequence]] fields read in order; a width may be an expression of
                earlier fields ("8 * min(dlc, 8)"), a field may be conditional
                (when = "ack == 1"), bit stuffing is a reader mode, checks are
                value / valid set / parity / CRC (SWD packets, CAN frames)

Anything that needs state or search (re-framing RVSWD at stop intervals) is not
expressed in the schema; the schema can only name a core kernel for it
(`frame.reframe = "rvswd_stop"`), so the exception stays in core code. The
expressions are deliberately small: integer arithmetic, comparisons, and/or/not,
`a if c else b`, min/max, over fields read earlier. No loops, no state across
frames; that is the boundary to script plugins.
"""

from __future__ import annotations

import ast
import math
import re
import tomllib
from pathlib import Path

import numpy as np

from . import typed
from .stack import Node, Stream

HERE = Path(__file__).resolve().parents[1] / "decl"


def q(n: float, scale: float = 8.0) -> float:
    return 1.0 - math.exp(-n / scale)


def _bits(spec: str) -> tuple[int, int]:
    """"hi:lo" or "b" -> (hi, lo), bit numbers of the word value (0 = LSB)."""
    if ":" in str(spec):
        hi, lo = str(spec).split(":")
        return int(hi), int(lo)
    return int(spec), int(spec)


def _field(v: int, spec: str) -> int:
    hi, lo = _bits(spec)
    return (v >> lo) & ((1 << (hi - lo + 1)) - 1)


def _length_ok(rule: str, n: int) -> bool:
    """Rules: "9n", "9n+1", "8n", "53|54", combinations with "|"."""
    for part in str(rule).split("|"):
        part = part.strip()
        m = re.fullmatch(r"(\d+)n(?:\+(\d+))?", part)
        if m:
            k, r = int(m.group(1)), int(m.group(2) or 0)
            if n > 0 and n >= r and (n - r) % k == 0:
                return True
        elif part.isdigit() and n == int(part):
            return True
    return False


def _chance(rule: str) -> float:
    """Probability that a random length satisfies the rule (for scoring above chance)."""
    p = 0.0
    for part in str(rule).split("|"):
        m = re.fullmatch(r"\s*(\d+)n(?:\+\d+)?\s*", part)
        if m:
            p += 1 / int(m.group(1))
    return min(1.0, p) if p else 0.02


def _parity(bits: np.ndarray) -> int:
    return int(bits.sum()) & 1


def _int_bits(bits) -> int:
    """Bits MSB first -> int."""
    v = 0
    for x in bits:
        v = (v << 1) | int(x)
    return v


REFRAMERS = {}


class Delimited(list):
    """Frames a protocol's own START and STOP delimit: clocks outside them belong to no frame (an idle clock, a
    reflection's glitches), so what the frames explain is measured against the clocks inside them."""


def reframer(name):
    def deco(f):
        REFRAMERS[name] = f
        return f
    return deco


@reframer("rvswd_stop")
def _rvswd_stop(fr, cap):
    from .rvswd import reframe, reframe_rolling, reframe_startstop
    ss = Delimited(reframe_startstop(fr, cap))
    # START and STOP seen: the frames are theirs. The cuts at stop intervals are for captures that hide them (where
    # they would read a stretch of other activity as frames that pass a parity or two by chance, and a piece of a
    # long capture would read otherwise than the whole)
    if ss:
        return [ss]
    return [ss, reframe(fr), reframe_rolling(fr)]


# ---------------- expressions (sequence shape) ----------------

_OK_NODES = (ast.Expression, ast.BinOp, ast.UnaryOp, ast.BoolOp, ast.Compare, ast.IfExp, ast.Name, ast.Load,
             ast.Constant, ast.Call, ast.Add, ast.Sub, ast.Mult, ast.FloorDiv, ast.Mod, ast.LShift, ast.RShift,
             ast.BitOr, ast.BitAnd, ast.BitXor, ast.Invert, ast.Not, ast.USub, ast.And, ast.Or, ast.Eq,
             ast.NotEq, ast.Lt, ast.LtE, ast.Gt, ast.GtE)
_FUNCS = {"min": min, "max": max}
_EXPR_CACHE: dict[str, object] = {}


def expr(src, env: dict) -> int:
    """Evaluate a schema expression over earlier fields; ints pass through."""
    if isinstance(src, (int, bool)):
        return int(src)
    code = _EXPR_CACHE.get(src)
    if code is None:
        tree = ast.parse(src, mode="eval")
        for n in ast.walk(tree):
            if not isinstance(n, _OK_NODES) or (isinstance(n, ast.Call) and not (
                    isinstance(n.func, ast.Name) and n.func.id in _FUNCS)) or (
                    isinstance(n, ast.Constant) and not isinstance(n.value, int)):
                raise ValueError(f"expression {src!r}: {type(n).__name__} is not allowed")
        code = _EXPR_CACHE[src] = compile(tree, src, "eval")
    try:
        return int(eval(code, {"__builtins__": {}}, {**_FUNCS, **env}))  # noqa: S307 (whitelisted AST)
    except NameError as e:
        raise ValueError(f"expression {src!r}: {e}") from None


class _Stuffed(Exception):
    pass


class BitReader:
    """Reads fields from a bit array. With stuffing on, after `after` equal bits
    (of `value`, or of either value when value is None) the next bit must be the
    complement and is dropped; a violation ends the parse."""

    def __init__(self, bits: np.ndarray, pos: int, end: int, stuffing: dict | None):
        self.bits, self.pos, self.end = bits, pos, end
        self.after = int(stuffing["after"]) if stuffing else 0
        self.value = stuffing.get("value") if stuffing else None
        self.active = False
        self.last, self.run = -1, 0
        self.removed = 0

    def _raw(self) -> int:
        if self.pos >= self.end:
            raise IndexError
        b = int(self.bits[self.pos])
        self.pos += 1
        return b

    def bit(self) -> int:
        b = self._raw()
        if not self.active:
            return b
        counts = self.value is None or b == int(self.value)
        self.run = self.run + 1 if (counts and b == self.last) else (1 if counts else 0)
        self.last = b
        if self.after and self.run == self.after:
            s = self._raw()
            if s == b:
                raise _Stuffed
            self.removed += 1
            self.last, self.run = s, (1 if self.value is None or s == int(self.value) else 0)
        return b

    def read(self, n: int) -> list[int]:
        return [self.bit() for _ in range(n)]


def _value(bits: list[int], order: str) -> int:
    v = 0
    for b in (bits if order == "msb" else bits[::-1]):
        v = (v << 1) | b
    return v


def crc_bits(bits, poly: int, width: int, init: int = 0) -> int:
    crc, top, mask = init, 1 << (width - 1), (1 << width) - 1
    for b in bits:
        fb = b ^ (1 if crc & top else 0)
        crc = (crc << 1) & mask
        if fb:
            crc ^= poly
    return crc


def fixed_prefix(seq: dict) -> list[tuple[int, list[list[int]]]]:
    """What a frame must start with: (offset, the bit patterns allowed there) of the leading fields of a fixed width
    and position whose value is expected or one of valid values, up to the first field that is conditional, of a
    computed width, or stuffed."""
    if seq.get("stuffing"):
        return []
    out, off = [], 0
    for f in seq["fields"]:
        w = f.get("width", 1)
        if "when" in f or not isinstance(w, int):
            break
        vals = [int(f["expect"])] if "expect" in f else [int(v) for v in f["valid"]] if "valid" in f else None
        if vals is not None:
            msb = f.get("order", seq.get("order", "msb")) == "msb"
            pats = []
            for v in vals:
                p = [(v >> (w - 1 - i)) & 1 for i in range(w)]
                pats.append(p if msb else p[::-1])
            out.append((off, pats))
        off += w
    return out


def _shifted_eq(bits: np.ndarray, off: int, b: int) -> np.ndarray:
    n = len(bits)
    sh = np.zeros(n, bool)
    if off < n:
        sh[:n - off] = bits[off:] == b
    return sh


def prefix_positions(seqs: list[dict], bits: np.ndarray) -> np.ndarray | None:
    """Where a frame of any of the sequences can start, by their fixed leading fields (None: any position)."""
    n = len(bits)
    any_ok = np.zeros(n, bool)
    for seq in seqs:
        pre = fixed_prefix(seq)
        if not pre:
            return None
        ok = np.ones(n, bool)
        for off, pats in pre:
            one = np.zeros(n, bool)
            for pat in pats:
                m = np.ones(n, bool)
                for i, b in enumerate(pat):
                    m &= _shifted_eq(bits, off + i, b)
                one |= m
            ok &= one
        any_ok |= ok
    return np.flatnonzero(any_ok)


def parse_sequence(seq: dict, bits: np.ndarray, pos: int, end: int, fail_fast: bool = False):
    """One frame from `pos`. Returns (record, consumed_bits, passed, checked) or None
    when the bits run out or stuffing is violated (or, fail_fast, a field is not its expected or a valid value)."""
    order0 = seq.get("order", "msb")
    st = seq.get("stuffing")
    rd = BitReader(bits, pos, end, st)
    env, raw, rec = {}, {}, {}
    passed = checked = 0
    info = 0.0   # bits of evidence: -log2(chance that random bits pass) over the passed checks
    try:
        for f in seq["fields"]:
            if "when" in f and not expr(f["when"], env):
                continue
            if st and f["name"] == st["from"]:
                rd.active = True
            w = expr(f.get("width", 1), env)
            b = rd.read(w) if w > 0 else []
            if st and f["name"] == st["to"]:
                rd.active = False
            v = _value(b, f.get("order", order0))
            env[f["name"]], raw[f["name"]] = v, b
            if fail_fast and (("expect" in f and v != int(f["expect"])) or ("valid" in f and v not in f["valid"])):
                return None
            if "expect" in f:
                checked += 1
                passed += v == int(f["expect"])
                info += w if v == int(f["expect"]) else 0
            if "valid" in f:
                checked += 1
                passed += v in f["valid"]
                info += (w - math.log2(len(f["valid"]))) if v in f["valid"] else 0
            if not f.get("hide"):
                if f.get("format") == "hex":               # bytes, most significant first, as hex
                    rec[f["name"]] = v.to_bytes((w + 7) // 8, "big").hex() if w else ""
                else:
                    rec[f["name"]] = f["enum"].get(str(v), v) if "enum" in f else v
    except (IndexError, _Stuffed):
        return None
    for d in seq.get("derive", []):
        env[d["name"]] = rec[d["name"]] = expr(d["expr"], env)
    names = [f["name"] for f in seq["fields"] if f["name"] in raw]
    for chk in seq.get("checks", []):
        if chk["field"] not in raw:
            continue
        checked += 1
        if chk["kind"] == "parity":
            ones = sum(sum(raw[x]) for x in chk["over"] if x in raw)
            good = env[chk["field"]] == ((ones & 1) ^ int(chk.get("odd", 0)))
            info += 1 if good else 0
        elif chk["kind"] == "crc":
            a, z = names.index(chk["from"]), names.index(chk["to"])
            data = [b for x in names[a:z + 1] for b in raw[x]]
            good = env[chk["field"]] == crc_bits(data, int(chk["poly"]), int(chk["width"]), int(chk.get("init", 0)))
            info += int(chk["width"]) if good else 0
        else:
            raise ValueError(f"unknown check kind {chk['kind']!r} (parity, crc)")
        passed += good
    rec["ok"] = passed == checked
    return rec, rd.pos - pos, passed, checked, info


def _active_bits(bits: np.ndarray, a: int, b: int, run: int = 16) -> int:
    """Bits in [a, b) outside constant runs of `run` or more (idle, line resets)."""
    x = bits[a:b]
    if len(x) == 0:
        return 0
    chg = np.flatnonzero(np.diff(x)) + 1
    edges = np.concatenate(([0], chg, [len(x)]))
    lens = np.diff(edges)
    return int(lens[lens < run].sum())


def _active_bits_in(bits: np.ndarray, bounds, run: int = 16) -> int:
    """_active_bits summed over frames [a, b) (sorted, apart), at once."""
    if not len(bounds):
        return 0
    bd = np.asarray(bounds, np.int64).reshape(-1, 2)
    a, b = bd[:, 0], bd[:, 1]
    chg = np.flatnonzero(np.diff(bits)) + 1
    cuts = np.unique(np.concatenate((chg, a, b)))
    seg_s, seg_e = cuts[:-1], cuts[1:]
    k = np.searchsorted(a, seg_s, side="right") - 1
    inside = (k >= 0) & (seg_s < b[np.maximum(k, 0)])
    lens = (seg_e - seg_s)[inside]
    return int(lens[lens < run].sum())


def _refined_bitrate(nb, spans) -> float:
    """Bit rate from the decoded frames: samples from SOF to the last edge inside
    each frame over the bits between them (the block estimate is coarse at a few
    samples per bit)."""
    bits_n = samples = 0
    for a, k in spans:
        x = nb.bits[a:a + k]
        chg = np.flatnonzero(np.diff(x)) + 1
        if len(chg):
            j = int(chg[-1])
            bits_n += j
            samples += int(nb.t[a + j] - nb.t[a])
    return nb.rate * bits_n / samples if samples else nb.bitrate()


class DeclarativePlugin:
    @property
    def _ext(self) -> dict:
        return {"external": True} if self.external else {}

    def __init__(self, spec: dict, path: str = ""):
        self.spec = spec
        self.path = path
        p = spec["plugin"]
        self.name = p["name"]
        self.consumes = tuple(p["input"]) if isinstance(p["input"], list) else (p["input"],)
        self.edge = p.get("sample_edge")
        self.data_pins = p.get("data_pins")
        # open_tail: also take the frame a capture ended inside (no STOP); it is reported, never counted as evidence
        self.open_tail = bool(p.get("open_tail", False))
        self.external = False

    @classmethod
    def load(cls, path: Path) -> "DeclarativePlugin":
        # the definition's name under decl/ (provenance in the output; never a local absolute path)
        name = path.relative_to(HERE).as_posix() if path.is_relative_to(HERE) else path.name
        return cls(tomllib.loads(path.read_text()), name)

    # ---------------- word-based ----------------
    def _words(self, fr, row):
        w = self.spec["word"]
        size = int(w["bits"])
        return typed.words(fr, size, row), size

    def _run_words(self, c, fr, g):
        sb = fr.source
        sp = self.spec
        tail = bool(getattr(fr, "open_tail", False))
        fr = fr.closed()                     # the evidence: closed frames only (the adapter sees the open one)
        lens = np.diff(fr.bounds, axis=1).ravel()
        keep = lens > 0
        if not keep.any() and not tail:
            return []
        rule = sp["frame"]["length"]
        fits = np.array([_length_ok(rule, int(n)) for n in lens[keep]], dtype=bool)
        fit = float(fits.mean()) if len(fits) else 0.0
        fit_evidence = fit
        if sp.get("score", {}).get("count_frames"):
            # each fitting frame is evidence of its own: one frame fits a length rule by chance (1/8 for "8n"), k frames
            # all by chance only (1/8)^k - so one CS window holding a whole other bus does not tie with the real framing
            fit_evidence *= 1 - _chance(rule) ** int(fits.sum())
        checks = [f for f in sp["word"].get("fields", []) if "expect" in f]
        rows = range(len(sb.data)) if sp["frame"].get("per_data_pin") else [0]
        records, n_words, check_hits, check_total = [], 0, 0, 0
        per_pin = {}
        for row in rows:
            w, size = self._words(fr, row)
            vals, fof = w.values, w.frame_of
            n_words += len(vals)
            for f in checks:
                got = np.array([_field(int(v), f["bits"]) for v in vals])
                check_hits += int(np.sum(got == int(f["expect"])))
                check_total += len(got)
            per_pin[sb.data[row]] = (vals, fof)
        first_fields = sp["frame"].get("first", [])
        rest = sp["frame"].get("rest")
        byte_field = {f["name"]: f["bits"] for f in sp["word"].get("fields", [])}
        vals0, fof0 = per_pin[sb.data[0]]
        nfr = len(fr.bounds)

        def by_frame(vals, fof):
            """Each frame's words: slices when the words are in frame order (as words() gives them)."""
            if len(fof) and np.all(np.diff(fof) >= 0):
                cut = np.searchsorted(fof, np.arange(nfr + 1))
                return lambda i: vals[cut[i]:cut[i + 1]]
            return lambda i: vals[fof == i]
        of0 = by_frame(vals0, fof0)
        of_pin = {pin: by_frame(vals, fof) for pin, (vals, fof) in per_pin.items()}
        for i, (a, b) in enumerate(fr.bounds.tolist()):
            ws = of0(i)
            if len(ws) == 0:
                continue
            rec = {"start": int(sb.t[a]), "end": int(sb.t[max(a, b - 1)])}
            src = lambda v, name: _field(int(v), byte_field[name]) if name in byte_field else int(v)  # noqa: E731
            for f in first_fields:
                v = _field(src(ws[0], f.get("from", "")), f["bits"]) if f.get("from") else _field(int(ws[0]), f["bits"])
                if "enum" in f:
                    v = f["enum"].get(str(v), v)
                rec[f["name"]] = v
            if rest:
                start = 1 if first_fields else 0
                if sp["frame"].get("per_data_pin"):
                    for pin in per_pin:
                        rec[pin] = bytes(src(v, rest["from"]) & 0xFF for v in of_pin[pin](i)).hex()
                else:
                    rec[rest["name"]] = [src(v, rest["from"]) for v in ws[start:]]
            for f in checks:
                if f.get("record"):
                    rec[f["name"]] = [_field(int(v), f["bits"]) == int(f["expect"]) for v in ws]
            records.append(rec)
        sc = sp.get("score", {})
        check_rate = check_hits / check_total if check_total else 1.0
        cw = float(sc.get("check_weight", 0.2))
        clk = c.sv.clocks[sb.clock].clock_score
        # a bus claim is only as strong as its weakest assigned data pin
        pair = float(np.min([c.sv.pairs[(sb.clock, d)].data_score for d in sb.data]))
        score = fit_evidence * (1 - cw + cw * check_rate) * q(n_words, sc.get("evidence_scale", 8)) \
            * (0.5 + 0.5 * clk) * (0.5 + 0.5 * pair)
        # evidence terms the core provides by name (not protocol code)
        terms = sc.get("terms", [])
        if "select_boundary" in terms and sb.select:
            from .survey import clock_info, pair_relation
            view = getattr(sb, "view", c.cap)
            ci = clock_info(view, c.sv.features[sb.clock])
            b = pair_relation(view, ci, sb.select).boundary if ci else 0.0
            score *= b
        if "degenerate" in terms:
            # a bus is suspicious only if every data pin is dominated by one value
            # (a flash MISO idles at 0xFF most of the time; that is normal)
            best = 0.0
            for vals, _ in per_pin.values():
                if len(vals) >= 4:
                    top = np.bincount(vals.astype(np.int64)).max() / len(vals)
                    best = max(best, 1.0 if top <= 0.5 else max(0.05, 1 - (top - 0.5) / 0.5))
                else:
                    best = max(best, 1.0)
            score *= best
        if "single_frame_penalty" in terms and fr.delimiter == "gap" and int(keep.sum()) <= 1:
            score *= 0.5
        roles = {sp["roles"]["clock"]: sb.clock}
        if sb.select and "select" in sp["roles"]:
            roles[sp["roles"]["select"]] = sb.select
        names = sp["roles"]["data"]
        for i, d in enumerate(sb.data):
            roles[names[i] if isinstance(names, list) and i < len(names) else f"{names}{i}"] = d
        m = {"length_fit": fit, "check_rate": check_rate, "words": n_words, "frames": int(keep.sum()), "clk": clk, "pair": pair}
        return [Node(self.name, {"sample_edge": sb.sample_edge, "declared": self.path, **self._ext}, roles,
                     Stream("records", records), m, layer_score=score, total=score)]

    # ---------------- layout-based ----------------
    def _layouts(self):
        """Exact lengths (bits = 53 or [52, 53]) and length rules (bits = "38n+15", with min_bits)."""
        exact, rules = {}, []
        for L in self.spec["layout"]:
            b = L["bits"]
            if isinstance(b, str):
                rules.append(L)
            else:
                for n in (b if isinstance(b, list) else [b]):
                    exact[int(n)] = L
        return exact, rules

    def _run_layout(self, c, fr, g):
        sb = fr.source
        sp = self.spec
        exact, rules = self._layouts()

        def layout(n):
            return exact.get(n) or next((L for L in rules if n >= int(L.get("min_bits", 0)) and _length_ok(L["bits"], n)),
                                        None)

        reframe = sp.get("frame", {}).get("reframe")
        views = REFRAMERS[reframe](fr, c.cap) if reframe else [fr.bounds.tolist()]
        best = None
        bits = sb.bits[0]
        ends = bool(self.spec.get("frame", {}).get("ends"))
        for frames in views:
            delimited = isinstance(frames, Delimited)
            recs, checked, passed, matched, checked_frames, explained = [], 0, 0, 0, 0, 0
            # frames of one length and layout read together (a matrix of their bits), the others one by one
            rows: list = [None] * len(frames)
            groups: dict = {}
            for i, fr_ in enumerate(frames):
                a, b = fr_[0], fr_[1]
                skip = fr_[2] if len(fr_) > 2 else ()      # glitch clocks dropped (a reframer's repair)
                L = layout(b - a - len(skip))
                if L is None or (delimited and L.get("undelimited_only")):
                    continue
                if skip or "repeat" in L:
                    rows[i] = (L, None)
                else:
                    groups.setdefault(b - a, (L, []))[1].append(i)
            for n, (L, idx) in groups.items():
                starts = np.asarray([frames[i][0] for i in idx], np.int64)
                M = bits[starts[:, None] + np.arange(n)[None, :]].astype(np.int64)
                vals = {}
                for fld in L["fields"]:
                    w = fld["width"]
                    vals[fld["name"]] = M[:, fld["at"]:fld["at"] + w] @ (1 << np.arange(w - 1, -1, -1, dtype=np.int64))
                q = L.get("unanswered_if_ones")
                unans = M[:, q[0]:q[1] + 1].all(axis=1) if q else np.zeros(len(idx), bool)
                goods = []
                for chk in L.get("checks", []):
                    if chk["kind"] == "parity":
                        lo, hi = chk["over"]
                        goods.append(M[:, chk["bit"]] == ((M[:, lo:hi + 1].sum(axis=1) & 1) ^ int(chk.get("odd", 0))))
                    elif chk["kind"] == "equal":     # one run of bits repeats another (an echo)
                        (a0, a1), (b0, b1) = chk["a"], chk["b"]
                        goods.append((M[:, a0:a1 + 1] == M[:, b0:b1 + 1]).all(axis=1))
                    else:
                        raise ValueError(f"unknown layout check kind {chk['kind']!r} (parity, equal)")
                for k, i in enumerate(idx):
                    rows[i] = (L, (vals, k, bool(unans[k]), [bool(g[k]) for g in goods]))
            for i, row in enumerate(rows):
                if row is None:
                    continue
                L, got = row
                fr_ = frames[i]
                a, b = fr_[0], fr_[1]
                skip = fr_[2] if len(fr_) > 2 else ()
                matched += 1
                explained += b - a
                rec = {"start": int(sb.t[a]), **L.get("const", {})}
                if L.get("keep_span"):                   # the frame's sampling edges, for an adapter that reads more
                    rec["_span"] = (int(a), int(b), tuple(skip))
                if skip:
                    rec["repaired"] = len(skip)
                if ends:
                    rec["end"] = int(sb.t[b - 1])
                if got is None:                          # one by one
                    f = bits[a:b].astype(np.int64)
                    if skip:
                        f = np.delete(f, [x - a for x in skip])
                    fv = {fld["name"]: _int_bits(f[fld["at"]:fld["at"] + fld["width"]]) for fld in L["fields"]}
                    q = L.get("unanswered_if_ones")
                    unanswered = bool(q and f[q[0]:q[1] + 1].all())
                    goods = []
                    for chk in L.get("checks", []):
                        if chk["kind"] == "parity":
                            lo, hi = chk["over"]
                            goods.append(int(f[chk["bit"]]) == (_parity(f[lo:hi + 1]) ^ int(chk.get("odd", 0))))
                        elif chk["kind"] == "equal":
                            (a0, a1), (b0, b1) = chk["a"], chk["b"]
                            goods.append(bool(np.array_equal(f[a0:a1 + 1], f[b0:b1 + 1])))
                        else:
                            raise ValueError(f"unknown layout check kind {chk['kind']!r} (parity, equal)")
                    if "repeat" in L:            # words of one width at a fixed pitch (a burst)
                        r = L["repeat"]
                        k = (b - a - len(skip) - int(r["at"]) - int(r.get("tail", 0))) // int(r["every"])
                        rep_vals = [_int_bits(f[r["at"] + r["every"] * j:r["at"] + r["every"] * j + r["width"]])
                                    for j in range(k)]
                else:
                    vals, k, unanswered, goods = got
                    fv = {name: int(v[k]) for name, v in vals.items()}
                for fld in L["fields"]:
                    v = fv[fld["name"]]
                    if "enum" in fld:
                        v = fld["enum"].get(str(v), v)
                    rec[fld["name"]] = v
                if got is None and "repeat" in L:
                    rec[L["repeat"]["name"]] = rep_vals
                if unanswered:                           # the answer's bits all 1: the line left released
                    rec["answered"] = False          # what the answer carries is not there
                    rec["ok"] = False
                    for fld in L["fields"]:
                        if fld["at"] + fld["width"] - 1 >= L["unanswered_if_ones"][0]:
                            rec[fld["name"]] = None
                    recs.append(rec)
                    continue
                if L.get("checks"):
                    checked_frames += 1
                checked += len(goods)
                passed += sum(goods)
                rec["ok"] = bool(all(goods))
                recs.append(rec)
            key = passed
            if best is None or key > best[0]:
                inside = sum(f_[1] - f_[0] for f_ in frames) if isinstance(frames, Delimited) else None
                best = (key, recs, checked, passed, matched, len(frames), checked_frames, explained, inside)
        _, recs, checked, passed, matched, total, checked_frames, explained, inside = best
        if matched < 2:
            return []
        parity = passed / checked if checked else 0.0
        total_bits = inside or int(np.diff(fr.bounds, axis=1).sum()) or 1
        cover = min(1.0, explained / total_bits)
        # the checked frames are the evidence; frames of a known structure only add coverage
        score = parity ** 2 * (0.5 + 0.5 * cover) * (1 - 0.5 ** (checked_frames / 2))
        roles = {sp["roles"]["clock"]: sb.clock, sp["roles"]["data"]: sb.data[0]}
        m = {"frames": total, "matched": matched, "checked_frames": checked_frames, "parity_ok": parity,
             "bit_coverage": cover}
        return [Node(self.name, {"sample_edge": sb.sample_edge, "declared": self.path, **self._ext}, roles,
                     Stream("records", recs), m, layer_score=score, total=score)]

    # ---------------- sequence ----------------
    def _run_sequence(self, c, src, g):
        """src: Frames over SyncBits (clocked) or NrzBits (async, one pin)."""
        sp = self.spec
        seqs = sp["sequence"]
        fs = sp.get("frame", {})
        scan = bool(fs.get("scan"))
        if hasattr(src, "source"):
            sb = src.source
            bits, t, bounds = sb.bits[0], sb.t, src.bounds.tolist()
        else:
            bits, t, bounds = src.bits, src.t, src.bounds.tolist()
        recs, n_ok, n_tried, used, active = [], 0, 0, 0, 0
        spans = []
        info = 0.0
        starts = prefix_positions(seqs, bits) if scan else None
        active = _active_bits_in(bits, [(a, b) for a, b in bounds if b > a])
        for a, b in bounds:
            p = a
            while p < b:
                if starts is not None:           # skip to the next position the fixed leading bits allow
                    i = int(np.searchsorted(starts, p))
                    if i >= len(starts) or starts[i] >= b:
                        break
                    p = int(starts[i])
                got = None
                for seq in seqs:
                    r = parse_sequence(seq, bits, p, b, fail_fast=scan)
                    if r is not None and (r[0]["ok"] or not scan):
                        got = r
                        break
                if not scan:
                    n_tried += 1
                    if got is not None:
                        rec, k, _, _, bits_i = got
                        rec["start"] = int(t[a])
                        recs.append(rec)
                        n_ok += rec["ok"]
                        info += bits_i if rec["ok"] else 0
                        used += k if rec["ok"] else 0
                        if rec["ok"]:
                            spans.append((a, k))
                    break
                if got is None:
                    p += 1
                    continue
                rec, k, _, _, bits_i = got
                rec["start"] = int(t[p])
                recs.append(rec)
                n_ok += 1
                info += bits_i
                used += k
                p += k
        if n_ok < 2:
            return []
        cover = min(1.0, used / active) if active else 0.0
        # evidence quantity in bits; a scan tries many positions, so each hit
        # pays log2(positions tried per hit) (look-elsewhere correction). A hit
        # is tried right after the previous one, so the positions tried are the
        # hits plus the active bits no hit explains.
        if scan:
            tries = max(0, active - used) + n_ok
            info = max(0.0, info - n_ok * math.log2(max(1.0, tries / n_ok)))
        evidence = 1 - 0.5 ** (info / float(sp.get("score", {}).get("evidence_bits", 8)))
        if scan:
            score = cover * evidence
        else:
            rate = n_ok / n_tried
            score = rate ** 2 * (0.5 + 0.5 * cover) * evidence
        m = {"frames": len(bounds), "decoded": n_ok, "bit_coverage": cover, "evidence_bits": round(info, 1)}
        if not scan:
            m["ok_rate"] = n_ok / n_tried
        if hasattr(src, "source"):
            # same clock / pairing evidence as the word shape (weakest data pin)
            clk = c.sv.clocks[sb.clock].clock_score
            pair = float(min(c.sv.pairs[(sb.clock, d)].data_score for d in sb.data))
            score *= (0.5 + 0.5 * clk) * (0.5 + 0.5 * pair)
            m.update(clk=clk, pair=pair)
        if "degenerate" in sp.get("score", {}).get("terms", []):
            # a periodic line read as frames repeats one frame; real traffic varies
            keys = [tuple(v for k, v in r.items() if k not in ("start", "ok")) for r in recs if r["ok"]]
            top = max(keys.count(k) for k in set(keys)) / len(keys)
            m["top_share"] = top
            score *= 1.0 if top <= 0.5 else max(0.05, 1 - (top - 0.5) / 0.5)
        if hasattr(src, "source"):
            roles = {sp["roles"]["clock"]: sb.clock, sp["roles"]["data"]: sb.data[0]}
            params = {"sample_edge": sb.sample_edge, "declared": self.path, **self._ext}
        else:
            roles = {sp["roles"]["data"]: src.pin}
            params = {"declared": self.path, "idle": src.idle, "bitrate": _refined_bitrate(src, spans), **self._ext}
        return [Node(self.name, params, roles, Stream("records", recs), m, layer_score=score, total=score)]

    def run(self, c, fr, g):
        if "sequence" in self.spec and not hasattr(fr, "source"):
            nodes = self._run_sequence(c, fr, g)
            adapt = ADAPTERS.get(self.name)
            return [adapt(c, fr, n) for n in nodes] if adapt else nodes
        sb = fr.source
        if self.edge and sb.sample_edge != self.edge:
            return []
        if self.data_pins and len(sb.data) != self.data_pins:
            return []
        if "sequence" in self.spec:
            nodes = self._run_sequence(c, fr, g)
        else:
            nodes = self._run_layout(c, fr, g) if "layout" in self.spec else self._run_words(c, fr, g)
        adapt = ADAPTERS.get(self.name)
        return [adapt(c, fr, n) for n in nodes] if adapt else nodes


def load_dir(root: Path) -> list[DeclarativePlugin]:
    """The protocol definitions at the top of a definitions directory added from outside (wireskein.protocols);
    each marked external, its provenance the directory's name and the file's."""
    out = []
    for p in sorted(Path(root).glob("*.toml")):
        plug = DeclarativePlugin(tomllib.loads(p.read_text()), f"{Path(root).name}/{p.name}")
        plug.external = True
        out.append(plug)
    return out


def load_all(names: list[str] | None = None) -> list[DeclarativePlugin]:
    out = []
    for p in sorted(HERE.glob("*.toml")):
        if names is None or p.stem in names:
            out.append(DeclarativePlugin.load(p))
    return out


# ---- output adapters: shape the generic records like the hand-written plugins' outputs ----

def _adapt_i2c(c, fr, n):
    # the transactions as the checks read them, from all frames (an open last one: complete=False, pending_bits)
    # SCL from the view the bits were read from (a deglitched one, when it was)
    scl = getattr(fr.source, "view", c.cap).channel(fr.source.clock)
    n.output = Stream("i2c.transactions", typed.i2c_from_words(fr, typed.words(fr, 9), scl))
    return n


def _adapt_spi(c, fr, n):
    sb = fr.source
    lines = {pin: [np.frombuffer(bytes.fromhex(r[pin]), dtype=np.uint8).astype(np.int64) for r in n.output.items]
             for pin in sb.data}
    first = np.concatenate(lines[sb.data[0]]) if lines[sb.data[0]] else np.zeros(0, np.int64)
    clk_lv = c.sv.features[sb.clock].idle_level or 0
    want = 1 if sb.sample_edge == "rise" else 0
    n.metrics["mode"] = clk_lv * 2 + (0 if (want == 1) != bool(clk_lv) else 1)
    n.params.update(bit_order="msb", cs_active=None, delimiter=fr.delimiter)
    roles = {"clk": sb.clock, **({"cs": sb.select} if sb.select else {})}
    for i, d in enumerate(sb.data):
        roles[f"data{i}"] = d
    n.roles = roles
    if not sb.select:
        n.metrics["cs"] = 1.0
    n.output = Stream("bytes", {"lines": lines, "value": first, "start": np.zeros(len(first), np.int64)}, meta={"framed": True})
    return n


def _adapt_rvswd(c, fr, n):
    tx = []
    for r in n.output.items:
        if "data" not in r:                  # structure-only layouts carry no fields
            continue
        x = {"t": r["start"], **({"e": r["end"]} if "end" in r else {}), "op": r.get("op", "R"), "addr": r.get("addr"), "data": r["data"],
             "status": r.get("status", 0), "ok": r["ok"], "form": r.get("form", "short"),
             **({"answered": False} if r.get("answered") is False else {})}
        if "_span" in r:
            x["_span"] = r["_span"]                  # the frame's details are read when it is exported
        tx.append(x)
    n.output = Stream("dmi", tx)
    return n


def _adapt_swd(c, fr, n):
    n.output = Stream("swd.packets", [r for r in n.output.items if r["ok"]])
    return n


def _adapt_can(c, fr, n):
    n.output = Stream("can.frames", n.output.items)
    return n


ADAPTERS = {"i2c": _adapt_i2c, "spi": _adapt_spi, "rvswd": _adapt_rvswd, "swd": _adapt_swd, "can": _adapt_can}
