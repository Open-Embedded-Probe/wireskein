#!/usr/bin/env python3
"""Put the viewer of the pinned wireskein-web release into src/wireskein/web/.

tools/web.json names the npm package, its version and the tarball's
integrity (sha512, as npm publishes it). The tarball is downloaded from the
npm registry, checked against that hash, and its site/ directory is
extracted. The result is not in git; the wheel carries it (`wireskein gui`).

    python tools/fetch_web.py            # when the pin changed or the directory is missing
    python tools/fetch_web.py --force
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import io
import json
import shutil
import tarfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PIN = ROOT / "tools" / "web.json"
DEST = ROOT / "src" / "wireskein" / "web"


def fetch(force: bool = False) -> Path:
    pin = json.loads(PIN.read_text())
    stamp = DEST / "VERSION"
    if not force and stamp.exists() and stamp.read_text().strip() == pin["version"]:
        print(f"{DEST} already has {pin['package']} {pin['version']}")
        return DEST
    name = pin["package"]
    url = f"https://registry.npmjs.org/{name}/-/{name.split('/')[-1]}-{pin['version']}.tgz"
    with urllib.request.urlopen(url, timeout=60) as r:
        data = r.read()
    algo, _, want = pin["integrity"].partition("-")
    got = base64.b64encode(hashlib.new(algo, data).digest()).decode()
    if got != want:
        raise SystemExit(f"{url}: {algo} {got} does not match the pin {want}")
    if DEST.exists():
        shutil.rmtree(DEST)
    DEST.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for m in tar.getmembers():
            if not m.isfile() or not m.name.startswith("package/site/"):
                continue
            rel = Path(m.name[len("package/site/"):])
            if rel.is_absolute() or ".." in rel.parts:
                raise SystemExit(f"{url}: unsafe path {m.name}")
            out = DEST / rel
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_bytes(tar.extractfile(m).read())
    stamp.write_text(pin["version"] + "\n")
    print(f"{pin['package']} {pin['version']} -> {DEST}")
    return DEST


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--force", action="store_true")
    fetch(ap.parse_args().force)
