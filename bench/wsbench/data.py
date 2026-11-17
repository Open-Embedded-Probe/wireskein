"""Cases: a capture read from a source, transformed (noise, crop, decimate), with per-line truth.

A dataset is a source and a chain of transforms, both written as strings so that a ledger entry names exactly what
was measured:

    source      set:<name>[a:b]   a frozen set under eval/corpus/synth (cases a..b in file order)
                real              eval/corpus/real (the records with about a million edges left out)
                dir:<path>[a:b]   a directory of this repository of <id>.wireskein beside <id>.truth.json (the
                                  eval/ truth format)
                gen:<profile>[/<stress>][a:b][@<rate>]   eval/synth.py's scenarios of seeds a..b, sampled at <rate>
                                  (the same traffic whatever the rate: a base for decimate, sweeping the rate), or at
                                  the scenario's own
                wchfix:<sub>[a:b]   wch-protocols' LinkE fixture, as copied to bench/data/wire-linke-p4-2026-09-25
                                  (<sub>: l103, v203, v203-100mhz, v003, extra/l103; the captures of 300 kB or less):
                                  real RVSWD / SWIO with truth from wch-protocols' own decoder (.dmi.txt)
                ebdd:<name>[/tune|/holdout][a:b]  EmbedBenchDeviceData's runs, as copied to bench/data/<name>/runs
                                  (/tune, /holdout: half set aside by the case's name) (real I2C taken by
                                  an OEP probe): truth the bus (SDA / SCL renamed D0 / D1), its rate (from the run's
                                  name) and the bytes the firmware said it read (its dut lines: bytes=.. crc=1, data=..)
                linke:<dir>[/rate]/<tune|holdout>[a:b]   runs of bench/data/<dir> (/rate: the same operations taken at
                                  rates from 160 MHz to 627 kHz, whole and fractional dividers of 160 MHz) (wireskein-run/1 + truth.json: a LinkE's
                                  RVSWD taken at 160 MS/s by an OEP probe, one ch32rv operation each), the half
                                  set aside for checking or the other (run.json meta.holdout, from the scenario's
                                  name). Truth: what ch32rv read (read.bin), and the DMI accesses as read at 160 MS/s
                                  where they agree with every DMI operation ch32rv sent over USB (reading.json, truth.json; for
                                  the run decimated: decimate:K, never for 160 MS/s itself). SWCLK / SWDIO renamed
                                  D0 / D1
                i2cbench:h417_slow/stream[a:b]   real captures of tests/data/i2c_bench: I2C through weak
                                  pull-ups, 50 times "S, 0x13 write, NACK, P" each (what the device answered: the
                                  summaries beside them), truth made on reading; SCL / SDA renamed D0 / D1
    transforms  "spike:0.1+bounce:0.1"  applied left to right; "" for none
                spike / bounce / xtalk: noise in ticks of the capture (as wide whatever the rate)
                pspike:L:NS / pbounce:L:NS: noise in time (NS nanoseconds), the way a line shows it: wider in
                samples the faster it is taken, missed when shorter than a sample and no sample falls in it
                odslow:NS: an open-drain bus (I2C) through a weak pull-up: each rise NS x U(0.6, 1) ns late, a high
                shorter than that gone (clocks clipped to nothing, as on tests/data/i2c_bench's bench)

Every source is a file of this repository (bench/data/ holds the copies of other repositories' records the suites
use: bench/data/extract.py): the bench reads nothing else, on GitHub's runners as here.

Noise is seeded from the case id and the transform text (never Python's hash()), so a dataset is the same on every
run and machine. The clean capture (the same transforms without the noise) is kept beside the noisy one: stages
that score a cleaning compare against it.
"""

from __future__ import annotations

import hashlib
import json
import re
import zlib
from dataclasses import dataclass, field, replace
from pathlib import Path

import numpy as np

from wireskein import capture as _capture
from wireskein._engine import kernels as K
from wireskein._engine.model import Capture

REPO = Path(__file__).resolve().parents[2]
SETS = REPO / "eval" / "corpus" / "synth"
REAL = REPO / "eval" / "corpus" / "real"
DATA = REPO / "bench" / "data"            # other repositories' records, copied (bench/data/extract.py)

# truth labels of a line (stage lineclass)
LABELS = ("clock", "data", "select", "periodic", "random", "static")


@dataclass
class LineTruth:
    label: str
    period_s: float | None = None      # clock / periodic lines: one period (rising to rising)
    unit_s: float | None = None        # asynchronous lines: one bit
    variable: bool = False             # the rate changes inside the capture (no single value is right)
    nominal: bool = False              # the value is the nominal one (a real record), not the generator's
    protocol: str | None = None
    role: str | None = None


@dataclass
class Case:
    id: str                            # case id + dataset transforms
    dataset: str
    cap: Capture                       # what algorithms see
    clean: Capture | None              # the same without noise (None for real records)
    truth: dict                        # the eval/ truth.json (algorithms never see it)
    lines: dict[str, LineTruth] = field(default_factory=dict)
    input_sha: str = ""
    bus_tags: list = field(default_factory=list)   # per truth bus: why it is hard to read (see hardness), [] if not
    aliased: set = field(default_factory=set)     # lines that alias at this rate (a decoy clock too fast for it)


