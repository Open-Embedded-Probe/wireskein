"""I2S and its kin (left- / right-justified, and TDM: a frame sync once a frame of three slots or more): a bit clock (SCK / BCLK), a word select (WS / LRCLK)
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
I2S_SLOT_MAX = 64         # ... and at most (a longer one is a TDM frame: read_tdm)
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


TDM_SLOTS = (32, 24, 16, 8)   # slot widths tried in a TDM frame (one dividing it); 32 taken where no padding tells
TDM_FRAME_MAX = 1024         # bits a TDM frame at most (16 slots of 64)
TDM_DISTINCT = 4             # distinct slots, at least, for their padding to tell the slot and the word's place
TDM_WORD_BITS = (8, 12, 16, 20, 24, 32)   # word lengths a TDM slot carries


def _frame_starts(fs: np.ndarray):
    """A TDM frame sync's frame starts (at the sampling edges): where FS turns to its active level, the level of its
    shorter run (a pulse a bit or a slot long; at 50 % duty, high). (starts, frame bits, regular share, active
    level) or None."""
    ch, runs = _runs(fs)
    if len(runs) < 4:
        return None
    lv = fs[ch[:-1]]
    hi, lo = runs[lv == 1], runs[lv == 0]
    if not len(hi) or not len(lo):
        return None
    active = 1 if np.median(hi) <= np.median(lo) else 0
    starts = ch[fs[ch] == active]
    if len(starts) < 3:
        return None
    period = np.diff(starts)
    vals, cnt = np.unique(period, return_counts=True)
    frame = int(vals[np.argmax(cnt)])
    return starts, frame, float(cnt.max() / len(period)), active


def read_tdm(t: np.ndarray, fs: np.ndarray, sd: np.ndarray):
    """TDM: a frame sync (FS) once a frame of three slots or more, the frame's slots one after another on SD. The
    frame starts where FS turns active; its first slot's MSB at that edge (DSP mode B, `offset` 0) or a bit after
    (DSP mode A and I2S-style TDM, `offset` 1), told by the padding as in I2S. The slot width is the one whose
    slots share padding at their end (or start); with none, 32 bits where the frame holds three or more of them.
    (frames, metrics, params) or None. A frame sync of two equal runs of up to I2S_SLOT_MAX each is I2S's."""
    fr = _frame_starts(fs)
    if fr is None:
        return None
    starts, frame, regular, active = fr
    ch, runs = _runs(fs)
    lv = fs[ch[:-1]]
    hi, lo = float(np.median(runs[lv == 1])), float(np.median(runs[lv == 0]))
    two_equal = hi == lo and hi <= I2S_SLOT_MAX
    if two_equal or regular < I2S_REGULAR or not 3 * I2S_SLOT_MIN <= frame <= TDM_FRAME_MAX:
        return None
    best = None
    for off in (1, 0):
        s = starts + off
        s = s[s + frame <= len(sd)]
        if len(s) < 2:
            continue
        rows = sd[s[:, None] + np.arange(frame)]
        for slot in TDM_SLOTS:
            if frame % slot or frame // slot < 3:
                continue
            tail, head = _padding(rows.reshape(-1, slot))
            pad = max(tail, head)
            # more padding first; then the offset whose padding is at the end; then the narrower slot (two slots of
            # 16 with the same padding read as one of 32 show it only once, at its end)
            key = (min(pad, slot - 1), tail >= head, off == 1, -slot)
            if best is None or key > best[0]:
                best = (key, off, slot, s, rows, tail, head)
    if best is None:
        return None
    (pad, _, _, _), off, slot, s, rows, tail, head = best
    # slots of a few values (a test pattern, silence) leave their own leading zeros "padding": no evidence
    few = len(np.unique(rows.reshape(-1, slot), axis=0)) < TDM_DISTINCT
    assumed = pad == 0 or few
    if assumed:
        # no padding tells the slot or the offset: 32 bits where the frame holds three or more, a bit after FS
        slot = 32 if frame % 32 == 0 and frame // 32 >= 3 else slot
        off, tail, head = 1, 0, 0
        s = starts + off
        s = s[s + frame <= len(sd)]
        rows = sd[s[:, None] + np.arange(frame)]
    n_slots = frame // slot
    cells = rows.reshape(len(s), n_slots, slot)
    right = head > tail
    # a word's length is one of the usual ones: padding of a bit or two is a test pattern's even values more often
    # than a word of 15 bits
    bits = min([b for b in TDM_WORD_BITS if b >= slot - (head if right else tail)] + [slot])
    cells = cells[:, :, slot - bits:] if right else cells[:, :, :bits]
    words = (cells.astype(np.int64) << np.arange(bits - 1, -1, -1)).sum(2)
    words = np.where(words >= 1 << (bits - 1), words - (1 << bits), words)
    frames = [{"s": int(t[st]), "slots": [int(w) for w in row]} for st, row in zip(s.tolist(), words.tolist())]
    m = {"slot": slot, "regular": regular, "slots": int(len(s) * n_slots), "bits": bits, "frame": frame}
    params = {"format": "tdm", "slot_bits": slot, "word_bits": bits, "slots_per_frame": n_slots, "offset": off,
              "fs_active": int(active), **({"justify": "right"} if right else {}),
              **({"format_assumed": True} if assumed else {})}
    return frames, m, params


class I2sPlugin:
    """I2S (or TDM) on a clocked group: WS (FS) one of its data lines, SD another."""
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
                r = read_i2s(sb.t, sb.bits[wi], sb.bits[si]) or read_tdm(sb.t, sb.bits[wi], sb.bits[si])
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
