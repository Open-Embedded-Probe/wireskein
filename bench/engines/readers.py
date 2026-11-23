"""Readers shared by the bench's engines: UART, I2C and SPI read from given roles, to the analysis format's layers
(docs/analysis-format.md §3.2), each with a fit in 0..1 from the protocol's own structure. Each line is cleaned
against the hypothesis's own length (a clock's half period, a UART's bit time): runs shorter than CLEAN of it are
taken out (a spike, a bounce, crosstalk) - no rule guessing what the noise is.

Standalone: numpy and wireskein._engine.views.pulses_out only, so an engine built on them is apart from the current
engine's survey and stages."""
from __future__ import annotations

import numpy as np

CLEAN = 0.3          # a run under this share of the hypothesis's length is noise
UART_FORMATS = ((8, "none"), (7, "even"), (7, "odd"), (8, "even"), (8, "odd"), (7, "none"))


def level_at(edges, initial, t):
    return (int(initial) + np.searchsorted(edges, t, side="right")) & 1


_CLEANED: dict = {}     # id(edges) -> (a weak reference to them, {short: cleaned}): a line is read many times


def clean(edges, short):
    """The line's changes with its pulses under `short` taken out, worked out once a line and width (the readers try
    each line in many roles: light spent 7 tenths of its time cleaning the same lines again)."""
    import weakref
    if len(edges) < 3 or short <= 1:
        return np.asarray(edges, np.int64)
    k = id(edges)
    got = _CLEANED.get(k)
    if got is None or got[0]() is not edges:               # a new array at an old one's address is another line
        try:
            ref = weakref.ref(edges, lambda _, k=k: _CLEANED.pop(k, None))
        except TypeError:                                  # (a list: no weak reference, not kept)
            from wireskein._engine import views as V
            return V.pulses_out(np.asarray(edges, np.int64), int(short))
        got = _CLEANED[k] = (ref, {})
    by = got[1]
    if int(short) not in by:
        from wireskein._engine import views as V
        by[int(short)] = V.pulses_out(np.asarray(edges, np.int64), int(short))
    return by[int(short)]


# ---------------- UART ----------------

def uart_unit(edges) -> float | None:
    d = np.diff(np.asarray(edges, np.int64)).astype(float)
    d = d[d >= 2]                               # a spike's runs of one left out
    if len(d) < 8:
        return None
    u = float(np.quantile(d, 0.1))
    for _ in range(3):
        if u <= 0:
            return None
        k = np.maximum(1, np.round(d / u))
        ok = k <= 12
        u = float(np.median(d[ok] / k[ok]))
    return u


def _frames(e, initial, idle, u, n, bits, parity):
    """Characters of `bits` data bits (LSB first), parity, one stop bit: each sampled three times a bit (majority)."""
    L = 1 + bits + (parity != "none") + 1
    starts = e[level_at(e, initial, e) != idle]
    out = []
    t_next = -1
    for t in starts:
        if t < t_next:
            continue
        if t + L * u >= n:
            break
        pos = t + (np.arange(L)[:, None] + np.array([0.3, 0.5, 0.7])[None, :]) * u
        v = level_at(e, initial, pos.astype(np.int64).ravel()).reshape(L, 3)
        bit = (v.sum(1) >= 2).astype(int)
        if idle == 0:
            bit = 1 - bit
        val = int(sum(int(b) << i for i, b in enumerate(bit[1:1 + bits])))
        ok = bit[0] == 0 and bit[-1] == 1
        if parity != "none":
            par = int(bit[1:1 + bits].sum() + bit[1 + bits]) & 1
            ok = ok and par == (0 if parity == "even" else 1)
        out.append((int(t), int(t + L * u), val, bool(ok)))
        t_next = t + (L - 0.4) * u
    return out


