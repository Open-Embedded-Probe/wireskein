"""The learnt engine: the readers' candidates on the fixed family of views, each given the probability of being right
by a logistic regression over its own figures (fit, lines, units read, line activity, protocol), learnt on the
tuning side only (set:tuning, its noise kinds; never the heldout seeds), and the best set taken by that probability.
Train: python -m engines.learned train (from bench/) writes learned_model.json."""
from __future__ import annotations

import json
from pathlib import Path

import numpy as np

from . import claims_of, _views
from . import candidates as cands_of

PROTOS = ("uart", "i2c", "spi")
MODEL = Path(__file__).parent / "learned_model.json"
TRAIN = ["set:tuning[0:100]|", "set:tuning[0:100]|xtalk:0.3:01", "set:tuning[0:100]|bounce:0.1",
         "set:tuning[0:100]|bounce:0.5", "set:tuning[0:100]|spike:0.1", "set:tuning[0:100]|decimate:4",
         "gen:spivar[0:100]|"]


def units(layers):
    for k in ("chars", "transactions", "transfers"):
        if k in layers:
            return len(layers[k])
    return 0


def features(c, cap):
    proto, roles, fit, params, layers, ev = c
    edges = [len(cap.channel(x).edges) for x in roles.values()]
    f = [1.0, fit, fit * fit, len(set(roles.values())), np.log1p(units(layers)), np.log1p(min(edges)),
         np.log1p(max(edges)), float("cs" in roles), float(params.get("view", "raw") != "raw")]
    f += [float(proto == p) for p in PROTOS]
    f += [fit * float(proto == p) for p in PROTOS]
    return f


def fit_model(X, y, l2=1e-2, it=3000, lr=0.1, groups=None):
    """Logistic regression; with groups (each row's protocol), right and wrong rows weighed to balance within each
    (a protocol whose candidates are mostly wrong - UART on data and decoy lines - is not learnt as never right)."""
    X, y = np.asarray(X, float), np.asarray(y, float)
    mu, sd = X.mean(0), X.std(0) + 1e-9
    mu[0], sd[0] = 0, 1
    Z = (X - mu) / sd
    wt = np.ones(len(y))
    if groups is not None:
        g = np.asarray(groups)
        for k in set(groups):
            for v in (0.0, 1.0):
                m = (g == k) & (y == v)
                if m.any():
                    wt[m] = 0.5 * (g == k).sum() / m.sum()
        wt *= len(y) / wt.sum()
    w = np.zeros(Z.shape[1])
    for _ in range(it):
        p = 1 / (1 + np.exp(-Z @ w))
        w -= lr * (Z.T @ (wt * (p - y)) / len(y) + l2 * w)
    return {"w": w.tolist(), "mu": mu.tolist(), "sd": sd.tolist(), "trained_on": TRAIN}


def prob(model, f):
    z = (np.asarray(f) - model["mu"]) / model["sd"]
    return float(1 / (1 + np.exp(-z @ np.asarray(model["w"]))))


def all_candidates(cap):
    best = {}
    for vname, v in _views(cap):
        for c in cands_of(v):
            k = (c[0], tuple(sorted(c[1].items())))
            if k not in best or c[2] > best[k][2]:
                best[k] = (*c[:3], {**c[3], "view": vname}, *c[4:])
    return list(best.values())


_MODEL = None


def engine(cap):
    global _MODEL
    if _MODEL is None:
        _MODEL = json.loads(MODEL.read_text())
    cs = [(c[0], c[1], prob(_MODEL, features(c, cap)), *c[3:]) for c in all_candidates(cap)]
    return claims_of(cs)


def train():
    import sys
    sys.path.insert(0, str(Path(__file__).parents[2] / "eval"))
    from wsbench import data
    import evaluate as ev
    X, y, g = [], [], []
    for spec in TRAIN:
        src, chain = spec.split("|")
        for ref in data.source_items(src):
            case = data.load_case(ref, src, chain)
            if sum(len(x.edges) for x in case.cap.channels) > 300000:
                continue
            for c in all_candidates(case.cap):
                claim = {"protocol": c[0], "roles": c[1], "params": c[3], "layers": c[4], "evidence": c[5]}
                right = False
                for b in case.truth["buses"]:
                    if b["protocol"] == c[0]:
                        try:
                            m = ev.match(b, claim)
                        except (KeyError, TypeError):
                            continue
                        right = right or (m["roles"] and m["params"])
                X.append(features(c, case.cap))
                y.append(float(right))
                g.append(c[0])
    MODEL.write_text(json.dumps(fit_model(X, y, groups=g)))
    print("rows", len(y), "right", int(sum(y)))


if __name__ == "__main__":
    train()
