"""Iterate evaluation cases: real fixtures plus seeded synthetic scenarios."""

from __future__ import annotations

from pathlib import Path

from . import fixture, synth

ROOT = Path(__file__).resolve().parents[2]
REAL = ROOT / "corpus/fixtures/real"


def real_cases(include_large: bool = False):
    for d in sorted(REAL.iterdir()):
        if not include_large and "flash" in d.name and not d.name.startswith("i2cdb"):
            continue
        yield fixture.load_capture(d), fixture.load_truth(d)


def synth_cases(n: int, profile: str = "mixed", start: int = 0):
    for seed in range(start, start + n):
        yield synth.scenario(seed, profile)
