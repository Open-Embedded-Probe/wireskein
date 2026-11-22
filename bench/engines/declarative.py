"""The declarative engine: protocols as tables (bench/engines/protocols.toml), read by one interpreter that knows two
kinds of timing (async: a line with a bit time of its own; clocked: a clock and lines read at its edges) and three
ways of delimiting units (frames from a start bit, START / STOP, a select or the clock's bursts). Nothing per
protocol is code: candidates come from the declaration (a clock changes no less than the lines it clocks, a select
less than once a word, lines start at their declared idle), noise is handled from the timing alone (each line
cleaned against the hypothesis's length, a fixed family of views read), and the output layer is declared.
Expected (2026-11-23): the best to extend, below the hand-written readers in score, above them in cost."""
from __future__ import annotations

import itertools
import tomllib
from pathlib import Path

import numpy as np

from . import readers as R
from . import claims_of, _views

SPECS = tomllib.loads((Path(__file__).parent / "protocols.toml").read_text())


# ---------------- reading by timing ----------------

def read_async(spec, ch, n, rate):
    e0 = np.asarray(ch.edges, np.int64)
    u = R.uart_unit(e0)
    if not u or u < 3:
        return None
    e = R.clean(e0, R.CLEAN * u)
    if len(e) < 8:
        return None
    idle = 1 - int(spec.get("start", 0))
    best = None
    for bits in spec["data_bits"]:
        for parity in spec["parity"]:
            fr = R._frames(e, ch.initial, idle, u, n, bits, parity)
            if len(fr) < spec.get("min_frames", 4):
                continue
            ok = sum(f[3] for f in fr) / len(fr)
            inner = np.mean([np.searchsorted(e, b) - np.searchsorted(e, a) for a, b, _, _ in fr])
            busy = min(1.0, max(0.0, (bits + 1.5 - inner) / 2))
            fit = ok * busy * (1 - 0.5 ** (len(fr) / 4))
            if best is None or fit > best[0] + 1e-9:
                best = (fit, bits, parity, fr)
    if best is None:
        return None
    fit, bits, parity, fr = best
    good = [f for f in fr if f[3]]
    params = {"baud": rate / u, "idle": idle, "data_bits": bits, "parity": parity, "bit_order": spec["order"]}
    layers = {"bytes": {"s": [f[0] for f in good], "hex": bytes(f[2] & 0xFF for f in good).hex()},
              "chars": [{"s": a, "e": b, "value": v, "ok": ok} for a, b, v, ok in fr]}
    return fit, params, layers, {}


def _units_startstop(ck, data, n):
    de = R.clean(data.edges, R.CLEAN * ck["half"])
    hi = R.level_at(ck["edges"], ck["initial"], de) == 1
    lvl = R.level_at(de, data.initial, de)
    cond, kind = de[hi], np.where(lvl[hi] == 0, 1, 2)
    out = []
    for i in range(len(cond) - 1):
        if kind[i] == 1:
            out.append((int(cond[i]), int(cond[i + 1]), kind[i + 1] == 2))
    return out, {data.name: de}


def _word(bits, order):
    return int(sum(int(x) << (len(bits) - 1 - j if order == "msb" else j) for j, x in enumerate(bits)))


