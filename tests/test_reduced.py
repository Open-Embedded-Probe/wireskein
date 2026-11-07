"""Logic reduced over intervals (wireskein-format §4.5: OEP capture format 1's any_high, any_low, change_latch): the
file form, the reading (any_* as samples at the step, change_latch as events with lower-bound counts), and checks that
answer only what a reduction makes sure. The reductions are made here from lines written here."""

import numpy as np
import pytest

from wireskein import _fileformat as ff
from wireskein import capture as wc
from wireskein._engine import fileio
from wireskein._engine.model import Capture, Channel, edges_from_dense
from wireskein.runlog import level, pulses, spi, starts
from wireskein.verify import check_level, check_pulses, check_spi, check_starts


def reduce(raw, d: int, kind: str, start: int = 0):
    """A line's samples reduced over intervals of d (the last one cut), as format 1 does."""
    raw = np.asarray(raw, np.uint8)
    out, prev = [], start
    for k in range(0, len(raw), d):
        x = raw[k:k + d]
        if kind == "any_high":
            out.append(int(x.max()))
        elif kind == "any_low":
            out.append(int(x.min()))
        else:
            full = np.concatenate(([prev], x))
            out.append(int(x[-1]) | (int(np.any(full[1:] != full[:-1])) << 1))
            prev = int(x[-1])
    return out


RAW = np.array([0, 0, 1, 1, 0, 1, 0, 0, 0, 1, 1, 1, 1, 0], np.uint8)    # 14 samples


def test_the_file_keeps_the_intervals_and_the_parts_own_length(tmp_path):
    p = tmp_path / "r.wireskein"
    chans = [ff.Channel("RAW", ff.pack(RAW.tobytes()), 14), ff.reduced("H", "any_high", reduce(RAW, 4, "any_high"), 4),
             ff.reduced("L", "any_low", reduce(RAW, 4, "any_low"), 4),
             ff.reduced("C", "change_latch", reduce(RAW, 4, "change_latch"), 4, pin=3)]
    ff.write(p, 1000, chans, ticks=14)
    head, got = ff.read(p)
    assert head["ticks"] == 14 and [c["encoding"] for c in head["channels"]] == ["bits", "any_high", "any_low",
                                                                                "change_latch"]
    assert [c.get("start_level") for c in head["channels"]] == [None, None, None, 0]
    _, h, lo, c = got
    assert h.values().tolist() == [1, 1, 1, 1] and lo.values().tolist() == [0, 0, 0, 0]
    assert c.values().tolist() == [3, 2, 3, 2] and c.acquisition == {"pin": 3}
    ks, lv, counts = c.latches()                      # every interval changed an odd number of times
    assert ks.tolist() == [0, 1, 2, 3] and lv.tolist() == [1, 0, 1, 0] and counts.tolist() == [1, 1, 1, 1]
    assert c.changes()[1].tolist() == [3, 7, 11, 15]
    cap = fileio.load(p)
    assert cap.n_samples == 14 and cap.channel("C").edges.tolist() == [3, 7, 11, 13]   # the last cut at the end
    assert cap.channel("H").acquisition == {"reduction": "any_high"}
    fileio.save(tmp_path / "again.wireskein", cap)    # written back as they were
    head2, again = ff.read(tmp_path / "again.wireskein")
    assert head2["ticks"] == 14
    assert {c.name: (c.encoding, getattr(c, "values", lambda: None)() is not None and c.values().tolist())
            for c in again if c.name != "RAW"} == {"H": ("any_high", [1, 1, 1, 1]), "L": ("any_low", [0, 0, 0, 0]),
                                                   "C": ("change_latch", [3, 2, 3, 2])}
    assert next(c for c in again if c.name == "C").acquisition == {"pin": 3}


def test_a_change_latch_that_contradicts_itself_is_refused():
    with pytest.raises(ValueError, match="change bit 0"):
        ff.reduced("C", "change_latch", [1], 4)               # at 1 after 0, but no change
    with pytest.raises(ValueError, match="2 bytes for 3 intervals"):
        ff.ReducedChannel("C", "change_latch", b"\0\0", 3, 4)
    with pytest.raises(ValueError, match="encoding one of"):
        ff.ReducedChannel("C", "any", b"", 0, 4)


