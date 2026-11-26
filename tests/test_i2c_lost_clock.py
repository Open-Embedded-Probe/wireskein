"""A read whose target stretches the clock longer than the controller waits (bench/notes/2026-11-25-field-failures.ja.md,
sht30-modes): the controller gives up, clocks on while SCL is still held - that clock is not on the lines - and the two
count the bits after it apart. The byte read there is not one the lines tell: it is left unread (complete False,
pending_bits, clock_lost), by the Python engine and the Rust one, in decode and in verify. Truth: the generator here
(what each side drove), never a device's reading."""

import importlib.util
import os

import numpy as np
import pytest

from wireskein import _engines, _rust, verify
from wireskein._engine import gen
from wireskein.decode import decode_file
from wireskein.runlog import Recorder, i2c

HAS_CORE = importlib.util.find_spec("wireskein._core") is not None
ENGINES = [{"WIRESKEIN_RUST": "none"}] + ([{"WIRESKEIN_ENGINE": "rust"}, {"WIRESKEIN_RUST": "decode"}] if HAS_CORE else [])
VERIFIERS = [{"WIRESKEIN_RUST": "none"}] + ([{"WIRESKEIN_ENGINE": "rust"}, {"WIRESKEIN_RUST": "verify"}] if HAS_CORE else [])
ADDR, TARGET_BYTE = 0x44, 0x61


def _with(env: dict, fn):
    keep = {k: os.environ.pop(k, None) for k in (_rust.RUST_ENV, _engines.ENGINE_ENV)}
    os.environ.update(env)
    try:
        return fn()
    finally:
        for k in (_rust.RUST_ENV, _engines.ENGINE_ENV):
            os.environ.pop(k, None)
            if keep[k] is not None:
                os.environ[k] = keep[k]


def stretched_read(gave_up: bool, before: list[int], freq: float = 100e3, wait: float = 300e-6):
    """A write, then a read of 0x44 whose target holds SCL low for `wait` before the byte after `before` (the bytes
    read, ACKed, ahead of the wait), its byte TARGET_BYTE, ended by a repeated START and a write.
    gave_up: the controller stops waiting part way and clocks on unseen (its first bit's clock lost in the wait):
    after the wait it clocks seven bits, its NACK and the repeated START's rise - nine rises, the first cut short as
    it was already high by its count - while the target, seeing them, shifts its eight bits on the first eight.
    Otherwise it waits: the eight bits, its NACK, and the repeated START's rise of its own.
    -> (the waves, the end, the wait's start)."""
    scl, sda = gen.Wave(1), gen.Wave(1)
    q = 1 / freq / 4                               # a quarter period
    t = gen.i2c(scl, sda, 20e-6, [{"addr": ADDR, "rw": "write", "bytes": [0x2C, 0x06]}], freq, gap=30e-6)

    def clock(v: int, high: float = 2 * q) -> None:
        nonlocal t
        sda.set(t + q, v)
        scl.set(t + 2 * q, 1)
        scl.set(t + 2 * q + high, 0)
        t += 2 * q + high

    sda.set(t, 1)                                   # START
    scl.set(t + q, 1)
    sda.set(t + 2 * q, 0)
    scl.set(t + 3 * q, 0)
    t += 3 * q
    for b in [(ADDR << 1) | 1] + before:
        for k in range(7, -1, -1):
            clock((b >> k) & 1)
        clock(0)                                    # ACK: the target's after the address, the controller's after a byte
    sda.set(t + q, 1)                               # (the target lets SDA go; the wait starts as SCL falls)
    held = t
    first = (TARGET_BYTE >> 7) & 1
    t += wait
    sda.set(t - 6e-6, first)                        # the target's first bit, set before it lets SCL go
    bits = [(TARGET_BYTE >> k) & 1 for k in range(7, -1, -1)]
    # the target lets SCL go: its first bit is clocked by the rise it sees
    scl.set(t, 1)
    high = 0.55 * 2 * q if gave_up else 2 * q
    scl.set(t + high, 0)
    t += high
    for v in bits[1:]:
        clock(v)
    if gave_up:                                     # the controller's NACK slot is the target's last bit's rise:
        sda.set(t + q, 1)                           # what follows is the repeated START's rise
        scl.set(t + 2 * q, 1)
        t += 2 * q
    else:
        clock(1)                                    # the controller's NACK
        sda.set(t + q, 1)
        scl.set(t + 2 * q, 1)
        t += 2 * q
    sda.set(t + 2 * q, 0)                           # repeated START
    scl.set(t + 3 * q, 0)
    t += 3 * q
    for b in [ADDR << 1, 0x24, 0x0B]:
        for k in range(7, -1, -1):
            clock((b >> k) & 1)
        clock(0)
    sda.set(t + q, 0)                               # STOP
    scl.set(t + 2 * q, 1)
    sda.set(t + 3 * q, 1)
    t += 4 * q + 30e-6
    return {"SCL": scl, "SDA": sda}, t, held


def _run(tmp_path, gave_up: bool, before: list[int], rate: float, expect: list[dict]):
    waves, end, held = stretched_read(gave_up, before)
    cap = gen.quantize(waves, rate, end, np.random.default_rng(0))
    lv = np.zeros(cap.n_samples, np.uint8)
    for k, ch in enumerate(cap.channels):
        e = np.asarray(ch.edges, np.int64)
        lv |= ((ch.initial ^ (np.searchsorted(e, np.arange(cap.n_samples), side="right") & 1)) & 1).astype(
            np.uint8) << k
    rec = Recorder(tmp_path / "run")
    with rec.section(1, "t", expect=[i2c("SCL", "SDA", transactions=expect)]):
        rec.capture(rec.armed(), rate, interleaved=lv.tobytes(), names=list(waves))
    rec.close()
    return tmp_path / "run", int(round(held * rate))


