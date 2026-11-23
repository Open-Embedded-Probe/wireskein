"""Protocols of one's own, declared as tables (`protocols/*.toml` in a definitions directory, docs/protocols.md).

A declaration reads what the analysis exports (the bus's layers), never the wire: the same table works whatever finds
the buses, and in any language that reads the analysis format. Two shapes:

- **frames** (`[frame]`): byte frames, on a UART's byte stream or an SPI's transfer (searched, resynchronizing on the
  next start that checks), or one frame each in a CAN, LIN or DMX512 frame's data (whole). Fields in order, a check
  (a CRC or a sum, by name) over some of them; an optional sequence counted per frame identifier.
- **lines** (`[lines]`): text lines on a UART pair, a command and its answer. Patterns (regular expressions with
  named groups) for a command, an answer and an unasked event; an answer belongs to the command just before it
  (with the same value of the group `pair` names, when it names one).

The checks a frame passes and fails, and the answers that belong or do not, are the evidence (plugins._score).
"""

from __future__ import annotations

import re
import tomllib
from pathlib import Path

FORMAT = "wireskein-protocol/1"
STREAM = {"uart", "spi", "i2c"}                 # a byte stream (searched)
FRAMED = {"can", "lin", "dmx512"}               # frames of their own (each one whole)
PAIR = {"uart_pair"}

# ---------------- checks ----------------

CRC = {   # name: (width, poly, init, refin, refout, xorout)
    "crc8": (8, 0x07, 0x00, False, False, 0x00),
    "crc8_maxim": (8, 0x31, 0x00, True, True, 0x00),
    "crc8_sae_j1850": (8, 0x1D, 0xFF, False, False, 0xFF),
    "crc16_modbus": (16, 0x8005, 0xFFFF, True, True, 0x0000),
    "crc16_ccitt_false": (16, 0x1021, 0xFFFF, False, False, 0x0000),
    "crc16_xmodem": (16, 0x1021, 0x0000, False, False, 0x0000),
    "crc16_kermit": (16, 0x1021, 0x0000, True, True, 0x0000),
    "crc32": (32, 0x04C11DB7, 0xFFFFFFFF, True, True, 0xFFFFFFFF),
}
SUMS = ("sum8", "xor8", "sum16")


def _reflect(v: int, n: int) -> int:
    return int(f"{v:0{n}b}"[::-1], 2)


def crc(data: bytes, width: int, poly: int, init: int, refin: bool, refout: bool, xorout: int) -> int:
    top, mask, c = 1 << (width - 1), (1 << width) - 1, init
    for b in data:
        c ^= (_reflect(b, 8) if refin else b) << (width - 8)
        for _ in range(8):
            c = ((c << 1) ^ poly) & mask if c & top else (c << 1) & mask
    return (_reflect(c, width) if refout else c) ^ xorout


def _check_value(spec: dict, data: bytes, attrs: dict) -> int:
    def num(v):
        return int(attrs[v[1:]]) if isinstance(v, str) and v.startswith("@") else int(v)
    kind = spec["kind"]
    if kind in CRC:
        w, poly, init, refin, refout, xorout = CRC[kind]
        return crc(data, w, int(spec.get("poly", poly)), num(spec.get("init", init)), bool(spec.get("refin", refin)),
                   bool(spec.get("refout", refout)), int(spec.get("xorout", xorout)))
    if kind == "crc":
        return crc(data, int(spec["width"]), int(spec["poly"]), num(spec.get("init", 0)), bool(spec.get("refin", False)),
                   bool(spec.get("refout", False)), int(spec.get("xorout", 0)))
    v = num(spec.get("init", 0))
    if kind == "xor8":
        for b in data:
            v ^= b
        return (v & 0xFF) ^ int(spec.get("xorout", 0))
    w = 0xFF if kind == "sum8" else 0xFFFF
    return ((v + sum(data)) & w) ^ int(spec.get("xorout", 0))


# ---------------- text of a message ----------------

_FIELD = re.compile(r"\{([@\w]+)(?:\|([^}:]*))?(?::([^}]*))?\}")


