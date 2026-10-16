"""Lines kept as their changes (encoding "events", wireskein-format §4.4; OEP capture's format 1 events): the file
form, step 1 read as an exact Channel, step > 1 answered from the quanta (exact counts and levels, timing to the
quantum), and the checks that need every edge unchecked only when a quantum hides more than one change."""

import pytest

from wireskein import fileformat
from wireskein.analyze import load, save
from wireskein.runlog import Recorder, ends, level, only_moving, pulses, square, starts
from wireskein.verify import verify


def quantize(line, d):
    """A line (levels by tick from 0) as OEP's events at resolution d: the level at tick 0, then per quantum with a
    change (quantum, level at its last tick, changes in it)."""
    out, prev = [], line[0]
    for k in range((len(line) + d - 1) // d):
        c = 0
        for t in range(k * d, min((k + 1) * d, len(line))):
            if line[t] != prev:
                c += 1
                prev = line[t]
        if c:
            out.append((k, prev, c))
    return line[0], out


def ev(name, line, d):
    start, evs = quantize(line, d)
    return fileformat.events(name, [k for k, _, _ in evs], [lv for _, lv, _ in evs], [c for _, _, c in evs],
                             (len(line) + d - 1) // d, d, 0, start)


def test_the_file_keeps_them(tmp_path):
    line = [0] * 10 + [1] * 3 + [0] * 19 + [1] * 32           # a pulse at 10-12 inside quantum 1 (d 8), a rise at 32
    chans = [fileformat.Channel("CLK", fileformat.pack(bytes(64)), 64), ev("IRQ", line, 8), ev("EXACT", line, 1)]
    p = fileformat.write(tmp_path / "a.wireskein", 1_000_000, chans, trigger_tick=10)
    head, back = fileformat.read(p)
    assert [c["encoding"] for c in head["channels"]] == ["bits", "events", "events"]
    assert head["ticks"] == 64 and head["meta"]["trigger_tick"] == 10
    assert back[1].decode() == ([1, 4], [0, 1], [2, 1])     # quantum 1: two changes, low after; quantum 4: up
    assert back[2].decode() == ([10, 13, 32], [1, 0, 1], [1, 1, 1])
    cap = load(p)
    assert [t.name for t in cap.events] == ["IRQ"] and cap.events[0].hidden == 1
    exact = cap.channel("EXACT")                             # step 1: an exact Channel
    assert exact.edges.tolist() == [10, 13, 32] and exact.initial == 0
    with pytest.raises(KeyError):
        cap.channel("IRQ")                                   # a hidden pulse: no Channel view
    save(tmp_path / "b.wireskein", cap)                      # through the engine and back
    again = {c.name: c for c in fileformat.read(tmp_path / "b.wireskein")[1]}
    assert again["IRQ"].decode() == back[1].decode()        # the quanta kept as they were
    assert load(tmp_path / "b.wireskein").channel("EXACT").edges.tolist() == [10, 13, 32]   # bits or events: the smaller
    with pytest.raises(ValueError, match="quanta hide changes: IRQ"):
        save(tmp_path / "c.sr", cap)


def test_a_rewrite_keeps_each_step_1_line_in_the_smaller_form(tmp_path):
    n = 100_000
    quiet = [0] * 40_000 + [1] * 60_000                      # one change in 100k samples: smaller as events
    busy = [(t // 2) & 1 for t in range(n)]                  # a change every 2 samples: smaller as bits
    p = fileformat.write(tmp_path / "q.wireskein", 1_000_000,
                         [fileformat.Channel("QUIET", fileformat.pack(bytes(quiet)), n), ev("BUSY", busy, 1)])
    save(tmp_path / "r.wireskein", load(p))
    head, (q, b) = fileformat.read(tmp_path / "r.wireskein")
    assert [c["encoding"] for c in head["channels"]] == ["events", "bits"]
    assert q.decode() == ([40_000], [1], [1]) and q.n == n
    assert load(tmp_path / "r.wireskein").channel("BUSY").edges.tolist() == list(range(2, n, 2))


def test_events_refuse_what_cannot_be():
    with pytest.raises(ValueError, match="ends at 0 after 1 changes from 0"):
        fileformat.events("X", [0], [0], [1], 4, 8, 0, 0)    # an odd count must flip the level
    with pytest.raises(ValueError, match="out of order"):
        fileformat.events("X", [2, 1], [1, 0], [1, 1], 4, 8)
    with pytest.raises(ValueError, match="out of order"):
        fileformat.events("X", [1], [1], [2], 4, 1)          # step 1: one change an event
    c = fileformat.events("X", [3], [0], [200], 4, 8)        # a count past 127 is kept as 127: at least
    assert c.decode() == ([3], [0], [127])


def run(tmp_path, expect, channels):
    rec = Recorder(tmp_path)
    with rec.section(1, "t", expect=expect):
        rec.capture(rec.armed(), 1_000_000, channels=channels)
    rec.close()
    return [(r["check"], r["ok"], r["reason"]) for r in verify(tmp_path)["results"]]


def test_counts_and_levels_are_exact_even_where_quanta_hide_pulses(tmp_path):
    idle = [1] * 64                                          # an active-low CS that never moved
    busy = [1] * 20 + [0] * 2 + [1] * 2 + [0] * 2 + [1] * 38   # two selects inside quantum 2 (d 8): hidden
    got = run(tmp_path, [level("IDLE", 1), level("BUSY", 1), starts({"BUSY": 1}), ends({"BUSY": 1}),
                         pulses("BUSY", count=2), pulses("BUSY", count=1), only_moving(["BUSY"]), square("BUSY", 1000)],
              [ev("IDLE", idle, 8), ev("BUSY", busy, 8)])
    assert [o for _, o, _ in got] == [True, False, True, True, True, False, True, None], got
    assert "2 rising edges, want 1" in got[5][2] and "needs every edge" in got[7][2]


def test_a_line_with_at_most_a_change_a_quantum_is_decoded_at_its_step(tmp_path):
    clk = [(t // 50) & 1 for t in range(1000)]               # a 10 kHz square at 1 MHz ticks, d 4: one change a quantum
    got = run(tmp_path, [square("CLK", 10_000), pulses("CLK", count=10), pulses("CLK", period_s=100e-6)],
              [ev("CLK", clk, 4)])
    assert [o for _, o, _ in got] == [True, True, True], got


def test_a_saturated_count_is_a_lower_bound(tmp_path):
    fast = [(t // 2) & 1 for t in range(1024)]               # 511 changes in one quantum of 1024: count 127
    got = run(tmp_path, [pulses("F", count=300), pulses("F", count=10)], [ev("F", fast, 1024)])
    assert [o for _, o, _ in got] == [None, False], got
    assert "at least 64 rising edges" in got[0][2]


def test_info_names_them(tmp_path):
    import subprocess
    import sys
    busy = [1] * 20 + [0] * 2 + [1] * 2 + [0] * 2 + [1] * 38
    p = fileformat.write(tmp_path / "i.wireskein", 1_000_000, [ev("CS", busy, 32)], trigger_tick=5)
    out = subprocess.run([sys.executable, "-m", "wireskein", "info", str(p)], capture_output=True, text=True,
                         check=True).stdout
    assert "events 1 at 32 ticks a quantum" in out and "1 quanta hold more than one change" in out
    assert "trigger_tick 5" in out