def read_uart(ch, n: int, rate: float, u: float | None = None):
    """-> (fit, params, layers) for a UART on this line, or None."""
    e0 = np.asarray(ch.edges, np.int64)
    u = u or uart_unit(e0)
    if not u or u < 3:
        return None
    e = clean(e0, CLEAN * u)
    if len(e) < 8:
        return None
    runs = np.diff(np.concatenate(([0], e, [n])))
    lv = (int(ch.initial) + np.arange(len(runs))) & 1
    idle = int(np.bincount(lv, weights=runs, minlength=2).argmax())
    best = None
    for bits, parity in UART_FORMATS:
        fr = _frames(e, ch.initial, idle, u, n, bits, parity)
        if len(fr) < 4:
            continue
        ok = sum(f[3] for f in fr) / len(fr)
        # a character changes at most bits + 2 times, random data about half of that; a clock read as characters
        # changes at every bit: only near that is it held against the reading
        inner = np.mean([np.searchsorted(e, b) - np.searchsorted(e, a) for a, b, _, _ in fr])
        busy = min(1.0, max(0.0, (bits + 1.5 - inner) / 2))
        fit = ok * busy * (1 - 0.5 ** (len(fr) / 4))
        if best is None or fit > best[0] + 1e-9:
            best = (fit, bits, parity, fr)
    if best is None:
        return None
    fit, bits, parity, fr = best
    good = [f for f in fr if f[3]]
    params = {"baud": rate / u, "idle": idle, "data_bits": bits, "parity": parity, "bit_order": "lsb"}
    layers = {"bytes": {"s": [f[0] for f in good], "hex": bytes(f[2] & 0xFF for f in good).hex()},
              "chars": [{"s": a, "e": b, "value": v, "ok": ok} for a, b, v, ok in fr]}
    return fit, params, layers


# ---------------- clocked lines ----------------

def clock_of(ch):
    e = np.asarray(ch.edges, np.int64)
    if len(e) < 8:
        return None
    half = float(np.median(np.diff(e)))
    c = clean(e, CLEAN * half)
    if len(c) < 8:
        return None
    half = float(np.median(np.diff(c)))
    lv = level_at(c, ch.initial, c)
    return {"edges": c, "rises": c[lv == 1], "falls": c[lv == 0], "half": half, "initial": int(ch.initial)}


# ---------------- I2C ----------------

def read_i2c(scl, sda, n: int):
    """-> (fit, params, layers) for an I2C bus, or None. Bits read at SCL's rises between START and STOP; 9 a byte."""
    ck = clock_of(scl)
    if ck is None:
        return None
    de = clean(sda.edges, CLEAN * ck["half"])
    if len(de) < 4:
        return None
    hi = level_at(ck["edges"], ck["initial"], de) == 1
    lvl = level_at(de, sda.initial, de)
    cond, kind = de[hi], np.where(lvl[hi] == 0, 1, 2)          # 1 START, 2 STOP
    if len(cond) < 2:
        return None
    rises = ck["rises"]
    txs, ok = [], 0
    for i in range(len(cond) - 1):
        if kind[i] != 1:
            continue
        a, b = cond[i], cond[i + 1]
        r = rises[(rises > a) & (rises < b)]
        if kind[i + 1] == 2 and len(r):
            r = r[:-1]                               # the rise a STOP is made after
        bits = level_at(de, sda.initial, r)
        nb = len(bits)
        good = nb >= 9 and nb % 9 == 0
        ok += good
        if nb < 9:
            continue
        addr = int(sum(int(x) << (6 - j) for j, x in enumerate(bits[:7])))
        rw = "read" if bits[7] else "write"
        acks = [int(bits[k]) == 0 for k in range(8, nb, 9)]
        data = [int(sum(int(x) << (7 - j) for j, x in enumerate(bits[k:k + 8]))) for k in range(9, nb - 8, 9)]
        txs.append({"s": int(a), "e": int(b), "addr": addr, "rw": rw, "addr_ack": acks[0] if acks else False,
                    "bytes": bytes(data).hex(), "acks": acks, "ok": bool(good)})
    n_tx = sum(1 for k in kind[:-1] if k == 1)
    if n_tx < 1:
        return None
    fit = ok / n_tx * (1 - 0.5 ** (n_tx / 1.5))
    return fit, {"clock_hz": None}, {"transactions": txs}


