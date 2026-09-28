"""UART line decoder: edges -> bytes.

Proposals: every active channel x every baud candidate (units from L2 plus all
standard rates the sample rate can resolve) x idle level x frame format.
"""

from __future__ import annotations

import numpy as np

from ..stack import Context, Node, Stream

STANDARD = [300, 1200, 2400, 4800, 9600, 14400, 19200, 31250, 38400, 57600, 74880, 115200, 230400, 250000,
            460800, 500000, 921600, 1_000_000, 1_500_000, 2_000_000, 3_000_000]
FORMATS = [(db, par, sb) for db in (7, 8, 9) for par in ("none", "even", "odd") for sb in (1, 2)]


class Uart:
    name = "uart"
    consumes = "capture"
    produces = "bytes"

    def __init__(self, standard_bauds: bool = True, formats=FORMATS):
        self.standard_bauds = standard_bauds
        self.formats = formats
        self._cache: dict = {}

    def propose(self, ctx: Context, parent):
        rate = ctx.capture.rate
        for ch in ctx.active:
            f = ctx.features[ch]
            bauds = {round(rate / u.samples, 1): "unit" for u in f.units}
            if self.standard_bauds:
                for b in STANDARD:
                    if rate / b >= 3 and rate / b * 12 < ctx.capture.n_samples:
                        bauds.setdefault(float(b), "standard")
            for baud, src in sorted(bauds.items()):
                for idle in (1, 0):
                    for db, par, sb in self.formats:
                        yield {"ch": ch, "baud": baud, "idle": idle, "data_bits": db, "parity": par, "stop_bits": sb,
                               "baud_src": src}

    def probe_channels(self, p):
        return [p["ch"]]

    def run(self, ctx: Context, parent, p):
        cap = ctx.capture
        ch = cap.channel(p["ch"])
        T = cap.rate / p["baud"]
        idle = p["idle"]
        n_par = 0 if p["parity"] == "none" else 1
        nbits = 1 + p["data_bits"] + n_par          # start + data + parity
        frame = nbits + p["stop_bits"]               # in bit times
        e = ch.edges
        after = ch.initial ^ ((np.arange(len(e)) + 1) & 1)
        starts = e[after == 1 - idle].astype(np.float64)
        if len(starts) == 0:
            return {"data": p["ch"]}, None, {"frames": 0}
        # Sample every candidate start at bit centers, shared by all frame formats
        # of the same (channel, baud, idle): the widest format needs 13 bits.
        key = (id(cap), p["ch"], p["baud"], idle)
        if key not in self._cache:
            if len(self._cache) > 8:
                self._cache.clear()
            offs = (np.arange(13) + 0.5) * T
            pos = np.minimum(np.floor(starts[:, None] + offs[None, :]).astype(np.int64), cap.n_samples - 1)
            full = ch.level_at(pos.ravel()).reshape(pos.shape)
            self._cache[key] = full if idle == 1 else 1 - full
        lv = self._cache[key][:, :nbits + p["stop_bits"]]
        start_ok = lv[:, 0] == 0
        stop_ok = np.all(lv[:, nbits:] == 1, axis=1)
        data = lv[:, 1:1 + p["data_bits"]]
        values = (data << np.arange(p["data_bits"])).sum(axis=1)
        if n_par:
            par = (data.sum(axis=1) + lv[:, 1 + p["data_bits"]]) & 1
            parity_ok = par == (0 if p["parity"] == "even" else 1)
        else:
            parity_ok = np.ones(len(starts), bool)
        # Greedy chain: a start edge can open a frame only after the previous
        # frame's stop-bit center (standard UART receiver behaviour).
        busy_until = starts + (nbits + 0.5) * T
        nxt = np.searchsorted(starts, busy_until, side="left")
        chosen = []
        i, n_st = 0, len(starts)
        nl = nxt.tolist()
        while i < n_st:
            chosen.append(i)
            i = nl[i]
        idx = np.asarray(chosen, dtype=np.int64)
        fs = starts[idx]
        fe = fs + frame * T
        ok = start_ok[idx] & stop_ok[idx] & parity_ok[idx]
        # Edge coverage: edges inside a well-formed frame that fall on a bit boundary.
        k = np.searchsorted(fs, e, side="right") - 1
        inside = (k >= 0) & (e < fe[np.maximum(k, 0)])
        ph = (e - fs[np.maximum(k, 0)]) / T
        # Tolerance: 0.1 bit, widened only as far as sample quantization requires.
        tol = min(0.3, max(0.1, 0.75 / T))
        on_grid = np.abs(ph - np.round(ph)) < tol
        good_frame = ok[np.maximum(k, 0)]
        explained = inside & on_grid & good_frame
        n_edges = len(e)
        vals_ok = values[idx][ok]
        # Idle agreement: the share of time outside frames spent at the idle level.
        run_start, run_len, run_lev = ch.runs(cap.n_samples)
        t_idle = float(run_len[run_lev == idle].sum())
        in_frame_idle = float((lv[idx][:, 1:] == 1).sum() * T)  # data/parity/stop bits at idle
        outside = cap.n_samples - float(len(idx)) * frame * T
        idle_share = (t_idle - in_frame_idle) / outside if outside > T else 1.0
        metrics = {
            "idle_share": float(min(1.0, max(0.0, idle_share))),
            "clk_role_score": ctx.features[p["ch"]].scores.get("clock", 0.0),
            "frames": int(len(idx)),
            "frames_ok": int(ok.sum()),
            "framing_rate": float(ok.mean()) if len(idx) else 0.0,
            "stop_fail": int((~stop_ok[idx]).sum()),
            "parity_fail": int((~parity_ok[idx]).sum()) if n_par else 0,
            "edge_coverage": float(explained.sum() / n_edges) if n_edges else 0.0,
            "grid_residual": float(np.mean(np.abs(ph[inside] - np.round(ph[inside])))) if inside.any() else 1.0,
            "samples_per_bit": float(T),
            "top_byte_share": float(np.bincount(vals_ok).max() / len(vals_ok)) if len(vals_ok) else 1.0,
            "distinct_bytes": int(len(np.unique(vals_ok))),
        }
        out = Stream("bytes", {
            "start": fs[ok].astype(np.int64), "end": fe[ok].astype(np.int64), "value": vals_ok.astype(np.int64),
            "bad_start": fs[~ok].astype(np.int64),
        }, meta={"char_samples": frame * T, "data_bits": p["data_bits"]})
        return {"data": p["ch"]}, out, metrics
