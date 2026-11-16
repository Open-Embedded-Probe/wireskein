"""Stages: what an algorithm of each stage is given, what it returns, and how it is scored.

The stage scores, never the algorithm: every algorithm of a stage is measured by the same code against the same
truth. An algorithm sees only the capture (channels named D0..Dn in a shuffled order), never the truth.

    lineclass     fn(cap) -> {line: {"label": one of data.LABELS, ...}}
    clock_period  fn(cap, lines) -> {line: {"period_s": float | None, "confidence": float}}
    bit_unit      fn(cap, lines) -> {line: {"unit_s": float | None, "confidence": float}}
    clean         fn(cap) -> Capture (the same channels, edges cleaned)
    grouping      fn(cap) -> [{"clock", "data": [...], "select": str | None, "score"}] (candidate buses, any order)
    e2e           fn(cap) -> claims (wireskein.decode.export(..., mode="all", alternatives=True)["claims"])

clock_period and bit_unit are asked only about the lines whose truth has the value (a clock's period, an
asynchronous line's bit), so they are measured as if the line's class were known; lineclass measures that class.
"""

from __future__ import annotations

import math
import sys
from collections import Counter

import numpy as np

from .data import Case

sys.path.insert(0, str(__import__("pathlib").Path(__file__).resolve().parents[2] / "eval"))


def _ratio_scores(got: float | None, want: float) -> dict:
    if not got or not math.isfinite(got) or got <= 0:
        return {"answered": False, "hit3": False, "hit10": False, "octave": False, "err": None}
    r = got / want
    x = r if r >= 1 else 1 / r
    return {"answered": True, "hit3": abs(r - 1) <= 0.03, "hit10": abs(r - 1) <= 0.10,
            # off by a whole factor (2, 3 ... or 1/2, 1/3 ...): a harmonic, not a measuring error
            "octave": abs(r - 1) > 0.10 and round(x) >= 2 and abs(x / round(x) - 1) < 0.05,
            "err": abs(math.log(r)), "got": got, "want": want}


def _rate_summary(rows: list[dict]) -> dict:
    n = len(rows)
    if not n:
        return {"n": 0}
    errs = [r["err"] for r in rows if r.get("err") is not None]
    return {"n": n, "hit3": sum(r["hit3"] for r in rows) / n, "hit10": sum(r["hit10"] for r in rows) / n,
            "answered": sum(r["answered"] for r in rows) / n, "octave": sum(r["octave"] for r in rows) / n,
            "median_err": float(np.median(errs)) if errs else None}


class Stage:
    name = ""
    about = ""
    headline = ""                # the summary metric tables show first (higher is better unless in LOWER)
    LOWER: tuple = ()

    def wants(self, case: Case) -> bool:
        return True

    def run(self, algo, case: Case):
        raise NotImplementedError

    def score(self, out, case: Case) -> list[dict]:
        """Per item (a line, a case) rows; each has "item"."""
        raise NotImplementedError

    def summarize(self, rows: list[dict]) -> dict:
        raise NotImplementedError


class LineClass(Stage):
    name = "lineclass"
    about = "each line's class: clock / data / select / periodic / random / static"
    headline = "accuracy_ns"        # select is grouping's to tell (see summarize); accuracy is kept beside it

    def run(self, algo, case):
        return algo(case.cap)

    def score(self, out, case):
        rows = []
        for ch, t in case.lines.items():
            o = out.get(ch) or {}
            got = o.get("label")
            rows.append({"item": ch, "truth": t.label, "protocol": t.protocol, "got": got, "correct": got == t.label,
                         "confidence": o.get("confidence")})
        return rows

    def summarize(self, rows):
        n = len(rows)
        tp = sum(r["truth"] == "clock" and r["got"] == "clock" for r in rows)
        said = sum(r["got"] == "clock" for r in rows)
        real = sum(r["truth"] == "clock" for r in rows)
        conf = Counter(f"{r['truth']}->{r['got']}" for r in rows if not r["correct"])
        # select is a role a line plays for others (it frames their bursts): told by grouping, not by the line alone.
        # accuracy_ns counts select and data as one class
        ns = lambda x: "data" if x == "select" else x  # noqa: E731
        return {"n": n, "accuracy": sum(r["correct"] for r in rows) / n if n else None,
                "accuracy_ns": sum(ns(r["truth"]) == ns(r["got"]) for r in rows) / n if n else None,
                "clock_precision": tp / said if said else None, "clock_recall": tp / real if real else None,
                "confusion": dict(conf.most_common(8))}


