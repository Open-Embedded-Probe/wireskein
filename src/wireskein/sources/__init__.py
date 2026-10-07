"""Capturing from a device into a .wireskein (logic channels).

A source turns a Request (channel names and the device's channel ids, rate,
samples, trigger) into a Result (tick clock, fileformat.Channels, metadata). The
built-in sources:

    oep:<target>      an OEP probe through oep-client-python (pip install "wireskein[oep]");
                      <target> as oep-client opens it: a serial port, tcp://HOST:PORT (a broker), usb[:VID:PID[:SERIAL]]
    sigrok:<driver>   a device sigrok supports, through sigrok-cli (e.g. sigrok:fx2lafw, sigrok:demo,
                      sigrok:dreamsourcelab-dslogic:conn=1.6)

Other packages add sources through the entry point group "wireskein.sources"
(name = module or object with capture(target: str, request: Request) -> Result).

    from wireskein import sources
    req = sources.Request([("SDA", "47"), ("SCL", "48")], rate=20_000_000, samples=200_000)
    sources.capture("oep:/dev/ttyACM0", req, "i2c.wireskein")
"""

from __future__ import annotations

import importlib
import re
from dataclasses import dataclass, field
from fractions import Fraction
from importlib.metadata import entry_points
from pathlib import Path

from .. import fileformat

BUILTIN = {"oep": "wireskein.sources.oep", "sigrok": "wireskein.sources.sigrok"}
TRIGGERS = ("rise", "fall", "both", "high", "low")
POLICIES = ("sample", "any", "latch")     # how a reduced logic channel keeps the line (OEP multirate)


@dataclass
class Request:
    channels: list[tuple[str, str]]          # logic: (name to record, the device's channel id), in this order
    rate: int                                # logic samples per second asked for (the device may answer another)
    samples: int
    trigger: tuple[str, str] | None = None   # (logic channel name, one of TRIGGERS); None: start at once
    pretrigger: int | None = None            # samples kept before the trigger
    timeout: float = 10.0                    # seconds to wait for the capture to finish
    analog: list[tuple[str, str, int | None]] = field(default_factory=list)   # (name, channel id, input range / frontend)
    analog_rate: int | None = None           # analog samples per second (default: the source's choice or `rate`)
    analog_samples: int | None = None        # default: as long as the logic capture
    # logic channels kept at fewer values than the rate (a probe's multirate): name -> (policy, d, param).
    # "sample": the level every d samples, from sample param (the phase); "any": per d samples, whether the line was at
    # level param (the active level) at any of them; "latch": per d samples, the level at the last one and whether it
    # went to level param inside. rate, samples, pretrigger stay in samples of the rate (base samples).
    reduce: dict[str, tuple[str, int, int]] = field(default_factory=dict)

    def __post_init__(self):
        names = [n for n, _ in self.channels] + [n for n, *_ in self.analog]
        if not names or len(set(names)) != len(names):
            raise ValueError(f"channel names must be given and differ: {names}")
        if self.trigger is not None:
            name, kind = self.trigger
            if name not in [n for n, _ in self.channels] or kind not in TRIGGERS:
                raise ValueError(f"trigger {name}:{kind}: the channel must be one captured, the kind one of {', '.join(TRIGGERS)}")
        logic = [n for n, _ in self.channels]
        for name, (policy, d, param) in self.reduce.items():
            if name not in logic or policy not in POLICIES or d < 1:
                raise ValueError(f"{name}: reduce {policy}/{d}: a logic channel captured, one of {', '.join(POLICIES)}, "
                                 f"d >= 1")
            if policy == "sample" and not 0 <= param < d or policy != "sample" and (d < 2 or param not in (0, 1)):
                raise ValueError(f"{name}: reduce {policy}/{d} with {param}: sample takes a phase below d; "
                                 f"any and latch take d >= 2 and the active level 0 or 1")


@dataclass
class Result:
    tick_hz: Fraction
    channels: list[fileformat.Channel]
    meta: dict = field(default_factory=dict)


def _module(scheme: str):
    if scheme in BUILTIN:
        return importlib.import_module(BUILTIN[scheme])
    for ep in entry_points(group="wireskein.sources"):
        if ep.name == scheme:
            return ep.load()
    known = sorted({*BUILTIN, *(ep.name for ep in entry_points(group="wireskein.sources"))})
    raise ValueError(f"unknown source {scheme!r} (known: {', '.join(known)})")


