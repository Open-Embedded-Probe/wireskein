"""What WireSkein does today: survey.clock_info's period (the median rising-to-rising interval) on the raw line."""

from wsbench import algorithm
from wireskein._engine.features import channel_features
from wireskein._engine.survey import clock_info


@algorithm("clock_period", "current", status="main", about="survey.clock_info: median rising-to-rising interval")
def current(cap, lines):
    out = {}
    for n in lines:
        f = channel_features(cap, cap.channel(n))
        ci = clock_info(cap, f) if not f.static else None
        out[n] = {"period_s": ci.period / cap.rate if ci and ci.period else None,
                  "confidence": ci.clock_score if ci else 0.0}
    return out
