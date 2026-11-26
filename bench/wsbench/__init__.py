"""WireSkein's algorithm bench: stages, algorithms (main / trial / retired), datasets and a ledger of every run.

Separate from the package: nothing under src/ imports it. See bench/README.md for the rules.
"""

import os

# The package's default is the Rust core for every part (src/wireskein/_rust.py, bench/notes/2026-11-24-rust-plan.ja.md
# §51). The bench keeps Python's for wireskein's own calls (e2e "wireskein" and every algorithm built on
# wireskein.decode.analyze: the ledger's main line as before), and the Rust core is judged as an engine of its own
# (bench/engines/rust.py, engine.rust). WIRESKEIN_RUST / WIRESKEIN_ENGINE set by hand still win.
if not os.environ.get("WIRESKEIN_RUST", "").strip() and not os.environ.get("WIRESKEIN_ENGINE", "").strip():
    os.environ["WIRESKEIN_RUST"] = "none"

from .registry import algorithm  # noqa: E402, F401