# ---------------- sources ----------------

_RANGE = re.compile(r"^(?P<base>[^\[]+)(\[(?P<a>\d*):(?P<b>\d*)\])?$")


def _slice(paths: list[Path], a: str | None, b: str | None) -> list[Path]:
    return paths[int(a) if a else None:int(b) if b else None]


_GEN = re.compile(r"^gen:(?P<profile>[a-z]+)(/(?P<stress>[a-z]+))?\[(?P<a>\d+):(?P<b>\d+)\](@(?P<rate>[0-9.e+]+))?$")


def source_items(source: str) -> list[str]:
    """The cases of a source: file paths, i2cbench:<path>, or gen:<profile>:<stress>:<seed>:<rate>."""
    g = _GEN.match(source)
    if g:
        rate = float(g["rate"]) if g["rate"] else 0.0          # 0: the scenario's own rate
        return [f"gen:{g['profile']}:{g['stress'] or ''}:{s}:{rate}" for s in range(int(g["a"]), int(g["b"]))]
    m = _RANGE.match(source)
    if m and m["base"].startswith("wchfix:"):
        d = _inside(WCH_FIXTURE / m["base"].split(":", 1)[1])
        if not d.is_dir():
            raise FileNotFoundError(d)
        return [f"wchfix:{p}" for p in _slice(sorted(d.glob("*.wireskein")), m["a"], m["b"])]
    if m and m["base"].startswith("linke:"):
        sub, _, split = m["base"][len("linke:"):].rpartition("/")
        if split not in ("tune", "holdout"):
            raise ValueError(f"source {source!r}: linke:<dir>[/rate]/tune or linke:<dir>[/rate]/holdout")
        rate = sub.endswith("/rate")
        d = _inside(DATA / sub.removesuffix("/rate")) / "runs"
        if not d.is_dir():
            raise FileNotFoundError(d)
        runs = [p.parent for p in sorted(d.rglob("truth.json"))
                if (p.parent.relative_to(d).parts[0] == "rate") == rate
                and json.loads((p.parent / "run.json").read_text())["meta"].get("holdout") == (split == "holdout")]
        return [f"linke:{r}" for r in _slice(runs, m["a"], m["b"])]
    if m and m["base"].startswith("ebdd:"):
        sub = m["base"][len("ebdd:"):]
        split = None
        if sub.endswith(("/tune", "/holdout")):
            sub, _, split = sub.rpartition("/")
        d = _inside(DATA / sub) / "runs"
        if not d.is_dir():
            raise FileNotFoundError(d)
        runs = sorted(d.glob("*/run.json"))
        if split:          # half set aside by the case's name: one case on one side at every bus rate
            runs = [p for p in runs if (hashlib.sha256(p.parent.name.split("__")[0].encode()).digest()[0] & 1 == 1)
                    == (split == "holdout")]
        return [f"ebdd:{p.parent}" for p in _slice(runs, m["a"], m["b"])]
    if m and m["base"].startswith("i2cbench:"):
        d = I2C_BENCH / m["base"][len("i2cbench:"):]
        if not d.is_dir():
            raise FileNotFoundError(d)
        return [f"i2cbench:{p}" for p in _slice(sorted(d.rglob("*.wireskein")), m["a"], m["b"])]
    return [str(p) for p in source_paths(source)]


I2C_BENCH = REPO / "tests" / "data" / "i2c_bench"


def _i2cbench(path: Path) -> tuple[Capture, dict]:
    """A capture of tests/data/i2c_bench/h417_slow: stream/ 50 transactions "S, 0x13 write, NACK, P" (the device
    answered NACK to each: summary-<tag>.json's dut_nack2), noise/<tag>/run<k> one of them; SCL 100 kHz (20 kHz for
    the clk20000 ones)."""
    cap = _capture.read(path).parts[0].view()
    names = {ch.name for ch in cap.channels}
    if names != {"SCL", "SDA"}:
        raise ValueError(f"{path}: channels {sorted(names)}, want SCL and SDA")
    flip = zlib.crc32(path.name.encode()) & 1          # which is D0, and the order: from the name, not the role
    new = {"SCL": "D1" if flip else "D0", "SDA": "D0" if flip else "D1"}
    chans = sorted((replace(ch, name=new[ch.name]) for ch in cap.channels), key=lambda c: c.name)
    one = path.parent.parent.name == "noise"                       # noise/<tag>/run<k>: one transaction
    tag = (path.parent.name if one else path.stem).split("-")[0]
    hz = 20e3 if tag == "clk20000" else 100e3
    rel = path.relative_to(I2C_BENCH)
    truth = {"id": "i2cbench-" + str(rel.with_suffix("")).replace("/", "-"),
             "source": {"path": str(path.relative_to(REPO))},
             "buses": [{"protocol": "i2c", "roles": {"scl": new["SCL"], "sda": new["SDA"]}, "params": {"clock_hz": hz},
                        "expect": {"compare_ack": True,
                                   "transactions": [{"addr": 0x13, "rw": "write", "bytes": [], "addr_ack": False}]
                                   * (1 if one else 50)}}],
             "decoys": [],
             "notes": "real capture; truth: what the controller was told to send and the device's answer (NACK)"}
    return Capture(cap.rate, cap.n_samples, chans), truth


