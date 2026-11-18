"""Analyzers of one line told by pulse widths (pulsecode.py), on the line itself ("line"): an RGB LED chain, a
1-Wire bus, an IR remote's NEC frames. The evidence is each protocol's own structure: whole bytes in steady bit
periods, resets answered by presence pulses, 32 bits with their inverse."""

from __future__ import annotations

import math

import numpy as np

from .. import pulsecode as pc
from ..scoring import degenerate
from ..stack import Node, Stream


def q(n: float, scale: float) -> float:
    return 1.0 - math.exp(-n / scale)


class RgbLedPlugin:
    name = "rgb_led"
    consumes = ("line",)

    def run(self, c, ch, g):
        if ch.initial != 0:
            return []
        r = pc.rgb_led(ch, c.cap.n_samples, c.cap.rate)
        fr = [f for f in r["frames"] if f["bits"] >= 8]
        if not fr:
            return []
        bit_s = r["bit"] / c.cap.rate
        if not 0.5e-6 <= bit_s <= 3e-6:
            return []
        per = np.array([d for _, d in r["period"]], np.float64)
        steady = float(np.mean(np.abs(per / r["bit"] - 1) < 0.25)) if len(per) else 0.0
        whole = float(np.mean([f["leftover_bits"] == 0 for f in fr]))
        bits = sum(f["bits"] for f in fr)
        data = bytes.fromhex("".join(f["data"] for f in fr))
        # both kinds of bit: a PWM of one duty reads as all 0s (or all 1s)
        mixed = 1.0 if r["t0h"] and r["t1h"] else 0.3
        score = steady * whole * mixed * q(bits / 24, 2)
        m = {"frames": len(fr), "bits": bits, "steady": steady, "whole_bytes": whole, "bit_s": bit_s}
        return [Node("rgb_led", {"bit_s": bit_s}, {"data": ch.name},
                     Stream("led.frames", {"frames": fr, "split": r["split"], "bytes": data.hex()}), m,
                     layer_score=score, total=score)]


ONEWIRE_PACE = 0.25   # a slot period within this of the median keeps the master's pace


class OneWirePlugin:
    name = "onewire"
    consumes = ("line",)

    def run(self, c, ch, g):
        if ch.initial != 1:
            return []
        best = None
        for speed in ("standard", "overdrive"):
            r = pc.onewire(ch, c.cap.n_samples, c.cap.rate, speed)
            rs = r["resets"]
            # slots of both kinds (a clock's lows are all alike), whole bytes after most resets
            if not rs or not r["low_0"] or not r["low_1"]:
                continue
            pres = float(np.mean([z["presence"] for z in rs]))
            whole = float(np.mean([z["leftover_bits"] == 0 and z["bits"] > 0 for z in rs]))
            if whole < 0.5:
                continue
            bits = sum(z["bits"] for z in rs)
            # a master starts its slots at one pace (the slot time), whatever the bits: the periods from one slot's
            # start to the next keep to their median; a UART's lows read as slots come whole bits apart, scattered
            per = np.array([p for _, p in r["slot"]], np.float64)
            paced = float(np.mean(np.abs(per / np.median(per) - 1) <= ONEWIRE_PACE)) if len(per) else 0.0
            # a clock's lows are all alike: its "bytes" are one value (0x00 or 0xff), a master's commands and data vary
            data = np.frombuffer(bytes.fromhex("".join(z["data"] for z in rs)), np.uint8)
            score = ((0.6 + 0.4 * pres) * whole * q(bits / 8, 2) * q(len(rs), 1) * float(np.clip((paced - 0.5) / 0.3, 0, 1))
                     * degenerate(data))
            if score >= 0.2 and (best is None or score > best[0]):
                best = (score, speed, r)
        if best is None:
            return []
        score, speed, r = best
        rs = r["resets"]
        per = np.array([p for _, p in r["slot"]], np.float64)
        m = {"resets": len(rs), "presence": sum(z["presence"] for z in rs), "bits": sum(z["bits"] for z in rs),
             "paced": round(float(np.mean(np.abs(per / np.median(per) - 1) <= ONEWIRE_PACE)), 3) if len(per) else 0.0}
        return [Node("onewire", {"speed": speed}, {"dq": ch.name}, Stream("onewire.resets", rs), m,
                     layer_score=score, total=score)]


class NecPlugin:
    name = "ir_nec"
    consumes = ("line",)

    def run(self, c, ch, g):
        r = pc.nec(ch, c.cap.n_samples, c.cap.rate)
        fr = r["frames"]
        data = [f for f in fr if not f["repeat"]]
        good = [f for f in data if f.get("cmd_ok")]
        if not good:
            return []
        score = len(good) / len(data) * q(2 * len(good) + 0.5 * (len(fr) - len(data)), 1)
        m = {"frames": len(data), "repeats": len(fr) - len(data), "cmd_ok": len(good),
             "carrier_hz": r.get("carrier_hz")}
        return [Node("ir_nec", {"carrier_hz": r.get("carrier_hz"), "idle": r["idle"]}, {"data": ch.name},
                     Stream("ir.frames", fr), m, layer_score=score, total=score)]
