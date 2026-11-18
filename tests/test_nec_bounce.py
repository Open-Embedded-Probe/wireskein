"""An IR receiver's output (NEC, no carrier) whose edges bounce is read with the pulses shorter than 4 us taken out:
as it is, the bounces break the marks and spaces and the line read as a UART at the mark's rate. Lines made here."""

from types import SimpleNamespace

import numpy as np

from wireskein._engine import gen
from wireskein._engine.analyzers.pulse import NecPlugin

RATE = 1e6


def test_bouncing_receiver_output_is_read():
    w = gen.Wave(1)
    frames = [{"addr": 0x10, "cmd": 0x42}, {"addr": 0x10, "cmd": 0x07}, {"addr": 0x22, "cmd": 0x99}]
    t = gen.nec(w, 2e-3, frames, carrier_hz=None)
    cap = gen.quantize({"D0": w}, RATE, t + 2e-3, np.random.default_rng(0))
    ch = cap.channel("D0")
    e = np.asarray(ch.edges, np.int64)
    b = e[::3] + 2                                       # every third edge bounces back for a tick
    ch = type(ch)(ch.name, ch.initial, np.sort(np.concatenate([e, b, b + 1])))
    nodes = NecPlugin().run(SimpleNamespace(cap=SimpleNamespace(n_samples=cap.n_samples, rate=RATE)), ch, None)
    assert nodes and nodes[0].total > 0.85 and nodes[0].params["deglitch"] >= 2
    got = [(f["addr"], f["cmd"]) for f in nodes[0].output.items if not f["repeat"]]
    assert got == [(f["addr"], f["cmd"]) for f in frames]