def _text(template: str, values: dict) -> str:
    """A message's text: {name}, {name:spec} (a Python-like spec: x, 02x, #x, d), {name|default} for an empty value."""
    def one(m):
        v = values.get(m[1].lstrip("@"), "")
        if (v == "" or v is None) and m[2] is not None:
            return m[2]
        return format(v, m[3]) if m[3] else str(v)
    return _FIELD.sub(one, template)


# ---------------- frames ----------------

class _Frames:
    def __init__(self, spec: dict, where: str):
        self.fields = list(spec["fields"])
        self.show = list(spec.get("show") or [f["name"] for f in self.fields if not f.get("check")])
        self.text = spec.get("text")
        self.seq = spec.get("sequence")
        names = set()
        for f in self.fields:
            if not isinstance(f.get("name"), str):
                raise ValueError(f"{where}: a field without a name")
            if "of" in f:
                if f["of"] not in names:
                    raise ValueError(f"{where}: field {f['name']} is of {f['of']!r}, not a field before it")
            else:
                size = f.get("size", 1)
                if not (isinstance(size, int) or size == "rest" or size in names):
                    raise ValueError(f"{where}: field {f['name']}: size is a number, a field before it, or \"rest\"")
            if "check" in f:
                k = f["check"].get("kind")
                if k not in CRC and k not in SUMS and k != "crc":
                    raise ValueError(f"{where}: field {f['name']}: check kind {k!r} (one of "
                                     f"{', '.join([*CRC, *SUMS, 'crc'])})")
                for o in f["check"].get("over", []):
                    if o not in names:
                        raise ValueError(f"{where}: field {f['name']}: the check is over {o!r}, not a field before it")
            names.add(f["name"])
        self._fixed_after = []
        for i in range(len(self.fields)):
            rest = [g for g in self.fields[i + 1:] if "of" not in g]
            self._fixed_after.append(sum(g.get("size", 1) for g in rest) if all(isinstance(g.get("size", 1), int)
                                                                                 for g in rest) else None)

    def at(self, data: bytes, i: int, whole: bool, attrs: dict):
        """The frame at i: None (not one here), or (end, values, check passed: True / False / None)."""
        raw, vals, pos, ok = {}, {}, i, None
        for k, f in enumerate(self.fields):
            if "of" in f:
                b = raw[f["of"]]
                vals[f["name"]] = self._value(f, b, int.from_bytes(b, f.get("endian", "big")) if 0 < len(b) <= 8
                                              else None)
                continue
            size = f.get("size", 1)
            if size == "rest":
                after = self._fixed_after[k]
                if after is None:
                    return None
                size = len(data) - pos - after
            elif isinstance(size, str):
                size = vals[size] if isinstance(vals[size], int) else int.from_bytes(raw[size], "big")
            if size < 0 or pos + size > len(data):
                return None
            b = data[pos:pos + size]
            pos += size
            raw[f["name"]] = b
            n = int.from_bytes(b, f.get("endian", "big")) if 0 < len(b) <= 8 else None
            if "equals" in f and n != int(f["equals"]):
                return None
            if "max" in f and (n is None or n > int(f["max"])):
                return None
            if "min" in f and (n is None or n < int(f["min"])):
                return None
            if "check" in f:
                c = f["check"]
                got = _check_value(c, b"".join(raw[o] for o in c.get("over", [])), attrs)
                ok = got == n
                if not ok:
                    return pos, vals, False
                continue
            vals[f["name"]] = self._value(f, b, n)
        if whole and pos != len(data):
            return None
        return pos, vals, ok

    @staticmethod
    def _value(f: dict, b: bytes, n):
        if "mask" in f:
            m = int(f["mask"])
            v = n & m
            v >>= (m & -m).bit_length() - 1
            return bool(v) if f.get("as") == "bool" else v
        if f.get("as") == "hex" or n is None:
            return b.hex()
        if f.get("as") == "bool":
            return bool(n)
        return n

    def read(self, data: bytes, whole: bool, attrs: dict):
        """(frames [(start, values)], passed, failed) in a byte unit: the whole of it, or searched."""
        if whole:
            got = self.at(data, 0, True, attrs)
            if got is None:
                return [], 0, 0
            _, vals, ok = got
            return ([(0, vals)] if ok is not False else []), int(ok is True), int(ok is False)
        out, ok, bad, i = [], 0, 0, 0
        while i < len(data):
            got = self.at(data, i, False, attrs)
            if got is None:
                i += 1
                continue
            end, vals, good = got
            if good is False:
                bad += 1
                i += 1
                continue
            ok += good is True
            out.append((i, vals))
            i = max(end, i + 1)
        return out, ok, bad

    def message(self, vals: dict, attrs: dict, s, extra: dict) -> dict:
        allv = {**{k: v for k, v in attrs.items()}, **vals}
        m = {"s": s, **{k.lstrip("@"): allv.get(k.lstrip("@")) for k in self.show}, **extra}
        return m


