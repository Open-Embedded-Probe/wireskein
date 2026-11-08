"""tools/degrade.py: test data made from a capture by lowering its rate and adding noise, the steps and the seed kept
in the file (meta.degraded) so it is made again exactly."""

import importlib.util
from pathlib import Path

import numpy as np

from wireskein import _fileformat as ff
from wireskein.decode import decode_file, load

ROOT = Path(__file__).parent.parent
D = Path(__file__).parent / "data" / "rvswd_long_truth"
SPEC = {"buses": [{"protocol": "rvswd", "pins": {"clk": "SWCLK", "dio": "SWDIO"}}]}
spec = importlib.util.spec_from_file_location("degrade", ROOT / "tools" / "degrade.py")
degrade = importlib.util.module_from_spec(spec)
spec.loader.exec_module(degrade)


def _edges(path):
    cap = load(path)
    return {c.name: (c.initial, c.edges.tolist()) for c in cap.channels}, cap


def test_the_seed_and_the_steps_are_in_the_file_and_make_it_again(tmp_path):
    src = D / "linke_l103_target_info.sr"
    argv = ["--pins", "SWCLK,SWDIO", "--glitch", "SWCLK:0.02", "--ring", "SWDIO:0.5", "--crosstalk",
            "SWDIO:SWCLK:0.05", "--seed", "42"]
    degrade.main([str(src), str(tmp_path / "a.wireskein"), *argv])
    degrade.main([str(src), str(tmp_path / "b.wireskein"), *argv])
    a, _ = _edges(tmp_path / "a.wireskein")
    b, _ = _edges(tmp_path / "b.wireskein")
    assert a == b                                                 # the same seed: the same capture
    meta = ff.read_header(tmp_path / "a.wireskein")["meta"]["degraded"]
    assert meta["seed"] == 42 and [o["op"] for o in meta["ops"]] == ["glitch", "ring", "crosstalk"]
    assert meta["source"]["file"] == src.name and len(meta["source"]["sha256"]) == 64
    clean = decode_file(src, SPEC)["parts"][0]["buses"][0]["reading"]
    noisy = decode_file(tmp_path / "a.wireskein", SPEC)["parts"][0]["buses"][0]["reading"]
    assert noisy["glitch_share"] > clean["glitch_share"] and noisy["glitches"] > clean["glitches"]


def test_a_lower_rate_samples_the_line_at_every_nth_tick(tmp_path):
    src = D / "linke_l103_target_info.sr"
    degrade.main([str(src), str(tmp_path / "s.wireskein"), "--pins", "SWCLK,SWDIO", "--every", "5:2", "--seed", "1"])
    got, cap = _edges(tmp_path / "s.wireskein")
    orig = load(src)
    assert cap.rate == orig.rate / 5
    ticks = 2 + 5 * np.arange(cap.n_samples)
    for c in orig.channels:
        if c.name in got:
            mine = cap.channel(c.name)
            assert np.array_equal(mine.level_at(np.arange(cap.n_samples)), c.level_at(ticks))
    meta = ff.read_header(tmp_path / "s.wireskein")["meta"]["degraded"]
    assert meta["ops"] == [{"op": "every", "n": 5, "phase": 2}]


def test_a_rate_too_low_for_the_clock_is_told(tmp_path):
    # 160 MS/s taken every 3rd sample: a LinkE's highs of 20-25 ns at 53 MS/s
    degrade.main([str(D / "linke_v203_160mhz_flash_pattern4k.sr"), str(tmp_path / "l.wireskein"), "--every", "3",
                  "--seed", "7"])
    b = decode_file(tmp_path / "l.wireskein", SPEC)["parts"][0]["buses"][0]
    assert b["reading"]["rate_ok"] is False and b["undersampled"]["least_rate_hz"] > 53.4e6
