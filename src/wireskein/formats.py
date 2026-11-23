"""What a capture file is, by its content: "wireskein", "sr" or "vcd" (another format is converted to a WireSkein
file outside, with wireskein.capture: docs/extending.md)."""

from __future__ import annotations

from pathlib import Path

__all__ = ["kind"]


def kind(path: str | Path) -> str | None:
    """What a file is: "wireskein", "sr", "vcd", or None."""
    from . import _fileformat as fileformat
    return fileformat.sniff(path)
