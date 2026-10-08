"""Capture sources: sigrok through sigrok-cli's demo driver (skipped without
sigrok-cli), and the OEP source against a stand-in for oep-client."""

import shutil
import subprocess
import sys
import types
from fractions import Fraction

import numpy as np
import pytest

from wireskein import sources, fileformat


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
    from wireskein.analyze import load
    assert out.suffix == ".sr" and load(out).channel("D0") and not list(tmp_path.glob("*.wireskein"))


class StandIn:
    """Just enough of oep-client for the source: a 3-channel probe answering w=4 (two samples per byte)."""

    def __init__(self, n=10, refuse=()):
        self.calls, self.n, self.refuse = [], n, set(refuse)
        rng = np.random.default_rng(0)
        self.levels = rng.integers(0, 2, (3, n)).astype(np.uint8)
        nib = self.levels[0] | self.levels[1] << 1 | self.levels[2] << 2           # bit 3 undefined
        nib = np.concatenate([nib, np.zeros(n % 2, np.uint8)])
        self.data = (nib[0::2] | (nib[1::2] << 4) | 0x88).astype(np.uint8).tobytes()   # undefined bits set on purpose

    def modules(self):
        me = self

        class OepError(Exception):
            pass

        class Unsupported(OepError):
            def __init__(self, tag):
                super().__init__(f"unsupported {tag}")
                self.tag = tag

        class Cap:
            def __init__(self, hst):
                self.fn = 7
                self.generation = None

            def configure(self, **kw):
                me.calls.append(("configure", kw))
                if kw.get("trigger") and 0x45 in me.refuse:
                    raise Unsupported(0x45)                      # the tag as sent
                return types.SimpleNamespace(rate=Fraction(160_000_000, 8), width=4, positions=[0, 1, 2], block=None)

            def start(self):
                me.calls.append(("start",))
                self.generation = 3                     # the capture's generation

            def wait(self, timeout):
                return [types.SimpleNamespace(samples=me.n, start_ns=123_000, start_uncertainty_ns=500, trigger_index=4,
                                              slipped=True)]

            def read_segment(self, seg):
                return me.data

        host = types.SimpleNamespace(end=lambda: me.calls.append(("end",)))
        link = types.SimpleNamespace(open_host=lambda t: me.calls.append(("open", t)) or host)
        core = types.SimpleNamespace(take=lambda h, ms, owner: me.calls.append(("take", owner)) or
                                     types.SimpleNamespace(boot_id=9),
                                     plan_apply=lambda h, a: me.calls.append(("plan", a)),
                                     plan_release=lambda h, f: me.calls.append(("release", f)))
        capture = types.SimpleNamespace(LogicCapture=Cap, ONE_SHOT=0, TRIGGER=0x45, PRETRIGGER=0x46,
                                        mr=types.SimpleNamespace(TAG=0x60))
        message = types.SimpleNamespace(UNKNOWN_OPERATION=2)
        pkg = types.ModuleType("oep_client")
        pkg.link, pkg.core, pkg.capture, pkg.message = link, core, capture, message
        pkg.host = types.SimpleNamespace(OepError=OepError, Unsupported=Unsupported)
        return {"oep_client": pkg, "oep_client.link": link, "oep_client.core": core, "oep_client.capture": capture,
                "oep_client.host": pkg.host, "oep_client.message": message}


def test_oep_source_against_a_stand_in(tmp_path, monkeypatch):
    fake = StandIn(n=11)
    for name, mod in fake.modules().items():
        monkeypatch.setitem(sys.modules, name, mod)
    req = sources.Request([("SDA", "47"), ("SCL", "48"), ("INT", "5")], 20_000_000, 11, trigger=("SCL", "fall"),
                          pretrigger=3)
    out = sources.capture("oep:/dev/ttyACM9", req, tmp_path / "o.wireskein")
    assert ("plan", [(7, 0, 47), (7, 1, 48), (7, 2, 5)]) in fake.calls
    conf = next(kw for c, *kw in fake.calls if c == "configure")[0]
    assert conf["trigger"] == (2, 1, 1) and conf["pretrigger"] == 3 and conf["samples"] == 11 and conf["rate"] == 20_000_000
    assert "critical" not in conf                                               # OEP v1: refused, never ignored
    assert [c[0] for c in fake.calls][-2:] == ["release", "end"]                 # plan released, session ended
    head, chans = fileformat.read(out)
    assert head["tick_hz"] == [20_000_000, 1]
    assert [fileformat.unpack(c) for c in chans] == [row.tobytes() for row in fake.levels]
    meta = head["meta"]
    assert meta["source"] == "oep:/dev/ttyACM9" and meta["start_ns"] == 123_000
    assert meta["start_uncertainty_ns"] == 500 and meta["probe"]["boot_id"] == 9
    assert meta["probe"]["generation"] == {"logic": 3}
    assert meta["time_base_slipped"] is True
    assert meta["trigger_tick"] == 4 and meta["probe_channels"] == {"SDA": 47, "SCL": 48, "INT": 5}


