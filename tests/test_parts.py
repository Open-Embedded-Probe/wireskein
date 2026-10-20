"""A capture in parts and a capture that lost data (wireskein-format §3.4): parts on one tick clock, analysis and
checks per part, never across them; a loss makes the capture a failure that readers and checks show."""

import subprocess
import sys

import pytest

from wireskein import fileformat
from wireskein.analyze import load
from wireskein.runlog import Recorder, level, pulses
from wireskein.verify import verify


def ch(name, line):
    return fileformat.Channel(name, fileformat.pack(bytes(line)), len(line))


def two_parts(path, lost=None, layout="timeline"):
    a = [0] * 10 + [1] * 10                                   # one rise in part 0
    b = [1] * 5 + [0] * 5 + [1] * 10                          # one rise in part 1; its first sample is a start, not an edge
    parts = [fileformat.Part([ch("IRQ", a)], {"start_ns": 1000, "trigger_tick": 10}, 0),
             fileformat.Part([ch("IRQ", b)], {"start_ns": 9000}, 100 if layout == "timeline" else None)]
    return fileformat.write(path, 1_000_000, parts=parts, layout=layout, lost=lost, probe={"model": "x"})


def test_parts_are_kept_and_read_one_at_a_time(tmp_path):
    p = two_parts(tmp_path / "p.wireskein")
    raw = fileformat.read_header(p)
    assert raw["layout"] == "timeline" and raw["complete"] is True and "lost" not in raw
    assert [x["tick0"] for x in raw["parts"]] == [0, 100] and raw["meta"] == {"probe": {"model": "x"}}
    with pytest.raises(ValueError, match="2 parts"):
        fileformat.read(p)                                    # never the first part silently
    (h0, c0), (h1, c1) = fileformat.read_parts(p)
    assert h0["meta"] == {"probe": {"model": "x"}, "start_ns": 1000, "trigger_tick": 10} and h1["meta"]["start_ns"] == 9000
    assert c0[0].n == 20 and c1[0].n == 20 and h1["tick0"] == 100
    cap = load(p, part=1)
    assert cap.channel("IRQ").initial == 1 and cap.channel("IRQ").edges.tolist() == [5, 10]
    with pytest.raises(ValueError, match="give channels"):
        fileformat.write(tmp_path / "x.wireskein", 1, [ch("A", [0])], parts=[])
    with pytest.raises(ValueError, match="same"):
        fileformat.write(tmp_path / "x.wireskein", 1, parts=[fileformat.Part([ch("A", [0])]), fileformat.Part([ch("B", [0])], {}, 5)])
    with pytest.raises(ValueError, match="needs its tick0"):
        fileformat.write(tmp_path / "x.wireskein", 1, parts=[fileformat.Part([ch("A", [0])]), fileformat.Part([ch("A", [0])])])


def test_a_loss_is_shown_first_and_fails_the_capture(tmp_path):
    lost = [{"after_part": 0, "reason": "probe dropped a segment (no room)", "ticks": 80}]
    p = two_parts(tmp_path / "l.wireskein", lost)
    raw = fileformat.read_header(p)
    assert raw["complete"] is False and raw["lost"][0]["stopped"] is False
    out = subprocess.run([sys.executable, "-m", "wireskein", "info", str(p)], capture_output=True, text=True,
                         check=True).stdout.splitlines()
    assert out[1] == "INCOMPLETE: 1 loss, 80 ticks lost against 40 taken (67 %)"
    assert out[2] == "  after part 0: probe dropped a segment (no room) (80 ticks)"
    assert "2 parts (timeline)" in out[0] and any(x.startswith("part 1: 20 ticks from tick 100") for x in out)

    rec = Recorder(tmp_path / "run")
    with rec.section(1, "t", expect=[pulses("IRQ", count=1), level("IRQ", 1)]):
        name = rec.capture(rec.armed(), 1_000_000, channels=[ch("IRQ", [0])])
    rec.close()
    two_parts(tmp_path / "run" / name, lost)                   # the recorded capture, lost in the middle
    res = verify(tmp_path / "run")["results"]
    by = [(r["check"], r["capture"], r["ok"]) for r in res]
    assert ("capture", name, False) in by                      # the capture fails, whatever its parts say
    assert "data lost while capturing: after part 0" in next(r["reason"] for r in res if r["check"] == "capture")
    assert (("pulses", f"{name}#part0", True) in by and ("pulses", f"{name}#part1", True) in by)   # per part, no edge
    assert ("level", f"{name}#part1", False) in by                                                # across the parts


def test_segments_need_no_tick0_and_a_rewrite_refuses_one_part_of_many(tmp_path):
    from wireskein.analyze import save
    p = two_parts(tmp_path / "s.wireskein", layout="segments")
    raw = fileformat.read_header(p)
    assert raw["layout"] == "segments" and "tick0" not in raw["parts"][1]
    with pytest.raises(ValueError, match="2 parts and this is one of them"):
        save(tmp_path / "t.wireskein", load(p, part=0))


def test_a_large_channel_is_deflated_in_pieces_at_once(tmp_path, monkeypatch):
    """Data past PARALLEL_BYTES is deflated in pieces by a thread a core and joined (pigz's way): one deflate stream,
    the same bytes read back, the zip's CRC checked."""
    import zipfile
    import numpy as np
    monkeypatch.setattr(fileformat, "PARALLEL_BYTES", 1 << 16)
    monkeypatch.setattr(fileformat, "PARALLEL_CHUNK", 1 << 16)
    monkeypatch.setattr(fileformat, "PARALLEL_PIECE", 1 << 12)
    rng = np.random.default_rng(0)
    bits = np.packbits(np.cumsum(rng.random(8 * 300_001) < 0.01) & 1, bitorder="little").astype(np.uint8).tobytes()
    raw = tmp_path / "a.bits"
    raw.write_bytes(bits)
    out = tmp_path / "a.wireskein"
    fileformat.write(out, 1000, [fileformat.Channel("A", fileformat.OnDisk(str(raw)), 8 * 300_001)])
    with zipfile.ZipFile(out) as z:
        assert z.getinfo("part/0/ch/0.bits").compress_type == zipfile.ZIP_DEFLATED
        assert z.read("part/0/ch/0.bits") == bits


def test_markers_name_their_part(tmp_path):
    """wireskein-format §5.2: in a file of several parts a marker always names its part (0 when not given)."""
    chans = [fileformat.Channel("A", fileformat.pack(bytes(8)), 8)]
    out = tmp_path / "m.wireskein"
    fileformat.write(out, 1000, parts=[fileformat.Part(chans, {}, 0), fileformat.Part(chans, {}, 20)])
    fileformat.set_markers(out, [{"t": 3, "label": "a"}, {"t": 5, "label": "b", "part": 1}])
    assert [(m["label"], m["part"]) for m in fileformat.markers(out)] == [("a", 0), ("b", 1)]
    with pytest.raises(ValueError, match="part"):
        fileformat.set_markers(out, [{"t": 3, "label": "c", "part": 2}])
    one = tmp_path / "one.wireskein"
    fileformat.write(one, 1000, chans)
    fileformat.set_markers(one, [{"t": 3, "label": "a"}])
    assert "part" not in fileformat.markers(one)[0]                 # one part: nothing to name
