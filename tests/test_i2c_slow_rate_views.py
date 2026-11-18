"""At a rate too coarse to show I2C's spike filter (t_SP 50 ns under 2 samples), I2C is read on the views with short
pulses taken out too, as any clocked bus: a pulse of one sample is then as likely crosstalk as a clipped clock, and
the input view (timing.i2c_input) keeps it, an extra clock in every byte it falls in. Lines made here."""

import numpy as np

from wireskein import decode
from wireskein._engine import gen
from wireskein._engine.model import Capture

RATE = 8e6     # 125 ns a sample


def test_crosstalk_on_a_slow_rate_i2c_is_read():
    scl, sda = gen.Wave(1), gen.Wave(1)
    txs = [{"addr": 0x48, "rw": "write", "bytes": bytes([1, 2, 3])}, {"addr": 0x48, "rw": "read", "bytes": bytes([7, 8])},
           {"addr": 0x50, "rw": "write", "bytes": bytes([0x10, 0x20, 0x30, 0x40])}] * 3
    t = gen.i2c(scl, sda, 1e-4, txs, 100e3)
    cap = gen.quantize({"SCL": scl, "SDA": sda}, RATE, t + 1e-4, np.random.default_rng(0))
    r = np.random.default_rng(1)
    chans = []
    for ch in cap.channels:                               # crosstalk: pulses of one sample, away from the edges
        e = np.asarray(ch.edges, np.int64)
        s = np.sort(r.integers(5, cap.n_samples - 5, len(e) // 3))
        s = s[np.min(np.abs(s[:, None] - e[None, :]), axis=1) >= 3]
        s = s[np.concatenate(([True], np.diff(s) >= 3))]
        chans.append(type(ch)(ch.name, ch.initial, np.unique(np.concatenate([e, s, s + 1]))))
    cap = Capture(cap.rate, cap.n_samples, chans)
    claims = decode.export(decode.analyze(cap), cap, mode="all")["claims"]
    i2c = [c for c in claims if c["protocol"] == "i2c" and c["verdict"] in ("confirmed", "likely")]
    assert i2c, [(c["protocol"], c["verdict"]) for c in claims]
