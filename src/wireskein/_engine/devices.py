"""Device packs (declarative) matched against transactions and transfers.

Three kinds of description, following how datasheets describe devices:

  register_map   I2C/SPI register devices: pointer write then read, identity registers
  command_table  command codes with argument/response lengths and checks (SHT30, SPI NOR)
  (message framing for UART upper protocols lives in analyzers/upper.py and duplex.py)

Evidence levels, from strong to weak:

  identified     an identity register/response matched, or checks (CRC) passed
  consistent     the traffic fits the command/register table, but nothing self-verifying
  address_only   only the bus address matches -- never a claim, only a candidate list

The matcher never reports a device that is not in the loaded packs, and never
turns address-only matches into claims.
"""

from __future__ import annotations

import os
import re
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parents[1] / "decl" / "devices"   # <bus>/<vendor or class>/<pack>.toml
# I2CDeviceDB (https://github.com/tanakamasayuki/I2CDeviceDB) chips/ folder, for
# address-only candidates; optional
I2CDB = Path(os.environ["WIRESKEIN_I2CDB"]) if os.environ.get("WIRESKEIN_I2CDB") else None


def crc8(data: bytes, poly: int, init: int) -> int:
    c = init
    for b in data:
        c ^= b
        for _ in range(8):
            c = ((c << 1) ^ poly) & 0xFF if c & 0x80 else (c << 1) & 0xFF
    return c


@dataclass
class Match:
    device: str
    level: str                 # identified / consistent / address_only
    evidence: dict
    messages: list = field(default_factory=list)
    roles: dict = field(default_factory=dict)


class PackSet:
    """The device packs selected by path patterns under decl/devices/<bus>/...

    Patterns are POSIX globs relative to the root ("i2c/**", "i2c/sensirion/*",
    "spi/flash/spi_nor.toml"); a leading "!" excludes. Only the [device] headers
    are read up front (cached by mtime); a pack's tables are parsed the first
    time its bus address or bus is seen, so a large library costs little.
    """

    def __init__(self, select: list[str] | None = None, root: Path = HERE):
        self.root = root
        inc = [x for x in (select or ["**"]) if not x.startswith("!")] or ["**"]
        exc = [x[1:] for x in (select or []) if x.startswith("!")]
        rels = [p.relative_to(root) for p in sorted(root.rglob("*.toml"))]
        self.paths = [r for r in rels if any(_match(r, g) for g in inc) and not any(_match(r, g) for g in exc)]
        self.headers = _headers(root, self.paths)
        self.by_addr: dict[int, list[Path]] = {}
        for r in self.paths:
            h = self.headers[str(r)]
            if h["bus"] == "i2c":
                for a in h.get("addresses", []):
                    self.by_addr.setdefault(int(a), []).append(r)
        self._full: dict[Path, dict] = {}

    def pack(self, rel: Path) -> dict:
        if rel not in self._full:
            self._full[rel] = tomllib.loads((self.root / rel).read_text()) | {"_file": str(rel)}
        return self._full[rel]

    def i2c(self, addresses) -> list[dict]:
        rels = {r for a in addresses for r in self.by_addr.get(int(a), [])}
        return [self.pack(r) for r in sorted(rels)]

    def bus(self, bus: str) -> list[dict]:
        return [self.pack(r) for r in self.paths if self.headers[str(r)]["bus"] == bus]

    def __len__(self):
        return len(self.paths)


def _glob(pattern: str) -> re.Pattern:
    """A path pattern as a regex: "**" is any number of folders, "*" and "?" stay inside one name
    (PurePath.full_match, which needs Python 3.13)."""
    out = []
    for i, part in enumerate(pattern.split("/")):
        last = i == len(pattern.split("/")) - 1
        if part == "**":
            out.append("(?:[^/]+/)*" if not last else "(?:[^/]+/)*[^/]*")
            continue
        out.append(re.escape(part).replace(r"\*", "[^/]*").replace(r"\?", "[^/]") + ("" if last else "/"))
    return re.compile("".join(out) + r"\Z")


def _match(rel: Path, pattern: str) -> bool:
    path = rel.as_posix()
    if pattern.endswith("/**"):          # "i2c/**": everything below that folder
        return path.startswith(pattern[:-2]) or bool(_glob(pattern + "/*").match(path))
    return bool(_glob(pattern).match(path))


CACHE = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "wireskein" / "device-headers.json"