def source_paths(source: str) -> list[Path]:
    m = _RANGE.match(source)
    if not m:
        raise ValueError(f"source {source!r}: set:<name>[a:b], real, dir:<path>[a:b] or gen:<profile>[a:b]@<rate>")
    base, a, b = m["base"], m["a"], m["b"]
    if base.startswith("set:"):
        d = SETS / base[4:]
    elif base == "real":
        d = REAL
    elif base.startswith("dir:"):
        d = _inside(REPO / base[4:])
    else:
        raise ValueError(f"source {source!r}: set:<name>[a:b], real or dir:<path>[a:b]")
    if not d.is_dir():
        raise FileNotFoundError(d)
    paths = sorted(d.glob("*.wireskein"))
    if base == "real":       # the flash writes (about a million edges each) are for large runs only
        paths = [p for p in paths if not ("flash" in p.name and not p.name.startswith("i2cdb"))]
    return _slice(paths, a, b)


def _inside(d: Path) -> Path:
    """d, which must be inside this repository: the bench reads no one's local files (put a copy under bench/data)."""
    d = d.resolve()
    if not d.is_relative_to(REPO):
        raise ValueError(f"{d}: outside this repository (the bench reads only its files: copy what a suite needs "
                         f"under bench/data, as bench/data/extract.py does)")
    return d


WCH_FIXTURE = DATA / "wch-linke-p4-2026-09-25"    # wch-protocols' captures/fixtures/wire-linke-p4-2026-09-25, in part


def _dmi_rows(dmi: Path) -> list:
    """wch-protocols' tools/dmi_decode.py output, as eval/convert_real.py reads it: [W|R, register, data]."""
    rows = []
    for line in dmi.read_text().splitlines():
        f = line.split()
        if len(f) >= 4 and f[1] in ("W", "R"):
            rows.append([f[1], f[2], int(f[3], 16)])
    return rows


def _wchfix(path: Path) -> tuple[Capture, dict]:
    """A capture of the LinkE fixture (copied: bench/data/extract.py) with its truth from wch-protocols' own decoder
    (not WireSkein): RVSWD's DMI accesses from <op>.dmi.txt (where only the 50 MHz run of the same operation has one -
    v203-100mhz - that one, marked cross-session: ch32rv's steps are fixed, a polling count may differ). A .dmi.txt
    whose decoder left over 10 % of its frames unread is no truth. SWIO has no DMI reference (wch-protocols' swio.py
    is gone from it). A read operation's <op>.bin (what ch32rv read: read_flash_4k, read_ram_256) is truth for RVSWD
    and SWIO alike, the reading measured by it (eval: expect.read_bytes). Channels renamed D0.. in an order from the file's name (the sigrok
    file's, <op>.sr, as before the copy: the same cases, the same input_sha)."""
    from wireskein import decode as _decode
    cap = _decode.load(path)
    rel = path.relative_to(WCH_FIXTURE).with_suffix(".sr")
    order = sorted(cap.channels, key=lambda c: zlib.crc32(f"{rel}|{c.name}".encode()))
    new = {c.name: f"D{i}" for i, c in enumerate(order)}
    chans = sorted((replace(c, name=new[c.name]) for c in cap.channels), key=lambda c: int(c.name[1:]))
    names = {c.name for c in cap.channels}
    expect, source = {}, None
    if "SWCLK" in names:
        dmi = path.with_suffix(".dmi.txt")
        if not dmi.exists() and path.parent.name.startswith("v203-"):
            alt = WCH_FIXTURE / "v203" / dmi.name
            if alt.exists():
                dmi, source = alt, "wch-protocols tools/dmi_decode.py, the 50 MHz run of the same operation (cross-session)"
        if dmi.exists():
            rows = _dmi_rows(dmi)
            # frames the reference decoder left unread: raw lines of a length no known frame has (the long form's
            # 80-86 clocks and the 99-clock one every run has are frames it prints raw, not failures). V203's
            # SWCLK reflections leave 4, 322, 17171 ... (target_info 0 accesses read against L103's 132): no truth
            # then, the bus's lines only
            unread = sum(1 for n in re.findall(r"RAW(\d+)", dmi.read_text()) if not (80 <= int(n) <= 86 or int(n) == 99))
            if rows and unread <= 0.1 * (len(rows) + unread):
                expect["dmi"] = rows
                # its R / W is not always right (it splits frames by idle time: the fixture's README), its
                # addresses and data are
                expect["dmi_ignore_op"] = True
                source = source or "wch-protocols tools/dmi_decode.py (.dmi.txt)"
        bus = ({"protocol": "swio", "roles": {"dio": "D0"}, "params": {}} if names == {"SWIO"} else
           {"protocol": "rvswd", "roles": {"clk": new["SWCLK"], "dio": new["SWDIO"]}, "params": {}})
    else:
        bus = {"protocol": "swio", "roles": {"dio": new["SWIO"]}, "params": {}}
    # what ch32rv itself read in this run (<op>.bin, a memory dump of 256 bytes or more: read_flash_4k, read_ram_256),
    # independent of any decoder: eval then measures the reading by it (the share of its words among data0's reads)
    dump = path.with_suffix(".bin")
    if dump.exists() and dump.stat().st_size >= 256:
        expect["read_bytes"] = dump.read_bytes().hex()
        source = (source + "; " if source else "") + "ch32rv's read (<op>.bin)"
    if expect:
        bus["expect"], bus["expect_source"] = expect, source
    truth = {"id": "wchfix-" + str(rel.with_suffix("")).replace("/", "-"),
             "source": {"path": str(path.relative_to(REPO)), "repo": "wch-protocols", "file": str(rel)},
             "buses": [bus], "decoys": [], "notes": "real LinkE <-> target capture (wch-protocols fixture 2026-09-25)"}
    return Capture(cap.rate, cap.n_samples, chans), truth


