"""I2C decoder: (scl, sda) edges -> transactions. Proposals: every ordered pair."""

from __future__ import annotations

import numpy as np

from .. import kernels as K
from ..stack import Context, Stream, pairs


class I2c:
    name = "i2c"
    consumes = "capture"
    produces = "i2c.transactions"

    def propose(self, ctx: Context, parent):
        for scl, sda in pairs(ctx.active):
            yield {"scl": scl, "sda": sda}

    def probe_channels(self, p):
        return [p["scl"], p["sda"]]

    def run(self, ctx: Context, parent, p):
        cap = ctx.capture
        scl, sda = cap.channel(p["scl"]), cap.channel(p["sda"])
        roles = {"scl": p["scl"], "sda": p["sda"]}
        ev = K.merge_events(scl.edges, sda.edges)
        if len(ev) == 0:
            return roles, None, {"starts": 0}
        c, d = scl.initial, sda.initial
        txs, cur, bits = [], None, []
        starts = stops = 0
        seg_total = seg_ok = 0
        scl_rise_in = 0
        sda_explained = 0
        misc_sda = 0

        def close_segment(end_sample, stop):
            nonlocal seg_total, seg_ok
            if cur is None:
                return
            if bits:  # START immediately followed by START/STOP carries no bits to check
                seg_total += 1
                seg_ok += len(bits) % 9 == 0
            nbytes = len(bits) // 9
            vals = [int("".join(map(str, bits[i * 9:i * 9 + 8])), 2) for i in range(nbytes)]
            acks = [bits[i * 9 + 8] == 0 for i in range(nbytes)]
            if vals:
                cur.update(addr=vals[0] >> 1, rw="read" if vals[0] & 1 else "write", addr_ack=acks[0],
                           bytes=vals[1:], acks=acks[1:])
            cur.update(end=int(end_sample), stop=stop, bits=len(bits))
            txs.append(dict(cur))

        pending = None  # a bit is committed on the SCL fall; a rise followed by START/STOP is not data
        for t, which in ev.tolist():
            if which == 0:
                c ^= 1
                if cur is not None:
                    scl_rise_in += c == 1
                    if c == 1:
                        pending = d
                    elif pending is not None:
                        bits.append(pending)
                        pending = None
            else:
                d ^= 1
                if c == 1:
                    pending = None
                    if d == 0:  # START / repeated START
                        if cur is not None:
                            close_segment(t, False)
                        starts += 1
                        cur, bits = {"start": int(t)}, []
                    else:       # STOP
                        if cur is not None:
                            close_segment(t, True)
                            stops += 1
                        cur, bits = None, []
                    sda_explained += 1
                elif cur is not None:
                    sda_explained += 1
                else:
                    misc_sda += 1
        n_rise = int(np.sum((scl.initial ^ ((np.arange(len(scl.edges)) + 1) & 1)) == 1))
        with_addr = [x for x in txs if "addr" in x]
        addrs = [x["addr"] for x in with_addr]
        top_addr = max((addrs.count(a) for a in set(addrs)), default=0)
        metrics = {
            "starts": starts,
            "stops": stops,
            "transactions": len(txs),
            "bytes": sum(1 + len(x["bytes"]) for x in with_addr),
            "segment_ok_rate": seg_ok / seg_total if seg_total else 0.0,
            "scl_coverage": scl_rise_in / n_rise if n_rise else 0.0,
            "sda_explained": sda_explained / len(sda.edges) if len(sda.edges) else 0.0,
            "addr_ack_rate": float(np.mean([x["addr_ack"] for x in with_addr])) if with_addr else 0.0,
            "distinct_addr": len(set(addrs)),
            "top_addr_share": top_addr / len(addrs) if addrs else 0.0,
            "clk_role_score": ctx.features[p["scl"]].scores.get("clock", 0.0),
        }
        return roles, Stream("i2c.transactions", txs), metrics
