"""A UART line whose edges nearly all bounce (a pulse back of a tick to three just after the edge) is read through
a view with the pulses under 4 ticks taken out: the bit time estimated from the line as it is is a bounce's, and the
view of single-tick spikes is not made when they are that many. Lines made here (gen.uart)."""

import numpy as np

from wireskein import decode
from wireskein._engine import gen
from wireskein._engine.model import Capture

RATE = 1e6


def test_bouncing_uart_is_read():
    w = gen.Wave(1)
    data = b"bounce test: the quick brown fox 0123456789"
    t = gen.uart(w, 1e-3, data, 19200.0)
    cap = gen.quantize({"D0": w}, RATE, t + 1e-3, np.random.default_rng(0))
    ch = cap.channel("D0")
    e = np.asarray(ch.edges, np.int64)
    r = np.random.default_rng(1)
    sel = e[r.random(len(e)) < 0.8]                      # 8 edges in 10 bounce: unread before the bounce view
    width = r.integers(1, 4, len(sel))
    b0 = sel + r.integers(1, 4, len(sel))
    ch = type(ch)(ch.name, ch.initial, np.unique(np.concatenate([e, b0, b0 + width])))
    cap = Capture(cap.rate, cap.n_samples, [ch])
    claims = decode.export(decode.analyze(cap), cap, mode="all")["claims"]
    uart = [c for c in claims if c["protocol"] == "uart" and c["verdict"] in ("confirmed", "likely")]
    assert uart and abs(uart[0]["params"]["baud"] / 19200 - 1) < 0.03, [(c["protocol"], c["verdict"]) for c in claims]
    assert data.hex() in uart[0]["layers"]["bytes"]["hex"]
