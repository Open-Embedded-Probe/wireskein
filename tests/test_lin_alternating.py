"""A clock's bursts between breaks read as LIN frames whose every byte is 0x55 (the sync holds, 0x55 is a valid PID,
the checksum by a burst's length's luck): no evidence of LIN, so such a line's LIN hypothesis scores nothing (seven
false LIN claims on the bench, on burst clocks with noise among other buses). A real LIN's frames score as before.
Characters made here."""

from types import SimpleNamespace

import numpy as np

from wireskein._engine import plugins_uartlike as UL
from wireskein._engine.gen import lin_checksum, lin_pid
from wireskein._engine.staged import LinPlugin
from wireskein._engine.typed import Chars

UNIT = 200                                       # samples a bit


def chars(frames: list[bytes]) -> Chars:
    t, start, values, breaks = 1000, [], [], []
    for f in frames:
        breaks.append(t)
        t += 15 * UNIT
        for v in f:
            start.append(t)
            values.append(v)
            t += 10 * UNIT
        t += 40 * UNIT
    n = len(values)
    return Chars("D0", 10, np.asarray(values, np.int64), np.ones(n, bool), np.asarray(start, np.int64),
                 np.asarray(breaks, np.int64))


def score(ch: Chars) -> float:
    c = SimpleNamespace(cap=SimpleNamespace(rate=4e6))
    nodes = LinPlugin().run(c, SimpleNamespace(idle=1, pin="D0"), [((0, 10**7, UNIT), None, ch)])
    return nodes[0].total if nodes else 0.0


def test_runs_of_0x55_are_no_lin():
    rng = np.random.default_rng(0)
    ch = chars([bytes([0x55] * int(rng.integers(4, 12))) for _ in range(20)])
    ck = UL.lin(ch)["checks"]
    assert ck["sync"] == ck["pid_parity"] == ck["frames"] == ck["alternating"] == 20
    assert score(ch) == 0.0


def test_real_frames_score():
    rng = np.random.default_rng(1)
    frames = []
    for _ in range(8):
        pid = lin_pid(int(rng.integers(0, 60)))
        data = bytes(rng.integers(0, 256, int(rng.integers(1, 9))).tolist())
        frames.append(bytes([0x55, pid]) + data + bytes([lin_checksum(data, pid)]))
    ch = chars(frames)
    assert UL.lin(ch)["checks"]["alternating"] == 0
    assert score(ch) > 0.85
