"""A UART of 7 data bits and a parity is as long as one of 8 without: read as 8, the parity was taken for the eighth
data bit and the text came out wrong though confirmed (sigrok-dumps' hello_world 7E1 / 7O1, a meter's 2400 7O1).
The parity is told by the last bit being the parity of the others; a line whose last bit never changes (ASCII sent as
8N1) has none. Waveforms made here."""
import numpy as np
import pytest

from wireskein._engine import gen
from wireskein.decode import analyze_file

from test_protocols import claims, write

TEXT = b"Hello World!\r\n$GPGGA,123519,4807.038,N,01131.000,E*47\r\n" * 3


@pytest.mark.parametrize("bits,parity", [(7, "even"), (7, "odd"), (8, "even"), (8, "odd"), (8, "none")])
def test_the_text_is_read_with_its_parity(tmp_path, bits, parity):
    tx = gen.Wave(1)
    t = gen.uart(tx, 50e-6, TEXT, 19200, data_bits=bits, parity=parity, gap_bits=[0.0, 0.0, 3.0] * len(TEXT))
    doc = analyze_file(write(tmp_path, {"TX": tx}, t + 500e-6), depth="frames")
    (c,) = [c for c in claims(doc) if c["protocol"] == "uart"]
    assert bytes.fromhex(c["result"]["bytes"]["hex"]) == TEXT
    assert c["params"].get("parity", "none") == parity
    if parity != "none":
        assert c["params"]["data_bits"] == bits


def test_a_last_bit_that_never_changes_is_no_parity():
    from wireskein._engine.staged import uart_parity
    v = np.frombuffer(TEXT, np.uint8).astype(np.int64)          # 8N1 ASCII: the eighth bit 0
    assert uart_parity(v, 8) is None
    rng = np.random.default_rng(0)
    assert uart_parity(rng.integers(0, 256, 200), 8) is None     # binary 8N1: the eighth bit no parity