def _want(before: list[int], read_bytes: list[int] | None = None, **more) -> list[dict]:
    rb = before + [TARGET_BYTE] if read_bytes is None else read_bytes
    return [{"addr": ADDR, "rw": "write", "bytes": [0x2C, 0x06]},
            {"addr": ADDR, "rw": "read", "bytes": rb, "end": "Sr", **more},
            {"addr": ADDR, "rw": "write", "bytes": [0x24, 0x0B], "start": "Sr"}]


def _decoded(run, env):
    spec = {"buses": [{"protocol": "i2c", "pins": {"scl": "SCL", "sda": "SDA"}}]}
    f = next(run.glob("*.wireskein"))
    return _with(env, lambda: decode_file(f, spec))["parts"][0]["buses"][0]["result"]["transactions"]


@pytest.mark.parametrize("env", ENGINES, ids=lambda e: ",".join(f"{k}={v}" for k, v in e.items()) or "python")
@pytest.mark.parametrize("rate", [2e6, 10e6, 50e6])
@pytest.mark.parametrize("before", [[], [0x5A]], ids=["first byte", "second byte"])
def test_a_byte_read_past_a_controller_that_gave_up_is_left_unread(tmp_path, env, rate, before):
    run, held = _run(tmp_path, True, before, rate, _want(before))
    txs = _decoded(run, env)
    assert [(t["addr"], t["rw"], t["end_cond"]) for t in txs] == \
        [(ADDR, "write", "P"), (ADDR, "read", "Sr"), (ADDR, "write", "P")]
    assert txs[0]["bytes"] == "2c06" and txs[2]["bytes"] == "240b"
    t = txs[1]
    assert t["addr_ack"] is True
    assert t["bytes"] == bytes(before).hex() and t["acks"] == [True] * len(before)
    assert t["complete"] is False
    # the nine rises after the wait, as sampled: the target's eight bits, then SDA high at the repeated START's rise
    assert t["pending_bits"] == [(TARGET_BYTE >> k) & 1 for k in range(7, -1, -1)] + [1]
    assert abs(t["clock_lost"] - held) <= 2
    assert "restored" not in t


@pytest.mark.parametrize("env", ENGINES, ids=lambda e: ",".join(f"{k}={v}" for k, v in e.items()) or "python")
@pytest.mark.parametrize("rate", [2e6, 50e6])
def test_a_stretch_waited_out_reads_its_byte(tmp_path, env, rate):
    run, _ = _run(tmp_path, False, [0x5A], rate, _want([0x5A]))
    t = _decoded(run, env)[1]
    assert t["bytes"] == "5a61" and t["acks"] == [True, False]
    assert "complete" not in t and "clock_lost" not in t


@pytest.mark.parametrize("env", VERIFIERS, ids=lambda e: ",".join(f"{k}={v}" for k, v in e.items()) or "python")
def test_verify_says_neither_match_nor_difference(tmp_path, env):
    # the bytes expected past the wait: unchecked, with a notice; the words before it still compared
    run, held = _run(tmp_path, True, [0x5A], 10e6, _want([0x5A]))
    r = _with(env, lambda: verify.verify(run))["results"][0]
    assert r["status"] == "unchecked", r["reason"]
    assert "clock was lost in an SCL wait" in r["reason"]
    lost = [n for n in r["notices"] if n["kind"] == "clock_lost"]
    assert len(lost) == 1 and abs(lost[0]["tick"] - held) <= 2 and lost[0]["bits"] == 9
    got = r["measured"]["transactions"][1]
    assert got["bytes"] == [0x5A] and got["complete"] is False


@pytest.mark.parametrize("env", VERIFIERS, ids=lambda e: ",".join(f"{k}={v}" for k, v in e.items()) or "python")
def test_verify_still_fails_what_was_read_wrong(tmp_path, env):
    # a byte before the wait that differs is a difference, the cut after it notwithstanding
    run, _ = _run(tmp_path / "a", True, [0x5A], 10e6, _want([0x5A], read_bytes=[0x5B, TARGET_BYTE]))
    r = _with(env, lambda: verify.verify(run))["results"][0]
    assert r["status"] == "ng" and "transactions differ" in r["reason"]


@pytest.mark.parametrize("env", VERIFIERS, ids=lambda e: ",".join(f"{k}={v}" for k, v in e.items()) or "python")
def test_verify_a_cut_expected(tmp_path, env):
    # a test that makes the controller give up says so (complete False): the bytes before the cut compared, ok
    run, _ = _run(tmp_path, True, [0x5A], 10e6, _want([0x5A], read_bytes=[0x5A], complete=False, acks=[True]))
    r = _with(env, lambda: verify.verify(run))["results"][0]
    assert r["status"] == "ok", r["reason"]
    assert [n["kind"] for n in r["notices"]].count("clock_lost") == 1


@pytest.mark.parametrize("env", VERIFIERS, ids=lambda e: ",".join(f"{k}={v}" for k, v in e.items()) or "python")
def test_verify_a_stretch_waited_out(tmp_path, env):
    run, _ = _run(tmp_path, False, [0x5A], 10e6, _want([0x5A]))
    r = _with(env, lambda: verify.verify(run))["results"][0]
    assert r["status"] == "ok", r["reason"]
    assert not [n for n in r["notices"] if n["kind"] == "clock_lost"]
