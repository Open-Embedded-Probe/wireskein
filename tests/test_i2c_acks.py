"""The I2C check compares the ACK after each data byte, on synthetic captures (gen.i2c: the target ACKs a write's
bytes unless nack_at, the controller ACKs a read's bytes but the last)."""

import numpy as np

from wireskein._engine import gen
from wireskein.verify import check_i2c


def capture(transactions):
    scl, sda = gen.Wave(1), gen.Wave(1)
    end = gen.i2c(scl, sda, 20e-6, transactions, 100e3)
    return gen.quantize({"SCL": scl, "SDA": sda}, 10e6, end + 50e-6, np.random.default_rng(0))


def check(cap, want):
    return check_i2c(cap, {"kind": "i2c", "scl": "SCL", "sda": "SDA", "transactions": want, "freq_hz": None,
                           "freq_rel": 0.1, "released": True})


def test_a_write_byte_nacked_fails_a_plain_expectation():
    cap = capture([{"addr": 0x42, "rw": "write", "bytes": [0x20], "nack_at": 1}])
    ok, got, why = check(cap, [{"addr": 0x42, "rw": "write", "bytes": [0x20]}])
    assert got["transactions"][0]["acks"] == [False]
    assert ok is False and "[20:NACK]" in why


def test_nack_at_expects_it():
    cap = capture([{"addr": 0x42, "rw": "write", "bytes": [0x20, 0x21], "nack_at": 2}])
    ok, got, why = check(cap, [{"addr": 0x42, "rw": "write", "bytes": [0x20, 0x21], "nack_at": 1}])
    assert ok is True, why
    ok, _, _ = check(cap, [{"addr": 0x42, "rw": "write", "bytes": [0x20, 0x21], "acks": [True, False]}])
    assert ok is True


def test_writes_acked_and_a_read_ending_in_the_controllers_nack_pass_by_default():
    cap = capture([{"addr": 0x42, "rw": "write", "bytes": [0x01, 0x02]},
                   {"addr": 0x42, "rw": "read", "bytes": [0x10, 0x11, 0x12]}])
    ok, got, why = check(cap, [{"addr": 0x42, "rw": "write", "bytes": [0x01, 0x02]},
                               {"addr": 0x42, "rw": "read", "bytes": [0x10, 0x11, 0x12]}])
    assert ok is True, why
    assert [t["acks"] for t in got["transactions"]] == [[True, True], [True, True, False]]


def test_acks_given_that_differ_fail():
    cap = capture([{"addr": 0x42, "rw": "write", "bytes": [0x10, 0x11]}])
    ok, _, why = check(cap, [{"addr": 0x42, "rw": "write", "bytes": [0x10, 0x11], "acks": [True, False]}])
    assert ok is False and "want S 0x42 write [10 11:NACK]" in why or "want 0x42 write [10 11:NACK]" in why
