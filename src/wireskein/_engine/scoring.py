"""Scorer for the upper layers on byte streams (lines, NMEA, Modbus RTU,
markers): raw metrics -> layer score -> combined total with the support of
the layers above. The line-level plugins score themselves (staged.py).
"""

from __future__ import annotations

from .stack import Node


class DefaultScorer:
    name = "default-v1"
    upper_weight = 0.5

    def layer(self, n: Node) -> float:
        m = n.metrics
        f = getattr(self, "_" + n.analyzer, None)
        return float(f(m)) if f else 0.0

    def combine(self, n: Node) -> float:
        support = max((c.total for c in n.children), default=0.0)
        n.metrics["_support"] = support
        # Missing upper support is not a penalty (most payloads have no known
        # grammar); present support closes part of the remaining gap.
        return n.layer_score + self.upper_weight * (1 - n.layer_score) * support * n.layer_score

    # --- per upper layer ----------------------------------------------------
    def _lines(self, m):
        if not m.get("bytes"):
            return 0.0
        return (m["printable"] ** 4) * min(1.0, m["newlines"] / 2) * (0.5 + 0.5 * min(1.0, 3 * m["word_share"]))

    def _checked(self, m):
        ok, bad = m.get("checks_passed", 0), m.get("checks_failed", 0)
        if ok == 0:
            return 0.0
        return (1 - 0.5 ** ok) * ok / (ok + bad)

    _nmea = _checked
    _modbus_rtu = _checked

    def _markers(self, m):
        return m["match_rate"] * (0.5 + 0.5 * m["balanced"]) if m.get("matched") else 0.0

