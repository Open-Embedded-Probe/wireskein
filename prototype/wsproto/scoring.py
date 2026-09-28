"""Default scorer: raw metrics -> layer score -> combined total.

Kept separate from analyzers so alternative scorers can be compared on the
same expanded hypothesis tree (future plugin boundary).
"""

from __future__ import annotations

import math

from .stack import Node


def _quantity(n: float, scale: float = 8.0) -> float:
    """Little evidence (a handful of frames) cannot give a high score."""
    return 1.0 - math.exp(-n / scale)


def _degenerate(top_share: float) -> float:
    """Penalty factor for streams dominated by one value (a clock read as UART
    gives 0x55/0xF0...)."""
    return 1.0 if top_share <= 0.5 else max(0.05, 1 - (top_share - 0.5) / 0.5)


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

    # --- per analyzer -------------------------------------------------------
    def _uart(self, m):
        if m.get("frames", 0) < 2:
            return 0.0
        return (m["framing_rate"] * m["edge_coverage"] * _degenerate(m["top_byte_share"])
                * _quantity(m["frames_ok"]) * m["idle_share"] * (1 - 0.5 * m["clk_role_score"]))

    def _i2c(self, m):
        if m.get("starts", 0) == 0:
            return 0.0
        return (m["segment_ok_rate"] * m["sda_explained"] * m["scl_coverage"] * _quantity(m["bytes"])
                * (0.5 + 0.5 * m["clk_role_score"]))

    def _spi(self, m):
        if m.get("n_lines", 0) == 0:
            return 0.0
        # A single frame matches "multiple of 8 bits" by chance 1 in 8, so the
        # number of frames is evidence of its own.
        chance = 0.125 ** (m["bits_mod8_rate"] * m["frames"])
        return (m["bits_mod8_rate"] * (1 - chance) * m["clk_coverage"] * m["cs_windows_used"] * m.get("cs_tight", 1.0)
                * m["line_score"] * _quantity(m["bytes"]) * (0.5 + 0.5 * m["clk_role_score"]))

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


class LayerOnlyScorer(DefaultScorer):
    """Ablation: ignore upper-layer support."""
    name = "layer-only"
    upper_weight = 0.0
