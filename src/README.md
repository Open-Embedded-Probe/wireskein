# src

[日本語](README.ja.md)

The Python package `wireskein` (what `pip install wireskein` installs).

| Place | Contents |
| --- | --- |
| `wireskein/runlog.py` | The test-run recorder and the check helpers. Standard library only ([run format](../docs/run-format.md)) |
| `wireskein/verify.py` | Checking a recorded run (`wireskein verify`) |
| `wireskein/fileformat.py` | Reading and writing `.wireskein`. Standard library only ([file format](../docs/wireskein-format.md)) |
| `wireskein/analyze.py`, `annotate.py` | Decoding (`wireskein analyze`) and storing decode annotations |
| `wireskein/align.py` | Aligning analog channels and other files in time (`wireskein align`) |
| `wireskein/sources/` | Capture sources (`oep:`, `sigrok:`). More can be added through the `wireskein.sources` entry point |
| `wireskein/gui.py` | The viewer's server (`wireskein gui`). The viewer itself, `wireskein/web/`, is not in git: `tools/fetch_web.py` fetches it into the wheel |
| `wireskein/cli.py` | The command entry point |
| `wireskein/decl/` | Definitions: protocols (`*.toml`), devices (`devices/`), marker dialects (`markers/`) |
| `wireskein/_engine/` | Internal: the analysis engine (stages, the TOML interpreter, `.sr` and VCD I/O, ...). Not a promised API |

Design decisions and their reasons: [docs/design.md](../docs/design.md).
