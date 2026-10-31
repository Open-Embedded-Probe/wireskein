"""Capture sources: sigrok through sigrok-cli's demo driver (skipped without
sigrok-cli), and the OEP source against a stand-in for oep-client."""

import shutil
import subprocess
import sys
import types
from fractions import Fraction

import numpy as np
import pytest

from wireskein import capture, sources, _fileformat as fileformat


def test_parsers():
    assert sources.parse_count("20M") == 20_000_000 and sources.parse_count("250k") == 250_000
    assert sources.parse_count("1.5M") == 1_500_000 and sources.parse_count("1000") == 1000
    for bad in ("20m", "2x", "M"):
        with pytest.raises(ValueError, match="not a count"):
            sources.parse_count(bad)
    with pytest.raises(ValueError, match="M for mega"):
        sources.parse_count("20m")
    assert sources.parse_channels("SDA=47, SCL=48") == [("SDA", "47"), ("SCL", "48")]
    assert sources.parse_channels("D0,D1") == [("D0", "D0"), ("D1", "D1")]
    assert sources.parse_trigger("SDA:fall") == ("SDA", "fall")
    with pytest.raises(ValueError):
        sources.Request([("A", "1")], 1000, 10, trigger=("B", "rise"))
    with pytest.raises(ValueError):
        sources.run("nosuch:x", sources.Request([("A", "1")], 1000, 10))


