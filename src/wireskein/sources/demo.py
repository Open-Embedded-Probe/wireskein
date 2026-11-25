"""demo:<options> - the demo device: a logic analyser with no hardware, sending a counter as packed samples (line Dk
is bit k of the sample's index), streaming or kept in its memory and read out (wireskein-capture's demo; the options
in docs/devices.md: lines=N, type=stream|memory, pace=real|fast, drop=K, stall=MS, fail=K, firmware=FILE, ...).

It has no Python implementation: it is the Rust driver's (wireskein_core, pip install ./crates/wireskein-py) whether
or not WIRESKEIN_RUST is set.
"""

from __future__ import annotations

from . import Request, Result


def capture(target: str, req: Request) -> Result:
    from .. import _rust
    from .oep import _from_rust
    return _from_rust(*_rust.source_capture("demo:" + target, req))
