"""SPI decoder: (clk, optional cs, data lines) edges -> bytes.

A root hypothesis is a bus: clock, sampling edge, bit order and optional
chip-select. Every other active channel is decoded as a candidate data line
(exhaustively); lines that fit are included in the bus, the rest are kept in
the metrics with their scores. CPOL is not enumerated: decoding only needs the
sampling edge, and CPOL is the observed clock idle level.
"""

from __future__ import annotations

import numpy as np

from ..stack import Context, Stream

LINE_ACCEPT = 0.5


def _windows(ch, n, active):
    start, length, level = ch.runs(n)
    sel = level == active
    return np.stack([start[sel], start[sel] + length[sel]], 1)


def _in(win, x):
    k = np.searchsorted(win[:, 0], x, side="right") - 1
    return (k >= 0) & (x < win[np.maximum(k, 0), 1]), k


def _pack(bits: np.ndarray, seg_len: np.ndarray, order: str) -> np.ndarray:
    """Pack sampled bits into bytes per segment (frame), dropping incomplete
    trailing bits of each segment. Vectorized over all segments."""
    if len(bits) == 0:
        return np.zeros(0, np.int64)
    seg_start = np.concatenate(([0], np.cumsum(seg_len)[:-1]))
    seg_id = np.repeat(np.arange(len(seg_len)), seg_len)
    pos = np.arange(len(bits)) - seg_start[seg_id]
    keep = pos < (seg_len // 8 * 8)[seg_id]
    pos, b, sid = pos[keep], bits[keep], seg_id[keep]
    shift = 7 - (pos % 8) if order == "msb" else pos % 8
    byte_offset = np.concatenate(([0], np.cumsum(seg_len // 8)[:-1]))
    byte_id = byte_offset[sid] + pos // 8
    n_bytes = int((seg_len // 8).sum())
    return np.bincount(byte_id, weights=b << shift, minlength=n_bytes).astype(np.int64)


def line_score(m: dict) -> float:
    """A data line should change away from sampling edges, inside frames, and
    not look like a clock itself (a free-running clock is rarely data)."""
    if m["bytes"] < 1:
        return 0.0
    degenerate = 1.0 if m["top_byte_share"] <= 0.5 or m["bytes"] < 4 else max(0.05, 1 - (m["top_byte_share"] - 0.5) / 0.5)
    return m["setup_ok"] * m["phase_conc"] * m["data_in_frames"] * degenerate * (1 - 0.7 * m["clock_like"])


class Spi:
    name = "spi"
    consumes = "capture"
    produces = "bytes"

    def propose(self, ctx: Context, parent):
        act = ctx.active
        for clk in act:
            for cs in [None, *act]:
                if cs == clk:
                    continue
                for cs_active in ((None,) if cs is None else (0, 1)):
                    for edge in ("rise", "fall"):
                        for order in ("msb", "lsb"):
                            yield {"clk": clk, "cs": cs, "cs_active": cs_active, "sample_edge": edge, "bit_order": order}

    def probe_channels(self, p):
        return [p["clk"]] + ([p["cs"]] if p["cs"] else [])

    def run(self, ctx: Context, parent, p):
        cap = ctx.capture
        clk = cap.channel(p["clk"])
        roles = {"clk": p["clk"]}
        if p["cs"]:
            roles["cs"] = p["cs"]
        ce = clk.edges
        after = clk.initial ^ ((np.arange(len(ce)) + 1) & 1)
        want = 1 if p["sample_edge"] == "rise" else 0
        samp = ce[after == want]
        cpol = ctx.features[p["clk"]].idle_level or 0
        mode = cpol * 2 + (0 if (want == 1) != bool(cpol) else 1)
        if len(samp) < 8:
            return roles, None, {"frames": 0}
        half = float(np.median(np.diff(ce)))
        if p["cs"]:  # noqa: SIM108
            win = _windows(cap.channel(p["cs"]), cap.n_samples, p["cs_active"])
        else:
            gap = np.flatnonzero(np.diff(samp) > 8 * 2 * half)
            ws = np.concatenate(([samp[0]], samp[gap + 1])) - half
            we = np.concatenate((samp[gap], [samp[-1]])) + half
            win = np.stack([ws, we], 1).astype(np.int64)
        if len(win) == 0:
            return roles, None, {"frames": 0}
        in_win, k = _in(win, samp)
        ks, ss = k[in_win], samp[in_win]
        cut = np.flatnonzero(np.diff(ks)) + 1 if len(ks) else np.zeros(0, np.int64)
        seg_len = np.diff(np.concatenate(([0], cut, [len(ks)]))) if len(ks) else np.zeros(0, np.int64)
        used_windows = len(np.unique(ks)) if len(ks) else 0
        # evidence for the chip-select itself: its windows should carry clocks,
        # and the clock burst should start and end close to the CS edges
        cs_windows_used = used_windows / len(win) if p["cs"] else 1.0
        cs_tight = 1.0
        if p["cs"] and len(ks):
            uk = np.unique(ks)
            first = ss[np.searchsorted(ks, uk, side="left")]
            last = ss[np.searchsorted(ks, uk, side="right") - 1]
            lead, trail = first - win[uk, 0], win[uk, 1] - last
            cs_tight = float(np.mean((lead <= 8 * half) & (trail <= 8 * half)))

        lines, frames_by_line = {}, {}
        for d in ctx.active:
            if d in (p["clk"], p["cs"]):
                continue
            data = cap.channel(d)
            bits = data.level_at(ss).astype(np.int64)
            allb = _pack(bits, seg_len, p["bit_order"])
            de = data.edges
            if len(de):
                j = np.searchsorted(samp, de)
                dist = np.minimum(np.abs(de - samp[np.clip(j, 0, len(samp) - 1)]),
                                  np.abs(de - samp[np.clip(j - 1, 0, len(samp) - 1)]))
                d_in, _ = _in(win, de)
                setup_ok = float(np.mean(dist[d_in] >= 0.25 * half)) if d_in.any() else 0.0
                data_in = float(d_in.mean())
                # Phase concentration of data changes within the clock period:
                # a real data line shifts at a fixed phase (after the shift edge),
                # an unrelated line changes at uniformly random phases.
                sp = samp[np.clip(j - 1, 0, len(samp) - 1)]
                ph = 2 * np.pi * ((de - sp) / (2 * half))
                sel = d_in & (j > 0)
                phase_conc = float(np.hypot(np.cos(ph[sel]).mean(), np.sin(ph[sel]).mean())) if sel.sum() >= 2 else 0.0
            else:
                setup_ok, data_in, phase_conc = 0.0, 0.0, 0.0  # a static line carries no evidence
            m = {"clock_like": ctx.features[d].scores.get("clock", 0.0), "setup_ok": setup_ok, "phase_conc": phase_conc, "data_in_frames": data_in, "bytes": int(len(allb)),
                 "top_byte_share": float(np.bincount(allb).max() / len(allb)) if len(allb) else 1.0}
            m["score"] = line_score(m)
            lines[d] = m
            frames_by_line[d] = allb
        accepted = sorted((d for d, m in lines.items() if m["score"] >= LINE_ACCEPT), key=lambda d: -lines[d]["score"])
        for i, d in enumerate(accepted):
            roles[f"data{i}"] = d
        frame_ranges = []
        if len(ks):
            for seg in np.split(ss, cut):
                frame_ranges.append((int(seg[0]), int(seg[-1])))
        metrics = {
            "frames": int(len(seg_len)),
            "bits_mod8_rate": float(np.mean(seg_len % 8 == 0)) if len(seg_len) else 0.0,
            "clk_coverage": float(in_win.mean()),
            "cs_windows_used": cs_windows_used,
            "cs_tight": cs_tight,
            "line_score": float(np.mean([lines[d]["score"] for d in accepted])) if accepted else 0.0,
            "n_lines": len(accepted),
            "bytes": int(sum(lines[d]["bytes"] for d in accepted)),
            "half_period": half,
            "mode": mode,
            "clk_role_score": ctx.features[p["clk"]].scores.get("clock", 0.0),
            "lines": lines,
        }
        # frames per line: split the packed bytes by bytes-per-segment
        per_seg = seg_len // 8
        bounds = np.cumsum(per_seg)[:-1]
        items = {"lines": {d: np.split(frames_by_line[d], bounds) for d in accepted}, "frame_ranges": frame_ranges}
        items["value"] = frames_by_line[accepted[0]] if accepted else np.zeros(0, np.int64)
        items["start"] = (np.repeat([r[0] for r in frame_ranges], per_seg) if accepted and frame_ranges
                          else np.zeros(0, np.int64))
        return roles, Stream("bytes", items, meta={"framed": True}), metrics
