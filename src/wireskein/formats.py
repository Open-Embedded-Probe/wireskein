"""File formats of others that WireSkein reads, added from outside (a logic analyzer's export, a lab's own log).

A format is an object with

    name     its name ("saleae_csv"): letters, digits and "_", not one WireSkein reads itself
    sniff(head: bytes, path) -> bool
             whether the file is one of it, from its first 4096 bytes (and its name, when the content cannot say)
    read(path) -> wireskein.capture.Capture
             the capture, built with wireskein.capture (capture.logic, analog_volts, Part, Capture)

WireSkein's own files, sigrok sessions and VCDs are told apart first; a file none of them is goes to the formats
added, in the order they were. Everything that reads a capture (analyze, decode, states, convert, the viewer) then
reads it: `wireskein convert FILE out.wireskein` keeps it as a WireSkein file. A format of several parts is read a
part at a time (`--part`), as a WireSkein file's.

Formats are added with wireskein.plugins (register_format, or the entry point group "wireskein.formats").
"""

from __future__ import annotations

from pathlib import Path

__all__ = ["kind", "read"]


def kind(path: str | Path) -> str | None:
    """What a file is: "wireskein", "sr", "vcd", the name of a format added, or None."""
    from . import _fileformat as fileformat
    k = fileformat.sniff(path)
    if k is not None:
        return k
    from . import plugins
    fmts = plugins.formats()
    if not fmts:
        return None
    try:
        with open(path, "rb") as f:
            head = f.read(4096)
    except OSError:
        return None
    for name, fmt in fmts.items():
        try:
            if fmt.sniff(head, Path(path)):
                return name
        except Exception:  # noqa: BLE001 - another's sniff failing is no match
            continue
    return None


def read(path: str | Path, name: str | None = None):
    """A file of a format added, as a wireskein.capture.Capture."""
    name = name or kind(path)
    from . import plugins
    fmt = plugins.formats().get(name)
    if fmt is None:
        raise ValueError(f"{path}: no format added reads it")
    cap = fmt.read(Path(path))
    from .capture import Capture
    if not isinstance(cap, Capture):
        raise TypeError(f"{name}.read must return a wireskein.capture.Capture, not {type(cap).__name__}")
    return cap
