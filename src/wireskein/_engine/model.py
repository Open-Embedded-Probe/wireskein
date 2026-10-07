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
class IntervalTrace:
    """A logic line kept as one value per interval of `step` ticks (interval k: ticks [phase + k * step,
    phase + (k + 1) * step)); see fileformat.IntervalChannel. kind "any": values 0/1, `active` if the line was
    active at any tick of the interval. kind "latch": values 0-3, bit 0 the level at the interval's last tick, bit 1
    a change to `active` inside it (the first tick of the channel compared with nothing). Where in the interval,
    and how many times, is not kept."""
    name: str
    values: np.ndarray       # uint8
    step: int
    phase: int = 0
    kind: str = "any"        # "any" | "latch"
    active: int = 1
    acquisition: dict = field(default_factory=dict)

    @property
    def encoding(self) -> str:
        return f"interval-{self.kind}"

    @property
    def end(self) -> int:
        return self.phase + len(self.values) * self.step


@dataclass
class Capture:
    rate: float
    n_samples: int
    channels: list[Channel]
    meta: dict = field(default_factory=dict)
    analog: list[AnalogTrace] = field(default_factory=list)
    intervals: list[IntervalTrace] = field(default_factory=list)

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