# ---------------- SPI ----------------

def windows(ck, cs, n: int):
    """The transfers' windows: a select's active level (the one holding the clock's changes), or the clock's bursts."""
    ce = ck["edges"]
    if cs is not None:
        ke = clean(cs.edges, 2 * ck["half"])
        if len(ke) < 2:
            return None
        bounds = np.concatenate(([0], ke, [n]))
        lv = (int(cs.initial) + np.arange(len(bounds) - 1)) & 1
        cnt = np.diff(np.searchsorted(ce, bounds))
        act = int(np.bincount(lv, weights=cnt, minlength=2).argmax())
        return [(int(bounds[i]), int(bounds[i + 1])) for i in range(len(lv)) if lv[i] == act]
    gaps = np.diff(ce)
    cut = np.flatnonzero(gaps > 4 * np.median(gaps))
    return list(zip(np.concatenate(([ce[0]], ce[cut + 1])).tolist(),
                    (np.concatenate((ce[cut], [ce[-1]])) + 1).tolist()))


def read_spi(clk, datas, cs, n: int):
    """-> (fit, params, layers, evidence) for an SPI bus on these lines, or None."""
    ck = clock_of(clk)
    if ck is None:
        return None
    w = windows(ck, cs, n)
    if not w:
        return None
    des = {d.name: clean(d.edges, CLEAN * ck["half"]) for d in datas}
    # the sampling edge: the one farther from where the data changes
    rise_d, fall_d = [], []
    for de in des.values():
        if len(de) and len(ck["rises"]) and len(ck["falls"]):
            for t, out in ((ck["rises"], rise_d), (ck["falls"], fall_d)):
                j = np.clip(np.searchsorted(t, de), 1, len(t) - 1)
                out.append(float(np.median(np.minimum(np.abs(de - t[j]), np.abs(de - t[j - 1])))))
    if not rise_d:
        return None
    edge = "rise" if np.mean(rise_d) >= np.mean(fall_d) else "fall"
    take = ck["rises"] if edge == "rise" else ck["falls"]
    cpol = int(level_at(ck["edges"], ck["initial"], np.array([max(0, w[0][0] - 1)]))[0]) if w[0][0] > 0 else ck["initial"]
    leading = "rise" if cpol == 0 else "fall"
    mode = cpol * 2 + (0 if edge == leading else 1)
    transfers, counts = [], []
    for a, b in w:
        t = take[(take >= a) & (take < b)]
        if not len(t):
            continue
        counts.append(len(t))
        tr = {"s": int(a), "e": int(b)}
        for d in datas:
            bits = level_at(des[d.name], d.initial, t)
            nb = len(bits) // 8 * 8
            words = [int(sum(int(x) << (7 - j) for j, x in enumerate(bits[k:k + 8]))) for k in range(0, nb, 8)]
            tr[d.name] = bytes(words).hex()
        transfers.append(tr)
    if len(counts) < 2:
        return None
    whole = sum(c % 8 == 0 for c in counts) / len(counts)
    covered = sum(counts) / max(1, len(take))
    inside = []
    for de in des.values():
        if len(de) >= 2:
            m = np.zeros(len(de), bool)
            for a, b in w:
                m |= (de >= a) & (de < b)
            inside.append(m.mean())
    if not inside:
        return None
    fit = whole * min(1.0, covered / 0.9) * float(np.mean(inside)) * (1 - 0.5 ** (len(counts) / 2))
    return fit, {"bit_order": "msb", "sample_edge": edge}, {"transfers": transfers}, {"mode": mode}
