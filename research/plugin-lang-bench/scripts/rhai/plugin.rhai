fn scan(samples, n, ch) {
    let c = 0;
    let p = (samples[0] >> ch) & 1;
    for i in 1..n {
        let v = (samples[i] >> ch) & 1;
        if v != p { c += 1; p = v; }
    }
    c
}

fn i2c(bits, t, bounds) {
    let recs = [];
    let tx = 0; let nb = 0; let x = 0;
    let f = 0;
    let nf = bounds.len();
    while f + 1 < nf {
        let a = bounds[f];
        let b = bounds[f + 1];
        f += 2;
        let nw = (b - a) / 9;
        if nw == 0 { continue; }
        let bytes = [];
        let first_ack = false;
        for w in 0..nw {
            let v = 0;
            let base = a + w * 9;
            for k in 0..9 { v = (v << 1) | bits[base + k]; }
            x = x ^ v;
            if w == 0 { first_ack = (v & 1) == 0; }
            bytes.push(v >> 1);
        }
        let first = bytes.shift();
        tx += 1; nb += nw - 1;
        recs.push(#{
            addr: first >> 1,
            rw: if (first & 1) != 0 { "read" } else { "write" },
            addr_ack: first_ack,
            bytes: bytes,
            start: t[a],
            end: t[b - 1]
        });
    }
    [recs, tx, nb, x]
}

fn inc(x) { x + 1 }
