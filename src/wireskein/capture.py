"""A WireSkein file as one value: the capture, its parts, their channels (wireskein-format).

    cap = capture.read("c0001.wireskein")       # the header only; channels are read when asked for
    cap.tick_hz, cap.layout, cap.complete, cap.lost, cap.meta
    for p in cap.parts:                          # analysis never crosses parts (§3.4)
        p.tick0, p.ticks, p.meta                 # start_ns, start_uncertainty_ns, trigger_tick, ...
        scl = p.channel("SCL")                   # Channel: initial, edges (ticks from the part's tick 0)
        res = wireskein.analyze.analyze(p.view())

    capture.write("out.wireskein", capture.Capture(Fraction(10**6), [capture.Part(1000, [capture.Channel(...)])]))

A logic line is read as its changes (`Channel`: the level before the part and the ticks it changes at), whatever the
file keeps: the samples (`bits`) are decoded when the channel is asked for, so a long fast line costs nothing until
then. A line kept as changes at a coarser step (`events`, step > 1) is an `Events`: in each listed quantum it changed
`counts` times and ended at `levels`; where in the quantum is not kept. Analog values are `Analog` (raw with their
linear conversion, or volts).

Writing chooses each logic line's encoding by size: a step-1 line as its changes when they are smaller than its
samples, else as samples (both lossless).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from fractions import Fraction
from pathlib import Path

from . import fileformat
from ._engine import fileio
from ._engine.model import AnalogTrace as Analog
from ._engine.model import Capture as View
from ._engine.model import Channel
from ._engine.model import EventTrace as Events

__all__ = ["Analog", "Capture", "Channel", "Events", "Part", "View", "read", "write"]


class Part:
    """A block of ticks with no gap inside: tick 0 of its own, `ticks` long, `tick0` ticks after the first part's
    tick 0 (a timeline), its own facts in `meta` (start_ns, start_uncertainty_ns, trigger_tick, boot_id, ...)."""

    def __init__(self, ticks: int, channels: list | None = None, meta: dict | None = None, tick0: int | None = None):
        self.ticks = int(ticks)
        self.meta = dict(meta or {})
        self.tick0 = tick0
        self._decoded = {c.name: c for c in channels or []}
        self._names = [c.name for c in channels or []]
        self._file = None                      # (path, index, header seen from this part, stored channels or None)
        self._tick_hz = None                   # the capture's, set when the part joins one

    @classmethod
    def _from_file(cls, path: Path, index: int, head: dict) -> Part:
        p = cls(head["ticks"], None, head.get("meta"), head.get("tick0"))
        p._names = [c["name"] for c in head["channels"]]
        p._file = [path, index, head, None]
        return p

    @property
    def names(self) -> list[str]:
        return list(self._names)

    def _stored(self) -> dict:
        if self._file[3] is None:
            path, index, _, _ = self._file
            _, chans = fileformat.read(path, part=index)
            self._file[3] = {c.name: c for c in chans}
        return self._file[3]

    def channel(self, name: str) -> Channel | Events | Analog:
        """One channel, decoded on first use."""
        if name not in self._decoded:
            if self._file is None or name not in self._names:
                raise KeyError(name)
            stored = self._stored()
            if name not in stored:
                raise KeyError(f"{name}: its encoding is not one this version reads")
            self._decoded[name] = fileio.decode(stored[name])
        return self._decoded[name]

    @property
    def channels(self) -> list:
        """Every channel this version reads, decoded."""
        if self._file is not None:
            return [self.channel(n) for n in self._names if n in self._stored()]
        return [self._decoded[n] for n in self._names]

    def view(self) -> View:
        """The part as the decoders and checks read it (wireskein.analyze, wireskein.verify)."""
        if self._file is not None:
            path, _, head, _ = self._file
            return fileio.view(head, self.channels, path)
        if self._tick_hz is None:
            raise ValueError("a part not in a Capture has no tick rate: view it through its Capture")
        tick = Fraction(self._tick_hz)
        return fileio.view({"ticks": self.ticks, "tick_hz": [tick.numerator, tick.denominator], "meta": self.meta},
                           self.channels)


@dataclass
class Capture:
    """The ticks of one clock (`tick_hz`, exact), cut into parts. `lost` says what was lost while capturing: a
    capture with a loss is a failure (§3.4), its parts kept for what they show."""
    tick_hz: Fraction
    parts: list[Part]
    layout: str = "timeline"
    lost: list[dict] = field(default_factory=list)
    meta: dict = field(default_factory=dict)
    id: str | None = None
    path: Path | None = None

    def __post_init__(self):
        self.tick_hz = Fraction(self.tick_hz)
        for p in self.parts:
            p._tick_hz = self.tick_hz

    @property
    def complete(self) -> bool:
        return not self.lost


def read(path: str | Path) -> Capture:
    """A WireSkein file's capture. The header is read now; each part's channels when first asked for."""
    path = Path(path)
    raw = fileformat.read_header(path)
    parts = [Part._from_file(path, k, fileformat.part_view(raw, k)) for k in range(len(raw["parts"]))]
    return Capture(fileformat.tick_hz(raw), parts, raw.get("layout", "timeline"), list(raw.get("lost", [])),
                   dict(raw.get("meta", {})), raw.get("id"), path)


def _stored(c, ticks: int):
    if isinstance(c, Channel):
        return fileio.to_logic(c, ticks)
    if isinstance(c, Events):
        return fileio.to_events(c)
    if isinstance(c, Analog):
        return fileio.to_analog(c)
    return c                                     # already a stored channel (fileformat)


def write(path: str | Path, cap: Capture) -> Path:
    """Write a capture as a WireSkein file. A part read from a file and never decoded is copied as it is stored."""
    parts = []
    for p in cap.parts:
        if p._file is not None and len(p._decoded) == 0:
            chans = [p._stored()[n] for n in p._names if n in p._stored()]
        else:
            chans = [_stored(c, p.ticks) for c in p.channels]
        parts.append(fileformat.Part(chans, dict(p.meta), p.tick0))
    return fileformat.write(path, cap.tick_hz, parts=parts, layout=cap.layout, lost=[dict(x) for x in cap.lost] or None,
                            capture_id=cap.id, **cap.meta)