class _Rate(Stage):
    key = ""
    out_key = ""
    headline = "hit3"
    LOWER = ("median_err", "octave")

    def _lines(self, case):
        return [ch for ch, t in case.lines.items() if getattr(t, self.key) and not t.variable and t.label != "static"]

    def wants(self, case):
        return bool(self._lines(case))

    def run(self, algo, case):
        return algo(case.cap, self._lines(case))

    def score(self, out, case):
        rows = []
        for ch in self._lines(case):
            t = case.lines[ch]
            o = out.get(ch) or {}
            rows.append({"item": ch, "label": t.label, "protocol": t.protocol, "nominal": t.nominal,
                         "confidence": o.get("confidence"), **_ratio_scores(o.get(self.out_key), getattr(t, self.key))})
        return rows

    def summarize(self, rows):
        return _rate_summary(rows)


class ClockPeriod(_Rate):
    name = "clock_period"
    about = "a clock's (or periodic line's) period, rising edge to rising edge"
    key, out_key = "period_s", "period_s"


class BitUnit(_Rate):
    name = "bit_unit"
    about = "an asynchronous line's bit time"
    key, out_key = "unit_s", "unit_s"


def _match(want: np.ndarray, got: np.ndarray, tol: int) -> int:
    """Edges of `got` within tol ticks of one of `want`, each counted once on both sides."""
    if not len(want) or not len(got):
        return 0
    j = np.clip(np.searchsorted(got, want), 1, len(got) - 1)
    near = np.where(np.abs(got[j - 1] - want) <= np.abs(got[j] - want), j - 1, j)
    ok = np.abs(got[near] - want) <= tol
    return int(len(np.unique(near[ok])))


class Clean(Stage):
    name = "clean"
    about = "noise left out of the lines: edges against the capture without the noise"
    headline = "f1_rel"
    TOL = 2           # ticks (times the channel's step): f1
    TOL_REL = 0.25    # of the line's shortest run (its 5th percentile, without the noise): f1_rel. A cleaning that
                      # reads a bouncing edge at its last crossing moves it by the bounce, tens of ns: many ticks at
                      # a high rate, still a small part of a bit

    def wants(self, case):
        return case.clean is not None

    def run(self, algo, case):
        return algo(case.cap)

    def score(self, out, case):
        rows = []
        for ch in case.cap.channels:
            want = np.asarray(case.clean.channel(ch.name).edges, np.int64)
            noisy = np.asarray(ch.edges, np.int64)
            got = np.asarray(out.channel(ch.name).edges, np.int64)
            if not len(want) and not len(noisy):
                continue
            tp = _match(want, got, self.TOL * max(1, ch.step))
            short = float(np.percentile(np.diff(want), 5)) if len(want) > 2 else 0.0
            tp_rel = _match(want, got, max(self.TOL * max(1, ch.step), self.TOL_REL * short))
            rows.append({"item": ch.name, "label": case.lines[ch.name].label, "want": len(want), "got": len(got),
                         "noisy": len(noisy), "tp": tp, "fp": len(got) - tp, "fn": len(want) - tp,
                         "tp_rel": tp_rel, "fp_rel": len(got) - tp_rel, "fn_rel": len(want) - tp_rel,
                         "level_ok": (ch.initial == out.channel(ch.name).initial)})
        return rows

    def summarize(self, rows):
        tp, fp, fn = (sum(r[k] for r in rows) for k in ("tp", "fp", "fn"))
        p = tp / (tp + fp) if tp + fp else 1.0
        r = tp / (tp + fn) if tp + fn else 1.0
        exact = sum(r_["fp"] == 0 and r_["fn"] == 0 for r_ in rows)
        tq, fq, nq = (sum(r_.get(k, 0) for r_ in rows) for k in ("tp_rel", "fp_rel", "fn_rel"))
        f1_rel = 2 * tq / (2 * tq + fq + nq) if tq + fq + nq else 1.0
        return {"n": len(rows), "f1_rel": f1_rel, "f1": 2 * p * r / (p + r) if p + r else 0.0, "precision": p, "recall": r,
                "lines_exact": exact / len(rows) if rows else None,
                "spurious_per_1k": 1000 * fp / max(1, tp + fn)}


