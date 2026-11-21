"""1-Wire's overdrive: after an Overdrive Match ROM (0x69) or Overdrive Skip ROM (0x3C) at standard speed the devices
run at overdrive (slots of 10 us, resets of 70 us) until a reset of standard length. sigrok-dumps' SoCkit master read
as all ones after the 0x69 before. Waveforms made here."""
import numpy as np

from wireskein import decode
from wireskein._engine import gen

OD = {"reset": 75e-6, "low_0": 8e-6, "low_1": 1.2e-6, "slot": 11e-6}


def _od_bytes(w, t, data):
    for b in data:
        for k in range(8):
            w.set(t, 0)
            w.set(t + (OD["low_1"] if (b >> k) & 1 else OD["low_0"]), 1)
            t += OD["slot"]
    return t


def _od_reset(w, t):
    w.set(t, 0)
    w.set(t + OD["reset"], 1)
    w.set(t + OD["reset"] + 3e-6, 0)            # the presence pulse
    w.set(t + OD["reset"] + 13e-6, 1)
    return t + 2 * OD["reset"]


def test_overdrive_after_its_rom_command():
    rng = np.random.default_rng(2)
    rom = bytes([0x28]) + bytes(rng.integers(0, 256, 7).tolist())
    w = gen.Wave(1)
    t, want = 20e-6, []
    for _ in range(3):
        # standard: Overdrive Match ROM, then at overdrive the ROM and a command; an overdrive reset, Skip ROM, data
        t = gen.onewire(w, t, bytes([0x69]))
        t -= 100e-6
        cmd = bytes([0xBE]) + bytes(rng.integers(0, 256, 9).tolist())
        t = _od_bytes(w, t, rom + cmd) + 30e-6
        want.append((bytes([0x69]) + rom + cmd).hex())
        t = _od_reset(w, t)
        more = bytes([0xCC, 0x44])
        t = _od_bytes(w, t, more) + 30e-6
        want.append(more.hex())
        # a standard reset brings the devices back
        data = bytes([0xCC, 0xBE]) + bytes(rng.integers(0, 256, 4).tolist())
        t = gen.onewire(w, t + 100e-6, data) + 200e-6
        want.append(data.hex())
    cap = gen.quantize({"OWR": w}, 10e6, t, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "onewire"
    assert [r["data"] for r in c["result"]["resets"]] == want


def test_data_of_0x69_at_standard_speed_stays_standard():
    """A reset whose first byte is 0x69 (data, or a command no device took) followed by slots at standard speed:
    no switch to overdrive (the lows after it read at standard)."""
    rng = np.random.default_rng(3)
    w = gen.Wave(1)
    t, want = 20e-6, []
    for k in range(6):
        data = (bytes([0x69]) if k % 2 == 0 else bytes([0xCC])) + bytes(rng.integers(0, 256, 6).tolist())
        t = gen.onewire(w, t, data) + 200e-6
        want.append(data.hex())
    cap = gen.quantize({"OWR": w}, 10e6, t, rng)
    ex = decode.export(decode.analyze(cap), cap, mode="final", depth="frames")
    (c,) = [c for c in ex["claims"] if c["verdict"] in ("confirmed", "likely")]
    assert c["protocol"] == "onewire"
    assert [r["data"] for r in c["result"]["resets"]] == want
    assert not any(r.get("speed") for r in c["result"]["resets"])
