"""Copy what the bench needs of two data repositories into bench/data/, once: the bench reads nothing outside this
repository (on GitHub's runners as here). Run again only to take a newer version of the sources.

    uv run python bench/data/extract.py --wch ~/dev_wch/wch-protocols --ebdd ~/dev/EmbedBenchDeviceData

wch-protocols (MIT, CH32 RISC-V User Group): the LinkE fixture captures/fixtures/wire-linke-p4-2026-09-25, of the
sub-directories the suites read (l103, v203, v003, v203-100mhz, extra/l103) the captures of 300 kB or less as sigrok
files (the flash writes, a million edges, left out), each saved as a .wireskein (the same edges: checked) with its
reference decoder's output <op>.dmi.txt beside it as it is (the truth: wsbench/data.py reads it). A v203-100mhz
operation without its own .dmi.txt takes the 50 MHz run's (v203/<op>.dmi.txt), copied too.

EmbedBenchDeviceData (MIT, TANAKA Masayuki): every run of runs/, its capture as it is and its run.json cut to what
the truth is made of (the case, the capture's file and channels, the log's text lines by source); not the decodes
beside it (WireSkein's) nor the probe's and host's clocks and ids.

Each copy keeps the source's LICENSE and a SOURCE.json: the repository, its commit, and each file's source and
SHA-256.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
WCH_FIXTURE = "captures/fixtures/wire-linke-p4-2026-09-25"
WCH_SUBS = ["l103", "v203", "v003", "v203-100mhz", "extra/l103"]
WCH_MAX_BYTES = 300_000
WCH_OUT = HERE / "wch-linke-p4-2026-09-25"
EBDD_OUT = HERE / "embedbench-device-data"


def _sha(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def _repo(d: Path) -> dict:
    git = lambda *a: subprocess.run(["git", "-C", str(d), *a], capture_output=True, text=True).stdout.strip()  # noqa: E731
    return {"remote": git("remote", "get-url", "origin"), "commit": git("rev-parse", "HEAD"),
            "dirty": bool(git("status", "--porcelain"))}


def wch(repo: Path) -> None:
    from wireskein import decode
    src = repo / WCH_FIXTURE
    if WCH_OUT.exists():
        shutil.rmtree(WCH_OUT)
    files = {}
    for sub in WCH_SUBS:
        for sr in sorted((src / sub).glob("*.sr")):
            if sr.stat().st_size > WCH_MAX_BYTES:
                continue
            cap = decode.load(sr)
            out = WCH_OUT / sub / (sr.stem + ".wireskein")
            out.parent.mkdir(parents=True, exist_ok=True)
            decode.save(out, cap)
            back = decode.load(out)
            if not (back.rate == cap.rate and back.n_samples == cap.n_samples and len(back.channels) == len(cap.channels)
                    and all(a.name == b.name and a.initial == b.initial and len(a.edges) == len(b.edges)
                            and (a.edges == b.edges).all() for a, b in zip(cap.channels, back.channels))):
                raise SystemExit(f"{sr}: the .wireskein does not read back the same")
            files[str(out.relative_to(WCH_OUT))] = {"from": str(sr.relative_to(repo)), "sha256": _sha(sr)}
            dmi = sr.with_suffix(".dmi.txt")
            if not dmi.exists() and sub.startswith("v203-"):
                dmi = src / "v203" / dmi.name
            if dmi.exists():
                to = WCH_OUT / dmi.relative_to(src)
                to.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(dmi, to)
                files[str(to.relative_to(WCH_OUT))] = {"from": str(dmi.relative_to(repo)), "sha256": _sha(dmi)}
    shutil.copyfile(repo / "LICENSE", WCH_OUT / "LICENSE")
    (WCH_OUT / "SOURCE.json").write_text(json.dumps(
        {"repository": _repo(repo), "fixture": WCH_FIXTURE, "subdirs": WCH_SUBS, "max_sr_bytes": WCH_MAX_BYTES,
         "files": files}, indent=1) + "\n")
    print(f"wch: {sum(f.endswith('.wireskein') for f in files)} captures, "
          f"{sum(f.endswith('.dmi.txt') for f in files)} .dmi.txt -> {WCH_OUT.relative_to(HERE.parent.parent)}")


def ebdd(repo: Path) -> None:
    if EBDD_OUT.exists():
        shutil.rmtree(EBDD_OUT)
    files = {}
    for rj in sorted((repo / "runs").glob("*/run.json")):
        meta = json.loads(rj.read_text())
        out = EBDD_OUT / "runs" / rj.parent.name
        out.mkdir(parents=True)
        caps = []
        for c in meta["captures"][:1]:
            shutil.copyfile(rj.parent / c["file"], out / c["file"])
            files[str((out / c["file"]).relative_to(EBDD_OUT))] = {"from": str((rj.parent / c["file"]).relative_to(repo)),
                                                                    "sha256": _sha(rj.parent / c["file"])}
            caps.append({"file": c["file"], "channels": c.get("channels")})
        slim = {"format": meta.get("format"), "meta": {"case": meta.get("meta", {}).get("case")}, "captures": caps,
                "log": [{"src": x.get("src"), "text": x.get("text")} for x in meta.get("log", [])
                        if x.get("src") in ("dut", "marker")]}
        (out / "run.json").write_text(json.dumps(slim, indent=1, ensure_ascii=False) + "\n")
        files[str((out / "run.json").relative_to(EBDD_OUT))] = {"from": str(rj.relative_to(repo)), "sha256": _sha(rj),
                                                                 "cut": "case, captures (file, channels), log (src, text)"}
    shutil.copyfile(repo / "LICENSE", EBDD_OUT / "LICENSE")
    (EBDD_OUT / "SOURCE.json").write_text(json.dumps({"repository": _repo(repo), "files": files}, indent=1) + "\n")
    print(f"ebdd: {len(list((EBDD_OUT / 'runs').iterdir()))} runs -> {EBDD_OUT.relative_to(HERE.parent.parent)}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--wch", type=Path, help="a checkout of wch-protocols")
    ap.add_argument("--ebdd", type=Path, help="a checkout of EmbedBenchDeviceData")
    args = ap.parse_args()
    if not (args.wch or args.ebdd):
        ap.error("--wch and / or --ebdd")
    if args.wch:
        wch(args.wch.expanduser())
    if args.ebdd:
        ebdd(args.ebdd.expanduser())


if __name__ == "__main__":
    main()
