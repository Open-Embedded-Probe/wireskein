"""The resynthesis engine: each candidate (the readers' on the fixed family of views, its fit past a low gate) scored
by how much of its lines it explains - the changes found where the protocol, made again from what was read, puts
them - a scale shared by every protocol; the best set taken by the changes explained (each line once)."""
from __future__ import annotations

import numpy as np

from . import readers as R
from . import claims_of
from .learned import all_candidates

GATE = 0.3


def explained(c, cap):
    proto, roles, fit, params, layers, ev = c
    rate = cap.rate
    if proto == "uart":
        ch = cap.channel(roles["data"])
        e = np.asarray(ch.edges, np.int64)
        u = rate / params["baud"]
        ok = np.zeros(len(e), bool)
        for x in layers.get("chars", []):
            if not x["ok"]:
                continue
            j0, j1 = np.searchsorted(e, x["s"] - 0.25 * u), np.searchsorted(e, x["e"] + 0.25 * u)
            seg = e[j0:j1]
            if len(seg):
                k = (seg - x["s"]) / u
                ok[j0:j1] |= np.abs(k - np.round(k)) <= 0.25
        return int(ok.sum()), len(e)
    clk = cap.channel(roles.get("scl") or roles.get("clk"))
    ck = R.clock_of(clk)
    if ck is None:
        return 0, len(clk.edges)
    ce = np.asarray(clk.edges, np.int64)
    ex, tot = int(np.isin(ce, ck["edges"]).sum()), len(ce)
    for k, v in roles.items():
        if k in ("scl", "clk"):
            continue
        de = np.asarray(cap.channel(v).edges, np.int64)
        tot += len(de)
        if k == "cs":
            n_units = len(layers.get("transfers", []))
            ex += min(len(de), 2 * n_units)
        else:
            ex += int(np.isin(de, R.clean(de, R.CLEAN * ck["half"])).sum())
    return ex, tot


def engine(cap, gate_prob: float | None = None):
    """gate_prob: a candidate also needs the learnt engine's probability of being right this high (learned.py), its
    knowledge of which readings are mostly wrong (decoy lines, other protocols' lines) as the gate."""
    scored = []
    model = None
    if gate_prob is not None:
        import json
        from . import learned
        model = json.loads(learned.MODEL.read_text())
    for c in all_candidates(cap):
        if c[2] < GATE:
            continue
        if model is not None:
            from . import learned
            if learned.prob(model, learned.features(c, cap)) < gate_prob:
                continue
        ex, tot = explained(c, cap)
        if tot:
            scored.append((c, ex, ex / tot))
    # the claims by the changes they explain (a reading that explains more lines' changes first); fit stays the
    # verdict's figure, the share explained times it
    cs = [(c[0], c[1], share * c[2] ** 0.5, *c[3:]) for c, ex, share in sorted(scored, key=lambda x: -x[1])]
    return claims_of(cs)
