"""A UART claim's baud fitted again over its well-formed characters, after the analysis (the claims otherwise as the
base algorithm gives them): per character, the edges inside it at d = a + k u (a: the start edge's own offset, k the
bit index), u pooled over the characters with k centred within each. Edges off the bit grid by 0.3 bit or more (a
spike, a bounce) are left out. Twice, from the engine's u.

Why (2026-11-16, the calibration run): main's other wrong confirmed claims (8 of 49) were UART lines read right
(bytes decoded right) with a baud 3.6 to 4.8 % off (eval allows 3 %). The engine's unit comes from run lengths
(features.estimate_units: a least-squares fit of single runs); spikes and bounces split and shorten runs, and at a
few samples a bit one sample is a large share. Over a character the baseline is up to 9 bits. On the eight, the
refit came within 0.2 % of the true baud (clean ones unchanged).

Only the reported baud changes here (in the engine, the refit would come before the characters are read again)."""

import numpy as np

from wsbench import algorithm, registry


def refit_unit(cap, claim) -> float | None:
    ch = cap.channel(claim["roles"]["data"])
    e = np.asarray(ch.edges, np.int64)
    L = claim["params"].get("L") or 10
    u = float(cap.rate) / claim["params"]["baud"]
    for _ in range(2):
        num = den = 0.0
        for x in claim["layers"].get("chars", []):
            if not x.get("ok"):
                continue
            s = x["s"]
            ee = e[(e >= s) & (e < s + (L - 0.5) * u)]
            d = (ee - s).astype(float)
            k = np.round(d / u)
            keep = np.abs(d / u - k) < 0.3
            d, k = d[keep], k[keep]
            if len(k) < 2:
                continue
            kc, dc = k - k.mean(), d - d.mean()
            num += float((kc * dc).sum())
            den += float((kc * kc).sum())
        if den <= 0:
            return None
        u = num / den
    return u if u > 0 else None


@algorithm("e2e", "refit", status="trial", about="the base's claims, a UART's baud fitted again over its characters",
           variants={"uart": {"base": "wireskein"}, "uart_combo_select": {"base": "combo.clear_gated_select"}})
def refit(cap, base):
    claims = registry.get("e2e", base)(cap)
    for c in claims:
        if c["protocol"] == "uart" and c["params"].get("baud"):
            u = refit_unit(cap, c)
            if u:
                c["params"] = {**c["params"], "baud": float(cap.rate) / u}
    return claims
