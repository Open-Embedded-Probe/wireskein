"""The evaluation cases: the real records and the frozen generated sets (corpus/), and seeded generated scenarios.

A case on disk is <id>.wireskein (anonymized: channels D0..Dn in a shuffled order) beside <id>.truth.json (the ground
truth, read only by the evaluator)."""

from __future__ import annotations

import json
from pathlib import Path

from wireskein import capture

import synth

ROOT = Path(__file__).resolve().parent
REAL = ROOT / "corpus" / "real"
SETS = ROOT / "corpus" / "synth"
WORK = ROOT / "work"


def cases(directory: Path) -> list[Path]:
    return sorted(directory.glob("*.wireskein"))


def load(path: Path):
    """(the capture as the decoders read it, its truth)."""
    return capture.read(path).parts[0].view(), json.loads(path.with_suffix("").with_suffix(".truth.json").read_text())


def large(path: Path) -> bool:
    """The real records with about a million edges (RVSWD / SWIO flash writes)."""
    return "flash" in path.name and not path.name.startswith("i2cdb")


def real_cases(include_large: bool = False):
    for p in cases(REAL):
        if include_large or not large(p):
            yield load(p)


def synth_cases(n: int, profile: str = "mixed", start: int = 0):
    for seed in range(start, start + n):
        yield synth.scenario(seed, profile)
