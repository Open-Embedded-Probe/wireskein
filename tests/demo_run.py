"""A recorded run shaped like the ArduinoCore-CH32 x035 trace tests, with bugs
injected (used by tests/test_verify.py; also a script):

    uv run python tests/demo_run.py OUT_DIR && uv run wireskein verify OUT_DIR

Steps follow tests/manual/oep_periph_trace and oep_i2c_trace (rates, sample
counts, commands, replies): PWM at 2 MHz x 40 000 samples (x035 measures
1003.5 Hz), tone 440 Hz, I2C WRITE at 100 kHz captured at 1 MHz x 60 000,
SPI 1 MHz mode 0 at 5 MHz x 32 500 (4 channels). Injected: duty 192 comes out
at 70 %, PA0 toggles during the tone step, the second I2C write leaves SDA low.
"""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np

from wireskein._engine import gen
from wireskein.runlog import Recorder, i2c, level, only_moving, spi, square

rng = np.random.default_rng(5)


def dense(waves: dict, names: list[str], rate: float, n: int) -> bytes:
    cap = gen.quantize({k: waves[k] for k in names}, rate, n / rate, rng)
    out = np.zeros(n, np.uint8)
    for k, name in enumerate(names):
        ch = cap.channel(name)
        out |= (ch.level_at(np.arange(n)).astype(np.uint8) & 1) << k
    return out.tobytes()


def main(out: Path) -> Path:
    rec = Recorder(out, target="x035", note="synthetic (tests/demo_run.py)")
    with rec.section(1, "test_pwm"):
        for duty in (64, 128, 192, 255, 0):
            want = [square("PA1", 1003.5, duty / 255, tol_freq=0.01, tol_duty=0.01) if duty not in (0, 255)
                    else level("PA1", 1 if duty == 255 else 0), only_moving(["PA1"])]
            with rec.section(2, f"duty={duty}", expect=want):
                rec.command(f"PWM {duty}")
                rec.reply(f"PWM duty={duty}")
                rate, n = 2_000_000, 40_000
                w = {"PA1": gen.Wave(0), "PA0": gen.Wave(0)}
                real = 0.70 if duty == 192 else duty / 255                 # injected: duty 192 -> 70 %
                if duty == 255:
                    w["PA1"] = gen.Wave(1)
                elif duty:
                    gen.pwm(w["PA1"], 13e-6, n / rate, 1003.5, real)
                t = rec.armed()
                rec.capture(dense(w, ["PA1", "PA0"], rate, n), rate, ["PA1", "PA0"], t)
    with rec.section(1, "test_tone"):
        with rec.section(2, "440Hz", expect=[square("PA1", 440, 0.5, tol_freq=0.005, tol_duty=0.02), only_moving(["PA1"])]):
            rec.command("TONE 440")
            rec.reply("TONE hz=440")
            rate, n = 2_000_000, 60_000
            w = {"PA1": gen.Wave(0), "PA0": gen.Wave(0)}
            gen.pwm(w["PA1"], 7e-6, n / rate, 440.2, 0.5)
            gen.pwm(w["PA0"], 10e-3, 10.4e-3, 5000, 0.5)                 # injected: PA0 moves
            t = rec.armed()
            rec.capture(dense(w, ["PA1", "PA0"], rate, n), rate, ["PA1", "PA0"], t)
    with rec.section(1, "test_i2c_write"):
        for case, (addr, stuck) in enumerate([(0x42, False), (0x42, True), (0x43, False)]):
            payload = [(0x10 * case + i) & 0xFF for i in range(4)]
            ack = addr == 0x42
            tx = [{"addr": addr, "rw": "write", "bytes": payload if ack else [], "ack": ack}]
            with rec.section(2, f"case{case} addr={addr:02x}", expect=[i2c("PC16", "PC17", tx, hz=100e3)]):
                rate, n = 1_000_000, 60_000
                w = {"PC16": gen.Wave(1), "PC17": gen.Wave(1)}
                gtx = {"addr": addr, "rw": "write", "bytes": payload}
                if not ack:
                    gtx = {"addr": addr, "rw": "write", "bytes": [], "nack_at": 0}
                t_end = gen.i2c(w["PC16"], w["PC17"], 2e-3, [gtx], 100e3, gap=100e-6)
                if stuck:
                    w["PC17"].set(t_end + 50e-6, 0)                         # injected: SDA left low
                t = rec.armed()
                rec.command(f"WRITE 2 100000 {addr:02x} {bytes(payload).hex()}")
                rec.reply(f"WRITE rc={0 if ack else 2} route=2 hz=100000 n={4 if ack else 0}")
                rec.capture(dense(w, ["PC16", "PC17"], rate, n), rate, ["PC16", "PC17"], t)
    with rec.section(1, "test_spi"):
        mosi, miso = bytes.fromhex("9f000000"), bytes.fromhex("ffef4017")
        with rec.section(2, "1MHz mode0", expect=[spi("PA5", "PA7", "PA6", "PA4", mode=0, mosi_bytes=mosi.hex(),
                                                        miso_bytes=miso.hex(), hz=1e6)]):
            rate, n = 5_000_000, 32_500
            w = {"PA5": gen.Wave(0), "PA7": gen.Wave(0), "PA6": gen.Wave(0), "PA4": gen.Wave(1)}
            t = rec.armed()
            rec.command(f"SPI 1000000 0 {mosi.hex()}")
            gen.spi(w["PA5"], w["PA7"], w["PA6"], w["PA4"], 200e-6, [(mosi, miso)], 1e6, 0, "msb")
            rec.reply(f"SPI got={miso.hex()}")
            rec.capture(dense(w, ["PA5", "PA7", "PA6", "PA4"], rate, n), rate, ["PA5", "PA7", "PA6", "PA4"], t)
    # a marker mistake: a phase heading without its step level
    rec.heading(1, "test_misc")
    rec.heading(3, "orphan-phase")
    rec.heading(1)
    return rec.close()


if __name__ == "__main__":
    print(main(Path(sys.argv[1])))
