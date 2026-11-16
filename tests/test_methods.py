"""_engine.methods: a step of the analysis swapped within a block only (what the algorithm bench measures its trials
with); outside it, and by default, the engine's own steps."""

import json

import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen, methods, staged, survey as sv_mod, taxonomy


def _cap():
    w = gen.Wave(1)
    t = 1e-4
    for _ in range(4):
        t = gen.uart(w, t, b"hello, wire\n", 115200) + 2e-4
    return gen.quantize({"TX": w, "IDLE": gen.Wave(0)}, 2e6, t + 1e-4, np.random.default_rng(0))


def _claims(cap):
    return json.dumps(decode.export(staged.analyze(cap), cap, mode="all", alternatives=True)["claims"],
                      sort_keys=True, default=str)


def test_nothing_put_in_changes_nothing():
    cap = _cap()
    plain = _claims(cap)
    with methods.use({}):
        assert _claims(cap) == plain
    assert '"uart"' in plain


def test_a_step_is_swapped_within_the_block_only():
    calls = []

    def counting(sv, cap):
        calls.append(1)
        return taxonomy.pin_classes(sv, cap)
    cap = _cap()
    plain = _claims(cap)
    with methods.use({"pin_classes": counting}):
        assert _claims(cap) == plain           # the same classes: the same analysis
    assert calls
    n = len(calls)
    _claims(cap)
    assert len(calls) == n                     # not called outside


def test_preclean_and_pair_relation_are_steps():
    cap = _cap()
    with methods.use({"preclean": lambda c: type(c)(c.rate, c.n_samples, [type(x)(x.name, x.initial, x.edges[:0])
                                                                          for x in c.channels])}):
        assert json.loads(_claims(cap)) == []     # nothing left to read
    seen = []

    def rel(cap, clk, other):
        seen.append((clk.name, other))
        return sv_mod.pair_relation(cap, clk, other)
    clk = gen.Wave(0)
    gen.pwm(clk, 0, cap.n_samples / cap.rate, 100e3, 0.5)
    two = gen.quantize({"TX": gen.Wave(1), "CLK": clk}, cap.rate, cap.n_samples / cap.rate, np.random.default_rng(0))
    two = type(cap)(cap.rate, cap.n_samples, [cap.channel("TX"), two.channel("CLK")])
    with methods.use({"pair_relation": rel}):
        s = sv_mod.survey(two)
    assert ("CLK", "TX") in seen and s.pairs.keys() == sv_mod.survey(two).pairs.keys()
    n = len(seen)
    sv_mod.survey(two)
    assert len(seen) == n


def test_nested_blocks_and_errors_restore():
    a, b = (lambda *x: "a"), (lambda *x: "b")
    with methods.use({"preclean": a}):
        with methods.use({"pin_classes": b}):
            assert methods.get("preclean", None) is a and methods.get("pin_classes", None) is b
        assert methods.get("pin_classes", None) is None
        with pytest.raises(RuntimeError):
            with methods.use({"preclean": b}):
                raise RuntimeError
        assert methods.get("preclean", None) is a
    assert methods.get("preclean", None) is None


def test_unknown_steps_are_refused():
    with pytest.raises(KeyError):
        with methods.use({"no_such_step": print}):
            pass
    with pytest.raises(KeyError):
        methods.get("no_such_step", None)


def _i2c_cap(nack=False):
    scl, sda = gen.Wave(1), gen.Wave(1)
    tx = [{"addr": 0x13, "rw": "write", "bytes": [] if nack else [0x10, 0x20], "nack_at": 0 if nack else None}] * 20
    t = gen.i2c(scl, sda, 1e-5, tx, 100e3)
    return gen.quantize({"SCL": scl, "SDA": sda}, 4e6, t + 1e-4, np.random.default_rng(0))


def test_frame_score_and_verdict_are_steps():
    cap = _i2c_cap()
    plain = _claims(cap)
    seen, verdicts = [], []

    def score(e):
        seen.append(e["protocol"])
        from wireskein._engine.declarative import frame_score
        return frame_score(e)

    def verdict(total, margin):
        verdicts.append((total, margin))
        return staged.verdict(total, margin)
    with methods.use({"frame_score": score, "verdict": verdict}):
        assert _claims(cap) == plain             # the engine's own, called through the steps: the same claims
    assert "i2c" in seen and verdicts
    with methods.use({"verdict": lambda total, margin: "ambiguous"}):
        got = json.loads(_claims(cap))
    assert got and all(c["verdict"] in ("ambiguous", "unknown-sync") for c in got)


def test_select_frames_is_a_step():
    clk, mosi, miso, cs = gen.Wave(0), gen.Wave(0), gen.Wave(0), gen.Wave(1)
    t = gen.spi(clk, mosi, miso, cs, 1e-5, [(bytes([i, 0x5A, 0xC3]), bytes([0xA5, i, 0x3C])) for i in range(12)], 1e6)
    cap = gen.quantize({"CLK": clk, "MOSI": mosi, "MISO": miso, "CS": cs}, 20e6, t + 1e-4, np.random.default_rng(0))
    plain = _claims(cap)
    assert any(c["protocol"] == "spi" and c["roles"].get("cs") == "CS" for c in json.loads(plain))
    from wireskein._engine import typed
    seen = []

    def frames(cap, sv, sb):
        seen.append(sb.select)
        return typed.frames_select(cap, sv, sb)
    with methods.use({"select_frames": frames}):
        assert _claims(cap) == plain              # the engine's own, called through the step: the same claims
    assert "CS" in seen
