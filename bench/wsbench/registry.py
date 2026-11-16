"""Algorithms: one per file (or one file's variants), registered for a stage with a status.

    from wsbench import algorithm

    @algorithm("clock_period", "comb", status="trial", about="intervals as integer multiples, a cosine comb")
    def comb(cap, lines, tol=0.25):
        ...

    @algorithm("clean", "deglitch", status="trial", variants={"k2": {"k": 2}, "k3": {"k": 3}})
    def deglitch(cap, k): ...          # registered as deglitch.k2 and deglitch.k3

status: main (what WireSkein does today; one per stage), trial (being tried), retired (kept with its reason so the
trail of what was tried stays readable; not run unless asked by name).

engine: how the algorithm goes into WireSkein's analysis as one of its steps (wireskein._engine.methods), called
with the algorithm's parameters: engine(**params) -> {step name: function}. e2e/inject runs the analysis with it.

Algorithms are found in bench/algos/<stage>/*.py, then in each --algos-dir and in WSBENCH_ALGOS (os.pathsep
separated): a user's own algorithms live outside the repository and are measured the same way.
"""

from __future__ import annotations

import hashlib
import importlib.util
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

BENCH = Path(__file__).resolve().parents[1]
STATUSES = ("main", "trial", "retired")


@dataclass
class Algo:
    stage: str
    name: str
    fn: Callable
    status: str
    about: str
    params: dict = field(default_factory=dict)
    path: str = ""
    src_sha: str = ""
    reason: str = ""                  # retired: why
    engine: Callable | None = None    # (**params) -> {wireskein._engine.methods step: function}

    def engine_steps(self) -> dict:
        if self.engine is None:
            raise ValueError(f"{self.stage}/{self.name} has no engine step")
        return self.engine(**self.params)

    def __call__(self, *args):
        return self.fn(*args, **self.params)


_ALGOS: dict[tuple[str, str], Algo] = {}
_LOADING: list[Path] = []


def algorithm(stage: str, name: str, status: str = "trial", about: str = "", variants: dict | None = None,
              reason: str = "", engine: Callable | None = None):
    if status not in STATUSES:
        raise ValueError(f"{name}: status {status!r} (one of {STATUSES})")

    def deco(fn):
        path = _LOADING[-1] if _LOADING else Path(sys.modules[fn.__module__].__file__)
        # the file and the helpers every algorithm may use (a change to either is a change to the algorithm)
        sha = hashlib.sha256(path.read_bytes() + (Path(__file__).parent / "kit.py").read_bytes()).hexdigest()[:12]
        for suffix, params in (variants or {"": {}}).items():
            full = f"{name}.{suffix}" if suffix else name
            key = (stage, full)
            if key in _ALGOS and _ALGOS[key].path != str(path):
                raise ValueError(f"{stage}/{full}: registered twice ({_ALGOS[key].path}, {path})")
            _ALGOS[key] = Algo(stage, full, fn, status, about or (fn.__doc__ or "").strip().split("\n")[0],
                               dict(params), str(path), sha, reason, engine)
        return fn
    return deco


def _load_file(path: Path) -> None:
    mod = "wsbench_algo_" + hashlib.sha1(str(path).encode()).hexdigest()[:10]
    if mod in sys.modules:
        return
    spec = importlib.util.spec_from_file_location(mod, path)
    m = importlib.util.module_from_spec(spec)
    sys.modules[mod] = m
    _LOADING.append(path)
    try:
        spec.loader.exec_module(m)
    finally:
        _LOADING.pop()


def algo_dirs(extra: list[str] | None = None) -> list[Path]:
    dirs = [BENCH / "algos"]
    dirs += [Path(d).expanduser().resolve() for d in (extra or [])]
    dirs += [Path(d).expanduser().resolve() for d in os.environ.get("WSBENCH_ALGOS", "").split(os.pathsep) if d]
    return dirs


def load(extra: list[str] | None = None) -> dict[tuple[str, str], Algo]:
    for d in algo_dirs(extra):
        if not d.is_dir():
            raise FileNotFoundError(f"algorithm directory {d}")
        for p in sorted(d.rglob("*.py")):
            if not p.name.startswith("_"):
                _load_file(p)
    return _ALGOS


def get(stage: str, name: str) -> Algo:
    """Another stage's algorithm, for one that composes them (e2e/preclean: a clean, then the analysis)."""
    return _ALGOS[(stage, name)]


def pick(stage: str, names: list[str] | None, statuses: list[str]) -> list[Algo]:
    """By name (any status), else by status. main first."""
    have = [a for (s, _), a in _ALGOS.items() if s == stage]
    if names:
        out = []
        for n in names:
            got = [a for a in have if a.name == n or a.name.startswith(n + ".")]
            if not got:
                raise KeyError(f"{stage}: no algorithm {n!r} (have: {', '.join(sorted(a.name for a in have))})")
            out += got
    else:
        out = [a for a in have if a.status in statuses]
    return sorted(out, key=lambda a: (STATUSES.index(a.status), a.name))


def check(stage_names) -> list[str]:
    """Problems with the registry: a stage without exactly one main, an algorithm for an unknown stage."""
    warn = []
    for st in stage_names:
        mains = [a.name for (s, _), a in _ALGOS.items() if s == st and a.status == "main"]
        if len(mains) != 1:
            warn.append(f"stage {st}: {len(mains)} main algorithms ({', '.join(mains) or 'none'}), want 1")
    for (s, n) in _ALGOS:
        if s not in stage_names:
            warn.append(f"{s}/{n}: unknown stage")
    return warn