_DUT_READ = re.compile(r"(?:^|\s)(?:bytes|data)=([0-9a-fA-F]+)(?:\s|$)")


def _ebdd(run: Path) -> tuple[Capture, dict]:
    """An EmbedBenchDeviceData run: one capture of SDA and SCL, and the firmware's own lines (src dut) saying what it
    read. Truth: the bus, its rate (the run's name: i2c-100k), the payloads read (a CRC that failed, crc=0, left
    out). Not the decode beside it (c0001.i2c.json is WireSkein's)."""
    meta = json.loads((run / "run.json").read_text())
    capf = run / meta["captures"][0]["file"]
    cap = _capture.read(capf).parts[0].view()
    names = {ch.name for ch in cap.channels}
    if names != {"SCL", "SDA"}:
        raise ValueError(f"{capf}: channels {sorted(names)}, want SCL and SDA")
    flip = zlib.crc32(run.name.encode()) & 1
    new = {"SCL": "D1" if flip else "D0", "SDA": "D0" if flip else "D1"}
    chans = sorted((replace(ch, name=new[ch.name]) for ch in cap.channels), key=lambda c: c.name)
    m = re.search(r"__i2c-(\d+)k__", run.name)
    hz = float(m.group(1)) * 1e3 if m else None
    if run.name.startswith("wire-scan__"):
        hz = 100e3       # arduino-esp32's zero-length write (i2c_master_probe) goes at 100 kHz whatever the bus's rate
    reads = []
    for x in meta.get("log", []):
        if x.get("src") != "dut" or "crc=0" in x["text"]:
            continue
        reads += [h.lower() for h in _DUT_READ.findall(x["text"])]
    truth = {"id": f"ebdd-{run.name}", "source": {"path": str(run.relative_to(REPO)), "repo": "EmbedBenchDeviceData"},
             "buses": [{"protocol": "i2c", "roles": {"scl": new["SCL"], "sda": new["SDA"]},
                        "params": {"clock_hz": hz},
                        "expect": {"dut_reads": reads}}],
             "decoys": [], "notes": "real capture; truth: the bus and what the firmware said it read"}
    return Capture(cap.rate, cap.n_samples, chans), truth


def _linke(run: Path) -> tuple[Capture, dict]:
    """A run of a 160 MS/s LinkE capture, RVSWD (SWCLK, SWDIO) or SWIO (its one line), taken and cut by the tools of
    wireskein-captures' <dir>/tools (capture.py, truth.py, publish.py)."""
    doc = json.loads((run / "run.json").read_text())
    t = json.loads((run / "truth.json").read_text())
    cap = _capture.read(run / doc["captures"][0]["file"]).parts[0].view()
    names = {ch.name for ch in cap.channels}
    if names not in ({"SWCLK", "SWDIO"}, {"SWIO"}):
        raise ValueError(f"{run}: channels {sorted(names)}, want SWCLK and SWDIO (RVSWD) or SWIO")
    root = run
    while root.name != "runs":
        root = root.parent
    rel = run.relative_to(root)
    flip = zlib.crc32(str(rel).encode()) & 1
    new = {"SWCLK": "D1" if flip else "D0", "SWDIO": "D0" if flip else "D1", "SWIO": "D0"}
    chans = sorted((replace(ch, name=new[ch.name]) for ch in cap.channels), key=lambda c: c.name)
    expect, src = {}, []
    if t.get("read_bytes") and len(t["read_bytes"]) >= 512:          # 256 bytes or more: a memory dump
        expect["read_bytes"] = t["read_bytes"]
        src.append("ch32rv's read (read.bin)")
    if t.get("dmi_ops"):
        expect["dmi_ops"] = t["dmi_ops"]           # scored where neither read_bytes nor dmi is (eval/evaluate.py)
        src.append(f"ch32rv's {len(t['dmi_ops'])} DMI operations over USB, in order")
    rd = json.loads((run / "reading.json").read_text()) if (run / "reading.json").exists() else {}
    if t["agreement_160M"].get("usable") and rd.get("dmi"):
        expect["dmi"] = rd["dmi"]
        expect["dmi_same_run_160M"] = True                           # truth for the run decimated only
        src.append(f"the 160 MS/s reading, every one of ch32rv's {t['agreement_160M']['dmi_ops']} DMI operations found in it")
    bus = ({"protocol": "swio", "roles": {"dio": "D0"}, "params": {}} if names == {"SWIO"} else
           {"protocol": "rvswd", "roles": {"clk": new["SWCLK"], "dio": new["SWDIO"]}, "params": {}})
    if expect:
        bus["expect"], bus["expect_source"] = expect, "; ".join(src)
    truth = {"id": f"linke-{root.parent.name}-" + str(rel).replace("/", "-"),
             "source": {"path": str(run.relative_to(REPO)), "scenario": doc["meta"].get("scenario")},
             "buses": [bus], "decoys": [],
             "notes": f"real LinkE -> {doc['meta'].get('target', 'target')} {bus['protocol'].upper()} at 160 MS/s, tapped at "
                      "the LinkE (one ch32rv operation)"}
    return Capture(cap.rate, cap.n_samples, chans), truth


