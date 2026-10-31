"""A WireSkein file as one value: the capture, its parts, their channels (wireskein-format).

    cap = capture.read("c0001.wireskein")       # the header only; a part's channels when first asked for
    cap.tick_hz, cap.layout, cap.complete, cap.lost, cap.meta
    for p in cap.parts:                          # analysis never crosses parts (§3.4)
        p.tick0, p.ticks, p.meta                 # start_ns, start_uncertainty_ns, trigger_tick, ...
        initial, edges = p.channel("SCL").changes()        # ticks from the part's tick 0
        res = wireskein.decode.analyze(p.view())

    clk = capture.logic("CLK", 0, [10, 20, 30], n=1000)     # a line from its changes
    capture.write("out.wireskein", capture.Capture(Fraction(10**6), [capture.Part(1000, [clk])]))

Importing this module, reading, writing and the channels as stored need only the standard library (a test script
without numpy writes and reads captures); numpy is used when a channel is turned into its changes or a part into the
decoders' view.

Channels are kept as the file stores them (wireskein-format §4): a logic line as its samples (`Channel`, encoding
"bits") or as its changes (`Events`, "events"), an analog one as its values (`Analog`). Every logic line reads the
same way, `changes()`: its level before the first tick and the ticks it changes at. A line kept as changes at a
coarser step whose quanta hide changes is read with `Events.decode()` (where they fall in the quantum is not kept).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from fractions import Fraction
from pathlib import Path

from . import _fileformat as fileformat
from ._fileformat import AnalogChannel as Analog
from ._fileformat import Channel
from ._fileformat import EventsChannel as Events
from ._fileformat import analog_raw, analog_volts, events, from_interleaved, pack

__all__ = ["Analog", "Capture", "Channel", "Events", "Part", "analog_raw", "analog_volts", "events",
           "from_interleaved", "logic", "pack", "read", "write"]


def logic(name: str, initial: int, edges, n: int, step: int = 1, phase: int = 0, **acquisition) -> Channel | Events:
    """A logic line from its changes (ticks, increasing; one at tick e makes e the first tick of the new level),
    n samples at phase + k * step: kept as its changes (Events) when that is smaller than its samples (Channel),
    both lossless. Needs numpy."""
    import numpy as np
    from ._engine import fileio
    from ._engine.model import Channel as Edges
    return fileio.to_logic(Edges(name, int(initial), np.asarray(edges, np.int64), step, phase, dict(acquisition)),
                           phase + n * step)


class Part:
    """A block of ticks with no gap inside: tick 0 of its own, `ticks` long, `tick0` ticks after the first part's
    tick 0 (a timeline), its own facts in `meta` (start_ns, start_uncertainty_ns, trigger_tick, boot_id, ...)."""

    def __init__(self, ticks: int, channels: list | None = None, meta: dict | None = None, tick0: int | None = None):
        self.ticks = int(ticks)
        self.meta = dict(meta or {})
        self.tick0 = tick0
        self._channels = None if channels is None else list(channels)
        self._file = None                      # (path, index, header seen from this part)
        self._tick_hz = None                   # the capture's, set when the part joins one

    @classmethod
    def _from_file(cls, path: Path, index: int, head: dict) -> Part:
        p = cls(head["ticks"], None, head.get("meta"), head.get("tick0"))
        p._file = (path, index, head)
        return p

    @property
    def channels(self) -> list[Channel | Events | Analog]:
        """The channels as stored (read from the file on first use); those of an encoding this version does not
        read are left out (`skipped`)."""
        if self._channels is None:
            path, index, _ = self._file
            self._channels = fileformat.read(path, part=index)[1]
        return self._channels

    @property
    def skipped(self) -> list[dict]:
        """Channels of an encoding this version does not read: [{"name", "encoding"}]."""
        return fileformat.skipped(self._file[2]) if self._file else []

    @property
    def names(self) -> list[str]:
        return [c.name for c in self.channels]

    def channel(self, name: str) -> Channel | Events | Analog:
        for c in self.channels:
            if c.name == name:
                return c
        raise KeyError(name)

    def view(self):
        """The part as the decoders and checks read it (wireskein.decode, wireskein.verify). Needs numpy."""
        from ._engine import fileio
        decoded = [fileio.decode(c) for c in self.channels]
        if self._file is not None:
            path, _, head = self._file
            return fileio.view(head, decoded, path)
        if self._tick_hz is None:
            raise ValueError("a part not in a Capture has no tick rate: view it through its Capture")
        tick = Fraction(self._tick_hz)
        return fileio.view({"ticks": self.ticks, "tick_hz": [tick.numerator, tick.denominator], "meta": self.meta},
                           decoded)


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
        self.tick_hz = fileformat._fraction(self.tick_hz)
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


def write(path: str | Path, cap: Capture) -> Path:
    """Write a capture as a WireSkein file."""
    parts = [fileformat.Part(list(p.channels), dict(p.meta), p.tick0) for p in cap.parts]
    return fileformat.write(path, cap.tick_hz, parts=parts, layout=cap.layout, lost=[dict(x) for x in cap.lost] or None,
                            capture_id=cap.id, **cap.meta)
