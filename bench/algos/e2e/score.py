"""The framed protocols' score (wireskein._engine.declarative.frame_score) counted otherwise, the rest of the
analysis as it is.

The engine's score: fit x (1 - w + w x checks) x quantity x (0.5 + 0.5 clock-likeness) x (0.5 + 0.5 pair). For
I2C the checked field is the ACK bit, weighed 0.2: a bus whose addresses mostly answer no one (a scan, a device
not there) cannot pass 0.8, so it is never confirmed (0.85), however clean: NACK is I2C as much as ACK is.
"""

import math

from wsbench import algorithm
from wireskein._engine import methods
from wireskein._engine.declarative import _chance, frame_score, q


def _claims(cap):
    from wireskein import decode
    return decode.export(decode.analyze(cap), cap, mode="all", alternatives=True)["claims"]


def _score(ack_neutral: bool, soft: float):
    def score(e):
        if ack_neutral and e["protocol"] == "i2c":
            e = {**e, "check_weight": 0.0}       # ACK and NACK are both I2C: no evidence either way
        if soft == 0.5:
            return frame_score(e)
        a = 1 - soft
        return (e["fit_evidence"] * (1 - e["check_weight"] + e["check_weight"] * e["check_rate"])
                * q(e["words"], e["evidence_scale"]) * (a + soft * e["clk"]) * (a + soft * e["pair"]))
    return score


def _evidence(chance: float | None, soft: float):
    """I2C counted by its frames, not its words: k frames that each fit the length rule (9n + 1) by chance with
    probability p all do so with p^k; the score's quantity term is 1 - p^k (p: the rule's own chance, or the given
    one). An address no one answers is one word but a whole frame of evidence. ACK neutral; other protocols as the
    engine counts them."""
    def score(e):
        if e["protocol"] != "i2c":
            return frame_score(e)
        p = chance if chance is not None else _chance(e["rule"])
        a = 1 - soft
        return e["fit"] * (1 - p ** e["fits"]) * (a + soft * e["clk"]) * (a + soft * e["pair"])
    return score


def _tail(n: int, k: int, p: float) -> float:
    """P(X >= k) for X ~ Binomial(n, p): how likely k fits of n frames are by chance."""
    if k <= 0:
        return 1.0
    if k > n:
        return 0.0
    lp, lq = math.log(p), math.log1p(-p)
    terms = [math.lgamma(n + 1) - math.lgamma(i + 1) - math.lgamma(n - i + 1) + i * lp + (n - i) * lq
             for i in range(k, n + 1)]
    m = max(terms)
    return min(1.0, math.exp(m) * sum(math.exp(t - m) for t in terms))


def clearance(e, share: float = 0.15) -> float | None:
    """Of the frames' START / Sr / STOP events (SDA changing while SCL is high), the share at least `share` of the
    local SCL half away from any SCL change. I2C's are (t_SU;STA, t_HD;STA, t_SU;STO: microseconds into the high);
    a data line that changes with its clock (SPI mode 1 / 3) makes "events" on the clock's edges. None: not known."""
    fr, sb = e["frames_obj"], e["bits"]
    cap = getattr(sb, "view", None)
    events = [t for kind, t in (fr.events or []) if kind in ("S", "Sr", "P")]
    if cap is None or not events:
        return None
    import numpy as np
    ce = np.asarray(cap.channel(sb.clock).edges, np.int64)
    if len(ce) < 3:
        return None
    t = np.asarray(events, np.int64)
    j = np.clip(np.searchsorted(ce, t), 1, len(ce) - 1)
    d = np.minimum(np.abs(t - ce[j - 1]), np.abs(ce[j] - t))
    # the local half: the shorter of the two runs of the clock around the event's neighbours
    jj = np.clip(j, 2, len(ce) - 2)
    half = np.minimum(ce[jj] - ce[jj - 1], np.minimum(ce[jj + 1] - ce[jj], ce[jj - 1] - ce[jj - 2])).astype(float)
    step = max(1, cap.channel(sb.clock).step)
    return float(np.mean(d >= np.maximum(step, share * half)))


def bus_free(e, allow: int = 2) -> float | None:
    """Of the STOPs, the share after which SCL stays still (at most `allow` changes: a spike) until the next START:
    an I2C bus is free between a STOP and a START. A free-running clock (one aliased to an I2C-like rate) goes on.
    None: no STOP to judge by."""
    import numpy as np
    fr, sb = e["frames_obj"], e["bits"]
    cap = getattr(sb, "view", None)
    ev = list(fr.events or [])
    if cap is None or not any(k == "P" for k, _ in ev):
        return None
    ce = np.asarray(cap.channel(sb.clock).edges, np.int64)
    free = n = 0
    for i, (kind, t) in enumerate(ev):
        if kind != "P":
            continue
        nxt = next((u for k, u in ev[i + 1:] if k in ("S", "Sr")), cap.n_samples)
        n += 1
        free += int(np.searchsorted(ce, nxt) - np.searchsorted(ce, t, side="right")) <= allow
    return free / n if n else None


I2C_MAX_HZ = 5e6     # UM10204: Hs-mode 3.4 MHz, Ultra Fast-mode (one way) 5 MHz


def scl_hz(e) -> float | None:
    """SCL's rate inside the frames: the median interval between its sampling edges there."""
    import numpy as np
    fr, sb = e["frames_obj"], e["bits"]
    cap = getattr(sb, "view", None)
    t = np.asarray(sb.t, np.int64)
    if cap is None or len(fr.bounds) == 0 or len(t) < 3:
        return None
    iv = [np.diff(t[a:b]) for a, b in fr.bounds.tolist() if b - a >= 3]
    iv = np.concatenate(iv) if iv else np.array([], np.int64)
    return float(cap.rate) / float(np.median(iv)) if len(iv) else None


