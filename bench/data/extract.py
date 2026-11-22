"""Copy what the bench needs of two data repositories into bench/data/, once: the bench reads nothing outside this
repository (on GitHub's runners as here). Run again only to take a newer version of the sources.

    uv run python bench/data/extract.py --wch ~/dev_wch/wch-protocols --ebdd ~/dev/EmbedBenchDeviceData

wch-protocols (MIT, CH32 RISC-V User Group): the LinkE fixture captures/fixtures/wire-linke-p4-2026-09-25, of the
sub-directories the suites read (l103, v203, v003, v203-100mhz, extra/l103) the captures of 300 kB or less as sigrok
files (the flash writes, a million edges, left out), each saved as a .wireskein (the same edges: checked) with its
reference decoder's output <op>.dmi.txt beside it as it is (the truth: wsbench/data.py reads it), and for a read
operation what ch32rv read, <op>.bin (256 bytes or more). A v203-100mhz operation without its own .dmi.txt takes the
50 MHz run's (v203/<op>.dmi.txt), copied too.

EmbedBenchDeviceData (MIT, TANAKA Masayuki): every run of runs/ (those taken at 50 MS/s or more into
embedbench-device-data-50m, the earlier ones into embedbench-device-data), its capture as it is and its run.json cut to what
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
EBDD_FAST_OUT = HERE / "embedbench-device-data-50m"   # the runs taken at 50 MS/s (2026-11-17)


def _sha(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def _repo(d: Path) -> dict:
    git = lambda *a: subprocess.run(["git", "-C", str(d), *a], capture_output=True, text=True).stdout.strip()  # noqa: E731
    return {"remote": git("remote", "get-url", "origin"), "commit": git("rev-parse", "HEAD"),
            "dirty": bool(git("status", "--porcelain"))}


def _same(a, b) -> bool:
    return (a.rate == b.rate and a.n_samples == b.n_samples and len(a.channels) == len(b.channels)
            and all(x.name == y.name and x.initial == y.initial and len(x.edges) == len(y.edges)
                    and (x.edges == y.edges).all() for x, y in zip(a.channels, b.channels)))


def _prune(root: Path, keep: set) -> None:
    """Files under root that this extraction did not write (an operation gone from the source), removed."""
    for p in sorted(root.rglob("*")):
        if p.is_file() and p.relative_to(root).as_posix() not in keep | {"LICENSE", "SOURCE.json"}:
            p.unlink()
    for d in sorted((p for p in root.rglob("*") if p.is_dir()), key=lambda p: -len(p.parts)):
        if not any(d.iterdir()):
            d.rmdir()


def wch(repo: Path) -> None:
    from wireskein import decode
    src = repo / WCH_FIXTURE
    files = {}
    for sub in WCH_SUBS:
        for sr in sorted((src / sub).glob("*.sr")):
            if sr.stat().st_size > WCH_MAX_BYTES:
                continue
            cap = decode._load(sr)
            out = WCH_OUT / sub / (sr.stem + ".wireskein")
            out.parent.mkdir(parents=True, exist_ok=True)
            # a .wireskein saved again differs (its capture id) though its edges do not: one already there with the
            # same edges is kept as it is
            if not (out.exists() and _same(decode._load(out), cap)):
                decode._save(out, cap)
                if not _same(decode._load(out), cap):
                    raise SystemExit(f"{sr}: the .wireskein does not read back the same")
            files[str(out.relative_to(WCH_OUT))] = {"from": str(sr.relative_to(repo)), "sha256": _sha(sr)}
            dump = sr.with_suffix(".bin")              # what ch32rv read (read_flash_4k, read_ram_256)
            if dump.exists() and dump.stat().st_size >= 256:
                shutil.copyfile(dump, out.with_suffix(".bin"))
                files[str(out.with_suffix(".bin").relative_to(WCH_OUT))] = {"from": str(dump.relative_to(repo)),
                                                                             "sha256": _sha(dump)}
            dmi = sr.with_suffix(".dmi.txt")
            if not dmi.exists() and sub.startswith("v203-"):
                dmi = src / "v203" / dmi.name
            if dmi.exists():
                to = WCH_OUT / dmi.relative_to(src)
                to.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(dmi, to)
                files[str(to.relative_to(WCH_OUT))] = {"from": str(dmi.relative_to(repo)), "sha256": _sha(dmi)}
    _prune(WCH_OUT, set(files))
    shutil.copyfile(repo / "LICENSE", WCH_OUT / "LICENSE")
    (WCH_OUT / "SOURCE.json").write_text(json.dumps(
        {"repository": _repo(repo), "fixture": WCH_FIXTURE, "subdirs": WCH_SUBS, "max_sr_bytes": WCH_MAX_BYTES,
         "files": files}, indent=1) + "\n")
    print(f"wch: {sum(f.endswith('.wireskein') for f in files)} captures, "
          f"{sum(f.endswith('.dmi.txt') for f in files)} .dmi.txt -> {WCH_OUT.relative_to(HERE.parent.parent)}")


def _rate(cap: Path) -> float:
    from wireskein import _fileformat
    num, den = _fileformat.read_header(cap)["tick_hz"]
    return num / den


def ebdd(repo: Path) -> None:
    """Each run into the copy of its batch: the runs taken at 50 MS/s or more (2026-11-17, ENV III at 100 k / 400 k /
    1 MHz) apart from the earlier ones (2 MS/s and below: the real-ebdd suite's 35), so that a suite's cases stay."""
    for out_root, fast in ((EBDD_OUT, False), (EBDD_FAST_OUT, True)):
        files = {}
        for rj in sorted((repo / "runs").glob("*/run.json")):
            meta = json.loads(rj.read_text())
            if not meta["captures"] or (_rate(rj.parent / meta["captures"][0]["file"]) >= 50e6) != fast:
                continue
            out = out_root / "runs" / rj.parent.name
            out.mkdir(parents=True, exist_ok=True)
            caps = []
            for c in meta["captures"][:1]:
                shutil.copyfile(rj.parent / c["file"], out / c["file"])
                files[str((out / c["file"]).relative_to(out_root))] = {
                    "from": str((rj.parent / c["file"]).relative_to(repo)), "sha256": _sha(rj.parent / c["file"])}
                caps.append({"file": c["file"], "channels": c.get("channels")})
            slim = {"format": meta.get("format"), "meta": {"case": meta.get("meta", {}).get("case")}, "captures": caps,
                    "log": [{"src": x.get("src"), "text": x.get("text")} for x in meta.get("log", [])
                            if x.get("src") in ("dut", "marker")]}
            (out / "run.json").write_text(json.dumps(slim, indent=1, ensure_ascii=False) + "\n")
            files[str((out / "run.json").relative_to(out_root))] = {
                "from": str(rj.relative_to(repo)), "sha256": _sha(rj), "cut": "case, captures (file, channels), log (src, text)"}
        _prune(out_root, set(files))
        shutil.copyfile(repo / "LICENSE", out_root / "LICENSE")
        (out_root / "SOURCE.json").write_text(json.dumps({"repository": _repo(repo), "files": files}, indent=1) + "\n")
        print(f"ebdd: {len(list((out_root / 'runs').iterdir()))} runs -> {out_root.relative_to(HERE.parent.parent)}")


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
