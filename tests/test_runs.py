"""Timeline parts that follow on from one another without a gap (a writer that restarts its sample grid) are one
stretch of time for verify and states (fileio.load_runs); parts with a gap between them stay apart."""

import numpy as np

from wireskein import _fileformat as ff, capture as wc, states
from wireskein._engine import fileio


def _part(lv, tick0):
    lv = np.asarray(lv, np.uint8)
    return wc.Part(len(lv), [ff.Channel("P", np.packbits(lv, bitorder="little").tobytes(), len(lv), 1, 0)], {}, tick0)


def test_parts_without_a_gap_are_joined(tmp_path):
    wave = np.tile([1] * 3 + [0] * 7, 30)                  # 300 ticks of a 10-tick wave, cut at 145 and 233
    cap = wc.Capture(1_000_000, [_part(wave[:145], 0), _part(wave[145:233], 145), _part(wave[233:], 233)])
    p = wc.write(tmp_path / "c.wireskein", cap)
    (run,) = fileio.load_runs(p)
    assert run.n_samples == 300 and run.meta["parts"] == [0, 2]
    want = np.flatnonzero(np.diff(wave)) + 1
    assert np.array_equal(run.channels[0].edges, want)     # the edges at the cuts are kept (231 -> 233 falls)
    (x,) = states.of_file(p)["parts"][0]["lines"]["P"]
    assert x["periods"] == 28 and x["t1"] == 293            # one wave to its last whole pulse, not three


def test_a_gap_keeps_parts_apart(tmp_path):
    wave = np.tile([1] * 3 + [0] * 7, 10)
    cap = wc.Capture(1_000_000, [_part(wave, 0), _part(wave, 150)])
    p = wc.write(tmp_path / "c.wireskein", cap)
    assert [c.n_samples for c in fileio.load_runs(p)] == [100, 100]
