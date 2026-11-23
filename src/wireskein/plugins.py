"""Extending WireSkein from outside: the one place to add protocols, definitions, file formats, check kinds and
capture sources (docs/extending.md).

What each addition is:

- a protocol (register_protocol): an object with
      name     its name ("acme.link": owner.name)
      over     the buses it rides on: a tuple of "uart", "spi", "i2c", "uart_pair", "can", "lin", "dmx512"
      decode(bus) -> {"messages": [...], "checks_passed": int, "checks_failed": int}
  `bus` is the bus as the analysis exports it (wireskein-analysis/1, docs/analysis-format.md §4). The checks its
  messages pass and fail (a checksum, a length, a sequence number) are the evidence: the analysis gives the bus more
  confidence by them, as it does for Modbus RTU; with no check passed it shows nothing (docs/protocols.md §1-§4).
- a definitions directory (add_definitions): devices/ device packs, markers/ marker dialects (docs/protocols.md §5).
  A protocol on the wire itself is not one: WireSkein's own (decl/*.toml) are part of its analysis engine, which may
  change, and a directory with protocol tables at its top is refused.
- a file format (register_format): an object with
      name     its name ("examples.saleae_csv": owner.name)
      sniff(head: bytes, path) -> bool    whether the file is one of it, from its first 4096 bytes (and its name)
      read(path) -> wireskein.capture.Capture
  WireSkein's own files, sigrok sessions and VCDs are told apart first; a file none of them is goes to the formats
  added, in the order they were.
- a check kind (register_check): check(window, x) -> (ok: True / False / None for unchecked / verify.MEASURED,
  measured: dict, reason: str), window the segment's window as its changes (as wireskein.capture.Part.view
  gives a part: rate_hz, n_samples, channel(name) with .initial, .edges in ticks, .step and .level_at(ticks); analog), x the expectation ({"kind": kind, "pins": [...],
  ...}). It may put notices in measured["notices"] ([{"kind", "tick", ...}], ticks of the window); `notices` names
  their kinds and meanings. logic: it reads logic levels (an analog pin then needs threshold=); every_edge: it needs
  every edge (a line kept as changes that hide some is unchecked). (docs/capture-test-guide.md §4.10)
- an engine (register_engine): a name, engine(view, hints) -> claims in the analysis format (protocol, roles,
  verdict, score, margin, params, layers), and the protocols it reads (each owner.name). It reads protocols WireSkein
  does not, beside WireSkein's own engine: its claims join on the lines that engine left unexplained (no claim, or
  only unknown-sync / ambiguous ones), first where a hint names their protocol (wireskein._engines); protocols of
  others ride on them (over = the protocol's name). `view` is the part's changes view (wireskein.capture.Part.view).
- a capture source (register_source): a scheme ("acme") and an object with capture(target: str,
  request: wireskein.sources.Request) -> wireskein.sources.Result; `wireskein capture --source acme:TARGET` and
  sources.run("acme:TARGET", ...) use it.

Installed packages declare them in entry point groups, read once when first needed: "wireskein.protocols",
"wireskein.definitions" (a module whose directory holds them, or a callable returning a path), "wireskein.formats",
"wireskein.checks" (a check function, or an object with .check and optionally .notices, .logic, .every_edge),
"wireskein.sources" and "wireskein.engines" (an object with .engine and .protocols). The entry point's name is the addition's name: an object whose own `name` differs is refused.

The environment adds them too, where a process starts without the caller's registrations (`wireskein --plugin MODULE
--definitions DIR ...` sets it, so a split read's worker processes have them): WIRESKEIN_PLUGINS, modules to import or .py
files to run (each adds what it has with this module), and WIRESKEIN_DEFINITIONS, definitions directories, each list separated by
os.pathsep.

Names (B13): an addition's is its owner's namespace and its own, owner.name ("acme.link": lowercase letters, digits
and "_"), so it never meets one of WireSkein's own, which have none (registry.toml, the one ledger of them); a capture
source's is a URI scheme ("acme"), not one WireSkein has. An entry point's name is the addition's: an object named
otherwise is refused.
"""

from __future__ import annotations

import re
from pathlib import Path

__all__ = ["add_definitions", "buses", "checks", "definitions", "engines", "formats", "protocols", "register_check",
           "register_engine", "register_format", "register_protocol", "register_source", "registry",
           "remove_definitions", "sources", "unregister_check", "unregister_engine", "unregister_format",
           "unregister_protocol", "unregister_source"]

NAME = re.compile(r"[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*")     # an addition's name: owner.name (B13)
SCHEME = re.compile(r"[A-Za-z][A-Za-z0-9_]*")              # a source's: a URI scheme
REGISTRY = Path(__file__).with_name("registry.toml")

_PROTOCOLS: dict[str, object] = {}
_FORMATS: dict[str, object] = {}
_SOURCES: dict[str, object] = {}
_ENGINES: dict[str, tuple] = {}
_CHECKS: set[str] = set()
_DIRS: list[Path] = []
_LOADED: set = set()