def read_clocked(spec, roles, lines, n):
    clk = lines[roles[spec["clock"]]]
    ck = R.clock_of(clk)
    if ck is None:
        return None
    data = [lines[v] for k, v in roles.items() if k.startswith("data") or k == "sda"]
    sel = lines.get(roles.get("cs")) if roles.get("cs") else None
    w = spec["word"]
    if spec["delimit"] == "startstop":
        units, des = _units_startstop(ck, data[0], n)
    else:
        win = R.windows(ck, sel, n)
        if not win:
            return None
        units = [(a, b, False) for a, b in win]
        des = {d.name: R.clean(d.edges, R.CLEAN * ck["half"]) for d in data}
    if len(units) < spec.get("min_units", 1):
        return None
    if spec["sample"] == "either":
        dist = {}
        for edge, t in (("rise", ck["rises"]), ("fall", ck["falls"])):
            ds = []
            for de in des.values():
                if len(de) and len(t) > 1:
                    j = np.clip(np.searchsorted(t, de), 1, len(t) - 1)
                    ds.append(float(np.median(np.minimum(np.abs(de - t[j]), np.abs(de - t[j - 1])))))
            dist[edge] = np.mean(ds) if ds else 0.0
        edge = max(dist, key=dist.get)
    else:
        edge = spec["sample"]
    take = ck["rises"] if edge == "rise" else ck["falls"]
    out_units, whole = [], 0
    for a, b, stop in units:
        t = take[(take > a) & (take < b)]
        if stop and len(t):
            t = t[:-1]                               # the clock edge a closing condition is made after
        if not len(t):
            continue
        whole += len(t) % w == 0
        u = {"s": a, "e": b}
        for d in data:
            bits = R.level_at(des[d.name], d.initial, t)
            u[d.name] = [bits[k:k + w] for k in range(0, len(bits) // w * w, w)]
        out_units.append(u)
    if not out_units:
        return None
    covered = sum(sum(len(x) for x in u[data[0].name]) * w for u in out_units) / max(1, len(take))
    inside = []
    for de in des.values():
        if len(de) >= 2:
            m = np.zeros(len(de), bool)
            for a, b, _ in units:
                m |= (de >= a) & (de < b)
            inside.append(m.mean())
    fit = whole / len(out_units) * min(1.0, covered / 0.9) * (float(np.mean(inside)) if inside else 0.0) \
        * (1 - 0.5 ** (len(out_units) / 1.5))
    layers, ev = _layer(spec, out_units, data), {}
    if spec["delimit"] != "startstop":
        cpol = int(R.level_at(ck["edges"], ck["initial"], np.array([max(0, units[0][0] - 1)]))[0])
        ev["mode"] = cpol * 2 + (0 if edge == ("rise" if cpol == 0 else "fall") else 1)
    return fit, {"bit_order": spec["order"], "sample_edge": edge}, layers, ev


def _layer(spec, units, data):
    w, order = spec["word"], spec["order"]
    if spec["layer"] == "transactions":
        fw, ack = spec["first_word"], spec.get("ack_bit")
        out = []
        for u in units:
            words = u[data[0].name]
            if not words:
                continue
            first = words[0]
            a0, a1 = fw["addr"]
            body = [_word(x[:8], order) for x in words[1:]]
            out.append({"s": u["s"], "e": u["e"], "addr": _word(first[a0:a1], order),
                        "rw": "read" if first[fw["rw"]] else "write",
                        "addr_ack": bool(first[ack] == 0) if ack is not None else None,
                        "bytes": bytes(body).hex(), "acks": [bool(x[ack] == 0) for x in words] if ack is not None else []})
        return {"transactions": out}
    return {"transfers": [{"s": u["s"], "e": u["e"],
                           **{d.name: bytes(_word(x, order) for x in u[d.name]).hex() for d in data}} for u in units]}


# ---------------- candidates from the declarations ----------------

def _assignments(spec, act):
    """Role assignments the declaration allows: a clock no less busy than the lines it clocks, a select changing
    less than once a word, lines at their declared idle."""
    roles = spec["lines"]
    by = {c.name: c for c in act}
    if spec["timing"] == "async":
        for c in act:
            if len(c.edges) >= 20:
                yield {roles[0]: c.name}
        return
    clock = spec["clock"]
    fixed = [r for r in roles if not r.endswith(("+", "?")) and r != clock]
    multi = [r[:-1] for r in roles if r.endswith("+")]
    opt = [r[:-1] for r in roles if r.endswith("?")]
    idle = spec.get("idle")
    for clk in act:
        if idle is not None and clk.initial != idle:
            continue
        others = [c for c in act if c is not clk and len(c.edges) <= 1.5 * len(clk.edges)
                  and (idle is None or c.initial == idle)]
        for combo in itertools.permutations(others, len(fixed)):
            base = {clock: clk.name, **dict(zip(fixed, (c.name for c in combo)))}
            rest = [c for c in others if c not in combo]
            sels = [None] + ([c for c in act if c is not clk and len(c.edges) * spec["word"] <= len(clk.edges)]
                             if opt else [])
            for s in sels:
                r = dict(base)
                if s is not None:
                    if s.name in r.values():
                        continue
                    r[opt[0]] = s.name
                if multi:
                    for d in rest:
                        if d is not s:
                            yield {**r, f"{multi[0]}0": d.name}
                else:
                    yield r


def candidates(cap):
    n, rate = cap.n_samples, cap.rate
    act = [c for c in cap.channels if len(c.edges) >= 4]
    lines = {c.name: c for c in cap.channels}
    out = []
    for name, spec in SPECS.items():
        for roles in _assignments(spec, act):
            if spec["timing"] == "async":
                r = read_async(spec, lines[roles[spec["lines"][0]]], n, rate)
            else:
                r = read_clocked(spec, roles, lines, n)
            if r:
                out.append((name, roles, *r))
    # a protocol with lines declared "+": readings of the same clock and select joined (two data lines at most)
    joined = []
    for name, spec in SPECS.items():
        multi = [x[:-1] for x in spec["lines"] if x.endswith("+")]
        if not multi:
            continue
        key = lambda c: (c[1][spec["clock"]], c[1].get("cs"))
        good = sorted((c for c in out if c[0] == name and c[2] >= 0.6), key=lambda c: -c[2])
        seen = {}
        for c in good:
            seen.setdefault(key(c), []).append(c[1][f"{multi[0]}0"])
        for (clk, cs), ds in seen.items():
            if len(ds) < 2:
                continue
            roles = {spec["clock"]: clk, **({"cs": cs} if cs else {}),
                     **{f"{multi[0]}{i}": d for i, d in enumerate(ds[:2])}}
            r = read_clocked(spec, roles, lines, n)
            if r:
                joined.append((name, roles, *r))
    return out + joined


def engine(cap):
    best = {}
    for vname, v in _views(cap):
        for c in candidates(v):
            k = (c[0], tuple(sorted(c[1].items())))
            if k not in best or c[2] > best[k][2]:
                best[k] = (*c[:3], {**c[3], "view": vname}, *c[4:])
    return claims_of(list(best.values()))
