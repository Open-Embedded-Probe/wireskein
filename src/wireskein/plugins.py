"""What one adds to WireSkein's analysis, without code: a definitions directory (docs/extending.md).

A definitions directory holds tables that read what the analysis exports, whatever finds the buses:

- protocols/*.toml   protocols of one's own on the buses (wireskein-protocol/1: frames on a UART, SPI, I2C, CAN,
                     LIN or DMX512, or command and answer lines on a UART pair; docs/protocols.md)
- devices/<bus>/...  device packs (the registers and commands of a device on I2C or SPI)
- markers/*.toml     marker dialects (the marker lines of one's own test logs)

A protocol on the wire itself (its own framing on a clock and data, or on one line) is not one of these: WireSkein's
own such protocols (decl/*.toml) are part of its analysis engine, and a directory with protocol tables at its top is
refused. Anything a table cannot say is done outside, on the documented formats: a capture file in (another format
converted to a WireSkein file with wireskein.capture), the analysis format out (docs/analysis-format.md).

Adding one: add_definitions(path) in a script or a test, `wireskein --definitions DIR` on the command line, or the
environment's WIRESKEIN_DEFINITIONS (directories separated by os.pathsep; the command line sets it, so a split read's
worker processes have them).

Names (B13): a protocol's is its owner's namespace and its own, owner.name ("acme.link": lowercase letters, digits
and "_"), so it never meets one of WireSkein's own, which have none (registry.toml, the one ledger of them).
"""

from __future__ import annotations

import re
from pathlib import Path

__all__ = ["add_definitions", "buses", "definitions", "protocols", "registry", "remove_definitions"]

NAME = re.compile(r"[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*")     # an addition's name: owner.name (B13)
REGISTRY = Path(__file__).with_name("registry.toml")

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
    """The buses WireSkein reads."""
    return sorted(k for k, v in registry().items() if v["kind"] == "bus")


def add_definitions(path) -> None:
    """Add a definitions directory (protocols/, devices/, markers/: the module's docstring)."""
    p = Path(path).resolve()
    if not p.is_dir():
        raise ValueError(f"{path}: not a directory")
    tables = sorted(x.name for x in p.glob("*.toml"))
    if tables:
        raise ValueError(f"{path}: protocol tables at its top ({', '.join(tables)}): a protocol on the wire itself is "
                         f"part of WireSkein's analysis engine, not an extension point (docs/protocols.md §5)")
    if p not in _DIRS:
        from . import _declared
        _declared.declared([*_DIRS, p])           # a table in error is refused when it is added
        _DIRS.append(p)


def remove_definitions(path) -> None:
    p = Path(path).resolve()
    if p in _DIRS:
        _DIRS.remove(p)


def definitions() -> list[Path]:
    """The definitions directories added (and those the environment names)."""
    _load()
    return list(_DIRS)


def protocols() -> dict[str, object]:
    """The protocols declared in the definitions directories (protocols/*.toml): name -> a protocol with name, over
    and decode(bus) (wireskein._declared)."""
    from . import _declared
    return _declared.declared(definitions())


def load_environment() -> None:
    """The definitions directories the environment names (WIRESKEIN_DEFINITIONS)."""
    import os
    for d in filter(None, os.environ.get("WIRESKEIN_DEFINITIONS", "").split(os.pathsep)):
        add_definitions(d)


def _load() -> None:
    if "environment" not in _LOADED:
        _LOADED.add("environment")
        load_environment()


def _score(m: dict) -> float:
    """The confidence a protocol's checks give, as Modbus RTU's: (1 - 0.5^passed) * passed / (passed + failed)."""
    ok, bad = int(m.get("checks_passed", 0)), int(m.get("checks_failed", 0))
    if ok <= 0:
        return 0.0
    return (1 - 0.5 ** ok) * ok / (ok + bad)
