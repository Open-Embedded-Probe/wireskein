"""wireskein survey: the overview of a capture read once, on lines made here (its numbers known), and the same
whatever the blocks it is read in."""

import numpy as np

from wireskein import split
from wireskein._engine.model import Capture, Channel
from wireskein.survey import survey


def cap():
    clk = np.arange(100, 1_000_000, 50, dtype=np.int64)                    # 20 kHz at 1 MHz, duty 0.5: never stops
    burst = []
    for t in range(10_000, 1_000_000, 100_000):                             # bursts of 20 changes, 10 apart
        burst += list(range(t, t + 200, 10))
    noise = np.sort(np.concatenate((np.arange(5_000, 900_000, 997), np.arange(5_000, 900_000, 997) + 1)))
    return Capture(1e6, 1_000_000, [Channel("CLK", 0, clk), Channel("B", 0, np.asarray(burst, np.int64)),
                                     Channel("F", 0, noise)])


def test_lines_overview():
    doc = survey(split.CaptureSource(cap()), bins=100, top=3)
    ln = {x["name"]: x for x in doc["lines"]}
    fr = ln["CLK"]["free_running"]
    assert abs(fr["freq_hz"] - 10_000) < 1 and abs(fr["duty"] - 0.5) < 0.01     # rise to rise: 100 ticks
    assert "free_running" not in ln["B"]
    assert ln["B"]["edges"] == 200 and sum(ln["B"]["activity"]) == 200
    assert ln["B"]["longest_still"][0]["seconds"] > 0.09                         # between bursts
    assert ln["F"]["short_share"] > 0.4 and "F" in doc["trembling"]
    assert ln["B"]["intervals"] == {"8-15": 190, "65536-131071": 9}


def test_the_same_whatever_the_blocks():
    a = survey(split.CaptureSource(cap(), block=1 << 20))
    b = survey(split.CaptureSource(cap(), block=777))
    assert a == b


def test_plan_gives_the_pieces():
    doc = survey(split.CaptureSource(cap()), plan="max_edges=8000,min_edges=1000")
    assert len(doc["pieces"]) > 2
