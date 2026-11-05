"""Protocols of your own on top of a bus WireSkein already decodes (UART, SPI, I2C), for the analysis.

A protocol is an object with

    name     the name it is shown under ("acme_link"): letters, digits and "_", not one of WireSkein's own
    over     the buses it rides on: a tuple of "uart", "spi", "i2c"
    decode(bus) -> {"messages": [...], "checks_passed": int, "checks_failed": int}

`bus` is the bus as the analysis exports it (docs/analysis-format.md §4): its `roles` (pin of each role), `tick_hz`,
and its layers - for UART `bytes` ({"s": [tick...], "hex": "..."}) and `chars` (each {"s", "value", "ok"}) and
`breaks`; for SPI `transfers` (each {"s", "e", <pin>: hex}); for I2C `transactions` (each {"s", "e", "addr", "rw",
"bytes", "acks", "start_cond", "end_cond", ...}). It never sees WireSkein's internal types, so a protocol keeps
working as long as the analysis format does.

`decode` returns the messages it read (plain JSON values; a message's "s" is its first tick, so the viewer and the
reader can find it) and how many of its checks passed and failed (a checksum, a length, a sequence number...). A
check that passes on random bytes only by chance is the evidence: the analysis gives the bus more confidence by it,
the more passed and the fewer failed, as it does for Modbus RTU. A protocol with no check (checks_passed 0) shows
nothing: without evidence, anything parses as anything.

Installed packages declare protocols in the entry point group "wireskein.protocols" (NAME = "module:object"); a
script calls register(protocol). `wireskein analyze` shows each under its bus, and as the bus's final reading when
its checks held.
"""

from __future__ import annotations

import re

__all__ = ["register", "registered", "load"]

_REGISTERED: dict[str, object] = {}
_LOADED = False
BUSES = ("uart", "spi", "i2c")
_RESERVED = {"lines", "nmea", "modbus_rtu", "markers", "bytes", "chars", "breaks", "transfers", "transactions",
             "words", "bits", "frames", "packets", "sd", "devices", "modes", "modes_ok"}


def register(protocol) -> None:
    """Add a protocol for the analyses that run after it (see the module's docstring for its shape)."""
    name = getattr(protocol, "name", None)
    if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]*", name):
        raise ValueError(f"a protocol needs a name of letters, digits and _: {name!r}")
    if name in _RESERVED:
        raise ValueError(f"{name!r} is a layer of WireSkein's own")
    over = tuple(getattr(protocol, "over", ()))
    if not over or any(b not in BUSES for b in over):
        raise ValueError(f"{name}: over must name buses among {', '.join(BUSES)}, not {over!r}")
    if not callable(getattr(protocol, "decode", None)):
        raise ValueError(f"{name}: decode(bus) is missing")
    _REGISTERED[name] = protocol


def unregister(name: str) -> None:
    _REGISTERED.pop(name, None)


def load() -> None:
    """The protocols installed packages add (entry point group "wireskein.protocols"), once."""
    global _LOADED
    if _LOADED:
        return
    _LOADED = True
    from importlib.metadata import entry_points
    for ep in entry_points(group="wireskein.protocols"):
        obj = ep.load()
        if not getattr(obj, "name", None):
            try:
                obj.name = ep.name
            except AttributeError:
                pass
        register(obj)


def registered() -> dict[str, object]:
    load()
    return dict(_REGISTERED)


def score(m: dict) -> float:
    """The confidence the checks give, as Modbus RTU's: (1 - 0.5^passed) * passed / (passed + failed)."""
    ok, bad = int(m.get("checks_passed", 0)), int(m.get("checks_failed", 0))
    if ok <= 0:
        return 0.0
    return (1 - 0.5 ** ok) * ok / (ok + bad)
