"""I2C transactions say how they began and ended (S / Sr / P) and where SCL stayed low long (scl_waits)."""

import numpy as np

from wireskein import verify
from wireskein._engine import export, gen

RATE = 4e6


def _transactions(transactions, stretch=None):
    scl, sda = gen.Wave(1), gen.Wave(1)
    end = gen.i2c(scl, sda, 20e-6, transactions, 100e3, stretch=stretch)
    cap = gen.quantize({"SCL": scl, "SDA": sda}, RATE, end + 50e-6, np.random.default_rng(0))
    res = verify._decode(cap, {"protocols": ["i2c"], "pins": {"SCL": {"protocol": "i2c", "role": "scl"},
                                                             "SDA": {"protocol": "i2c", "role": "sda"}}})
    n = verify._best(res, "i2c", {"scl": "SCL", "sda": "SDA"})
    return n, [t for t in n.output.items if "addr" in t]


def test_a_register_read_through_a_repeated_start():
    n, txs = _transactions([{"addr": 0x48, "rw": "write", "bytes": [0x10], "repeated_start": True},
                            {"addr": 0x48, "rw": "read", "bytes": [0xAB, 0xCD]}])
    assert [(t["rw"], t["start_cond"], t["end_cond"]) for t in txs] == [("write", "S", "Sr"), ("read", "Sr", "P")]
    out = export.layers_of(n)["transactions"]
    assert [(t["start_cond"], t["end_cond"], t["scl_waits"]) for t in out] == [("S", "Sr", []), ("Sr", "P", [])]


def test_a_long_scl_low_is_a_wait_after_its_word():
    # bit 9 is the first bit after the address byte and its ACK: SCL held low 300 us before it
    _, (t,) = _transactions([{"addr": 0x50, "rw": "write", "bytes": [0x01, 0x02]}], stretch={9: 300e-6})
    assert t["scl_low_ticks"] == 20                     # 5 us at 4 MHz: half a 100 kHz bit
    (w,) = t["scl_waits"]
    assert (w["after_word"], w["bit"]) == (0, 0) and abs(w["ticks"] - (20 + 1200)) <= 2


def test_no_wait_on_an_even_clock():
    _, (t,) = _transactions([{"addr": 0x50, "rw": "write", "bytes": [0x01, 0x02]}])
    assert t["scl_waits"] == [] and (t["start_cond"], t["end_cond"]) == ("S", "P")