class E2E(Stage):
    name = "e2e"
    about = "the whole analysis: buses found against the truth (eval/evaluate.py's judging)"
    headline = "ok"
    LOWER = ("false_confirmed", "false_likely", "seconds")

    def run(self, algo, case):
        return algo(case.cap)

    def score(self, out, case):
        import evaluate  # eval/evaluate.py
        rec = evaluate.judge(case.cap, case.truth, out)
        # each truth bus with its hardness tags (data.hardness: judge keeps the truth's order)
        detail = [{"protocol": b["protocol"], "outcome": b["outcome"], "tags": t, "decode": b.get("decode")}
                  for b, t in zip(rec["buses"], case.bus_tags or [[]] * len(rec["buses"]))
                  if b["in_scope"] and b["outcome"] != "absent"]
        buses = [b for b in rec["buses"] if b["in_scope"] and b["outcome"] != "absent"]
        return [{"item": "case", "buses": len(buses),
                 "ok": sum(b["outcome"] in ("confirmed", "likely") for b in buses),
                 "confirmed": sum(b["outcome"] == "confirmed" for b in buses),
                 "outcomes": [f"{b['protocol']}:{b['outcome']}" for b in buses],
                 "false_confirmed": sum(not c["correct"] and c["verdict"] == "confirmed" for c in rec["claims"]),
                 "false_likely": sum(not c["correct"] and c["verdict"] == "likely" for c in rec["claims"]),
                 # wrong claims that use a line aliasing at this rate (counted in false_confirmed too)
                 "false_confirmed_aliased": sum(not c["correct"] and c["verdict"] == "confirmed"
                                                and bool(set(c["roles"].values()) & case.aliased) for c in rec["claims"]),
                 "detail": detail,
                 # what the output itself says (known at run time, without the truth): for switching strategies
                 "claims_confirmed": sum(c["verdict"] == "confirmed" for c in rec["claims"]),
                 "claims_likely": sum(c["verdict"] == "likely" for c in rec["claims"]),
                 "claims_ambiguous": sum(c["verdict"] == "ambiguous" for c in rec["claims"]),
                 "decode": [b.get("decode") for b in buses if b["outcome"] in ("confirmed", "likely")]}]

    def summarize(self, rows):
        nb = sum(r["buses"] for r in rows)
        dec = [d for r in rows for d in r["decode"] if d is not None]
        out = {"n": len(rows), "buses": nb, "ok": sum(r["ok"] for r in rows) / nb if nb else None,
               "confirmed": sum(r["confirmed"] for r in rows) / nb if nb else None,
               "false_confirmed": sum(r["false_confirmed"] for r in rows),
               "false_likely": sum(r["false_likely"] for r in rows),
               "decode_mean": float(np.mean(dec)) if dec else None,
               "false_confirmed_aliased": sum(r.get("false_confirmed_aliased", 0) for r in rows)}
        # apart: buses a capture can show (no hardness tag) and those it cannot well (data.hardness), each tag too
        det = [b for r in rows for b in r.get("detail", [])]
        good = lambda bs: sum(b["outcome"] in ("confirmed", "likely") for b in bs) / len(bs) if bs else None  # noqa
        conf = lambda bs: sum(b["outcome"] == "confirmed" for b in bs) / len(bs) if bs else None  # noqa
        easy, hard = [b for b in det if not b["tags"]], [b for b in det if b["tags"]]
        out.update({"buses_readable": len(easy), "ok_readable": good(easy), "confirmed_readable": conf(easy),
                    "buses_hard": len(hard), "ok_hard": good(hard), "confirmed_hard": conf(hard)})
        dh = [b["decode"] for b in hard if b["outcome"] in ("confirmed", "likely") and b["decode"] is not None]
        out["decode_hard"] = float(np.mean(dh)) if dh else None
        for tag in sorted({t for b in hard for t in b["tags"]}):
            bs = [b for b in hard if tag in b["tags"]]
            out[f"ok_{tag}"], out[f"confirmed_{tag}"] = good(bs), conf(bs)
        return out


