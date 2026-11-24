"""WIRESKEIN_ENGINE: the Python engine by default (nothing changes for anyone who does not set it), the Rust core
(wireskein-core, installed apart) when it names it - its claims through the same upper stage and export. The Rust
tests run only where wireskein_core is installed. Waveforms made here."""
import importlib.util

import numpy as np
import pytest

from wireskein import _engines, decode
from wireskein._engine import gen, staged

RATE = 1e6
HAS_CORE = importlib.util.find_spec("wireskein_core") is not None
rust_only = pytest.mark.skipif(not HAS_CORE, reason="wireskein_core (crates/wireskein-py) not installed")


def _uart(text: bytes = b"hello, engines\r\nsecond line\r\n" * 4):
    w = gen.Wave(1)
    end = gen.uart(w, 50e-6, text, 115200, gap_bits=2.0)
    return gen.quantize({"TX": w}, RATE, end + 200e-6, np.random.default_rng(0))


def test_default_is_python(monkeypatch):
    monkeypatch.delenv(_engines.ENGINE_ENV, raising=False)
    monkeypatch.setattr(_engines, "rust", lambda *a, **k: pytest.fail("the Rust core read by default"))
    assert _engines.engine() == "python"
    res = decode.analyze(_uart())
    assert not isinstance(res, _engines.ForeignResult)
    assert all(isinstance(c, staged.Claim) and not hasattr(c.node, "own_layers") for c in res.claims)
    monkeypatch.setenv(_engines.ENGINE_ENV, "python")
    assert _engines.engine() == "python"


def test_an_unknown_engine_is_refused(monkeypatch):
    monkeypatch.setenv(_engines.ENGINE_ENV, "fortran")
    with pytest.raises(ValueError, match="python, rust"):
        decode.analyze(_uart())


def test_rust_without_its_package_names_it(monkeypatch):
    monkeypatch.setenv(_engines.ENGINE_ENV, "rust")
    import builtins
    real = builtins.__import__

    def no_core(name, *a, **k):
        if name == "wireskein_core":
            raise ImportError("no module named wireskein_core")
        return real(name, *a, **k)
    monkeypatch.setattr(builtins, "__import__", no_core)
    with pytest.raises(RuntimeError, match="wireskein-core"):
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
