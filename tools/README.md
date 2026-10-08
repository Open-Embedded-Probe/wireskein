# tools

[日本語](README.ja.md)

Scripts for development and releases. Not part of the package.

| File | Contents |
| --- | --- |
| `fetch_web.py` | Fetches the wireskein-web viewer pinned in `web.json` from npm, checks its hash and puts it into `src/wireskein/web/`. For trying `wireskein gui` locally, and in CI and releases |
| `web.json` | The bundled viewer's package name, version and integrity (sha512) |
| `prepare_release.py` | Called by the release workflow: sets the version in `pyproject.toml` and `src/wireskein/__init__.py`, and moves `## Unreleased` in `CHANGELOG.md` under the version's heading |

The release steps are in the repository [README](../README.md#release).