class Declared:
    """A declared protocol, with the shape a protocol has (name, over, decode)."""

    def __init__(self, doc: dict, where: str):
        from . import plugins as P
        if doc.get("format") != FORMAT:
            raise ValueError(f"{where}: format must be {FORMAT!r}")
        self.name = P._name(doc.get("name"), "protocol")
        self.over = tuple(doc.get("over") or ())
        known = STREAM | FRAMED | PAIR
        if not self.over or not all(b in known for b in self.over):
            raise ValueError(f"{where}: over names buses among {', '.join(sorted(known))}, not {self.over!r}")
        self.frames = _Frames(doc["frame"], where) if "frame" in doc else None
        self.lines = doc.get("lines")
        if (self.frames is None) == (self.lines is None):
            raise ValueError(f"{where}: one of [frame] and [lines]")
        if self.lines is not None:
            if set(self.over) - PAIR:
                raise ValueError(f"{where}: [lines] rides on uart_pair")
            self._pat = {k: re.compile(self.lines[k]) for k in ("command", "answer", "event") if k in self.lines}
            if "command" not in self._pat or "answer" not in self._pat:
                raise ValueError(f"{where}: [lines] has a command and an answer")
        elif set(self.over) & PAIR:
            raise ValueError(f"{where}: [frame] rides on byte streams and frames, not uart_pair")
        self.path = where

    # --- frames ---
    def _frames(self, bus: dict) -> dict:
        fr, msgs, ok, bad = self.frames, [], 0, 0
        seq, last = fr.seq or {}, {}

        def add(vals, attrs, s, extra):
            m = fr.message(vals, attrs, s, extra)
            if seq:
                key = attrs.get(seq.get("per", "").lstrip("@")) if seq.get("per") else None
                v, mod = vals.get(seq["field"]), int(seq.get("modulo", 256))
                gap = key in last and v != (last[key] + 1) % mod
                last[key] = v
                m["level"] = "warn" if gap else "ok"
            if fr.text:
                t = _text(fr.text, {**attrs, **vals})
                if seq and m["level"] == "warn":
                    t += seq.get("note", " (sequence skipped)")
                m["text"] = t
            msgs.append(m)

        if "bytes" in bus and isinstance(bus["bytes"], dict):        # a UART: one byte stream
            data, ticks = bytes.fromhex(bus["bytes"]["hex"]), bus["bytes"]["s"]
            got, ok, bad = fr.read(data, False, {})
            for i, vals in got:
                add(vals, {}, ticks[i] if i < len(ticks) else None, {})
        for t in bus.get("transfers", []) or []:                      # SPI: each window on each data line
            for pin, hx in t.items():
                if pin in ("s", "e") or not isinstance(hx, str):
                    continue
                got, o, b = fr.read(bytes.fromhex(hx), False, {})
                ok, bad = ok + o, bad + b
                for _, vals in got:
                    add(vals, {}, t["s"], {"line": pin})
        for t in bus.get("transactions", []) or []:                   # I2C: each transaction's data
            data = t.get("bytes")
            if isinstance(data, str) and data:
                got, o, b = fr.read(bytes.fromhex(data), False, {"addr": t.get("addr")})
                ok, bad = ok + o, bad + b
                for _, vals in got:
                    add(vals, {"addr": t.get("addr")}, t.get("s"), {})
        for f in (bus.get("frames") or bus.get("packets") or []):      # CAN, LIN, DMX512: one frame each
            if not isinstance(f, dict) or f.get("ok") is False:
                continue
            attrs = {k: v for k, v in f.items() if k not in ("data", "s", "e")}
            got, o, b = fr.read(bytes.fromhex(f.get("data") or ""), True, attrs)
            ok, bad = ok + o, bad + b
            for _, vals in got:
                add(vals, attrs, f.get("s"), {})
        return {"messages": msgs, "checks_passed": ok, "checks_failed": bad}

    # --- lines ---
    @staticmethod
    def _lines_of(msg: dict):
        text, s, e = msg["text"], msg["s"], msg["e"]
        per = (e - s) / max(1, len(text))
        pos = 0
        for line in text.split("\n"):
            if line.strip("\r"):
                yield line.strip("\r"), int(s + pos * per), int(s + (pos + len(line) + 1) * per)
            pos += len(line) + 1

    def _groups(self, m: re.Match) -> dict:
        words = set(self.lines.get("words", []))
        return {k: (v or "").split() if k in words else v for k, v in m.groupdict().items()}

    def _pairs(self, bus: dict) -> dict:
        P, L = self._pat, self.lines
        key = L.get("pair")
        lines = sorted(((t, a, b, m["pin"]) for m in bus["messages"] for t, a, b in self._lines_of(m)),
                       key=lambda x: x[1])
        pins = {p for *_, p in lines}
        host = max(pins, key=lambda p: sum(1 for t, *_, q in lines if q == p and P["command"].match(t)
                                           and not P["answer"].match(t)), default=None)
        warn = L.get("warn", {})
        out, passed, failed, pending = [], 0, 0, None
        for text, a, b, pin in lines:
            if pin == host:
                c = P["command"].match(text)
                if not c or P["answer"].match(text):
                    continue
                if pending is not None:
                    failed += 1
                    pending.update(level="warn", text=pending["text"] + " -> (no answer)")
                pending = {"s": a, "e": b, **self._groups(c), "status": None, "text": text}
                out.append(pending)
                continue
            ev = P.get("event") and P["event"].match(text)
            if ev:
                out.append({"s": a, "e": b, **self._groups(ev), "text": text, "level": "ok"})
                continue
            r = P["answer"].match(text)
            if not r:
                continue
            g = self._groups(r)
            if pending is None or (key and r[key] != pending.get(key)):
                failed += 1
                out.append({"s": a, "e": b, **g, "text": text + " (answers no command)", "level": "error"})
                continue
            passed += 1
            raw = {**{k: v or "" for k, v in r.groupdict().items()}, "command": pending["text"]}
            bad = any(r.groupdict().get(k) == v for k, v in warn.items())
            pending.update({k: v for k, v in g.items() if k != key}, e=b, level="warn" if bad else "ok",
                           text=_text(L.get("answered", "{command} -> " + text), raw))
            pending = None
        return {"messages": out, "checks_passed": passed, "checks_failed": failed}

    def decode(self, bus: dict) -> dict:
        return self._pairs(bus) if self.lines is not None else self._frames(bus)


def load(path: Path) -> Declared:
    return Declared(tomllib.loads(Path(path).read_text()), str(path))


_CACHE: dict = {}


def declared(dirs) -> dict[str, Declared]:
    """The protocols declared in the definitions directories (protocols/*.toml), kept while their files are unchanged."""
    out = {}
    for d in dirs:
        for p in sorted((Path(d) / "protocols").glob("*.toml")):
            key = (str(p), p.stat().st_mtime_ns)
            if key not in _CACHE:
                _CACHE[key] = load(p)
            proto = _CACHE[key]
            if proto.name in out:
                raise ValueError(f"{p}: {proto.name} is declared twice ({out[proto.name].path})")
            out[proto.name] = proto
    return out