def test_oep_source_names_a_refusal(monkeypatch):
    fake = StandIn()
    mods = fake.modules()

    def refuse(h, a):
        raise mods["oep_client.host"].OepError("rejected: unavailable")
    mods["oep_client.core"].plan_apply = refuse
    for name, mod in mods.items():
        monkeypatch.setitem(sys.modules, name, mod)
    with pytest.raises(RuntimeError, match="probe /dev/x: rejected: unavailable"):
        sources.run("oep:/dev/x", sources.Request([("A", "1")], 1000, 10))


def test_oep_trigger_the_probe_cannot_do_is_an_error(monkeypatch):
    fake = StandIn(refuse={0x45})
    for name, mod in fake.modules().items():
        monkeypatch.setitem(sys.modules, name, mod)
    with pytest.raises(RuntimeError, match="a trigger"):
        sources.run("oep:/dev/x", sources.Request([("A", "1")], 1000, 10, trigger=("A", "rise")))
    assert [c[0] for c in fake.calls][-2:] == ["release", "end"]


def test_fewer_samples_than_asked_are_named(tmp_path, capsys):
    from wireskein.cli import _fewer
    out = tmp_path / "c.wireskein"
    fileformat.write(out, 1000, [fileformat.Channel("A", fileformat.pack(bytes(64)), 64),
                                 fileformat.analog_volts("V", [1.0] * 10, 100)])
    _fewer(out, sources.Request([("A", "0")], 1000, 400, analog_samples=10))
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


def test_uart_link_speed_is_asked_for_unless_turned_off(tmp_path, monkeypatch):
    import types
    from wireskein.sources import oep

    calls = []

    class Link:
        @staticmethod
        def raise_speed(hst, rates, *, flows=None, record=False, max_tries=None):
            calls.append((rates, record, max_tries))
            assert flows == oep.FLOWS                                     # a capture verifies the read-back way

    assert oep._raise(Link, "H", [1_500_000, 921_600]) == [1_500_000, 921_600]
    assert oep._raise(Link, "H", []) == []
    assert calls == [([1_500_000, 921_600], False, None)]                  # asked rates: tried as given
    calls.clear()
    hst = types.SimpleNamespace(link=types.SimpleNamespace(speed=types.SimpleNamespace(skipped=[1_500_000], retried=None)))
    assert oep._raise_default(Link, hst, "/dev/X") == (oep.FAST, True)     # the record shaped them
    assert calls == [(oep.FAST, True, oep.PER_SESSION)]                    # the default: the client's record decides
    assert oep._raise_default(Link, hst, None) == ([], False)              # not a serial port this host opened
    assert oep._rates("") == [] and oep.FAST[0] == 1_500_000
    fake = StandIn(n=11)
    for name, mod in fake.modules().items():
        monkeypatch.setitem(sys.modules, name, mod)
    req = sources.Request([("SDA", "47"), ("SCL", "48"), ("INT", "5")], 20_000_000, 11)
    out = sources.capture("oep:/dev/ttyACM9?fast=0", req, tmp_path / "o.wireskein")
    assert ("open", "/dev/ttyACM9") in fake.calls                              # the option is not part of the port
    link = fileformat.read(out)[0]["meta"]["probe"]["link"]
    assert set(link) == {"open_s", "read_bytes", "read_s"} and link["read_bytes"] == len(fake.data)
    with pytest.raises(ValueError, match="unknown option speed"):
        sources.capture("oep:/dev/ttyACM9?speed=2", req, tmp_path / "p.wireskein")


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


def test_probe_clock_against_the_host():
    import types
    from wireskein.sources import oep
    r = lambda host, up, unc, boot=7: types.SimpleNamespace(host_ns=host, uptime_ns=up, boot_id=boot, uncertainty_ns=unc)  # noqa: E731
    info = oep._clock_info(r(1_000_000_000, 5_000_000_000, 50_000), r(2_000_000_000, 6_000_100_000, 50_000))
    assert info["before"]["uptime_ns"] == 5_000_000_000 and info["after"]["host_ns"] == 2_000_000_000
    assert abs(info["rate_ppm"] - 100) < 1e-6 and abs(info["rate_ppm_uncertainty"] - 100) < 1e-6
    assert "rate_ppm" not in oep._clock_info(r(1, 5, 1), r(2, 6, 1, boot=8))        # restarted between: no rate
    assert oep._clock_info(None, None) == {}
    assert oep._clock(types.SimpleNamespace()) is None                              # no reading: the capture goes on
