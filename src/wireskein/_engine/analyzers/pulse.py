"""Analyzers of one line told by pulse widths (pulsecode.py), on the line itself ("line"): an RGB LED chain, a
1-Wire bus, an IR remote's NEC frames. The evidence is each protocol's own structure: whole bytes in steady bit
periods, resets answered by presence pulses, 32 bits with their inverse."""

from __future__ import annotations

import math
from dataclasses import replace

import numpy as np

from .. import views as V
from .. import pulsecode as pc
from ..scoring import degenerate
from ..stack import Node, Stream


def q(n: float, scale: float) -> float:
    return 1.0 - math.exp(-n / scale)


def _bounce_view(ch, rate: float):
    """The line with each edge's bounces merged at its first change, when its short runs (under 4 samples) come in
    pairs (a gap after the edge, the pulse back: a bounce; a spike is one), else None. The deglitched views pair a
    bounce's pulse with the edge before it and move the edge by the bounce."""
    e = np.asarray(ch.edges, np.int64)
    if len(e) < 16:
        return None
    if not V.bounces_paired(np.diff(e), V.BOUNCE_TICKS * ch.step):
        return None
    d = V.bounces_merged(e, V.BOUNCE_MERGE * ch.step)
    return replace(ch, edges=d) if len(d) != len(e) else None


RGB_SHORTEST = 0.15e-6  # s: no pulse of a WS2812-like line is shorter (T0H 0.25 us or more)
RGB_ONE_OVER_ZERO = 1.5  # a 1's high over a 0's, this many times at least
RGB_MIXED = 0.1         # the rarer kind of bit's share a string's data has at least
RGB_T1H_MAX = 0.9         # a 1's high, this share of the bit at most
RGB_T0H_MIN = 0.1         # a 0's high, this share of the bit at least
RGB_CUT_HIGH_S = 2e-6     # a capture begun inside a bit: its first fall within this
RGB_SAME_FRAMES = 4     # frames of one length (a whole number of LEDs) for a dark string's evidence