# the clock, data and select roles of the clocked buses, as eval's truth names them
SYNC_ROLES = {"i2c": ("scl", ("sda",), None), "spi": ("clk", ("mosi", "miso"), "cs"),
              "rvswd": ("clk", ("dio",), None), "swd": ("swclk", ("swdio",), None)}


def true_groups(case: Case) -> list[dict]:
    out = []
    for b in case.truth.get("buses", []):
        spec = SYNC_ROLES.get(b["protocol"])
        if not spec:
            continue
        clk, datas, sel = spec
        r = b["roles"]
        if clk not in r or len(case.cap.channel(r[clk]).edges) < 2:
            continue
        out.append({"protocol": b["protocol"], "clock": r[clk], "data": sorted(r[d] for d in datas if d in r),
                    "select": r.get(sel) if sel else None})
    return out


class Grouping(Stage):
    name = "grouping"
    about = "the clocked buses' lines grouped: clock + data lines (+ select) among the candidates"
    headline = "found"
    LOWER = ("groups_per_case", "wrong_clock_groups")

    def wants(self, case):
        return bool(true_groups(case))

    def run(self, algo, case):
        return algo(case.cap)

    def score(self, out, case):
        rows = []
        cand = [{"clock": g["clock"], "data": sorted(g["data"]), "select": g.get("select"),
                 "score": float(g.get("score", 0))} for g in out]
        clocks = {t["clock"] for t in true_groups(case)}
        for t in true_groups(case):
            same = sorted((g for g in cand if g["clock"] == t["clock"]), key=lambda g: -g["score"])
            hit = [i for i, g in enumerate(same) if g["data"] == t["data"] and g["select"] == t["select"]]
            best_data = same[0]["data"] if same else []
            rows.append({"item": t["clock"], "protocol": t["protocol"], "found": bool(hit),
                         "true_score": same[hit[0]]["score"] if hit else 0.0,
                         "top": bool(hit) and hit[0] == 0, "clock_seen": bool(same),
                         "data_recall": len(set(best_data) & set(t["data"])) / len(t["data"]) if t["data"] else 1.0,
                         "groups": len(cand), "wrong_clock_groups": sum(g["clock"] not in clocks for g in cand)})
        return rows

    def summarize(self, rows):
        n = len(rows)
        if not n:
            return {"n": 0}
        return {"n": n, "found": sum(r["found"] for r in rows) / n, "top": sum(r["top"] for r in rows) / n,
                # how strongly the true group is told (its score; 0 when not among the candidates): what the
                # protocols' scores build on, not only whether it passes the threshold
                "true_score": float(np.mean([r["true_score"] for r in rows])),
                "clock_seen": sum(r["clock_seen"] for r in rows) / n,
                "data_recall": float(np.mean([r["data_recall"] for r in rows])),
                "groups_per_case": float(np.median([r["groups"] for r in rows])),
                "wrong_clock_groups": float(np.mean([r["wrong_clock_groups"] for r in rows]))}


STAGES = {s.name: s for s in (LineClass(), ClockPeriod(), BitUnit(), Clean(), Grouping(), E2E())}
