"""survey.gated_view for a clock of four samples a period: the other lines' pulses of a sample (crosstalk) taken out
even where the clock's own run of a sample keeps its period (a rate a little off a whole number of samples). Lines
made here."""

import numpy as np

from wireskein._engine import survey as SV
from wireskein._engine.features import channel_features
from wireskein._engine.model import Capture, Channel


def lines():
    clk, data = [], []
    t, lv = 1000, 0
    rng = np.random.default_rng(1)
    for burst in range(8):
        for i in range(32):
            hi = 1 if (burst, i) == (3, 7) else 2          # one high of a sample, its low three: the period kept
            clk += [t, t + hi]
            b = int(rng.integers(2))
            if b != lv:
                data.append(t + 2 + 2)                     # mode 0: the data changes on the fall
                lv = b
            t += 4
        t += 400
    spikes = np.arange(500, t, 37)                        # a neighbour's crosstalk: pulses of a sample, all along
    data = np.sort(np.concatenate([np.asarray(data), spikes, spikes + 1]))
    return Capture(16e6, t + 500, [Channel("CLK", 0, np.asarray(clk, np.int64)), Channel("D", 0, data)])


def test_the_crosstalk_on_a_data_line_is_out_and_the_clock_kept():
    cap = lines()
    ck = SV.clock_info(cap, channel_features(cap, cap.channel("CLK")))
    assert ck is not None and abs(ck.period - 4) < 0.5
    view, vc = SV.gated_view(cap, ck)
    assert view is not cap
    assert np.array_equal(view.channel("CLK").edges, cap.channel("CLK").edges)
    assert np.all(np.diff(view.channel("D").edges) > 1)
    assert SV.pair_relation(view, vc, "D").data_score > SV.pair_relation(cap, ck, "D").data_score + 0.3
