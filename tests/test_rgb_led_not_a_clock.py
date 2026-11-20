"""A clock of 7 samples a period has highs of 3 and 4 samples, cut so by the samples: two kinds of width, as an LED
chain's 0s and 1s (an MDIO's MDC at 1.7 MHz captured at 12 MS/s was claimed as WS2812 data, 0.98). An LED's 1 is
about twice its 0 (WS2812 0.8 / 0.4 us); the clock's are a third apart. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen


def test_a_clock_of_highs_a_sample_apart_is_no_led_chain():
    w = gen.Wave(0)
    t, f = 10e-6, 12e6 / 7
    for burst in range(20):
        for k in range(64):
            w.set(t, 1)
            w.set(t + 0.5 / f, 0)
            t += 1 / f
        t += 40e-6
    cap = gen.quantize({"MDC": w}, 12e6, t, np.random.default_rng(0))
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    assert not [c for c in ex["claims"] if c["protocol"] == "rgb_led" and c["verdict"] in ("confirmed", "likely")]


def test_an_led_chain_stays():
    w = gen.Wave(0)
    rng = np.random.default_rng(1)
    end = gen.rgb_led(w, 10e-6, bytes(rng.integers(0, 256, 90).tolist()))
    cap = gen.quantize({"DIN": w}, 12e6, end + 100e-6, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    assert [c["verdict"] for c in ex["claims"] if c["protocol"] == "rgb_led"] == ["confirmed"]
