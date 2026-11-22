"""Definition files (protocols, device packs, marker dialects; docs/protocols.md §5): each says what it is and which
version of its shape in `format` at its top, and a reader refuses one it does not know."""

from __future__ import annotations

import tomllib
from pathlib import Path

FORMATS = {"protocol": "wireskein-protocol/1", "device": "wireskein-device/1",
           "marker-dialect": "wireskein-marker-dialect/1"}


def read(path: Path, kind: str, text: str | None = None) -> dict:
    """A definition file of this kind, its format checked."""
    d = tomllib.loads(text if text is not None else Path(path).read_text())
    want = FORMATS[kind]
    got = d.get("format")
    if got != want:
        name = Path(path).name
        if got is None:
            raise ValueError(f"{name}: no format: a {kind} definition starts with format = {want!r}")
        raise ValueError(f"{name}: format {got!r}, this WireSkein reads {want!r}")
    return d