class RgbLedPlugin:
    name = "rgb_led"
    consumes = ("line",)

    def run(self, c, ch, g):
        if ch.initial != 0:
            # idle is low: a capture begun inside a high (a bit's) is read from its first fall, the cut pulse left out
            # (sigrok-dumps' ws281x_4ch_5mhz begins so on all four lines)
            # (a bit's high is 1 us at most: a line high for longer at the start rests high, no LED string's - a USB
            # D+, an SPI line idle high)
            if len(ch.edges) < 2 or ch.edges[0] > RGB_CUT_HIGH_S * float(c.cap.rate):
                return []
            ch = replace(ch, initial=0, edges=np.asarray(ch.edges)[1:])
        # the line as it is, and with pulses shorter than any of a WS2812-like line (T0H from 0.25 us; RGB_SHORTEST)
        # taken out: a bounce at an edge is a bit of its own
        k = max(2, int(np.ceil(RGB_SHORTEST * float(c.cap.rate) - 1e-9)))
        views = [(ch, 0)]
        if k >= 2:
            d = V.spikes_out(np.asarray(ch.edges, np.int64), k)
            if len(d) != len(ch.edges):
                views.append((replace(ch, edges=d), k))
        b = _bounce_view(ch, c.cap.rate)
        if b is not None:
            views.append((b, -4))
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
        # (counted in the frames read: a clock's spikes make pieces of a few bits whose highs are of the other kind,
        # 14 % of all the highs beside 3 % of the frames' bits; 2026-11-21, synth-mixed-nack-99 with spikes)
        ones = int(np.unpackbits(np.frombuffer(data, np.uint8)).sum()) if data else 0
        minor = min(ones, 8 * len(data) - ones) / max(1, 8 * len(data))
        # a 1's high is about twice a 0's (WS2812 0.8 / 0.4 us, SK6812 0.6 / 0.3): a clock's highs of 3 and 4 samples
        # (7 a period, cut by the samples) are two kinds of width as well, a third apart (an MDIO's MDC at 1.7 MHz)
        if r["t0h"] and r["t1h"]:
            ratio = float(np.median([w for _, w in r["t1h"]]) / max(1e-9, np.median([w for _, w in r["t0h"]])))
            if ratio < RGB_ONE_OVER_ZERO:
                return []
            # and a 0's high is a good part of the bit (T0H 0.2-0.4 us of 1.2): "0s" of a few ticks in a slow bit are
            # a noise's pieces, a bounced clock's highs split (2026-11-22, a 1 MHz decoy at 200 MS/s with bounces)
            if float(np.median([w for _, w in r["t0h"]])) < RGB_T0H_MIN * r["bit"]:
                return []
            # and a 1's high is inside its bit (T1H 0.6-0.8 us of 1.2): "1s" longer than the bit are a line's long highs
            # between pulses of one width (a CPU's address line: 519 samples beside a bit of 21, sigrok-dumps' i8039)
            if float(np.median([w for _, w in r["t1h"]])) > RGB_T1H_MAX * r["bit"]:
                return []
        mixed = 0.3 + 0.7 * float(np.clip(minor / RGB_MIXED, 0, 1))
        # a string all dark (every bit 0) is one duty too: but its frames, between resets, are of one length, a whole
        # number of LEDs (24 or 32 bits each) every time, which a clock's bursts are not by chance
        lens = [f["bits"] for f in fr]
        mode = max(set(lens), key=lens.count)
        if len(fr) >= RGB_SAME_FRAMES and (mode % 24 == 0 or mode % 32 == 0):
            mixed = max(mixed, 0.3 + 0.7 * lens.count(mode) / len(fr))
        # T0H and T1H differ by about a third of the bit: under 5 samples a bit, a spike's tick is as wide as that
        # difference (a clock at 2 MS/s with spikes read as varied bits)
        resolvable = float(np.clip(r["bit"] - 4, 0, 1))
        score = steady * whole * mixed * q(bits / 24, 2) * resolvable
        m = {"frames": len(fr), "bits": bits, "steady": steady, "whole_bytes": whole, "bit_s": bit_s,
             "rarer_bits": round(minor, 3), "samples_per_bit": round(float(r["bit"]), 2)}
        return [Node("rgb_led", {"bit_s": bit_s, "deglitch": dg}, {"data": name},
                     Stream("led.frames", {"frames": fr, "split": r["split"], "bytes": data.hex()}), m,
                     layer_score=score, total=score)]


ONEWIRE_PRESENCE = 2     # presence pulses seen: this many count as every reset answered
ONEWIRE_PACE = 0.25   # a slot period within this of the median keeps the master's pace


def _runs_once(v: np.ndarray) -> np.ndarray:
    """The values with each run of one value kept once."""
    return v[np.concatenate(([True], v[1:] != v[:-1]))] if len(v) else v


