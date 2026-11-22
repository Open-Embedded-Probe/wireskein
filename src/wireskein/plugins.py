"""Extending WireSkein from outside: the one place to add protocols, definitions, file formats, check kinds and
capture sources (docs/extending.md).

What each addition is:

- a protocol (register_protocol): an object with
      name     its name ("acme_link")
      over     the buses it rides on: a tuple of "uart", "spi", "i2c", "uart_pair", "can", "lin", "dmx512", or the
               name of a protocol a definitions directory adds (it gets that one's frames)
      decode(bus) -> {"messages": [...], "checks_passed": int, "checks_failed": int}
  `bus` is the bus as the analysis exports it (wireskein-analysis/1, docs/analysis-format.md §4). The checks its
  messages pass and fail (a checksum, a length, a sequence number) are the evidence: the analysis gives the bus more
  confidence by them, as it does for Modbus RTU; with no check passed it shows nothing (docs/protocols.md §1-§4).
- a definitions directory (add_definitions): laid out as WireSkein's own decl/: *.toml at its top are protocols on
  a bit layer WireSkein has (a clock and data, or one NRZ line), devices/ device packs, markers/ marker dialects
  (docs/protocols.md §5).
- a file format (register_format): an object with
      name     its name ("saleae_csv")
      sniff(head: bytes, path) -> bool    whether the file is one of it, from its first 4096 bytes (and its name)
      read(path) -> wireskein.capture.Capture
  WireSkein's own files, sigrok sessions and VCDs are told apart first; a file none of them is goes to the formats
  added, in the order they were.
- a check kind (register_check): check(window, x) -> (ok: True / False / None for unchecked / verify.MEASURED,
  measured: dict, reason: str), window the segment's window as changes (rate, n_samples, channel(name) with
  .initial, .edges in ticks, .step and .level_at(ticks); analog), x the expectation ({"kind": kind, "pins": [...],
  ...}). It may put notices in measured["notices"] ([{"kind", "tick", ...}], ticks of the window); `notices` names
  their kinds and meanings. logic: it reads logic levels (an analog pin then needs threshold=); every_edge: it needs
  every edge (a line kept as changes that hide some is unchecked). (docs/capture-test-guide.md §4.10)
- a capture source (register_source): a scheme ("acme") and an object with capture(target: str,
  request: wireskein.sources.Request) -> wireskein.sources.Result; `wireskein capture --source acme:TARGET` and
  sources.run("acme:TARGET", ...) use it.

Installed packages declare them in entry point groups, read once when first needed: "wireskein.protocols",
"wireskein.definitions" (a module whose directory holds them, or a callable returning a path), "wireskein.formats",
"wireskein.checks" (a check function, or an object with .check and optionally .notices, .logic, .every_edge) and
"wireskein.sources". The entry point's name is the addition's name: an object whose own `name` differs is refused.

Names are letters, digits and "_", starting with a letter, and none of WireSkein's own (its protocols, layers,
formats, check kinds and sources): those are refused, as is an entry point naming one.
"""

from __future__ import annotations

import re
from pathlib import Path

__all__ = ["add_definitions", "checks", "definitions", "formats", "protocols", "register_check", "register_format",
           "register_protocol", "register_source", "remove_definitions", "sources", "unregister_check",
           "unregister_format", "unregister_protocol", "unregister_source"]

BUSES = ("uart", "spi", "i2c", "uart_pair", "can", "lin", "dmx512")   # and any protocol a definition adds
LAYERS = {"lines", "nmea", "modbus_rtu", "markers", "bytes", "chars", "breaks", "transfers", "transactions", "words",
          "bits", "frames", "packets", "sd", "devices", "modes", "modes_ok"}
FORMATS = ("wireskein", "sr", "vcd")
NAME = re.compile(r"[A-Za-z][A-Za-z0-9_]*")

_PROTOCOLS: dict[str, object] = {}
_FORMATS: dict[str, object] = {}
_SOURCES: dict[str, object] = {}
_CHECKS: set[str] = set()
_DIRS: list[Path] = []
_LOADED: set[str] = set()


def _name(name, what: str) -> str:
    if not isinstance(name, str) or not NAME.fullmatch(name):
        raise ValueError(f"a {what} needs a name of letters, digits and _: {name!r}")
    return name


def own_protocols() -> set[str]:
    """The names of WireSkein's own protocols and layers (a protocol or a definition may not take one)."""
    from ._engine import staged
    return {*BUSES, *LAYERS, *(p.name for p in staged.PLUGINS), *(u.name for u in staged.UPPER)}


def own_sources() -> set[str]:
    from .sources import BUILTIN
    return set(BUILTIN)


# --- protocols --------------------------------------------------------------------------------------------------

def register_protocol(protocol) -> None:
    """Add a protocol for the analyses that run after it (its shape: the module's docstring)."""
    name = _name(getattr(protocol, "name", None), "protocol")
    if name in own_protocols():
        raise ValueError(f"{name!r} is a protocol or layer of WireSkein's own")
    over = tuple(getattr(protocol, "over", ()))
    if not over or not all(isinstance(b, str) and NAME.fullmatch(b) for b in over):
        raise ValueError(f"{name}: over must name buses ({', '.join(BUSES)}, or a protocol defined in a definitions "
                         f"directory), not {over!r}")
    if not callable(getattr(protocol, "decode", None)):
        raise ValueError(f"{name}: decode(bus) is missing")
    _PROTOCOLS[name] = protocol