def _headers(root: Path, rels: list[Path]) -> dict[str, dict]:
    """[device] header of each pack; the bus must match the top folder."""
    import json
    try:
        cache = json.loads(CACHE.read_text())
    except (OSError, ValueError):
        cache = {}
    out, dirty = {}, False
    for r in rels:
        p = root / r
        key, mt = str(p), p.stat().st_mtime_ns
        c = cache.get(key)
        if c is None or c["mtime"] != mt:
            d = tomllib.loads(p.read_text())["device"]
            c = {"mtime": mt, "header": {k: d[k] for k in ("name", "bus", "kind", "addresses") if k in d}}
            cache[key], dirty = c, True
        h = c["header"]
        if h["bus"] != r.parts[0]:
            raise ValueError(f"{r}: device.bus = {h['bus']!r} but the pack is under {r.parts[0]}/")
        out[str(r)] = h
    if dirty:
        try:
            CACHE.parent.mkdir(parents=True, exist_ok=True)
            CACHE.write_text(json.dumps(cache))
        except OSError:
            pass    # only a speed-up
    return out


def load_packs(select: list[str] | None = None, root: Path = HERE) -> PackSet:
    return PackSet(select, root)


def address_table() -> dict[int, list[str]]:
    """Bus address -> chip names from I2CDeviceDB (address-only candidates)."""
    out: dict[int, list[str]] = {}
    if I2CDB is None or not I2CDB.exists():
        return out
    for p in sorted(I2CDB.glob("*.yaml")):
        txt = p.read_text()
        name = re.search(r"^name:\s*(.+)$", txt, re.M)
        for a in re.findall(r'addr:\s*"?(0x[0-9A-Fa-f]+)"?', txt):
            out.setdefault(int(a, 16), []).append(name.group(1).strip() if name else p.stem)
    return out


# ---------------- I2C ----------------

def _i2c_command_table(pack, txs) -> Match | None:
    d = pack["device"]
    cb = int(d.get("command_bytes", 2))
    table = {int(c["code"]): c for c in pack.get("command", [])}
    crc = d.get("crc")
    known = unknown = passed = failed = 0
    msgs = []
    pending = None
    for t in txs:
        if t["addr"] not in d["addresses"]:
            continue
        b = bytes(t["bytes"])
        if t["rw"] == "write" and len(b) >= cb:
            code = int.from_bytes(b[:cb], "big")
            cmd = table.get(code)
            if cmd is None:
                unknown += 1
                msgs.append({"s": t.get("start"), "command": f"0x{code:0{2 * cb}x}", "known": False})
                pending = None
                continue
            known += 1
            msgs.append({"s": t.get("start"), "command": cmd["name"]})
            pending = cmd if cmd.get("response") else None
        elif t["rw"] == "read" and b and pending is not None:
            want = int(pending["response"])
            ok_len = len(b) == want
            crc_ok = None
            if crc and ok_len:
                w = int(crc["word"])
                oks = [crc8(b[i:i + w], int(crc["poly"]), int(crc["init"])) == b[i + w] for i in range(0, len(b), w + 1)]
                passed += sum(oks)
                failed += len(oks) - sum(oks)
                crc_ok = all(oks)
            msgs.append({"s": t.get("start"), "response_to": pending["name"], "bytes": b.hex(), "crc_ok": crc_ok})
            pending = None
    if known + unknown == 0:
        return None
    ev = {"known_commands": known, "unknown_commands": unknown, "crc_passed": passed, "crc_failed": failed}
    if passed >= 2 and failed == 0:
        level = "identified"
    elif known >= 3 and known / (known + unknown) >= 0.8:
        level = "consistent"
    else:
        level = "address_only"
    return Match(d["name"], level, ev, msgs)


def _i2c_register_map(pack, txs) -> Match | None:
    d = pack["device"]
    pb = int(d.get("pointer_bytes", 1))
    ident = {(int(i["register"]), int(i["value"])) for i in pack.get("identify", [])}
    regs = {int(r["address"]): r["name"] for r in pack.get("register", [])}
    hits = misses = accesses = 0
    msgs = []
    ptr = None
    for t in txs:
        if t["addr"] not in d["addresses"]:
            continue
        b = bytes(t["bytes"])
        if t["rw"] == "write" and len(b) == pb:
            ptr = int.from_bytes(b, "big")
        elif t["rw"] == "write" and len(b) > pb:
            reg = int.from_bytes(b[:pb], "big")
            accesses += 1
            msgs.append({"s": t.get("start"), "write": regs.get(reg, f"0x{reg:02x}"), "value": b[pb:].hex()})
        elif t["rw"] == "read" and b and ptr is not None:
            accesses += 1
            for reg, val in ident:
                if reg == ptr:
                    if b[0] == val:
                        hits += 1
                    else:
                        misses += 1
            msgs.append({"s": t.get("start"), "read": regs.get(ptr, f"0x{ptr:02x}"), "value": b.hex()})
            ptr = None
    if accesses == 0:
        return None
    ev = {"identity_hits": hits, "identity_misses": misses, "accesses": accesses}
    level = "identified" if hits >= 1 and misses == 0 else "address_only"
    return Match(d["name"], level, ev, msgs)