@pytest.mark.skipif(shutil.which("sigrok-cli") is None, reason="sigrok-cli not installed")
def test_sigrok_demo_through_the_cli(tmp_path):
    out = tmp_path / "d.wireskein"
    r = subprocess.run([sys.executable, "-m", "wireskein", "capture", "--source", "sigrok:demo", "--channels", "A=D0,B=D1",
                        "--rate", "1M", "--samples", "20k", "--note", "demo", "-o", str(out)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    head, chans = fileformat.read(out)
    assert [c.name for c in chans] == ["A", "B"] and all(c.n == 20_000 for c in chans)
    assert head["tick_hz"] == [1_000_000, 1] and head["meta"]["driver"] == "demo"
    assert head["meta"]["device_channels"] == {"A": "D0", "B": "D1"}
    assert fileformat.notes(out)[0]["content"] == "demo"
    assert any(fileformat.unpack(c).count(1) not in (0, c.n) for c in chans)          # the demo pattern moves


@pytest.mark.skipif(shutil.which("sigrok-cli") is None, reason="sigrok-cli not installed")
def test_sigrok_to_sr(tmp_path):
    out = sources.capture("sigrok:demo", sources.Request([("D0", "D0")], 100_000, 1000), tmp_path / "d.sr")
    from wireskein.decode import load
    assert out.suffix == ".sr" and load(out).channel("D0") and not list(tmp_path.glob("*.wireskein"))


def _acquire_stand_in(calls, res):
    """oep_client.acquire as far as the oep source uses it: its types, and capture_at answering `res`."""
    from oep_client import acquire as real
    mod = types.ModuleType("oep_client.acquire")
    for name in ("Request", "Result", "Part", "Bits", "Changes", "Analog", "OnDisk", "changes", "analog_raw", "pack"):
        setattr(mod, name, getattr(real, name))

    def capture_at(address, req, fast=None, owner=None):
        calls.append((address, req, fast, owner))
        return res
    mod.capture_at = capture_at
    return mod


def test_oep_source_writes_the_clients_result_one_to_one(tmp_path, monkeypatch):
    pytest.importorskip("oep_client")
    from oep_client import acquire as acq
    a = acq.Bits("SDA", acq.pack(bytes([0, 1, 1, 0, 1])), 5, acquisition={"pin": 47})
    b = acq.changes("SCL", [1, 3], [1, 0], [1, 1], 5, pin=48)
    v = acq.analog_raw("VBUS", [100, 200], 1000, width=16, value_bits=12, zero=0, scale_nv=805_860, pin=17)
    parts = [acq.Part([a, b, v], {"start_ns": 123_000, "start_uncertainty_ns": 500}, 0),
             acq.Part([a, b, v], {"start_ns": 999_000}, 40)]
    res = acq.Result(Fraction(20_000_000), parts[0].channels, {"start_ns": 123_000, "probe": {"boot_id": 9}}, parts,
                     [{"after_part": 0, "reason": "probe dropped a segment", "ticks": 30}])
    calls = []
    monkeypatch.setitem(sys.modules, "oep_client.acquire", _acquire_stand_in(calls, res))
    import oep_client
    monkeypatch.setattr(oep_client, "acquire", sys.modules["oep_client.acquire"], raising=False)
    req = sources.Request([("SDA", "47"), ("SCL", "48")], 20_000_000, 5, trigger=("SCL", "fall"), pretrigger=3,
                          analog=[("VBUS", "17", None)], fast=[])
    out = sources.capture("oep:oep://30eda0ea068b?via=usb", req, tmp_path / "o.wireskein")
    (address, areq, fast, owner), = calls
    assert address == "oep://30eda0ea068b?via=usb" and fast == [] and owner == "wireskein capture"
    assert (areq.channels, areq.trigger, areq.pretrigger, areq.analog) == (req.channels, req.trigger, 3, req.analog)
    cap = capture.read(out)
    assert cap.tick_hz == 20_000_000 and cap.lost[0]["ticks"] == 30 and [p.tick0 for p in cap.parts] == [0, 40]
    p0 = cap.parts[0]
    assert p0.meta["start_ns"] == 123_000 and cap.meta["source"] == "oep:oep://30eda0ea068b?via=usb"
    sda, scl, vbus = p0.channels
    assert isinstance(sda, capture.Channel) and sda.changes()[1].tolist() == [1, 3, 4] and sda.acquisition["pin"] == 47
    assert isinstance(scl, capture.Events) and scl.decode() == ([1, 3], [1, 0], [1, 1])
    assert isinstance(vbus, capture.Analog) and vbus.values() == [100, 200] and vbus.scale_nv == 805_860


def test_fast_is_this_sources_option():
    assert sources.parse_fast(None) is None and sources.parse_fast("1") is None
    assert sources.parse_fast("0") == [] and sources.parse_fast("1500000,921600") == [1_500_000, 921_600]
    with pytest.raises(ValueError, match="--fast x"):
        sources.parse_fast("x")


def test_fewer_samples_than_asked_are_named(tmp_path, capsys):
    from wireskein.cli import _fewer
    out = tmp_path / "c.wireskein"
    fileformat.write(out, 1000, [fileformat.Channel("A", fileformat.pack(bytes(64)), 64),
                                 fileformat.analog_volts("V", [1.0] * 10, 100)])
    _fewer(fileformat.read_header(out), sources.Request([("A", "0")], 1000, 400, analog_samples=10))
    assert capsys.readouterr().out == "note: A: 64 samples of the 400 asked (the probe's limit)\n"


def test_a_probe_error_ends_in_one_line(monkeypatch, capsys):
    from oep_client.message import OepError

    class Expired(OepError):                          # stands for oep_client.host.Expired (OEP v1)
        pass

    def fail(*a, **k):
        raise Expired("the lease (5000 ms) ran out")
    monkeypatch.setattr(sources, "capture", fail)
    from wireskein import cli
    monkeypatch.setattr(sys, "argv", ["wireskein", "capture", "--source", "oep:/dev/null", "--channels", "A=0",
                                      "--rate", "1M", "--samples", "10", "-o", "x.wireskein"])
    with pytest.raises(SystemExit) as e:
        cli.main()
    assert str(e.value) == "wireskein capture: Expired: the lease (5000 ms) ran out"


def test_link_line():
    from wireskein.cli import _link_line
    line = _link_line({"open_s": 4.3, "read_bytes": 64000, "read_s": 6.1, "asked": [1_500_000], "rate": 115200,
                       "raised": False, "trials": [{"rate": 1_500_000, "result": "verify broke", "broken_in": 3,
                                                    "broken_out": 0}]})
    assert line == "115200 baud (the boot speed), tried 1500000 verify broke, opened in 4.30 s, read 64.0 KB in 6.10 s (10.5 KB/s)"
    line = _link_line({"open_s": 0.5, "read_bytes": 64000, "read_s": 0.8, "rate": 500_000, "raised": True,
                       "trials": [{"rate": 500_000, "result": "committed"}], "skipped": [1_500_000, 921_600],
                       "remembered": True})
    assert "skipped 1500000, 921600 (failed before)" in line and "what this port and probe did before" in line


def test_a_busy_port_ends_in_one_line(monkeypatch):
    class PortBusy(OSError):                          # stands for oep_client.link.PortBusy
        pass

    def fail(*a, **k):
        raise PortBusy("/dev/ttyUSB0 is open in another program")
    monkeypatch.setattr(sources, "capture", fail)
    from wireskein import cli
    monkeypatch.setattr(sys, "argv", ["wireskein", "capture", "--source", "oep:/dev/ttyUSB0", "--channels", "A=0",
                                      "--rate", "1M", "--samples", "10", "-o", "x.wireskein"])
    with pytest.raises(SystemExit) as e:
        cli.main()
    assert str(e.value) == "wireskein capture: PortBusy: /dev/ttyUSB0 is open in another program"


