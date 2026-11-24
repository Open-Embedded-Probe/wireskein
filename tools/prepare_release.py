#!/usr/bin/env python3
from __future__ import annotations

import argparse
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PYPROJECT = ROOT / "pyproject.toml"
INIT_FILE = ROOT / "src" / "wireskein" / "__init__.py"
CHANGELOG = ROOT / "CHANGELOG.md"
CARGO = ROOT / "Cargo.toml"
CARGO_LOCK = ROOT / "Cargo.lock"


def update_pyproject(version: str) -> None:
    text = PYPROJECT.read_text(encoding="utf-8")
    updated = re.sub(
        r'(?m)^version = "[^"]+"$',
        f'version = "{version}"',
        text,
        count=1,
    )
    if not re.search(r'(?m)^version = "[^"]+"$', text):
        raise RuntimeError("Failed to update version in pyproject.toml")
    PYPROJECT.write_text(updated, encoding="utf-8")


def update_cargo(version: str) -> None:
    """The Rust core's crates share wireskein's version: the workspace's, and the crates' lines in Cargo.lock."""
    text = CARGO.read_text(encoding="utf-8")
    new, n = re.subn(r'(?ms)^(\[workspace\.package\]\s*\nversion = )"[^"]+"', rf'\1"{version}"', text)
    if n != 1:
        raise RuntimeError("Failed to update [workspace.package] version in Cargo.toml")
    CARGO.write_text(new, encoding="utf-8")
    lock = CARGO_LOCK.read_text(encoding="utf-8")
    lock, n = re.subn(r'(?m)^(name = "wireskein-(?:core|py)"\nversion = )"[^"]+"', rf'\1"{version}"', lock)
    if n != 2:
        raise RuntimeError("Failed to update the crates' versions in Cargo.lock")
    CARGO_LOCK.write_text(lock, encoding="utf-8")


def update_init(version: str) -> None:
    text = INIT_FILE.read_text(encoding="utf-8")
    updated = re.sub(
        r'(?m)^__version__ = "[^"]+"$',
        f'__version__ = "{version}"',
        text,
        count=1,
    )
    if not re.search(r'(?m)^__version__ = "[^"]+"$', text):
        raise RuntimeError("Failed to update __version__ in __init__.py")
    INIT_FILE.write_text(updated, encoding="utf-8")


def update_changelog(version: str) -> None:
    lines = CHANGELOG.read_text(encoding="utf-8").splitlines()
    try:
        unreleased_idx = lines.index("## Unreleased")
    except ValueError as exc:
        raise RuntimeError("CHANGELOG.md must contain '## Unreleased'") from exc

    tail_start = len(lines)
    for i in range(unreleased_idx + 1, len(lines)):
        if lines[i].startswith("## "):
            tail_start = i
            break

    unreleased_block = lines[unreleased_idx + 1 : tail_start]
    unreleased_items = [line for line in unreleased_block if line.strip()]

    new_lines: list[str] = []
    new_lines.extend(lines[: unreleased_idx + 1])
    new_lines.append("")
    new_lines.append(f"## {version}")
    if unreleased_items:
        new_lines.extend(unreleased_items)
    else:
        new_lines.append("- (EN) No user-facing changes recorded.")
        new_lines.append("- (JA) ユーザー向け変更の記録はありません。")
    if tail_start < len(lines):
        new_lines.append("")
        new_lines.extend(lines[tail_start:])

    CHANGELOG.write_text("\n".join(new_lines).rstrip() + "\n", encoding="utf-8")


def current_version() -> str:
    match = re.search(r'(?m)^version = "([^"]+)"$', PYPROJECT.read_text(encoding="utf-8"))
    if not match:
        raise RuntimeError("No version in pyproject.toml")
    return match.group(1)


def bumped(version: str, level: str) -> str:
    """The next release after `version` at `level` (patch / minor / major); a pre-release (0.1.0b1) goes to its own
    X.Y.Z at patch, and to the next minor / major otherwise."""
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)((a|b|rc)\d+)?", version)
    if not match:
        raise SystemExit(f"The current version {version!r} is not X.Y.Z or X.Y.Z{{a|b|rc}}N")
    major, minor, patch = (int(x) for x in match.group(1, 2, 3))
    pre = match.group(4) is not None
    if level == "major":
        return f"{major + 1}.0.0"
    if level == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch if pre else patch + 1}"


def main() -> None:
    parser = argparse.ArgumentParser(description="Prepare release version and changelog.")
    parser.add_argument("--version", default="", help="Release version (e.g. 0.1.0, 0.1.0b1); empty: --level decides")
    parser.add_argument("--level", choices=("patch", "minor", "major"), default="patch",
                        help="The part of the current version to bump when --version is empty")
    parser.add_argument("--github-output", default="", help="Append version=<the release version> to this file")
    args = parser.parse_args()

    version = args.version.strip() or bumped(current_version(), args.level)
    # X.Y.Z, optionally a PEP 440 pre-release (0.1.0b1, 1.0.0rc1)
    if not re.fullmatch(r"\d+\.\d+\.\d+((a|b|rc)\d+)?", version):
        raise SystemExit("Version must be X.Y.Z or X.Y.Z{a|b|rc}N")

    update_pyproject(version)
    update_cargo(version)
    update_init(version)
    update_changelog(version)
    if args.github_output:
        with open(args.github_output, "a", encoding="utf-8") as out:
            out.write(f"version={version}\n")
    print(f"Prepared release: {version}")


if __name__ == "__main__":
    main()