def read(ref: str) -> tuple[Capture, dict]:
    if ref.startswith("linke:"):
        return _linke(Path(ref[len("linke:"):]))
    if ref.startswith("wchfix:"):
        return _wchfix(Path(ref[len("wchfix:"):]))
    if ref.startswith("ebdd:"):
        return _ebdd(Path(ref[len("ebdd:"):]))
    if ref.startswith("i2cbench:"):
        return _i2cbench(Path(ref[len("i2cbench:"):]))
    if ref.startswith("gen:"):
        import sys
        sys.path.insert(0, str(REPO / "eval"))
        import synth  # eval/synth.py
        _, profile, stress, seed, rate = ref.split(":")
        cap, truth = synth.scenario(int(seed), profile, stress or None, rate=float(rate) or None)
        if float(rate):
            truth["id"] += f"@{float(rate) / 1e6:g}M"
        return cap, truth
    path = Path(ref)
    truth = json.loads(path.with_suffix("").with_suffix(".truth.json").read_text())
    return _capture.read(path).parts[0].view(), truth


# ---------------- transforms ----------------

def _toggle(edges: np.ndarray, extra: np.ndarray) -> np.ndarray:
    """Add toggles: two on the same tick cancel."""
    u, c = np.unique(np.concatenate([np.asarray(edges, np.int64), np.asarray(extra, np.int64)]), return_counts=True)
    return u[c % 2 == 1]


def _rng(case_id: str, spec: str) -> np.random.Generator:
    return np.random.default_rng(zlib.crc32(f"{case_id}|{spec}".encode()))


def noise_spike(cap: Capture, lvl: float, rng) -> Capture:
    """1-tick pulses at random places, lvl per real edge of the channel."""
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        if len(e) == 0:
            out.append(ch)
            continue
        g = rng.integers(1, cap.n_samples - 2, rng.poisson(len(e) * lvl))
        out.append(replace(ch, edges=_toggle(e, np.concatenate([g, g + 1]))))
    return Capture(cap.rate, cap.n_samples, out)


def noise_bounce(cap: Capture, lvl: float, rng) -> Capture:
    """At a share lvl of the real edges, a pulse back to the old level 1-3 ticks wide within 4 ticks after the edge
    (and a second one at half of those): a slow or ringing edge crossing the threshold more than once."""
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        if len(e) == 0:
            out.append(ch)
            continue
        sel = e[rng.random(len(e)) < lvl]
        parts = []
        for keep in (1.0, 0.5):
            s = sel[rng.random(len(sel)) < keep]
            a = s + rng.integers(1, 5, len(s))
            parts += [a, a + rng.integers(1, 4, len(s))]
        extra = np.concatenate(parts)
        out.append(replace(ch, edges=_toggle(e, extra[extra < cap.n_samples])))
    return Capture(cap.rate, cap.n_samples, out)


def noise_xtalk(cap: Capture, lvl: float, rng) -> Capture:
    """At a share lvl of the next channel's edges, a 1-tick pulse on this one (coupling between neighbours)."""
    chans = cap.channels
    out = []
    for i, ch in enumerate(chans):
        e = np.asarray(ch.edges, np.int64)
        if len(e) == 0 or len(chans) < 2:
            out.append(ch)
            continue
        ne = np.asarray(chans[(i + 1) % len(chans)].edges, np.int64)
        g = ne[rng.random(len(ne)) < lvl] + 1
        g = g[g < cap.n_samples - 1]
        out.append(replace(ch, edges=_toggle(e, np.concatenate([g, g + 1]))))
    return Capture(cap.rate, cap.n_samples, out)


def _pulses(e0: np.ndarray, width: np.ndarray) -> np.ndarray:
    """Toggles for pulses from e0 (ticks, real-valued) width ticks long, as samples on whole ticks see them: a level
    is seen from the first tick at or after its start, so a pulse no tick falls in is not seen (both toggles on one
    tick cancel)."""
    return np.concatenate([np.ceil(e0), np.ceil(e0 + width)]).astype(np.int64)


def noise_pspike(cap: Capture, lvl: float, ns: float, rng) -> Capture:
    """Pulses ns nanoseconds wide at random places, lvl per real edge of the channel."""
    w = ns * 1e-9 * float(cap.rate)
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        if len(e) == 0:
            out.append(ch)
            continue
        t0 = rng.uniform(1, cap.n_samples - 2 - w, rng.poisson(len(e) * lvl))
        extra = _pulses(t0, np.full(len(t0), w))
        out.append(replace(ch, edges=_toggle(e, extra[(extra > 0) & (extra < cap.n_samples)])))
    return Capture(cap.rate, cap.n_samples, out)


