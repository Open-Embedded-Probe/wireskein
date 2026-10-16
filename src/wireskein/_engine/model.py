"""Edge-list representation of a digital capture.

Time is counted in ticks of one clock per capture (`Capture.rate`, ticks per
second; `n_samples` is the length in ticks). A channel is its level at tick 0
plus the ticks where the level toggles. The level at tick s is
initial ^ (number of edges <= s) & 1, so an edge at e means tick e is the first
tick of the new level.

A channel sampled slower than the tick clock (a probe that decimates some
channels to fit its link) has `step` ticks per sample, its samples at ticks
phase + k * step. Its edges fall on those ticks, and each one happened somewhere
in the step ticks before it: one sample of that channel is `step` ticks wide.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from fractions import Fraction

import numpy as np


@dataclass
class Channel:
    name: str
    initial: int
    edges: np.ndarray  # int64, strictly increasing
    step: int = 1      # ticks per sample of this channel
    phase: int = 0     # tick of its first sample
    acquisition: dict = field(default_factory=dict)   # how it was taken (pin, ...), kept through files
    kept_as: str = "bits"   # how the file held it ("bits" or "events"): a rewrite keeps the form

    def level_at(self, samples: np.ndarray) -> np.ndarray:
        from . import kernels
        return kernels.level_at(self.edges, self.initial, np.asarray(samples))

    def runs(self, n_samples: int) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        """Return (start, length, level) of each constant-level run, including the
        partial first and last runs."""
        from . import kernels
        return kernels.runs(self.edges, self.initial, n_samples)


@dataclass
class AnalogTrace:
    """An analog channel: values at their own rate. Sample k is at tick
    t0_ticks + k * tick_hz / rate_hz (tick_hz: the capture's rate). values are
    raw integers (encoding "analog") or volts ("analog-f32")."""
    name: str
    values: np.ndarray
    rate_hz: Fraction
    t0_ticks: Fraction = Fraction(0)
    encoding: str = "analog"
    width: int = 16
    value_bits: int | None = None
    zero: float | None = None
    scale_nv: float | None = None
    unit: str = "V"
    acquisition: dict = field(default_factory=dict)

    def ticks(self, tick_hz) -> np.ndarray:
        """The tick of each sample (float)."""
        per = float(Fraction(tick_hz) / self.rate_hz)
        return float(self.t0_ticks) + np.arange(len(self.values)) * per

    def volts(self) -> np.ndarray | None:
        if self.encoding == "analog-f32":
            return self.values.astype(np.float64)
        if self.zero is None or self.scale_nv is None:
            return None
        return (self.values.astype(np.float64) - self.zero) * self.scale_nv * 1e-9


@dataclass
class EventTrace:
    """A logic line kept as its changes at a resolution of `step` ticks (fileformat.EventsChannel, step > 1): quantum
    k is ticks [phase + k * step, phase + (k + 1) * step); the line is at start_level at tick `phase`, and in each
    quantum listed it changed counts[i] times (127: at least) and ended at levels[i]. Where in the quantum is not kept.
    When no quantum holds more than one change, the line is also a Channel of that step (as_channel), so the
    decoders read it at that resolution."""
    name: str
    step: int
    phase: int
    n: int                   # quanta covered
    start_level: int
    quanta: np.ndarray       # int64, increasing
    levels: np.ndarray       # uint8, the level at each listed quantum's last tick
    counts: np.ndarray       # uint8, 1-127
    acquisition: dict = field(default_factory=dict)

    encoding = "events"

    @property
    def end(self) -> int:
        return self.phase + self.n * self.step

    @property
    def hidden(self) -> int:
        """Quanta holding more than one change (where those changes fall is not kept)."""
        return int(np.count_nonzero(self.counts > 1))

    def as_channel(self) -> Channel | None:
        """The line as a Channel of this step, when every quantum holds at most one change: the edge of quantum k at
        its last tick, so it happened somewhere in the step ticks before (the Channel convention). None otherwise."""
        if self.hidden:
            return None
        edges = self.phase + self.quanta.astype(np.int64) * self.step + (self.step - 1)
        return Channel(self.name, int(self.start_level), edges, self.step, self.phase + self.step - 1,
                       dict(self.acquisition))


@dataclass
class Capture:
    rate: float
    n_samples: int
    channels: list[Channel]
    meta: dict = field(default_factory=dict)
    analog: list[AnalogTrace] = field(default_factory=list)
    events: list[EventTrace] = field(default_factory=list)   # lines kept as changes at step > 1

    def channel(self, name: str) -> Channel:
        for ch in self.channels:
            if ch.name == name:
                return ch
        raise KeyError(name)

    @property
    def duration(self) -> float:
        return self.n_samples / self.rate


def edges_from_dense(bits: np.ndarray, step: int = 1, phase: int = 0) -> tuple[int, np.ndarray]:
    """bits: uint8 array of 0/1 per sample of a channel whose samples are at
    ticks phase + k * step. Returns (initial level, edge ticks)."""
    if len(bits) == 0:
        return 0, np.zeros(0, dtype=np.int64)
    change = np.flatnonzero(bits[1:] != bits[:-1]) + 1
    return int(bits[0]), (phase + change.astype(np.int64) * step)
