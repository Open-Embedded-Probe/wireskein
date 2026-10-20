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
ANALOG_TRIGGERS = ("rise", "fall", "high", "low")   # crossing up / down, staying above / below a voltage


@dataclass
class Request:
    channels: list[tuple[str, str]]          # logic: (name to record, the device's channel id), in this order
    rate: int                                # logic samples per second asked for (the device may answer another)
    samples: int
    trigger: tuple | None = None             # (logic channel, one of TRIGGERS) or (analog channel, rise / fall / high /
                                             # low, volts); None: start at once
    pretrigger: int | None = None            # samples kept before the trigger
    arm: str = "pretrigger"                  # when the trigger is looked for: "pretrigger" (once it is there: the
                                             # samples before the trigger are always that many) or "now" (at once:
                                             # an early trigger leaves fewer)
    timeout: float = 10.0                    # seconds to wait for the capture to finish
    analog: list[tuple[str, str, int | None]] = field(default_factory=list)   # (name, channel id, input range / frontend)
    analog_rate: int | None = None           # analog samples per second (default: the source's choice or `rate`)
    analog_samples: int | None = None        # default: as long as the logic capture
    # logic channels kept as their changes (OEP capture's format 1 events): name -> D, the resolution in samples
    # (1: every change exact; D > 1: per D samples, how many times it changed and the level after)
    resolution: dict[str, int] = field(default_factory=dict)
    mode: str = "oneshot"                    # "oneshot" (samples, in the probe's memory) or "stream" (for duration
    duration: float | None = None            # seconds, sent as it is taken: longer than the probe's memory)
    on_loss: str = "stop"                    # stream: "stop" at the first loss (the default), or "continue" to the end;
                                             # either way a loss makes the capture a failure (wireskein-format §3.4)
    spool_dir: str | None = None             # stream: where its channels are put together on disk while it runs
                                             # (capture() puts them by the output; not /tmp, which may be memory)

    def __post_init__(self):
        names = [n for n, _ in self.channels] + [n for n, *_ in self.analog]
        if not names or len(set(names)) != len(names):
            raise ValueError(f"channel names must be given and differ: {names}")
        if self.trigger is not None:
            name, kind, *volts = self.trigger
            if name in [n for n, *_ in self.analog]:
                if kind not in ANALOG_TRIGGERS or len(volts) != 1:
                    raise ValueError(f"trigger {name}:{kind}: an analog channel takes {'/'.join(ANALOG_TRIGGERS)} and "
                                     f"the volts it crosses or stays beyond ({name}:fall:2.9)")
            elif name not in [n for n, _ in self.channels] or kind not in TRIGGERS or volts:
                raise ValueError(f"trigger {name}:{kind}: the channel must be one captured, the kind one of {', '.join(TRIGGERS)} "
                                 f"(volts only for an analog channel)")
        if self.mode not in ("oneshot", "stream") or self.on_loss not in ("stop", "continue"):
            raise ValueError(f"mode {self.mode!r} / on_loss {self.on_loss!r}: oneshot or stream / stop or continue")
        if self.mode == "stream" and not (self.duration and self.duration > 0):
            raise ValueError("a stream needs a duration (seconds)")
        if self.arm not in ("pretrigger", "now"):
            raise ValueError(f"arm {self.arm!r}: pretrigger or now")
        logic = {n for n, _ in self.channels}
        for name, d in self.resolution.items():
            if name not in logic or d < 1:
                raise ValueError(f"{name}/{d}: a resolution is for a logic channel captured, 1 or more samples")


@dataclass
class Result:
    tick_hz: Fraction
    channels: list[fileformat.Channel]
    meta: dict = field(default_factory=dict)
    parts: list[fileformat.Part] | None = None   # instead of channels: a capture in parts (wireskein-format §3.4)
    lost: list[dict] = field(default_factory=list)   # what was lost while capturing (a failure)
    layout: str = "timeline"
    temp_dir: str | None = None                  # files the channels are read from, removed once written

    @property
    def complete(self) -> bool:
        return not self.lost


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
    import dataclasses
    import shutil
    out = Path(out)
    if request.mode == "stream" and request.spool_dir is None:
        request = dataclasses.replace(request, spool_dir=str(out.resolve().parent))
    res = run(source, request)
    try:
        return _save(res, out, attachments, meta)
    finally:
        if res.temp_dir:
            shutil.rmtree(res.temp_dir, ignore_errors=True)


def _save(res: "Result", out: Path, attachments, meta) -> Path:
    tmp = out if out.suffix != ".sr" else out.with_name(out.name + fileformat.SUFFIX)
    if res.parts is not None:
        fileformat.write(tmp, res.tick_hz, None, attachments, parts=res.parts, layout=res.layout, lost=res.lost or None,
                         **{**res.meta, **meta})
    else:
        fileformat.write(tmp, res.tick_hz, res.channels, attachments, lost=res.lost or None, **{**res.meta, **meta})
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


def _parts(text: str):
    for part in filter(None, (p.strip() for p in text.split(","))):
        name, _, cid = part.partition("=")
        cid, _, d = (cid or name).strip().partition("/")
        yield name.strip(), cid.strip(), d.strip()


def parse_channels(text: str) -> list[tuple[str, str]]:
    """"SDA=47,SCL=48" (name=id) or "D0,D1" (the id is also the name). A resolution after the id (parse_resolution)
    is left out here."""
    return [(name, cid) for name, cid, _ in _parts(text)]


def parse_resolution(text: str) -> dict[str, int]:
    """The logic channels kept as their changes (Request.resolution), written after the id: "CS=5/64" (per 64
    samples, how many times it changed and the level after), "EN=7/1" (every change, exact)."""
    out = {}
    for name, _, d in _parts(text):
        if d:
            try:
                out[name] = parse_count(d)
            except ValueError:
                raise ValueError(f"{name}=.../{d}: the resolution is a number of samples (CS=5/64)") from None
    return out


def parse_analog(text: str) -> list[tuple[str, str, int | None]]:
    """"VBUS=16,SINE=17@3" -> [("VBUS", "16", None), ("SINE", "17", 3)] (@N: the input range / frontend)."""
    out = []
    for name, cid in parse_channels(text):
        cid, _, fe = cid.partition("@")
        out.append((name, cid, int(fe) if fe else None))
    return out


def parse_trigger(text: str) -> tuple:
    """"SDA:fall" -> ("SDA", "fall"); "VDD:fall:2.9" (or 2.9V) -> ("VDD", "fall", 2.9), an analog channel's voltage."""
    parts = text.split(":")
    if len(parts) == 3:
        try:
            return parts[0], parts[1], float(parts[2].rstrip("Vv"))
        except ValueError:
            raise ValueError(f"trigger {text}: NAME:KIND:VOLTS (VDD:fall:2.9)") from None
    name, _, kind = text.rpartition(":")
    return name, kind