def noise_pbounce(cap: Capture, lvl: float, ns: float, rng) -> Capture:
    """At a share lvl of the real edges, within ns nanoseconds after it, a pulse back to the old level 10-40 % of ns
    wide, and at half of those a second one: a slow or ringing edge crossing the threshold more than once."""
    span = ns * 1e-9 * float(cap.rate)
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        if len(e) == 0:
            out.append(ch)
            continue
        sel = e[rng.random(len(e)) < lvl].astype(np.float64)
        parts = []
        for keep, at in ((1.0, (0.0, 0.4)), (0.5, (0.5, 0.8))):
            s = sel[rng.random(len(sel)) < keep]
            parts.append(_pulses(s + span * rng.uniform(*at, len(s)), span * rng.uniform(0.1, 0.4, len(s))))
        extra = np.concatenate(parts)
        out.append(replace(ch, edges=_toggle(e, extra[(extra > 0) & (extra < cap.n_samples)])))
    return Capture(cap.rate, cap.n_samples, out)


OPEN_DRAIN = {"i2c": ("scl", "sda")}       # lines pulled up by a resistor: their rises are slow


def noise_odslow(cap: Capture, ns: float, rng, lines: set) -> Capture:
    """An open-drain line through a weak pull-up (tests/data/i2c_bench/h417_slow: 45 kOhm, about 5 us to the
    threshold): each rise reaches the threshold ns x U(0.6, 1) nanoseconds late, falls are driven and on time. A
    high shorter than its delay never shows (a clock clipped to nothing), as on the bench."""
    d0 = ns * 1e-9 * float(cap.rate)
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        if ch.name not in lines or len(e) == 0:
            out.append(ch)
            continue
        lv = K.edge_levels(e, ch.initial)
        rise = np.flatnonzero(lv == 1)
        late = e[rise] + np.round(d0 * rng.uniform(0.6, 1.0, len(rise))).astype(np.int64)
        nxt = np.where(rise + 1 < len(e), e[np.minimum(rise + 1, len(e) - 1)], cap.n_samples)
        gone = late >= nxt                      # the high ends before the line gets there
        keep = np.ones(len(e), bool)
        e2 = e.copy()
        e2[rise[~gone]] = late[~gone]
        keep[rise[gone]] = False
        keep[np.minimum(rise[gone] + 1, len(e) - 1)[rise[gone] + 1 < len(e)]] = False
        e2 = e2[keep]
        e2 = e2[e2 < cap.n_samples]
        out.append(replace(ch, edges=_toggle(np.sort(e2), np.array([], np.int64))))
    return Capture(cap.rate, cap.n_samples, out)


def crop(cap: Capture, a: float, b: float) -> Capture:
    """The part from a to b (fractions of the length)."""
    s0, s1 = int(a * cap.n_samples), int(b * cap.n_samples)
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64)
        initial = int(ch.initial ^ (int(np.sum(e <= s0)) & 1))
        out.append(replace(ch, initial=initial, edges=e[(e > s0) & (e < s1)] - s0))
    return Capture(cap.rate, s1 - s0, out)