def test_the_part_length_must_hold_every_line(tmp_path):
    chans = [ff.Channel("RAW", ff.pack(RAW.tobytes()), 14), ff.reduced("H", "any_high", [1, 1, 1, 1], 4)]
    with pytest.raises(ValueError, match="begin after it"):
        ff.write(tmp_path / "x.wireskein", 1000, chans, ticks=12)
    with pytest.raises(ValueError, match="end at 16"):
        ff.write(tmp_path / "x.wireskein", 1000, chans, ticks=17)


def test_an_interval_that_changed_and_came_back_hides_its_changes():
    c = ff.reduced("C", "change_latch", [0, 2, 0], 4)
    assert c.latches()[2].tolist() == [2]
    with pytest.raises(ValueError, match="came back"):
        c.changes()
    t = fileio.decode(c)
    assert t.hidden == 1 and t.as_channel() is None


def reduced_cap(raw, d, kind):
    v = np.asarray(reduce(raw, d, kind), np.uint8)
    if kind == "change_latch":
        t = fileio.decode(ff.reduced("P", kind, v, d))
        return fileio.view({"ticks": len(raw), "tick_hz": [1000, 1]}, [t])
    init, e = edges_from_dense(v, d, 0)
    return Capture(1000.0, len(raw), [Channel("P", init, e, d, 0, {"reduction": kind})])


def test_a_level_is_sure_only_at_the_other_level():
    high = np.ones(64, np.uint8)
    high[30] = 0                                                   # a 0 any_high cannot see
    ok, _, why = check_level(reduced_cap(high, 8, "any_high"), level("P", 1))
    assert ok is None and "may hold a 0" in why
    ok, _, _ = check_level(reduced_cap(np.zeros(64, np.uint8), 8, "any_high"), level("P", 0))
    assert ok is True                                              # every interval 0: 0 throughout
    ok, _, _ = check_level(reduced_cap(np.zeros(64, np.uint8), 8, "any_low"), level("P", 1))
    assert ok is False
    ok, _, why = check_starts(reduced_cap(np.zeros(64, np.uint8), 8, "any_low"), starts({"P": 0}))
    assert ok is None and "any_low" in why


def test_pulse_counts_are_lower_bounds_and_widths_unknown():
    raw = np.zeros(64, np.uint8)
    raw[[11, 13, 30, 50]] = 1                                      # two pulses in one interval of 8
    for kind in ("any_high", "change_latch"):
        c = reduced_cap(raw, 8, kind)
        ok, got, why = check_pulses(c, pulses("P", count=4))
        assert ok is None and got["rises_at_least"] == 3, (kind, why)
        ok, got, _ = check_pulses(c, pulses("P", count=2))
        assert ok is False, kind                                    # at least 3 is more than 2: surely wrong
    ok, _, why = check_pulses(reduced_cap(raw, 8, "any_high"), pulses("P", count=4, width_max_s=0.001))
    assert ok is None and "widths are not known" in why


def test_an_active_low_cs_kept_as_any_low_holds_every_clock():
    """SCK 10 MHz at 80 MS/s; CS reduced to any_low over 16 ticks: low from the interval it fell in, so the window
    already holds the first clock; not widened."""
    rate, per, n = 80e6, 8, 2000
    clk = [t for k in range(16) for t in (500 + k * per, 500 + k * per + per // 2)]
    mosi = np.zeros(n, np.uint8)
    cs = np.ones(n, np.uint8)
    cs[499:500 + 16 * per] = 0                                     # active one tick before the first clock
    d = 16
    v = np.asarray(reduce(cs, d, "any_low"), np.uint8)
    init, e = edges_from_dense(v, d, 0)
    cap = Capture(rate, n, [Channel("SCK", 0, np.asarray(clk)), Channel("MOSI", 0, np.zeros(0, np.int64)),
                            Channel("CS", init, e, d, 0, {"reduction": "any_low"})])
    ok, got, why = check_spi(cap, spi("SCK", "MOSI", None, "CS", mode=0, mosi_bytes="0000", bits=16))
    assert ok, why
    assert got["frame_bits"] == [16] and got["cs_sample_s"] == d / rate


def test_capture_reads_a_reduced_line_like_any_other(tmp_path):
    p = tmp_path / "w.wireskein"
    part = wc.Part(14, [ff.reduced("CS", "any_low", reduce(RAW, 4, "any_low"), 4)])
    wc.write(p, wc.Capture(1000, [part]))
    got = wc.read(p)
    ch = got.parts[0].channel("CS")
    init, edges = ch.changes()
    assert isinstance(ch, wc.Reduced) and got.parts[0].ticks == 14 and init == 0 and not len(edges)
