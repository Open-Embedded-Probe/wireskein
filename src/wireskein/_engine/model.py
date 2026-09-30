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

import numpy as np


@dataclass
class Channel:
    name: str
    initial: int
    edges: np.ndarray  # int64, strictly increasing
    step: int = 1      # ticks per sample of this channel
    phase: int = 0     # tick of its first sample

    def level_at(self, samples: np.ndarray) -> np.ndarray:
        from . import kernels
        return kernels.level_at(self.edges, self.initial, np.asarray(samples))

    def runs(self, n_samples: int) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        """Return (start, length, level) of each constant-level run, including the
        partial first and last runs."""
        from . import kernels
        return kernels.runs(self.edges, self.initial, n_samples)


@dataclass
class Capture:
    rate: float
    n_samples: int
    channels: list[Channel]
    meta: dict = field(default_factory=dict)

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