class OneWirePlugin:
    name = "onewire"
    consumes = ("line",)

    def run(self, c, ch, g):
        # a bus idle high: high most of the time, though a capture triggered on a reset's fall starts low
        if ch.initial != 1:
            _, length, level = ch.runs(c.cap.n_samples)
            if float(np.sum(np.asarray(length)[np.asarray(level) == 1])) < 0.5 * c.cap.n_samples:
                return []
        best = None
        # the line as it is, and with pulses shorter than the shortest 1-Wire has (low_1's least, 1 us: a spike, a
        # bounce) taken out: such a pulse is a slot of its own or splits a low or a reset
        k = max(2, int(np.ceil(min(v["low_1"][0] for v in pc.ONEWIRE.values()) * float(c.cap.rate) - 1e-9)))
        views = [(ch, 0)]
        if k >= 2:
            d = V.spikes_out(np.asarray(ch.edges, np.int64), k)
            if len(d) != len(ch.edges):
                views.append((replace(ch, edges=d), k))
        b = _bounce_view(ch, c.cap.rate)
        if b is not None:
            views.append((b, -4))
        # a view whose lows are mostly shorter than any 1-Wire low (1 us) is none (an LED string's bits: millions of
        # lows read one by one at each speed); a noise's short lows leave the view with them taken out
        low_min = min(v["low_1"][0] for v in pc.ONEWIRE.values()) * float(c.cap.rate)
        views = [(line, dg) for line, dg in views
                 if (lambda w: len(w) and float(np.median(w)) >= low_min)(pc.pulses(line, c.cap.n_samples, 0)[1])]
        for (line, dg), speed in ((v, s) for v in views for s in ("standard", "overdrive")):
            r = pc.onewire(line, c.cap.n_samples, c.cap.rate, speed)
            rs = r["resets"]
            # slots of both kinds (a clock's lows are all alike), whole bytes after most resets
            if not rs or not r["low_0"] or not r["low_1"]:
                continue
            # a device's presence pulse is evidence; a reset none answers is no evidence against (a master looking
            # for devices, one gone): two answers seen count as all
            n_pres = sum(bool(z["presence"]) for z in rs)
            pres = min(1.0, max(n_pres / len(rs), n_pres / ONEWIRE_PRESENCE))
            whole = float(np.mean([z["leftover_bits"] == 0 and z["bits"] > 0 for z in rs]))
            if whole < 0.5:
                continue
            bits = sum(z["bits"] for z in rs)
            # a master starts its slots at one pace (the slot time), whatever the bits: the periods from one slot's
            # start to the next keep to their median; a UART's lows read as slots come whole bits apart, scattered
            per = np.array([p for _, p in r["slot"]], np.float64)
            paced = float(np.mean(np.abs(per / np.median(per) - 1) <= ONEWIRE_PACE)) if len(per) else 0.0
            # a clock's lows are all alike: its "bytes" are one value (0x00 or 0xff), a master's commands and data vary.
            # A run of one value inside a reset (a blank memory page read: 0xff after 0xff) is one value there
            data = np.concatenate([np.zeros(0, np.uint8)] + [_runs_once(np.frombuffer(bytes.fromhex(z["data"]), np.uint8))
                                                              for z in rs])
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


NEC_SHORTEST = 4e-6     # s: a pulse this short on an IR line is a bounce (a 38 kHz carrier's pulses are 8.8 us)


class NecPlugin:
    name = "ir_nec"
    consumes = ("line",)

    def run(self, c, ch, g):
        # the line as it is, and with pulses shorter than NEC_SHORTEST taken out (a bounce: no carrier pulse is that
        # short, 38 kHz at a third is 8.8 us)
        k = max(2, int(np.ceil(NEC_SHORTEST * float(c.cap.rate) - 1e-9)))
        views = [(ch, 0)]
        if k >= 2:
            d = V.spikes_out(np.asarray(ch.edges, np.int64), k)
            if len(d) != len(ch.edges):
                views.append((replace(ch, edges=d), k))
        nodes = [n for line, dg in views for n in self._read(c, line, dg, ch.name)]
        return [max(nodes, key=lambda n: n.total)] if nodes else []

    def _read(self, c, ch, dg, name):
        r = pc.nec(ch, c.cap.n_samples, c.cap.rate)
        fr = r["frames"]
        data = [f for f in fr if not f["repeat"]]
        good = [f for f in data if f.get("cmd_ok")]
        if not good:
            return []
        score = len(good) / len(data) * q(2 * len(good) + 0.5 * (len(fr) - len(data)), 1)
        m = {"frames": len(data), "repeats": len(fr) - len(data), "cmd_ok": len(good),
             "carrier_hz": r.get("carrier_hz")}
        return [Node("ir_nec", {"carrier_hz": r.get("carrier_hz"), "idle": r["idle"], "deglitch": dg}, {"data": name},
                     Stream("ir.frames", fr), m, layer_score=score, total=score)]