def _binomial(chance: float | None, soft: float, clear: bool = False, free: bool = False,
              above_chance: float | None = None, max_hz: bool = False):
    """I2C counted by its frames above chance: of n frames, k fit the length rule; by chance (p per frame) at least
    k fit with probability P(X >= k). The score: the fit above chance ((fit - p) / (1 - p)) x (1 - P(X >= k)) x the
    clock and pair terms (soft). A true bus (every frame fits) comes near 1 with a few frames; lines whose frames fit
    about as often as chance stay near 0 however many frames. ACK neutral; other protocols as the engine counts."""
    def score(e):
        if e["protocol"] != "i2c":
            return frame_score(e)
        p = chance if chance is not None else _chance(e["rule"])
        n, k = e["frames"], e["fits"]
        pa = above_chance if above_chance is not None else p
        above = max(0.0, (e["fit"] - pa) / (1 - pa))
        a = 1 - soft
        s = above * (1 - _tail(n, k, p)) * (a + soft * e["clk"]) * (a + soft * e["pair"])
        if clear:
            cl = clearance(e)
            if cl is not None:
                s *= cl
        if free:
            fb = bus_free(e)
            if fb is not None:
                s *= fb
        if max_hz:
            hz = scl_hz(e)
            if hz is not None and hz > I2C_MAX_HZ:
                s = 0.0                          # no I2C runs this fast
        return s
    return score


@algorithm("e2e", "binomial", status="trial", about="I2C's score from its frames' fit above chance (binomial tail)",
           variants={  # and only as far as its START / STOP stand clear of SCL's changes
                     "half_clear": {"chance": 0.5, "soft": 0.25, "clear": True},
                     # and the bus free between a STOP and the next START; the fit above the rule's chance (2/9,
                     # so that one frame of ten out costs less); no faster than the spec's fastest SCL
                     "rule_clear_free_hz": {"chance": 0.5, "soft": 0.25, "clear": True, "free": True,
                                            "above_chance": 2 / 9, "max_hz": True}})
@algorithm("e2e", "binomial", status="retired", about="I2C's score from its frames' fit above chance (binomial tail)",
           variants={"half_clear_free": {"chance": 0.5, "soft": 0.25, "clear": True, "free": True},
                     "rule_clear_free": {"chance": 0.5, "soft": 0.25, "clear": True, "free": True,
                                         "above_chance": 2 / 9}},
           reason="steps to rule_clear_free_hz (2026-11-16, local nack / smoke / rate-smoke): bus_free brings nack's "
                  "false likely back to main's (8 -> 6 with bounces), the rule's chance gives a bus with one frame "
                  "of ten out its due (smoke spike ok 0.750 -> 0.792); the rate bound changed nothing there and "
                  "is the spec's, so it stays")
@algorithm("e2e", "binomial", status="retired", about="I2C's score from its frames' fit above chance (binomial tail)",
           variants={"rule": {"chance": None, "soft": 0.25}, "half": {"chance": 0.5, "soft": 0.25}},
           reason="2026-11-16, local (nack, smoke, rate-smoke): counting only the fit above chance still lets an I2C "
                  "hypothesis on an SPI data line (mode 1 / 3: it changes with the clock, so START-like events come "
                  "by the hundred and the lengths fit) take an SPI bus's margin (synth-mixed-0: 0.56 -> 0.03); rule's "
                  "chance (2/9) also confirmed wrongly twice. half_clear asks the START / STOP to stand clear")
def binomial(cap, chance, soft, clear=False, free=False, above_chance=None, max_hz=False):
    with methods.use({"frame_score": _binomial(chance, soft, clear, free, above_chance, max_hz)}):
        return _claims(cap)


@algorithm("e2e", "evidence", status="retired", about="I2C's score from its frames' chance fit, not its word count",
           variants={"rule": {"chance": None, "soft": 0.25}, "half": {"chance": 0.5, "soft": 0.25}},
           reason="2026-11-16, local: 1 - p^k counts every fitting frame, also those of lines that fit about as often "
                  "as chance: false likely doubled (nack 10 -> 20), clean buses lost (smoke 1.000 -> 0.958); binomial "
                  "counts the fit above chance")
def evidence(cap, chance, soft):
    with methods.use({"frame_score": _evidence(chance, soft)}):
        return _claims(cap)


@algorithm("e2e", "score", status="retired", about="frame_score counted otherwise (wireskein._engine.methods)",
           reason="2026-11-16, local (nack): ACK neutral alone confirms a little more (0.613 -> 0.654 / 0.677 clean) but the "
                  "word count (one word an addressed-and-NACKed frame) still holds such buses under 0.85",
           variants={"ack_neutral": {"ack_neutral": True, "soft": 0.5},
                     "ack_neutral_soft": {"ack_neutral": True, "soft": 0.25}})
def score(cap, ack_neutral, soft):
    with methods.use({"frame_score": _score(ack_neutral, soft)}):
        return _claims(cap)


@algorithm("e2e", "combo", status="trial",
           about="binomial.rule_clear_free_hz's I2C score with grouping/gated.w4's pair relation put in together",
           variants={"clear_gated": {},
                     # and SPI's CS read through the clock's period too (e2e select.w4_sparse_gated's steps)
                     "clear_gated_select": {"select": True}})
def combo(cap, select=False):
    from wsbench import registry
    steps = {"frame_score": _binomial(0.5, 0.25, True, True, 2 / 9, True),
             **registry.get("grouping", "gated.w4").engine_steps()}
    if select:
        steps.update(registry.get("e2e", "select.w4_sparse_gated").engine_steps())
    with methods.use(steps):
        return _claims(cap)
