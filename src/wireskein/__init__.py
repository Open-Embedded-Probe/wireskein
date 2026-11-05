"""WireSkein: logic-analyzer captures to protocol decodes, and checks of recorded test runs.

Public modules: wireskein.capture (a WireSkein file as one value; standard library only),
wireskein.runlog (recording a test run; standard library only), wireskein.verify (checking a
recorded run), wireskein.decode (decoding a capture), wireskein.sources (capturing from a device).
The rest (wireskein._engine, cli, gui and the modules they use) may change between versions.
"""

__all__ = ["__version__"]

__version__ = "0.0.18"
