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

    def __post_init__(self):
        names = [n for n, _ in self.channels] + [n for n, *_ in self.analog]
        if not names or len(set(names)) != len(names):
            raise ValueError(f"channel names must be given and differ: {names}")
        if self.trigger is not None:
            name, kind = self.trigger
            if name not in [n for n, _ in self.channels] or kind not in TRIGGERS:
                raise ValueError(f"trigger {name}:{kind}: the channel must be one captured, the kind one of {', '.join(TRIGGERS)}")


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

_SUFFIX = {"": 1, "k": 10**3, "m": 10**6, "g": 10**9}


def parse_count(text: str) -> int:
    """"20M", "250k", "1000" -> int."""
    m = re.fullmatch(r"\s*(\d+(?:\.\d+)?)\s*([kKmMgG]?)\s*", text)
    if not m:
        raise ValueError(f"not a number: {text!r}")
    return round(float(m.group(1)) * _SUFFIX[m.group(2).lower()])


def parse_channels(text: str) -> list[tuple[str, str]]:
    """"SDA=47,SCL=48" (name=id) or "D0,D1" (the id is also the name)."""
    out = []
    for part in filter(None, (p.strip() for p in text.split(","))):
        name, _, cid = part.partition("=")
        out.append((name.strip(), (cid or name).strip()))
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
