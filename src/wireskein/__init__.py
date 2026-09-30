"""WireSkein: logic-analyzer captures to protocol decodes, and checks of recorded test runs.

Public modules: wireskein.runlog (recording, standard library only),
wireskein.verify (checking a recorded run), wireskein.analyze (decoding a
capture). Everything under wireskein._engine may change between versions.
"""

__all__ = ["__version__"]

__version__ = "0.0.2"
