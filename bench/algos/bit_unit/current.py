"""What WireSkein does today: features.estimate_units' first candidate (the async unit the survey uses)."""

from wsbench import algorithm
from wireskein._engine.features import channel_features


@algorithm("bit_unit", "current", status="main", about="features.estimate_units: largest unit fitting the runs")
def current(cap, lines):
    out = {}
    for n in lines:
        f = channel_features(cap, cap.channel(n))
        u = f.units[0] if f.units else None
        out[n] = {"unit_s": u.samples / cap.rate if u else None, "confidence": u.fit if u else 0.0}
    return out
