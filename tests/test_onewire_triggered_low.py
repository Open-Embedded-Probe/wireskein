"""A 1-Wire capture triggered on a reset's fall starts low (sigrok-dumps' DS1985 polling, a SoCkit master, a DS2432):
the bus was taken for no 1-Wire, and its line claimed as a UART. A bus idle high is high most of the time, however
it starts. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen


def test_a_capture_starting_inside_a_reset():
    w = gen.Wave(1)
    rng = np.random.default_rng(4)
    t = 10e-6
    for _ in range(8):
        t = gen.onewire(w, t, bytes([0xCC, 0x44]) + bytes(rng.integers(0, 256, 6).tolist())) + 1e-3
    cap = gen.quantize({"OWR": w}, 1e6, t, rng, t_start=10e-6 + 200e-6)       # inside the first reset's low
    assert cap.channels[0].initial == 0
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    got = [(c["protocol"], c["verdict"]) for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert got == [("onewire", "confirmed")]
