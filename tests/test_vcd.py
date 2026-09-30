"""VCD: written with only the changes (and wireskein's own data in a $comment),
read back, and read from other tools."""

import subprocess
import sys
from fractions import Fraction

import numpy as np
import pytest

from wireskein import fileformat
from wireskein._engine import fileio
from wireskein.analyze import load, save


def cli(*args):
    return subprocess.run([sys.executable, "-m", "wireskein", *map(str, args)], capture_output=True, text=True)


def source(tmp_path):
    fast = np.array([(k // 3) & 1 for k in range(300)], np.uint8)
    slow = np.array([(k // 5) & 1 for k in range(30)], np.uint8)
    p = tmp_path / "a.wireskein"
    fileformat.write(p, Fraction(160_000_000, 3), [
        fileformat.Channel("CLK", fileformat.pack(fast.tobytes()), 300, acquisition={"pin": 4}),
        fileformat.Channel("SLOW", fileformat.pack(slow.tobytes()), 30, step=10, phase=2),
        fileformat.analog_raw("VBUS", [100, 200, 300], Fraction(160_000_000, 300), t0_ticks=5, zero=0,
                              scale_nv=1_000_000),
    ], probe={"chip": "esp32"})
    fileformat.note(p, "kept in the .wireskein only")
    return p


def test_round_trip_keeps_rates_steps_and_meta(tmp_path):
    p = source(tmp_path)
    r = cli("convert", p, tmp_path / "a.vcd")
    assert r.returncode == 0, r.stderr
    assert "not carried" in r.stdout                                  # the note has no place in a VCD
    text = (tmp_path / "a.vcd").read_text()
    assert "$timescale 10 ps $end" in text                            # 18.75 ns = 1875 x 10 ps
    assert fileformat.sniff(tmp_path / "a.vcd") == "vcd"
    back = load(tmp_path / "a.vcd")
    orig = fileio.load(p)
    assert back.meta["tick_hz"] == Fraction(160_000_000, 3) and back.n_samples == orig.n_samples
    for a, b in zip(orig.channels, back.channels):
        assert (a.name, a.initial, a.step, a.phase) == (b.name, b.initial, b.step, b.phase)
        assert np.array_equal(a.edges, b.edges)
    assert back.channel("CLK").acquisition == {"pin": 4}
    (v,) = back.analog
    assert v.rate_hz == Fraction(160_000_000, 300) and v.t0_ticks == 5
    assert np.allclose(v.volts(), [0.1, 0.2, 0.3])
    assert back.meta["probe"] == {"chip": "esp32"}
    assert cli("convert", tmp_path / "a.vcd", tmp_path / "b.wireskein").returncode == 0
    assert fileformat.read(tmp_path / "b.wireskein")[1][1].step == 10


def test_a_vcd_from_elsewhere(tmp_path):
    p = tmp_path / "saleae.vcd"                                       # 100 MHz samples at a 1 ns timescale
    p.write_text("""$date today $end
$timescale 1 ns $end
$scope module top $end
$var wire 1 ! SDA $end
$var wire 1 " SCL $end
$var wire 4 # BUS [3:0] $end
$var real 1 $ SAW $end
$upscope $end
$enddefinitions $end
#0
$dumpvars
1!
1"
b0101 #
r0.5 $
$end
#20
0!
x"
#30
b1 #
#50
1!
1"
r1.5 $
#70
r0.25 $
""")
    cap = load(p)
    assert cap.rate == 1e8                                            # the common divisor of the times: 10 ns
    assert [c.name for c in cap.channels] == ["SDA", "SCL", "BUS[0]", "BUS[1]", "BUS[2]", "BUS[3]"]
    sda = cap.channel("SDA")
    assert sda.initial == 1 and list(sda.edges) == [2, 5]
    assert list(cap.channel("SCL").edges) == [2, 5] and cap.meta["vcd_x_or_z"] == 1
    assert cap.channel("BUS[0]").initial == 1 and cap.channel("BUS[2]").initial == 1
    assert list(cap.channel("BUS[2]").edges) == [3]                   # 0101 -> 0001
    assert cap.analog == [] and cap.meta["vcd_not_read"][0]["name"] == "SAW"   # 0, 50, 70: not evenly spaced
    r = cli("convert", p, tmp_path / "s.wireskein")
    assert r.returncode == 0 and "SAW not read" in r.stdout


def test_evenly_spaced_reals_are_analog(tmp_path):
    p = tmp_path / "r.vcd"
    p.write_text("$timescale 1 us $end\n$scope module m $end\n$var real 64 a V $end\n$upscope $end\n"
                 "$enddefinitions $end\n#10\nr1.0 a\n#20\nr2.0 a\n#30\nr3.0 a\n")
    (v,) = load(p).analog
    assert v.rate_hz == 100_000 and list(v.volts()) == [1.0, 2.0, 3.0]


def test_not_a_vcd(tmp_path):
    (tmp_path / "x.vcd").write_text("hello\n")
    with pytest.raises(ValueError, match="not a WireSkein file, a sigrok session"):
        load(tmp_path / "x.vcd")