def _name(name, what: str) -> str:
    """An addition's name: its owner's namespace and its own, owner.name, each a lowercase letter then lowercase
    letters, digits and "_" (B13: WireSkein's own names have no namespace, so none is ever one of an addition's)."""
    if not isinstance(name, str) or not NAME.fullmatch(name):
        raise ValueError(f"a {what}'s name is its owner's namespace and its own, owner.name (lowercase letters, "
                         f"digits and _): {name!r}")
    return name


_REG = None


def registry() -> dict:
    """WireSkein's own protocol names (registry.toml): name -> {kind, roles, layers, result, over, engines}."""
    global _REG
    if _REG is None:
        import tomllib
        doc = tomllib.loads(REGISTRY.read_text())
        if doc.get("format") != "wireskein-registry/1":
            raise ValueError(f"{REGISTRY.name}: format {doc.get('format')!r}")
        _REG = doc["protocols"]
    return _REG


def buses() -> list[str]:
    """The buses WireSkein reads (a protocol's over names these, or an engine's protocols added from outside)."""
    return sorted(k for k, v in registry().items() if v["kind"] == "bus")


def own_sources() -> set[str]:
    from .sources import BUILTIN
    return set(BUILTIN)


# --- protocols --------------------------------------------------------------------------------------------------

def register_protocol(protocol) -> None:
    """Add a protocol for the analyses that run after it (its shape: the module's docstring)."""
    name = _name(getattr(protocol, "name", None), "protocol")
    over = tuple(getattr(protocol, "over", ()))
    known = set(buses())
    if not over or not all(isinstance(b, str) and (b in known or NAME.fullmatch(b)) for b in over):
        raise ValueError(f"{name}: over must name buses ({', '.join(sorted(known))}, or an added engine's "
                         f"owner.name), not {over!r}")
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
    """Add a directory of device packs (devices/) and marker dialects (markers/): the module's docstring."""
    p = Path(path).resolve()
    if not p.is_dir():
        raise ValueError(f"{path}: not a directory")
    tables = sorted(x.name for x in p.glob("*.toml"))
    if tables:
        raise ValueError(f"{path}: protocol tables at its top ({', '.join(tables)}): a protocol on the wire itself is "
                         f"part of WireSkein's analysis engine, not an extension point (docs/protocols.md §5)")
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
    if not isinstance(scheme, str) or not SCHEME.fullmatch(scheme):
        raise ValueError(f"a source's scheme is letters, digits and _: {scheme!r}")
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


# --- engines ----------------------------------------------------------------------------------------------------

def register_engine(name: str, engine, protocols) -> None:
    """Add an engine that reads protocols of its own beside WireSkein's (the module's docstring): name and each
    protocol owner.name; engine(view, hints) -> claims in the analysis format, each of one of these protocols."""
    _name(name, "engine")
    protocols = [_name(p, "protocol") for p in (protocols or [])]
    if not protocols:
        raise ValueError(f"{name}: an engine reads one protocol or more")
    if not callable(engine):
        raise ValueError(f"{name}: the engine is not callable")
    _ENGINES[name] = (engine, protocols)


def unregister_engine(name: str) -> None:
    _ENGINES.pop(name, None)


def engines() -> dict[str, tuple]:
    """The engines added: name -> (engine, protocols); by register_engine and installed packages
    ("wireskein.engines": an object with .engine and .protocols)."""
    _load()
    return dict(_ENGINES)


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


def load_environment() -> None:
    """The modules and definitions directories the environment names (WIRESKEIN_PLUGINS, WIRESKEIN_DEFINITIONS),
    once each."""
    import importlib
    import os
    for d in filter(None, os.environ.get("WIRESKEIN_DEFINITIONS", "").split(os.pathsep)):
        add_definitions(d)
    for m in filter(None, os.environ.get("WIRESKEIN_PLUGINS", "").split(os.pathsep)):
        if ("module", m) in _LOADED:
            continue
        _LOADED.add(("module", m))
        if m.endswith(".py"):               # a file of one's own, not installed
            import importlib.util
            spec = importlib.util.spec_from_file_location(Path(m).stem, m)
            if spec is None:
                raise ImportError(f"{m}: not a Python file")
            mod = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(mod)
        else:
            importlib.import_module(m)


def _load() -> None:
    """What installed packages and the environment add, each group once."""
    from importlib.metadata import entry_points
    if "environment" not in _LOADED:
        _LOADED.add("environment")
        load_environment()
    for group in ("wireskein.definitions", "wireskein.protocols", "wireskein.formats", "wireskein.checks",
                  "wireskein.sources", "wireskein.engines"):
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
            elif group == "wireskein.engines":
                register_engine(ep.name, getattr(obj, "engine", obj), getattr(obj, "protocols", ()))
            else:
                register_source(ep.name, obj)


def _score(m: dict) -> float:
    """The confidence a protocol's checks give, as Modbus RTU's: (1 - 0.5^passed) * passed / (passed + failed)."""
    ok, bad = int(m.get("checks_passed", 0)), int(m.get("checks_failed", 0))
    if ok <= 0:
        return 0.0
    return (1 - 0.5 ** ok) * ok / (ok + bad)