def unregister_protocol(name: str) -> None:
    _PROTOCOLS.pop(name, None)


def protocols() -> dict[str, object]:
    """The protocols added: by register_protocol, and by installed packages ("wireskein.protocols")."""
    _load()
    return dict(_PROTOCOLS)


def add_definitions(path) -> None:
    """Add a directory of declarative definitions (laid out as WireSkein's own decl/: the module's docstring)."""
    p = Path(path).resolve()
    if not p.is_dir():
        raise ValueError(f"{path}: not a directory")
    if p not in _DIRS:
        _DIRS.append(p)


def remove_definitions(path) -> None:
    p = Path(path).resolve()
    if p in _DIRS:
        _DIRS.remove(p)


def definitions() -> list[Path]:
    """The definition directories: those added, and those installed packages give ("wireskein.definitions")."""
    _load()
    return list(_DIRS)


# --- file formats -----------------------------------------------------------------------------------------------

def register_format(fmt) -> None:
    """Add a file format (its shape: the module's docstring)."""
    name = _name(getattr(fmt, "name", None), "format")
    if name in FORMATS:
        raise ValueError(f"{name!r} is a format WireSkein reads itself")
    for k in ("sniff", "read"):
        if not callable(getattr(fmt, k, None)):
            raise ValueError(f"{name}: {k} is missing")
    _FORMATS[name] = fmt


def unregister_format(name: str) -> None:
    _FORMATS.pop(name, None)


def formats() -> dict[str, object]:
    """The formats added: by register_format, and by installed packages ("wireskein.formats"), in that order."""
    _load()
    return dict(_FORMATS)


# --- check kinds ------------------------------------------------------------------------------------------------

def register_check(kind: str, check, *, notices: dict | None = None, logic: bool = True,
                   every_edge: bool = True) -> None:
    """Add a check kind (its shape: the module's docstring). A kind WireSkein has is not replaced."""
    from . import verify
    _name(kind, "check kind")
    if not callable(check):
        raise ValueError(f"{kind}: the check is not callable")
    if kind in verify.CHECKS and kind not in _CHECKS:
        raise ValueError(f"{kind!r} is a check of WireSkein's own")
    verify._add_check(kind, check, notices, logic, every_edge)
    _CHECKS.add(kind)


def unregister_check(kind: str) -> None:
    from . import verify
    if kind in _CHECKS:
        verify._remove_check(kind)
        _CHECKS.discard(kind)


def checks() -> list[str]:
    """The check kinds added: by register_check, and by installed packages ("wireskein.checks")."""
    _load()
    return sorted(_CHECKS)


# --- capture sources --------------------------------------------------------------------------------------------

def register_source(scheme: str, source) -> None:
    """Add a capture source under a scheme (its shape: the module's docstring)."""
    _name(scheme, "source")
    if scheme in own_sources():
        raise ValueError(f"{scheme!r} is a source of WireSkein's own")
    if not callable(getattr(source, "capture", None)):
        raise ValueError(f"{scheme}: capture(target, request) is missing")
    _SOURCES[scheme] = source


def unregister_source(scheme: str) -> None:
    _SOURCES.pop(scheme, None)


def sources() -> dict[str, object]:
    """The sources added: by register_source, and by installed packages ("wireskein.sources")."""
    _load()
    return dict(_SOURCES)


# --- entry points -----------------------------------------------------------------------------------------------

def _named(ep, obj, attr: str = "name"):
    """The entry point's object, its name the entry point's: given it where it has none, refused where it differs."""
    own = getattr(obj, attr, None)
    if own is None:
        try:
            setattr(obj, attr, ep.name)
        except AttributeError as e:
            raise ValueError(f"entry point {ep.group}:{ep.name}: its object takes no name") from e
    elif own != ep.name:
        raise ValueError(f"entry point {ep.group}:{ep.name}: its object is named {own!r}")
    return obj


def _load() -> None:
    """What installed packages add, each group once."""
    from importlib.metadata import entry_points
    for group in ("wireskein.definitions", "wireskein.protocols", "wireskein.formats", "wireskein.checks",
                  "wireskein.sources"):
        if group in _LOADED:
            continue
        _LOADED.add(group)
        for ep in entry_points(group=group):
            obj = ep.load()
            if group == "wireskein.definitions":
                add_definitions(obj() if callable(obj) else Path(obj.__file__).parent if hasattr(obj, "__file__")
                                else obj)
            elif group == "wireskein.protocols":
                register_protocol(_named(ep, obj))
            elif group == "wireskein.formats":
                register_format(_named(ep, obj))
            elif group == "wireskein.checks":
                register_check(ep.name, getattr(obj, "check", obj), notices=getattr(obj, "notices", None),
                               logic=getattr(obj, "logic", True), every_edge=getattr(obj, "every_edge", True))
            else:
                register_source(ep.name, obj)


def _score(m: dict) -> float:
    """The confidence a protocol's checks give, as Modbus RTU's: (1 - 0.5^passed) * passed / (passed + failed)."""
    ok, bad = int(m.get("checks_passed", 0)), int(m.get("checks_failed", 0))
    if ok <= 0:
        return 0.0
    return (1 - 0.5 ** ok) * ok / (ok + bad)
