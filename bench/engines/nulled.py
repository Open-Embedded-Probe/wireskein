"""The null-compared engine: the views engine's candidates, each held against the same reader on a surrogate of its
lines that keeps what is not the protocol's structure and breaks what is (an open-world test: "fits no better than
lines without the structure would"). The claim's fit is weighed by its lift over the
surrogate's: the share of the room above the surrogate's fit that the reading takes.

- one line (UART): the line's runs in another order (seeded): the same widths, the frames broken
- several lines (I2C, SPI): the first role's line kept, the others shifted in time by a part of the capture: each
  line as it is, their relation broken

Shared by every protocol and reader: nothing in it names one."""
from __future__ import annotations

from dataclasses import replace

import numpy as np

from . import claims_of, readers as R
from .learned import all_candidates

SEED = 0x5EED


def shuffled(ch, n: int, rng):
    e = np.asarray(ch.edges, np.int64)
    if len(e) < 3:
        return ch
    runs = np.diff(e)                      # the runs between changes; the first and the last idle kept in place
    rng.shuffle(runs)
    return replace(ch, edges=np.concatenate(([e[0]], e[0] + np.cumsum(runs))))


def shifted(ch, n: int, by: int):
    e = (np.asarray(ch.edges, np.int64) + by) % n
    k = np.sort(e)
    # the level at 0 after a cyclic shift is the level at n - by before it
    lv0 = int(R.level_at(np.asarray(ch.edges, np.int64), ch.initial, n - by - 1)) if len(e) else int(ch.initial)
    return replace(ch, edges=k, initial=lv0)


def read(proto, cap, roles, n):
    ch = {r: cap.channel(x) for r, x in roles.items()}
    if proto == "uart":
        r = R.read_uart(ch["data"], n, cap.rate)
    elif proto == "i2c":
        r = R.read_i2c(ch["scl"], ch["sda"], n)
    elif proto == "spi":
        datas = [ch[k] for k in sorted(ch) if k.startswith("data")]
        r = R.read_spi(ch["clk"], datas, ch.get("cs"), n)
    else:
        return 0.0
    return r[0] if r else 0.0


def surrogate(cap, roles, rng):
    n = cap.n_samples
    names = list(dict.fromkeys(roles.values()))
    chans = {c.name: c for c in cap.channels}
    if len(names) == 1:
        chans[names[0]] = shuffled(chans[names[0]], n, rng)
    else:
        for k, x in enumerate(names[1:], 1):
            chans[x] = shifted(chans[x], n, int(n * (0.17 + 0.29 * k)) % n)
    return replace(cap, channels=list(chans.values()))


def null_fit(c, cap, tries=2):
    rng = np.random.default_rng(SEED)
    return max(read(c[0], surrogate(cap, c[1], rng), c[1], cap.n_samples) for _ in range(tries))


def engine(cap, tries=2):
    out = []
    for c in all_candidates(cap):
        if c[2] < 0.6:                                        # LIKELY: no claim either way
            out.append(c)
            continue
        nf = null_fit(c, cap, tries)
        lift = max(0.0, c[2] - nf) / max(1e-9, 1 - nf)       # the share of the room above the surrogate taken
        out.append((c[0], c[1], c[2] * lift, {**c[3], "null_fit": round(nf, 4)}, *c[4:]))
    return claims_of(out)
