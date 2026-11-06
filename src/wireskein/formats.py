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

Installed packages declare formats in the entry point group "wireskein.formats" (NAME = "module:object"); a script
calls register(format).
"""

from __future__ import annotations

import re
from pathlib import Path

__all__ = ["kind", "read", "register", "registered", "unregister"]

_REGISTERED: dict[str, object] = {}
_LOADED = False
BUILTIN = ("wireskein", "sr", "vcd")


def register(fmt) -> None:
    """Add a format (see the module's docstring for its shape)."""
    name = getattr(fmt, "name", None)
    if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]*", name):
        raise ValueError(f"a format needs a name of letters, digits and _: {name!r}")
    if name in BUILTIN:
        raise ValueError(f"{name!r} is a format WireSkein reads itself")
    for k in ("sniff", "read"):
        if not callable(getattr(fmt, k, None)):
            raise ValueError(f"{name}: {k} is missing")
    _REGISTERED[name] = fmt


def unregister(name: str) -> None:
    _REGISTERED.pop(name, None)


def registered() -> dict[str, object]:
    """The formats added: by register, and by installed packages (entry point group "wireskein.formats")."""
    global _LOADED
    if not _LOADED:
        _LOADED = True
        from importlib.metadata import entry_points
        for ep in entry_points(group="wireskein.formats"):
            obj = ep.load()
            if not getattr(obj, "name", None):
                try:
                    obj.name = ep.name
                except AttributeError:
                    pass
            register(obj)
    return dict(_REGISTERED)


def kind(path: str | Path) -> str | None:
    """What a file is: "wireskein", "sr", "vcd", the name of a format added, or None."""
    from . import _fileformat as fileformat
    k = fileformat.sniff(path)
    if k is not None:
        return k
    fmts = registered()
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
    fmt = registered().get(name)
    if fmt is None:
        raise ValueError(f"{path}: no format added reads it")
    cap = fmt.read(Path(path))
    from .capture import Capture
    if not isinstance(cap, Capture):
        raise TypeError(f"{name}.read must return a wireskein.capture.Capture, not {type(cap).__name__}")
    return cap
