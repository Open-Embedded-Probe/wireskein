"""A capture started inside a character of back-to-back characters (a GPS's NMEA at 9600) has no start bit where it
begins: read from there, every character after stayed a bit off (the stop bits on the eighth data bits of ASCII,
always 0 then 1) and was confirmed. Read from the line's first start bit, they frame. Waveforms made here."""
import numpy as np
import pytest

from wireskein import decode
from wireskein._engine import gen

TEXT = b"$GPGSV,4,2,14,11,34,303,46,18,22,051,32,19,39,253,44*71\r\n$GPGGA,123519,4807.038,N,01131.000,E,1,08*47\r\n" * 4
BAUD, RATE = 9600, 200e3


@pytest.mark.parametrize("seed", range(6))
def test_back_to_back_text_cut_inside_a_character(seed):
    rng = np.random.default_rng(seed)
    tx = gen.Wave(1)
    gaps = rng.uniform(0, 0.6, len(TEXT))                     # stop bits a little long now and then, as a UART's
    gaps[np.frombuffer(TEXT, np.uint8) == 10] = 400            # and a pause after each sentence
    gaps = gaps.tolist()
    end = gen.uart(tx, 0.0, TEXT, BAUD, gap_bits=gaps)
    t = np.asarray(tx.times)
    lows = [(a, b) for a, b in zip(t[0::2], t[1::2])]          # the line's active runs (idle high)
    a, b = lows[int(rng.integers(40, 80))]
    t0 = a + (b - a) * 0.5                                      # inside one, past its start bit
    cap = gen.quantize({"TX": tx}, RATE, end + 2e-3, rng, t_start=t0)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["protocol"] == "uart"]
    got = bytes.fromhex(c["result"]["bytes"]["hex"])
    k = TEXT.find(got[:12])
    assert k > 0 and TEXT[k:].startswith(got) and len(got) >= len(TEXT) - k - 1
