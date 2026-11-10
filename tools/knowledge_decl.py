"""Write wireskein's device declarations (src/wireskein/decl/devices) from EmbedBenchDeviceData's knowledge: what a
part's identification reads, one TOML file per part, at <bus>/<manufacturer>/<chip>.toml. Addresses come from its
chips/<chip>.yaml; command or register names, answer lengths, the CRC and the identifying register from its
knowledge/<chip>.yaml. Nothing else (timing, behaviours, evidence) goes into the declaration. Not part of the package.

    uv run python tools/knowledge_decl.py [--devicedata DIR] [--out DIR] [CHIP ...]    write (default: decl/devices)
    uv run python tools/knowledge_decl.py [--devicedata DIR] --check                  compare, exit 1 on drift

DIR is a clone of EmbedBenchDeviceData (default: $WIRESKEIN_DEVICEDATA). With no CHIP, every knowledge file. Each file
names the DeviceData commit it was made from in its first line. --check makes them again and compares with
decl/devices, the comment lines left out (a new commit with the same knowledge is no drift): what differs, what is
generated here with no knowledge there, and what has knowledge there and is not generated here. Hand-written files
(no "Generated from" first line, spi/flash/spi_nor.toml) are left alone.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

import yaml

DECL = Path(__file__).resolve().parent.parent / "src" / "wireskein" / "decl" / "devices"
MARK = "# Generated from EmbedBenchDeviceData"


def source_commit(root: Path) -> str:
    def git(*args):
        return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True, check=True).stdout.strip()
    commit = git("rev-parse", "--short", "HEAD")
    if git("status", "--porcelain", "--", "knowledge", "chips"):
        commit += "+uncommitted"
    return commit


def folder(manufacturer: str) -> str:
    """'QST Corporation' -> 'qst': the manufacturer's first word, lowercase."""
    return re.sub(r"[^a-z0-9-]", "", manufacturer.split()[0].lower())


def string(text: str) -> str:
    return '"%s"' % text.replace("\\", "\\\\").replace('"', '\\"')


def render(root: Path, chip: str, commit: str) -> tuple[Path, str]:
    data = yaml.safe_load((root / "knowledge" / f"{chip}.yaml").read_text(encoding="utf-8"))
    catalog = yaml.safe_load((root / "chips" / f"{chip}.yaml").read_text(encoding="utf-8"))
    bus = data["bus"]
    interface = next(i for i in catalog["interfaces"] if i["bus"] == bus)
    addresses = ", ".join("0x%02X" % int(a["addr"], 16) for a in interface["addresses"])
    addressing = data.get("addressing") or {}
    pointer_bytes = addressing["pointer_bits"] // 8
    commands = data.get("commands") or []
    lines = [f"{MARK} {commit} knowledge/{chip}.yaml and",
             f"# chips/{chip}.yaml by wireskein's tools/knowledge_decl.py. Regenerate rather than edit.",
             "[device]",
             f"name = {string(catalog['name'])}",
             f"bus = {string(bus)}"]
    if commands:
        lines += ['kind = "command_table"', f"addresses = [{addresses}]",
                  f"command_bytes = {pointer_bytes}"]
        crc = addressing.get("crc")
        if crc:
            lines.append("crc = { poly = %s, init = %s, word = %d }" % (
                crc["poly"].upper().replace("0X", "0x"), crc["init"].upper().replace("0X", "0x"),
                crc["word"]))
        for command in commands:
            lines += ["", "[[command]]",
                      "code = 0x%0*X" % (pointer_bytes * 2, int(command["code"], 16)),
                      f"name = {string(command['name'])}"]
            if command["answer_bytes"]:
                lines.append(f"response = {command['answer_bytes']}")
    else:
        lines += ['kind = "register_map"', f"addresses = [{addresses}]",
                  f"pointer_bytes = {pointer_bytes}"]
        for reg in data["registers"]:
            if reg.get("identifies"):
                value = int(reg["reset"], 16)
                lines += ["", "[[identify]]", "register = 0x%02X" % int(reg["addr"], 16),
                          "value = 0x%0*X" % (reg["width"] * 2, value)]
        for reg in data["registers"]:
            lines += ["", "[[register]]", "address = 0x%02X" % int(reg["addr"], 16)]
            if reg.get("name"):
                lines.append(f"name = {string(reg['name'])}")
    path = Path(bus) / folder(catalog["manufacturer"]) / f"{chip}.toml"
    return path, "\n".join(lines) + "\n"


def write(root: Path, out: Path, chips: list[str]) -> list[Path]:
    commit = source_commit(root)
    done = []
    for chip in chips or sorted(p.stem for p in (root / "knowledge").glob("*.yaml")):
        path, text = render(root, chip, commit)
        (out / path).parent.mkdir(parents=True, exist_ok=True)
        (out / path).write_text(text, encoding="utf-8")
        done.append(path)
    return done


def _body(text: str) -> list[str]:
    return [ln for ln in text.splitlines() if not ln.startswith("#")]


def _commit(text: str) -> str | None:
    first = text.splitlines()[0] if text else ""
    return first[len(MARK):].split()[0] if first.startswith(MARK) else None


def check(root: Path, decl: Path = DECL) -> list[str]:
    """What differs between decl/devices and what the knowledge makes now (empty: none)."""
    have = {p.relative_to(decl): p.read_text(encoding="utf-8") for p in decl.rglob("*.toml")}
    generated = {k: v for k, v in have.items() if _commit(v) is not None}
    with tempfile.TemporaryDirectory() as tmp:
        made = {p: (Path(tmp) / p).read_text(encoding="utf-8") for p in write(root, Path(tmp), [])}
    out = []
    for p, text in sorted(made.items()):
        if p not in have:
            out.append(f"{p}: knowledge has it, decl/devices does not")
        elif p not in generated:
            out.append(f"{p}: hand-written in decl/devices, but knowledge has it")
        elif _body(have[p]) != _body(text):
            out.append(f"{p}: differs (decl from {_commit(have[p])}, knowledge at {_commit(text)})")
    for p in sorted(set(generated) - set(made)):
        out.append(f"{p}: generated from {_commit(generated[p])}, no knowledge for it now")
    return out


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("chips", nargs="*")
    ap.add_argument("--devicedata", type=Path, default=os.environ.get("WIRESKEIN_DEVICEDATA"),
                    help="a clone of EmbedBenchDeviceData (default: $WIRESKEIN_DEVICEDATA)")
    ap.add_argument("--out", type=Path, default=DECL)
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args(argv)
    if not args.devicedata:
        ap.error("--devicedata DIR or WIRESKEIN_DEVICEDATA: a clone of EmbedBenchDeviceData")
    root = Path(args.devicedata).expanduser()
    if args.check:
        drift = check(root)
        for line in drift:
            print(line)
        print(f"{'drift' if drift else 'no drift'} against EmbedBenchDeviceData {source_commit(root)}",
              file=sys.stderr)
        return 1 if drift else 0
    for p in write(root, args.out, args.chips):
        print(args.out / p)
    return 0


if __name__ == "__main__":
    sys.exit(main())
