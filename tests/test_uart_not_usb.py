"""A low-speed USB pair (D+ / D-) read as a UART: its NRZI bits, stuffed, frame as characters of 13 bits at 1.5 Mbaud
and were claimed likely (sigrok-dumps' USB mice and an STM32's DFU: not made here, the packets below read as no UART
anyway). The pair ends each packet with both lines low (SE0) for two bits, told here; a UART on a differential pair
(RS-485's A and B) is never so. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen

RATE, LS = 12e6, 1.5e6


def _usb_ls(dp: gen.Wave, dm: gen.Wave, t: float, packets: list[bytes]) -> float:
    """Low-speed USB packets: SYNC, the bytes LSB first, bit stuffed after six 1s, NRZI (0 toggles), EOP (SE0 for
    two bits, J for one). Idle J (low speed: D- high, D+ low)."""
    bt = 1 / LS
    for p in packets:
        bits = [0, 0, 0, 0, 0, 0, 0, 1] + [(b >> i) & 1 for b in p for i in range(8)]
        stuffed, ones = [], 0
        for v in bits:
            stuffed.append(v)
            ones = ones + 1 if v else 0
            if ones == 6:
                stuffed.append(0)
                ones = 0
        j = 1                                              # D- level; D+ is its opposite
        for v in stuffed:
            if v == 0:
                j ^= 1
            dm.set(t, j)
            dp.set(t, 1 - j)
            t += bt
        dm.set(t, 0)
        dp.set(t, 0)
        t += 2 * bt
        dm.set(t, 1)
        dp.set(t, 0)
        t += 1 * bt + 20e-6                                 # the next packet
    return t


def test_a_usb_pair_ends_its_packets_with_both_lines_low():
    from wireskein._engine.staged import usb_se0
    rng = np.random.default_rng(2)
    dp, dm = gen.Wave(0), gen.Wave(1)
    frames = []
    for _ in range(20):
        frames += [bytes([0x69, 0x81, 0x58]), bytes([0xc3, 0, int(rng.integers(0, 3)), 0, 0, 0xbf, 0x40])]
    end = _usb_ls(dp, dm, 10e-6, frames)
    cap = gen.quantize({"DP": dp, "DM": dm}, RATE, end, rng)
    assert usb_se0(cap, "DP", RATE / LS) == usb_se0(cap, "DM", RATE / LS) == len(frames)


def test_a_uart_on_a_differential_pair_stays():
    a, b = gen.Wave(1), gen.Wave(0)
    text = b"hello over RS-485\\r\\n" * 6
    gen.uart(a, 50e-6, text, 115200, gap_bits=[0, 0, 4] * len(text))
    end = gen.uart(b, 50e-6, bytes(x ^ 0xFF for x in text), 115200, gap_bits=[0, 0, 4] * len(text))
    b.initial = 0
    rng = np.random.default_rng(0)
    cap = gen.quantize({"A": a, "B": b}, 4e6, end + 200e-6, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    from wireskein._engine.staged import usb_se0
    assert usb_se0(cap, "A", 4e6 / 115200) == 0
    got = [c for c in ex["claims"] if c["protocol"] == "uart" and c["roles"]["data"] == "A"]
    assert got and got[0]["verdict"] == "confirmed" and bytes.fromhex(got[0]["result"]["bytes"]["hex"]) == text
