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


def _uart_in_parts(tmp_path, cuts):
    from wireskein._engine import gen
    tx = gen.Wave(1)
    end = gen.uart(tx, 100e-6, b"Hello, parts!\r\n", 115200) + 200e-6
    cap = gen.quantize({"TX": tx}, 2e6, end, np.random.default_rng(0))
    lv = cap.channels[0].level_at(np.arange(cap.n_samples)).astype(np.uint8)
    bounds = [0, *cuts, len(lv)]
    lv_parts = [lv[a:b] for a, b in zip(bounds[:-1], bounds[1:])]
    parts = [wc.Part(len(x), [ff.Channel("TX", np.packbits(x, bitorder="little").tobytes(), len(x), 1, 0)], {}, a)
             for x, a in zip(lv_parts, bounds[:-1])]
    return wc.write(tmp_path / "u.wireskein", wc.Capture(2_000_000, parts)), bounds


def test_analyze_reads_a_uart_across_parts_without_a_gap(tmp_path):
    from wireskein.decode import analyze_file
    p, bounds = _uart_in_parts(tmp_path, [397, 801])            # cuts inside characters, off any byte grid
    doc = analyze_file(p)
    (part,) = doc["parts"]
    assert part["part"] == 0 and part["parts"] == [0, 2] and part["ticks"] == bounds[-1]
    (c,) = [c for c in part["claims"] if c["protocol"] == "uart"]
    assert c["result"]["lines"] == ["Hello, parts!"], c["result"]


def test_annotations_of_a_joined_stretch_are_rows_of_each_part(tmp_path):
    from wireskein import annotate
    p, bounds = _uart_in_parts(tmp_path, [397, 801])
    rows = [r for r in annotate.build_file(p)["rows"] if r["name"].startswith("uart")]
    assert {r["part"] for r in rows} <= {0, 1, 2} and len({r["part"] for r in rows}) >= 2
    for r in rows:                                               # each item's ticks are of its own part
        length = bounds[r["part"] + 1] - bounds[r["part"]]
        assert all(0 <= it["s"] < length for it in r["items"]), r["part"]


def test_verify_marks_a_change_in_the_part_it_is_in(tmp_path):
    from wireskein import verify
    from wireskein.runlog import Recorder, becomes
    wave = np.concatenate([np.tile([1] * 25 + [0] * 75, 30), np.tile([1] * 50 + [0] * 50, 30)])   # the change at 3000
    rec = Recorder(tmp_path / "run")
    with rec.section(1, "t", expect=[becomes("P", freq_hz=10_000, duty=0.5)]):
        cap = wc.Capture(1_000_000, [_part(wave[:1777], 0), _part(wave[1777:], 1777)])
        rec.add(rec.armed(), cap)
    rec.close()
    rep = verify.verify(tmp_path / "run", mark=True)
    (r,) = rep["results"]
    assert r["status"] == "ok" and (r["measured"]["change_part"], r["measured"]["change_tick"]) == (1, 3000 - 1777)
    ms = [m for m in ff.markers(tmp_path / "run" / "c0001.wireskein") if m["by"] == "verify"]
    assert [(m["part"], m["t"]) for m in ms] == [(1, 1223)]


def _events_part(changes, n_ticks, step, t0, start):
    q = sorted({e // step for e in changes})
    levels, lv = [], start
    for _ in q:
        lv ^= 1
        levels.append(lv)
    return wc.Part(n_ticks, [ff.events("P", q, levels, [1] * len(q), -(-n_ticks // step), step, 0, start)], {}, t0)


def test_lines_kept_as_changes_join_across_a_grid_restart(tmp_path):
    # format 1 at a step of 4: the first part's last quantum runs 3 ticks past its own end, the second part's start
    p = wc.write(tmp_path / "e.wireskein", wc.Capture(1_000_000, [_events_part([100, 300, 500], 1001, 4, 0, 0),
                                                                  _events_part([99, 299], 999, 4, 1001, 1)]))
    (run,) = fileio.load_runs(p)
    assert run.meta["parts"] == [0, 1] and run.n_samples == 2000 and run.channels[0].step == 4
    assert run.channels[0].edges.tolist() == [103, 303, 503, 1100, 1300]     # each change at its quantum's end
    assert fileio.locate(run.meta, 1300) == (1, 299)


def test_a_line_that_hides_changes_stays_part_by_part(tmp_path):
    def hiding(t0):
        return wc.Part(1000, [ff.events("P", [10, 20], [1, 1], [2, 2], 250, 4, 0, 1)], {}, t0)   # two changes a quantum
    p = wc.write(tmp_path / "h.wireskein", wc.Capture(1_000_000, [hiding(0), hiding(1000)]))
    assert len(fileio.load_runs(p)) == 2
