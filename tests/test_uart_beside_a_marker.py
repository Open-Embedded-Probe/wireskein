"""A UART beside a line pulsed once a character (a test bench's marker, sigrok-dumps' uart_count_*: its `ch`): the
pulses are periodic and the UART's bits keep a phase to them, so the UART line read as synchronous data of that
"clock" and its claim was held at 0.07. Synchronous data changes once a clock period at most; the UART changes some
nine times a period. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen

BAUD, RATE = 19200, 500e3


def test_a_uart_beside_a_pulse_a_character_is_read():
    tx, ch = gen.Wave(1), gen.Wave(0)
    t, data = 1e-3, bytes(x & 0xFF for x in range(0x80, 0x80 + 200))
    for b in data:
        ch.set(t - 20e-6, 1)
        ch.set(t + 500e-6, 0)
        t = gen.uart(tx, t, bytes([b]), BAUD) + 11.75 / BAUD
    cap = gen.quantize({"tx": tx, "ch": ch}, RATE, t + 2e-3, np.random.default_rng(0))
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["protocol"] == "uart"]
    assert c["verdict"] == "confirmed" and bytes.fromhex(c["result"]["bytes"]["hex"]) == data
