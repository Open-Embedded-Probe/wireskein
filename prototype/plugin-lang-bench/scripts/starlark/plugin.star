def scan(samples, n, ch):
    c = 0
    p = (samples[0] >> ch) & 1
    for i in range(1, n):
        v = (samples[i] >> ch) & 1
        if v != p:
            c += 1
            p = v
    return c

def i2c(bits, t, bounds):
    recs = []
    tx = 0
    nb = 0
    x = 0
    for f in range(0, len(bounds) - 1, 2):
        a = bounds[f]
        b = bounds[f + 1]
        nw = (b - a) // 9
        if nw == 0:
            continue
        bytes = []
        first_ack = False
        for w in range(nw):
            v = 0
            base = a + w * 9
            for k in range(9):
                v = (v << 1) | bits[base + k]
            x = x ^ v
            if w == 0:
                first_ack = (v & 1) == 0
            bytes.append(v >> 1)
        first = bytes.pop(0)
        tx += 1
        nb += nw - 1
        recs.append({
            "addr": first >> 1,
            "rw": "read" if first & 1 else "write",
            "addr_ack": first_ack,
            "bytes": bytes,
            "start": t[a],
            "end": t[b - 1],
        })
    return (recs, tx, nb, x)

def inc(x):
    return x + 1
