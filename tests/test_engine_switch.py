"""WIRESKEIN_ENGINE: the Rust core (wireskein._core, built into the wheel) by default - its claims through the same
upper stage and export - and the Python engine when it names it. The Rust tests run only where the Rust core is
built. Waveforms made here."""
import importlib.util

import numpy as np
import pytest

from wireskein import _engines, decode
from wireskein._engine import gen, staged

RATE = 1e6
HAS_CORE = importlib.util.find_spec("wireskein._core") is not None
rust_only = pytest.mark.skipif(not HAS_CORE, reason="the Rust core (wireskein._core) not built")


@pytest.fixture(autouse=True)
def _only_engine_env(monkeypatch):
    """These tests name the engine by WIRESKEIN_ENGINE alone: WIRESKEIN_RUST (whose part `engine` is the same switch,
    and contradicts WIRESKEIN_ENGINE=python) is cleared, as when the whole suite runs under WIRESKEIN_RUST=all."""
    from wireskein import _rust
    monkeypatch.delenv(_rust.RUST_ENV, raising=False)


def _uart(text: bytes = b"hello, engines\r\nsecond line\r\n" * 4):
    w = gen.Wave(1)
    end = gen.uart(w, 50e-6, text, 115200, gap_bits=2.0)
    return gen.quantize({"TX": w}, RATE, end + 200e-6, np.random.default_rng(0))


@rust_only
def test_default_is_rust(monkeypatch):
    monkeypatch.delenv(_engines.ENGINE_ENV, raising=False)
    assert _engines.engine() == "rust"
    assert isinstance(decode.analyze(_uart()), _engines.ForeignResult)


def test_python_named_is_python(monkeypatch):
    monkeypatch.setenv(_engines.ENGINE_ENV, "python")
    monkeypatch.setattr(_engines, "rust", lambda *a, **k: pytest.fail("the Rust core read under python"))
    assert _engines.engine() == "python"
    res = decode.analyze(_uart())
    assert not isinstance(res, _engines.ForeignResult)
    assert all(isinstance(c, staged.Claim) and not hasattr(c.node, "own_layers") for c in res.claims)


def test_an_unknown_engine_is_refused(monkeypatch):
    monkeypatch.setenv(_engines.ENGINE_ENV, "fortran")
    with pytest.raises(ValueError, match="python, rust"):
        decode.analyze(_uart())


def test_rust_without_its_package_names_it(monkeypatch):
    import sys
    import wireskein
    monkeypatch.setenv(_engines.ENGINE_ENV, "rust")
    monkeypatch.setitem(sys.modules, "wireskein._core", None)
    monkeypatch.delattr(wireskein, "_core", raising=False)
    with pytest.raises(RuntimeError, match=r"wireskein\._core"):
        decode.analyze(_uart())


@rust_only
def test_rust_claims_through_the_upper_stage(monkeypatch):
    monkeypatch.setenv(_engines.ENGINE_ENV, "rust")
    cap = _uart()
    res = decode.analyze(cap)
    assert isinstance(res, _engines.ForeignResult)
    ex = decode.export(res, cap, mode="all")
    (c,) = [c for c in ex["claims"] if c["protocol"] == "uart"]
    assert c["roles"] == {"data": "TX"} and c["verdict"] == "confirmed"
    assert bytes.fromhex(c["layers"]["bytes"]["hex"]).startswith(b"hello, engines")
    assert c["layers"]["lines"]                       # the upper stage's lines on the bytes
    fin = decode.export(res, cap, mode="final")["claims"]
    assert any("lines" in x.get("result", {}) for x in fin)
    doc = decode.annotations(cap)
    assert doc["rows"]


@rust_only
def test_rust_and_python_agree_on_a_clean_bus(monkeypatch):
    cap = _uart()
    monkeypatch.setenv(_engines.ENGINE_ENV, "python")
    py = decode.export(decode.analyze(cap), cap, mode="final")["claims"]
    monkeypatch.setenv(_engines.ENGINE_ENV, "rust")
    rs = decode.export(decode.analyze(cap), cap, mode="final")["claims"]
    assert [(c["protocol"], c["roles"], c["verdict"]) for c in rs] == [(c["protocol"], c["roles"], c["verdict"]) for c in py]
    assert rs[0]["result"] == py[0]["result"]
