"""I2S and its kin (left- / right-justified, TDM of two slots): a bit clock (SCK / BCLK), a word select (WS / LRCLK)
changing once a slot, and a serial data line, both moving on the bit clock. WS is one of the clock's data lines to
the survey (it changes on the clock's edges), so the plugin takes a clocked group of two data lines or more and tries
each as WS beside each other as SD.

The word's place in its slot is told by the padding: a word shorter than its slot leaves the same bits 0 (or a sign
copy) in every slot, at its end (I2S: the MSB a bit after WS changes; left-justified: at the change) or at its start
(right-justified). A word filling its slot tells nothing: I2S, the common one, is taken and said (`format_assumed`).
"""
from __future__ import annotations

import numpy as np

from .stack import Node, Stream

I2S_SLOT_MIN = 8          # bits a slot at least ...
I2S_SLOT_MAX = 64         # ... and at most (TDM's frames of 8 slots of 32 are not read here)
I2S_REGULAR = 0.9         # of WS's runs, this share or more of one length (the slot)
I2S_SLOTS = 8             # slots for full evidence (q)


def _runs(ws: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Where WS (bits at the sampling edges) changes, and the lengths of its runs between changes."""
    ch = np.flatnonzero(np.diff(ws.astype(np.int8)) != 0) + 1
    return ch, np.diff(ch)


def _padding(sd_slots: np.ndarray) -> tuple[int, int]:
    """(bits always the same at the slot's end, at its start) over the slots (rows of bits, MSB first)."""
    const = np.all(sd_slots == sd_slots[:1], axis=0) & (sd_slots.shape[0] > 1)
    n = len(const)
    tail = n - int(np.flatnonzero(~const)[-1]) - 1 if (~const).any() else n
    head = int(np.flatnonzero(~const)[0]) if (~const).any() else n
    return tail, head


def read_i2s(t: np.ndarray, ws: np.ndarray, sd: np.ndarray):
    """The frames of an I2S-like stream from WS and SD sampled at the bit clock's sampling edges: (frames, metrics,
    params) or None when WS changes at no regular slot."""
    ch, runs = _runs(ws)
    if len(runs) < 4:
        return None
    vals, cnt = np.unique(runs, return_counts=True)
    slot = int(vals[np.argmax(cnt)])
    regular = float(cnt.max() / len(runs))
    if not I2S_SLOT_MIN <= slot <= I2S_SLOT_MAX or regular < I2S_REGULAR:
        return None
    # the slots as WS has them, each starting where WS changed: whole ones only
    starts = ch[:-1][runs == slot]
    if len(ws) - ch[-1] >= slot:
        starts = np.append(starts, ch[-1])          # the last slot, whole though no change closes it
    # I2S's word starts a bit after the change, left-justified at it: both read, the padding (bits never changing at
    # the slot's end) says which; with none, I2S
    def rows(off):
        s = starts + off
        s = s[s + slot <= len(sd)]
        return s, sd[s[:, None] + np.arange(slot)]
    s1, r1 = rows(1)
    s0, r0 = rows(0)
    tail1, head1 = _padding(r1)
    tail0, head0 = _padding(r0)
    if head1 > 0 and head1 >= tail1 and head1 >= tail0:
        fmt, s_, r_, bits = "right", s0, r0, slot - head0
    elif tail0 > tail1:
        fmt, s_, r_, bits = "left", s0, r0, slot - tail0
    else:
        fmt, s_, r_, bits = "i2s", s1, r1, slot - tail1
    bits = max(1, bits)
    assumed = tail0 == tail1 == head1 == 0
    if fmt == "right":
        r_ = r_[:, slot - bits:]
    else:
        r_ = r_[:, :bits]
    words = (r_.astype(np.int64) << np.arange(bits - 1, -1, -1)).sum(1)
    words = np.where(words >= 1 << (bits - 1), words - (1 << bits), words)       # two's complement
    # the channel a slot carries: WS's level through it (I2S: low left; justified formats: high left)
    lvl = ws[np.clip(s_ + bits // 2, 0, len(ws) - 1)]
    left_level = 0 if fmt == "i2s" else 1
    frames, cur = [], None
    for st, w, lv in zip(s_.tolist(), words.tolist(), lvl.tolist()):
        if lv == left_level:
            cur = {"s": int(t[st]), "left": int(w)}
            frames.append(cur)
        elif cur is not None and "right" not in cur:
            cur["right"] = int(w)
    m = {"slot": slot, "regular": regular, "slots": int(len(s_)), "bits": bits}
    params = {"format": fmt, "slot_bits": slot, "word_bits": bits, **({"format_assumed": True} if assumed else {})}
    return frames, m, params


class I2sPlugin:
    """I2S on a clocked group: WS one of its data lines, SD another."""
    name = "i2s"
    consumes = ("frames.gap",)

    def run(self, c, fr, g):
        sb = fr.source
        if len(sb.data) < 2:
            return []
        clk = c.sv.clocks[sb.clock].clock_score
        out = []
        for wi, w in enumerate(sb.data):
            for si, d in enumerate(sb.data):
                if si == wi:
                    continue
                r = read_i2s(sb.t, sb.bits[wi], sb.bits[si])
                if r is None:
                    continue
                frames, m, params = r
                pair = float(c.sv.pairs[(sb.clock, d)].data_score)
                q = 1 - 0.5 ** (m["slots"] / I2S_SLOTS)
                score = m["regular"] * q * (0.5 + 0.5 * clk) * (0.5 + 0.5 * pair)
                m.update(clk=clk, pair=pair)
                out.append(Node("i2s", {"sample_edge": sb.sample_edge, **params}, {"sck": sb.clock, "ws": w, "sd": d},
                                Stream("i2s.frames", frames), m, layer_score=score, total=score))
        return sorted(out, key=lambda n: -n.total)[:1]