DHT_START = 0.5e-3        # s: the host's start, a low this long or more (DHT22 1 ms, DHT11 18 ms)
DHT_RESPONSE = (40e-6, 110e-6)   # s: the sensor's answer, a low and a high each about 80 us (58 us seen)
DHT_BIT_LOW = (30e-6, 80e-6)     # s: each bit's low (50 us)
DHT_BIT_SPLIT = 48e-6     # s: a bit's high under this is 0 (26-28 us), over it 1 (70 us)
DHT_BIT_HIGH = 95e-6      # s: no bit's high is longer


def dht_frames(ch, n: int, rate: float) -> list[dict]:
    """The readings of a DHT11 / DHT22 (AM2302) line: after the host's long low and the sensor's 80 us low and
    high, 40 bits told by each high's length; each {"s", "bytes" (hex), "checksum_ok"} (the fifth byte the sum of
    the four)."""
    start, length, level = ch.runs(n)
    dur = np.asarray(length, np.float64) / rate
    lv = np.asarray(level)
    out = []
    i, m = 0, len(dur)
    while i < m:
        if not (lv[i] == 0 and dur[i] >= DHT_START):
            i += 1
            continue
        j = i + 2                                   # past the host's release (a high of 20-40 us)
        if j + 1 >= m or not (DHT_RESPONSE[0] <= dur[j] <= DHT_RESPONSE[1] and DHT_RESPONSE[0] <= dur[j + 1] <= DHT_RESPONSE[1]):
            i += 1
            continue
        k, bits = j + 2, []
        while len(bits) < 40 and k + 1 < m and lv[k] == 0 and DHT_BIT_LOW[0] <= dur[k] <= DHT_BIT_LOW[1] \
                and dur[k + 1] <= DHT_BIT_HIGH:
            bits.append(1 if dur[k + 1] > DHT_BIT_SPLIT else 0)
            k += 2
        if len(bits) == 40:
            b = [sum(bits[8 * x + y] << (7 - y) for y in range(8)) for x in range(5)]
            out.append({"s": int(start[i]), "bytes": bytes(b).hex(), "checksum_ok": (sum(b[:4]) & 0xFF) == b[4]})
        i = k
    return out


def dht_values(frames: list[dict]) -> str:
    """The model the readings fit: DHT11 (whole humidity and temperature: its second byte 0, a tenth of a degree at
    most) or DHT22 / AM2302 (tenths), and each reading's `rh` (%) and `t` (C) by it."""
    bs = [bytes.fromhex(f["bytes"]) for f in frames if f["checksum_ok"]]
    dht11 = bool(bs) and all(b[1] == 0 and b[0] <= 100 and b[3] < 10 for b in bs)
    for f in frames:
        b = bytes.fromhex(f["bytes"])
        if dht11:
            f["rh"], f["t"] = float(b[0]), b[2] + b[3] / 10 * (-1 if b[2] & 0x80 else 1)
        else:
            raw = ((b[2] & 0x7F) << 8) | b[3]
            f["rh"], f["t"] = ((b[0] << 8) | b[1]) / 10, (-raw if b[2] & 0x80 else raw) / 10
    return "dht11" if dht11 else "dht22"


class DhtPlugin:
    """A DHT11 / DHT22 (AM2302) humidity sensor's one line."""
    name = "dht"
    consumes = ("line",)

    def run(self, c, ch, g):
        rate = float(c.cap.rate)
        if rate < 50e3:                             # a bit's high of 26 us needs a sample or two
            return []
        frames = dht_frames(ch, c.cap.n_samples, rate)
        if not frames:
            return []
        ok = sum(f["checksum_ok"] for f in frames)
        if not ok:
            return []
        model = dht_values(frames)
        # evidence in bits: each reading's checksum held, 8, and its start and answer of their lengths, 8; the
        # readings' share whose checksum holds
        score = (ok / len(frames)) * (1 - 0.5 ** (16 * ok / 8))
        m = {"readings": len(frames), "checksum_ok": ok}
        return [Node("dht", {"model": model}, {"data": ch.name}, Stream("dht.readings", frames), m,
                     layer_score=score, total=score)]
