"""Analyzers of one line told by pulse widths (pulsecode.py), on the line itself ("line"): an RGB LED chain, a
1-Wire bus, an IR remote's NEC frames. The evidence is each protocol's own structure: whole bytes in steady bit
periods, resets answered by presence pulses, 32 bits with their inverse."""

from __future__ import annotations

import math
from dataclasses import replace

import numpy as np

from .. import kernels as K
from .. import pulsecode as pc
from ..scoring import degenerate
from ..stack import Node, Stream


def q(n: float, scale: float) -> float:
    return 1.0 - math.exp(-n / scale)


RGB_SHORTEST = 0.15e-6  # s: no pulse of a WS2812-like line is shorter (T0H 0.25 us or more)
RGB_MIXED = 0.1         # the rarer kind of bit's share a string's data has at least


class RgbLedPlugin:
    name = "rgb_led"
    consumes = ("line",)

    def run(self, c, ch, g):
        if ch.initial != 0:
            return []
        # the line as it is, and with pulses shorter than any of a WS2812-like line (T0H from 0.25 us; RGB_SHORTEST)
        # taken out: a bounce at an edge is a bit of its own
        k = int(np.ceil(RGB_SHORTEST * float(c.cap.rate) - 1e-9))
        views = [(ch, 0)]
        if k >= 2:
            d = K.deglitch(np.asarray(ch.edges, np.int64), k)
            if len(d) != len(ch.edges):
                views.append((replace(ch, edges=d), k))
        nodes = [n for line, dg in views for n in self._read(c, line, dg, ch.name)]
        return [max(nodes, key=lambda n: n.total)] if nodes else []

    def _read(self, c, ch, dg, name):
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
        # both kinds of bit: a PWM of one duty (a clock) reads as all 0s (or all 1s), but for what a noise leaves in
        # it; a string's colours have a tenth of the rarer kind or more
        minor = min(len(r["t0h"]), len(r["t1h"])) / max(1, len(r["t0h"]) + len(r["t1h"]))
        mixed = 0.3 + 0.7 * float(np.clip(minor / RGB_MIXED, 0, 1))
        # T0H and T1H differ by about a third of the bit: under 5 samples a bit, a spike's tick is as wide as that
        # difference (a clock at 2 MS/s with spikes read as varied bits)
        resolvable = float(np.clip(r["bit"] - 4, 0, 1))
        score = steady * whole * mixed * q(bits / 24, 2) * resolvable
        m = {"frames": len(fr), "bits": bits, "steady": steady, "whole_bytes": whole, "bit_s": bit_s,
             "rarer_bits": round(minor, 3), "samples_per_bit": round(float(r["bit"]), 2)}
        return [Node("rgb_led", {"bit_s": bit_s, "deglitch": dg}, {"data": name},
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
        # the line as it is, and with pulses shorter than the shortest 1-Wire has (low_1's least, 1 us: a spike, a
        # bounce) taken out: such a pulse is a slot of its own or splits a low or a reset
        k = int(np.ceil(min(v["low_1"][0] for v in pc.ONEWIRE.values()) * float(c.cap.rate) - 1e-9))
        views = [(ch, 0)]
        if k >= 2:
            d = K.deglitch(np.asarray(ch.edges, np.int64), k)
            if len(d) != len(ch.edges):
                views.append((replace(ch, edges=d), k))
        for (line, dg), speed in ((v, s) for v in views for s in ("standard", "overdrive")):
            r = pc.onewire(line, c.cap.n_samples, c.cap.rate, speed)
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
                best = (score, speed, r, dg)
        if best is None:
            return []
        score, speed, r, dg = best
        rs = r["resets"]
        per = np.array([p for _, p in r["slot"]], np.float64)
        m = {"resets": len(rs), "presence": sum(z["presence"] for z in rs), "bits": sum(z["bits"] for z in rs),
             "paced": round(float(np.mean(np.abs(per / np.median(per) - 1) <= ONEWIRE_PACE)), 3) if len(per) else 0.0}
        return [Node("onewire", {"speed": speed, "deglitch": dg}, {"dq": ch.name}, Stream("onewire.resets", rs), m,
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
