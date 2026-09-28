"""Edge-list representation of a digital capture.

A channel is its level at sample 0 plus the sample indices where the level
toggles. The level at sample s is initial ^ (number of edges <= s) & 1, so an
edge at index e means sample e is the first sample of the new level.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np


@dataclass
class Channel:
    name: str
    initial: int
    edges: np.ndarray  # int64, strictly increasing

    def level_at(self, samples: np.ndarray) -> np.ndarray:
        n = np.searchsorted(self.edges, samples, side="right")
        return (self.initial ^ (n & 1)).astype(np.int8)

    def runs(self, n_samples: int) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        """Return (start, length, level) of each constant-level run, including the
        partial first and last runs."""
        bounds = np.concatenate(([0], self.edges, [n_samples]))
        start = bounds[:-1]
        length = np.diff(bounds)
        level = (self.initial ^ (np.arange(len(start)) & 1)).astype(np.int8)
        return start, length, level


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


def edges_from_dense(bits: np.ndarray) -> tuple[int, np.ndarray]:
    """bits: uint8 array of 0/1 per sample."""
    if len(bits) == 0:
        return 0, np.zeros(0, dtype=np.int64)
    change = np.flatnonzero(bits[1:] != bits[:-1]) + 1
    return int(bits[0]), change.astype(np.int64)