def decimate(cap: Capture, k: int) -> Capture:
    """Sampled k times slower (every k-th tick)."""
    out = []
    for ch in cap.channels:
        e = np.asarray(ch.edges, np.int64) // k
        u, c = np.unique(e, return_counts=True)
        e = u[c % 2 == 1]
        initial = int(ch.initial ^ (int(np.sum(e == 0)) & 1))
        out.append(replace(ch, initial=initial, edges=e[e > 0], step=max(1, ch.step // k), phase=ch.phase // k))
    return Capture(cap.rate / k, cap.n_samples // k, out)


def resample(cap: Capture, rate_hz: float) -> Capture:
    """Taken at rate_hz from the capture's own grid, as a probe's fractional divider takes it (samples on the source
    clock's ticks, unevenly spaced: oep-probe-arduino docs/guide/capture): sample j at tick floor(j * r0 / rate_hz).
    An edge is counted as decimate counts it (floor). A whole divider gives decimate's capture exactly."""
    from fractions import Fraction
    # the ratio as a small fraction (a divider's: 133.3 / 160 = 5/6), so that edge x numerator stays in int64
    q = (Fraction(rate_hz) / Fraction(cap.rate)).limit_denominator(1 << 12)
    if q <= 0 or q > 1:
        raise ValueError(f"resample:{rate_hz:g}: not a rate below the capture's {cap.rate:g} Hz")
    out = []
    for ch in cap.channels:
        e = (np.asarray(ch.edges, np.int64) * q.numerator) // q.denominator
        u, c = np.unique(e, return_counts=True)
        e = u[c % 2 == 1]
        initial = int(ch.initial ^ (int(np.sum(e == 0)) & 1))
        out.append(replace(ch, initial=initial, edges=e[e > 0], step=1, phase=0))
    return Capture(float(cap.rate * q), int(cap.n_samples * q), out)


NOISE = {"spike": noise_spike, "bounce": noise_bounce, "xtalk": noise_xtalk}
TIMED_NOISE = {"pspike": noise_pspike, "pbounce": noise_pbounce}


def apply(cap: Capture, clean: Capture | None, chain: str, case_id: str,
          truth: dict | None = None) -> tuple[Capture, Capture | None]:
    for spec in [s for s in chain.split("+") if s]:
        kind, *args = spec.split(":")
        if kind == "odslow":
            lines = {b["roles"][r] for b in (truth or {}).get("buses", []) for r in OPEN_DRAIN.get(b["protocol"], ())
                     if r in b["roles"]}
            cap = noise_odslow(cap, float(args[0]), _rng(case_id, spec), lines)
        elif kind in NOISE:
            cap = NOISE[kind](cap, float(args[0]), _rng(case_id, spec))
        elif kind in TIMED_NOISE:
            cap = TIMED_NOISE[kind](cap, float(args[0]), float(args[1]), _rng(case_id, spec))
        elif kind == "crop":
            cap = crop(cap, float(args[0]), float(args[1]))
            clean = crop(clean, float(args[0]), float(args[1])) if clean else None
        elif kind == "decimate":
            cap = decimate(cap, int(args[0]))
            clean = decimate(clean, int(args[0])) if clean else None
        elif kind == "resample":
            cap = resample(cap, float(args[0]))
            clean = resample(clean, float(args[0])) if clean else None
        else:
            raise ValueError(f"transform {spec!r}: spike:L, bounce:L, xtalk:L, pspike:L:NS, pbounce:L:NS, odslow:NS, "
                             "crop:A:B, decimate:K or resample:HZ")
    return cap, clean


# ---------------- truth per line ----------------

ASYNC = {"uart": ("data",), "lin": ("data",), "dmx512": ("data",), "scpi": ("rx", "tx"), "can": ("data",)}
CLOCK = {"i2c": "scl", "spi": "clk", "rvswd": "clk", "swd": "swclk"}


def line_truth(truth: dict, cap: Capture, clean: Capture) -> dict[str, LineTruth]:
    real = "generator" not in truth
    freqhop = (truth.get("generator") or {}).get("stress") == "freqhop"
    out: dict[str, LineTruth] = {}
    for b in truth.get("buses", []):
        pr, pm = b["protocol"], b.get("params", {})
        for role, ch in b["roles"].items():
            if pr in ASYNC and role in ASYNC[pr]:
                baud = pm.get("baud") or pm.get("bitrate")
                out[ch] = LineTruth("data", unit_s=1 / baud if baud else None, variable="baud_segments" in pm,
                                    nominal=real, protocol=pr, role=role)
            elif CLOCK.get(pr) == role:
                hz = pm.get("clock_hz")
                out[ch] = LineTruth("clock", period_s=1 / hz if hz else None, variable=freqhop, nominal=real,
                                    protocol=pr, role=role)
            elif role == "cs":
                out[ch] = LineTruth("select", protocol=pr, role=role)
            else:
                out[ch] = LineTruth("data", protocol=pr, role=role)
    for d in truth.get("decoys") or []:
        k = d["kind"]
        if k in ("clock", "burst_clock"):
            out[d["channel"]] = LineTruth("clock", period_s=1 / d["freq"], role="decoy")
        elif k == "pwm":
            out[d["channel"]] = LineTruth("periodic", period_s=1 / d["freq"], role="decoy")
        elif k == "random":
            out[d["channel"]] = LineTruth("random", role="decoy")
    for ch in cap.channels:
        ref = clean.channel(ch.name) if clean else ch
        if len(ref.edges) < 2:       # nothing of it in this part (a crop): there is no rate to find
            out[ch.name] = LineTruth("static", role=out[ch.name].role if ch.name in out else None)
        out.setdefault(ch.name, LineTruth("static"))
    return out


def input_sha(cap: Capture) -> str:
    h = hashlib.sha256(f"{cap.rate}|{cap.n_samples}".encode())
    for ch in cap.channels:
        h.update(f"|{ch.name}|{ch.initial}|{ch.step}|".encode())
        h.update(np.asarray(ch.edges, np.int64).tobytes())
    return h.hexdigest()[:16]


# ---------------- hardness: what a capture cannot show, from the truth and physics alone ----------------
# Decided before looking at any algorithm's result on a case, and never from one: a bus tagged is still scored (how
# far an algorithm gets is worth knowing), and apart.

UNDER_PERIOD = 4      # a clocked bus under 4 samples a clock period (2 a half): each half seen as one sample or less
UNDER_BIT = 3         # an asynchronous bit under 3 samples: its middle not sampled clear of its edges
NOISE_REACH = 0.5     # noise lasting half a bit (half of a clock's half) or more reaches where a bit is sampled
ALIAS = 2.5           # a periodic line faster than rate / 2.5 shows in the samples as another rate
FREQHOP_MAX = 4       # eval/synth.py's freqhop: frames up to 4 times the bus's clock


def noise_span_s(chain: str, rate0: float, open_drain: bool = False) -> float:
    """How long the chain's noise lasts, in seconds (noise in ticks counted at the rate it was added at); odslow's
    late rises only on an open-drain bus."""
    rate, span = float(rate0), 0.0
    for spec in [x for x in chain.split("+") if x]:
        kind, *a = spec.split(":")
        if kind == "decimate":
            rate /= int(a[0])
        elif kind == "resample":
            rate = float(a[0])
        elif kind in ("pbounce", "pspike"):
            span = max(span, float(a[1]) * 1e-9)
        elif kind == "bounce":
            span = max(span, 7 / rate)           # pulses 1-3 ticks wide within 4 ticks of the edge
        elif kind in ("spike", "xtalk"):
            span = max(span, 1 / rate)
        elif kind == "odslow" and open_drain:
            span = max(span, float(a[0]) * 1e-9)
    return span


def defective_i2cbench(name: str) -> bool:
    """tests/data/i2c_bench/h417_slow: captures through the ESP32-P4's flex glitch filter, which its README records
    locking up or oscillating on these slow edges (hys / nohys with g 25-200, fexp2, lk1 g100, fhys g100 / g1025).
    The pin's fixed filter (fexp g25, pin-*) and no filter read fine."""
    parts = name.split("-")
    tag, g = parts[0], parts[-1] if len(parts) > 2 else ""
    g = g.split("h")[0].lstrip("g") if g.startswith("g") else ""
    if tag in ("hys", "nohys"):
        return g.isdigit() and int(g) > 0
    return tag == "fexp2" or (tag == "lk1" and g == "100") or (tag == "fhys" and g in ("100", "1025"))


CLOCK_ROLES = ("clk", "scl", "swclk", "sck")


def clock_phase_s(cap: Capture, name: str) -> float | None:
    """The shortest phase (high or low) a clock line holds, in seconds: the 2nd percentile of its runs between edges,
    idle gaps (over 20 x the median run) left out. For a bus whose truth has no rate (a real debug bus: RVSWD), read
    from the capture as recorded, before any transform. A clock of uneven duty (V203's fast SWCLK: high about 20 ns of
    100) is as hard to read as its shorter phase."""
    e = np.asarray(cap.channel(name).edges, np.int64)
    if len(e) < 16:
        return None
    runs = np.diff(e)
    runs = runs[runs <= 20 * np.median(runs)]
    return float(np.percentile(runs, 2)) / float(cap.rate) if len(runs) else None


def hardness(truth: dict, cap: Capture, chain: str, rate0: float, orig: Capture | None = None) -> tuple[list, set]:
    """(per truth bus the reasons it is hard to read, the lines that alias). orig: the capture before the transforms
    (a clock's shortest phase where the truth gives no rate)."""
    rate = float(cap.rate)
    gen = truth.get("generator") or {}
    stress = gen.get("stress")
    src = (truth.get("source") or {}).get("path", "")
    defective = "i2c_bench" in src and defective_i2cbench(Path(src).stem if "/stream/" in src
                                                         else Path(src).parent.name)
    tags = []
    for b in truth.get("buses", []):
        pm, t = b.get("params", {}), set()
        span = noise_span_s(chain, rate0, b["protocol"] in OPEN_DRAIN)
        hz = pm.get("clock_hz")
        bauds = [x["baud"] for x in pm.get("baud_segments", [])] or [pm.get("baud") or pm.get("bitrate")]
        if hz:
            fastest = hz * (FREQHOP_MAX if stress == "freqhop" else 1)
            if rate / fastest < UNDER_PERIOD:
                t.add("undersampled")
            if span and span >= NOISE_REACH * (0.5 / fastest):
                t.add("noise_over_bit")
        elif bauds[0]:
            fastest = max(bauds)
            if rate / fastest < UNDER_BIT:
                t.add("undersampled")
            if span and span >= NOISE_REACH / fastest:
                t.add("noise_over_bit")
        elif orig is not None and (clk := next((b["roles"][r] for r in CLOCK_ROLES if r in b.get("roles", {})), None)):
            phase = clock_phase_s(orig, clk)
            if phase:
                if rate * phase < UNDER_PERIOD / 2:
                    t.add("undersampled")
                if span and span >= NOISE_REACH * phase:
                    t.add("noise_over_bit")
        if defective:
            t.add("defective_capture")
        tags.append(sorted(t))
    aliased = {d["channel"] for d in truth.get("decoys") or []
               if d.get("kind") in ("clock", "pwm", "burst_clock") and d.get("freq") and d["freq"] * ALIAS > rate}
    return tags, aliased


def load_case(ref: str, source: str, chain: str) -> Case:
    cap, truth = read(ref)
    if "decimate" not in chain and "resample" not in chain:
        # a reading of this very capture is truth only for the run decimated (linke:): left out at its own rate
        for b in truth["buses"]:
            if (b.get("expect") or {}).pop("dmi_same_run_160M", False):
                b["expect"].pop("dmi", None)
                b["expect_source"] = "; ".join(x for x in b["expect_source"].split("; ") if "160 MS/s" not in x)
                if not b["expect"]:
                    del b["expect"], b["expect_source"]
    rate0, orig = float(cap.rate), cap
    # a generated case is its own clean reference, unless its frozen set already holds noise
    gen = truth.get("generator")
    clean = cap if gen and gen.get("stress") != "glitch" else None
    cap, clean = apply(cap, clean, chain, truth["id"], truth)
    dataset = source + (f"+{chain}" if chain else "")
    tags, aliased = hardness(truth, cap, chain, rate0, orig)
    return Case(truth["id"] + (f"+{chain}" if chain else ""), dataset, cap, clean, truth,
                line_truth(truth, cap, clean or cap), input_sha(cap), tags, aliased)
