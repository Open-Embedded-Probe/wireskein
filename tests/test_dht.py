"""A DHT11 / DHT22 (AM2302) humidity sensor's line (analyzers.pulse.DhtPlugin): 40 bits told by each high's length
after the host's start and the sensor's answer, the fifth byte the sum of the four. Waveforms made here (gen.dht)."""
import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen


def _read(readings, rate=1e6):
    w = gen.Wave(1)
    end = gen.dht(w, 1e-3, readings, period=0.05)
    cap = gen.quantize({"SDA": w}, rate, end, np.random.default_rng(0))
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    return [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]


@pytest.mark.parametrize("rate", [100e3, 1e6, 24e6])
def test_dht22_readings(rate):
    # 53.1 %, 25.5 C; 52.0 %, -3.2 C
    (c,) = _read([bytes([0x02, 0x13, 0x00, 0xff]), bytes([0x02, 0x08, 0x80, 0x20])] * 2, rate)
    assert c["protocol"] == "dht" and c["verdict"] == "confirmed" and c["params"]["model"] == "dht22"
    assert [(r["rh"], r["t"]) for r in c["result"]["readings"]] == [(53.1, 25.5), (52.0, -3.2)] * 2


def test_dht11_readings_and_one_alone_is_likely():
    (c,) = _read([bytes([45, 0, 23, 4])] * 3)
    assert c["params"]["model"] == "dht11" and c["result"]["readings"][0]["rh"] == 45 and c["result"]["readings"][0]["t"] == 23.4
    (c,) = _read([bytes([45, 0, 23, 4])])
    assert c["verdict"] == "likely"