def run(source: str, request: Request) -> Result:
    """source: "<scheme>:<target>", e.g. "oep:/dev/ttyACM0", "sigrok:fx2lafw"."""
    scheme, _, target = source.partition(":")
    res = _module(scheme).capture(target, request)
    res.meta = {"source": source, "requested_rate": request.rate, **res.meta}
    return res


def capture(source: str, request: Request, out: str | Path, attachments: dict | None = None, **meta) -> Path:
    """Capture and save to `out`: a WireSkein file, or a .sr by that name (slow channels repeated)."""
    res = run(source, request)
    out = Path(out)
    tmp = out if out.suffix != ".sr" else out.with_name(out.name + fileformat.SUFFIX)
    fileformat.write(tmp, res.tick_hz, res.channels, attachments, **{**res.meta, **meta})
    if tmp != out:
        from ..analyze import load, save
        try:
            save(out, load(tmp), **{**res.meta, **meta})
        finally:
            tmp.unlink()
    return out


# ---------------- command-line helpers ----------------

_SUFFIX = {"": 1, "k": 10**3, "K": 10**3, "M": 10**6, "G": 10**9}


def parse_count(text: str) -> int:
    """"20M", "250k", "1000" -> int. Only k / K, M and G: a lowercase m would read as milli."""
    m = re.fullmatch(r"\s*(\d+(?:\.\d+)?)\s*([a-zA-Z]?)\s*", text)
    if not m or m.group(2) not in _SUFFIX:
        hint = " (M for mega)" if m and m.group(2) == "m" else ""
        raise ValueError(f"not a count: {text!r}{hint}; use a number with k, M or G")
    return round(float(m.group(1)) * _SUFFIX[m.group(2)])


_REDUCE = re.compile(r"(?:(?:sample)?/(\d+)(?:\+(\d+))?|(any|latch)-(low|high)/(\d+))")


def _split(part: str) -> tuple[str, str, str]:
    name, _, cid = part.partition("=")
    cid = (cid or name).strip()
    k = min([i for i in (cid.find("/"), cid.find(":")) if i >= 0], default=len(cid))
    return name.strip(), cid[:k], cid[k:].lstrip(":")


def parse_channels(text: str) -> list[tuple[str, str]]:
    """"SDA=47,SCL=48" (name=id) or "D0,D1" (the id is also the name). A reduction after the id (parse_reduce) is
    left out here."""
    return [_split(p)[:2] for p in filter(None, (p.strip() for p in text.split(",")))]


def parse_reduce(text: str) -> dict[str, tuple[str, int, int]]:
    """The logic channels to reduce (Request.reduce), written after the id:
    "CLK=3/4" (the level every 4 samples), "CLK=3/4+1" (from sample 1), "CS=5:any-low/32" (per 32 samples, whether it
    was low at any), "IRQ=7:latch-high/8" (per 8 samples, the last level and whether it went high)."""
    out = {}
    for part in filter(None, (p.strip() for p in text.split(","))):
        name, _, spec = _split(part)
        if not spec:
            continue
        m = _REDUCE.fullmatch(spec)
        if not m:
            raise ValueError(f"{part}: not a reduction (ID/D, ID/D+PHASE, ID:any-low/D, ID:any-high/D, "
                             f"ID:latch-low/D or ID:latch-high/D)")
        if m.group(1):
            out[name] = ("sample", int(m.group(1)), int(m.group(2) or 0))
        else:
            out[name] = (m.group(3), int(m.group(5)), int(m.group(4) == "high"))
    return out


def parse_analog(text: str) -> list[tuple[str, str, int | None]]:
    """"VBUS=16,SINE=17@3" -> [("VBUS", "16", None), ("SINE", "17", 3)] (@N: the input range / frontend)."""
    out = []
    for name, cid in parse_channels(text):
        cid, _, fe = cid.partition("@")
        out.append((name, cid, int(fe) if fe else None))
    return out


def parse_trigger(text: str) -> tuple[str, str]:
    """"SDA:fall" -> ("SDA", "fall")."""
    name, _, kind = text.rpartition(":")
    return name, kind