def match_i2c(txs: list[dict], packs: PackSet, addr_db: dict | None = None) -> list[Match]:
    """Identified / consistent packs are claims; everything that only shares the bus
    address (packs without evidence, chip-DB names) is one candidate list per address."""
    seen = sorted({t["addr"] for t in txs if t.get("addr_ack")})
    claims, cands = [], {a: set() for a in seen}
    for p in packs.i2c(seen):
        kind = p["device"]["kind"]
        m = _i2c_command_table(p, txs) if kind == "command_table" else _i2c_register_map(p, txs)
        if m is None:
            continue
        if m.level == "address_only":
            for a in set(p["device"]["addresses"]) & set(seen):
                cands[a].add(m.device)
            continue
        m.evidence["pack"] = p["_file"]
        claims.append(m)
    out = _merge_indistinguishable(claims)
    claimed = {n for m in out for n in m.evidence.get("indistinguishable", [m.device])}
    for a in seen:
        names = sorted((cands[a] | set((addr_db or {}).get(a, []))) - claimed)
        if names:
            out.append(Match("candidates", "address_only", {"address": a, "chips": names}))
    return out


def _merge_indistinguishable(ms: list[Match]) -> list[Match]:
    """Identified packs whose evidence is the same (e.g. SHT30/SHT31: same commands,
    same CRC) are one claim, "one of these", never a single model."""
    keep = [m for m in ms if m.level != "identified"]
    groups: dict[str, list[Match]] = {}
    for m in (m for m in ms if m.level == "identified"):
        key = repr(sorted((k, v) for k, v in m.evidence.items() if k != "pack"))
        groups.setdefault(key, []).append(m)
    for g in groups.values():
        if len(g) == 1:
            keep.append(g[0])
        else:
            names = sorted(m.device for m in g)
            keep.append(Match(" | ".join(names), "identified", g[0].evidence | {"indistinguishable": names,
                              "pack": [m.evidence["pack"] for m in g]}, g[0].messages))
    return keep


# ---------------- SPI ----------------

def match_spi(lines: dict, packs: PackSet) -> list[Match]:
    """lines: {pin: [frame bytes arrays]} of one SPI bus. Tries each pin as the command line."""
    out = []
    for p in packs.bus("spi"):
        d = p["device"]
        table = {int(c["code"]): c for c in p.get("command", [])}
        makers = {int(k, 16): v for k, v in p.get("jedec_manufacturers", {}).items()}
        best = None
        pins = list(lines)
        for cmd_pin in pins:
            resp_pins = [x for x in pins if x != cmd_pin]
            known = unknown = consistent = 0
            ident, msgs = [], []
            for i, fr in enumerate(lines[cmd_pin]):
                f = bytes(int(v) & 0xFF for v in fr)
                if not f:
                    continue
                c = table.get(f[0])
                if c is None:
                    unknown += 1
                    continue
                known += 1
                need = 1 + int(c.get("address_bytes", 0)) + int(c.get("dummy_bytes", 0))
                ok = len(f) == int(c["length"]) if "length" in c else len(f) >= need + (1 if c.get("response") else 0)
                consistent += ok
                rec = {"frame": i, "command": c["name"], "length_ok": bool(ok)}
                if c.get("address_bytes") and len(f) >= need:
                    rec["address"] = int.from_bytes(f[1:1 + int(c["address_bytes"])], "big")
                for rp in resp_pins:
                    r = bytes(int(v) & 0xFF for v in lines[rp][i]) if i < len(lines[rp]) else b""
                    if c.get("identify") == "jedec" and len(r) >= 4:
                        mid = r[1]
                        if mid in makers and r[2:4] not in (b"\x00\x00", b"\xff\xff"):
                            ident.append(f"{makers[mid]} {r[1:4].hex()}")
                            rec["jedec_id"] = r[1:4].hex()
                    if c.get("identify") == "sfdp" and len(r) >= need + 4 and r[need:need + 4] == b"SFDP":
                        ident.append("SFDP signature")
                msgs.append(rec)
            if known + unknown == 0:
                continue
            ev = {"known_commands": known, "unknown_commands": unknown, "length_consistent": consistent,
                  "identity": sorted(set(ident)), "command_pin": cmd_pin}
            ratio = known / (known + unknown)
            if ident and ratio >= 0.5:
                level = "identified"
            elif known >= 3 and ratio >= 0.8 and consistent >= 0.8 * known:
                level = "consistent"
            else:
                level = None
            score = (2 if level == "identified" else 1 if level == "consistent" else 0, known)
            if best is None or score > best[0]:
                best = (score, Match(d["name"], level or "none", ev, msgs, {"mosi": cmd_pin}))
        if best is not None and best[1].level != "none":
            out.append(best[1])
    return out
