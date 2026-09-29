def scan(samples, n, ch):
    c = 0
    p = (samples[0] >> ch) & 1
    for i in range(1, n):
        v = (samples[i] >> ch) & 1
        if v != p:
            c += 1
            p = v
    return c


def scan_numpy(samples, n, ch):
    import numpy as np
    a = np.frombuffer(samples, dtype=np.uint16, count=n)  # zero-copy view of the memoryview
    b = (a >> ch) & 1
    return int(np.count_nonzero(b[1:] != b[:-1]))


def i2c(bits, t, bounds):
    recs = []
    tx = nb = x = 0
    for f in range(0, len(bounds) - 1, 2):
        a = bounds[f]
        b = bounds[f + 1]
        nw = (b - a) // 9
        if nw == 0:
            continue
        words = []
        for w in range(nw):
            v = 0
            base = a + w * 9
            for k in range(9):
                v = (v << 1) | bits[base + k]
            x ^= v
            words.append(v)
        first = words[0] >> 1
        tx += 1
        nb += nw - 1
        recs.append({
            "addr": first >> 1,
            "rw": "read" if first & 1 else "write",
            "addr_ack": (words[0] & 1) == 0,
            "bytes": [w >> 1 for w in words[1:]],
            "start": t[a],
            "end": t[b - 1],
        })
    return recs, tx, nb, x


def inc(x):
    return x + 1
