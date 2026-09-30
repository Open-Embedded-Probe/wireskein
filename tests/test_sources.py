"""Capture sources: sigrok through sigrok-cli's demo driver (skipped without
sigrok-cli), and the OEP source against a stand-in for oep-client."""

import shutil
import subprocess
import sys
import types
from fractions import Fraction

import numpy as np
import pytest

from wireskein import sources, wsc


def test_parsers():
    assert sources.parse_count("20M") == 20_000_000 and sources.parse_count("250k") == 250_000
    assert sources.parse_count("1.5M") == 1_500_000 and sources.parse_count("1000") == 1000
    assert sources.parse_channels("SDA=47, SCL=48") == [("SDA", "47"), ("SCL", "48")]
    assert sources.parse_channels("D0,D1") == [("D0", "D0"), ("D1", "D1")]
    assert sources.parse_trigger("SDA:fall") == ("SDA", "fall")
    with pytest.raises(ValueError):
        sources.Request([("A", "1")], 1000, 10, trigger=("B", "rise"))
    with pytest.raises(ValueError):
        sources.run("nosuch:x", sources.Request([("A", "1")], 1000, 10))


@pytest.mark.skipif(shutil.which("sigrok-cli") is None, reason="sigrok-cli not installed")
def test_sigrok_demo_through_the_cli(tmp_path):
    out = tmp_path / "d.wsc"
    r = subprocess.run([sys.executable, "-m", "wireskein", "capture", "--source", "sigrok:demo", "--channels", "A=D0,B=D1",
                        "--rate", "1M", "--samples", "20k", "--note", "demo", "-o", str(out)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    head, chans = wsc.read(out)
    assert [c.name for c in chans] == ["A", "B"] and all(c.n == 20_000 for c in chans)
    assert head["tick_hz"] == [1_000_000, 1] and head["meta"]["driver"] == "demo"
    assert head["meta"]["device_channels"] == {"A": "D0", "B": "D1"}
    assert wsc.notes(out)[0]["content"] == "demo"
    assert any(wsc.unpack(c).count(1) not in (0, c.n) for c in chans)          # the demo pattern moves


@pytest.mark.skipif(shutil.which("sigrok-cli") is None, reason="sigrok-cli not installed")
def test_sigrok_to_sr(tmp_path):
    out = sources.capture("sigrok:demo", sources.Request([("D0", "D0")], 100_000, 1000), tmp_path / "d.sr")
    from wireskein.analyze import load
    assert out.suffix == ".sr" and load(out).channel("D0") and not list(tmp_path.glob("*.wsc"))


class StandIn:
    """Just enough of oep-client for the source: a 3-channel probe answering w=4 (two samples per byte)."""

    def __init__(self, n=10):
        self.calls, self.n = [], n
        rng = np.random.default_rng(0)
        self.levels = rng.integers(0, 2, (3, n)).astype(np.uint8)
        nib = self.levels[0] | self.levels[1] << 1 | self.levels[2] << 2           # bit 3 undefined
        nib = np.concatenate([nib, np.zeros(n % 2, np.uint8)])
        self.data = (nib[0::2] | (nib[1::2] << 4) | 0x88).astype(np.uint8).tobytes()   # undefined bits set on purpose

    def modules(self):
        me = self

        class OepError(Exception):
            pass

        class Cap:
            def __init__(self, hst):
                self.fn = 7

            def configure(self, **kw):
                me.calls.append(("configure", kw))
                return types.SimpleNamespace(rate=Fraction(160_000_000, 8), width=4, positions=[0, 1, 2], jitter_ns=0)

            def start(self):
                me.calls.append(("start",))

            def wait(self, timeout):
                return [types.SimpleNamespace(samples=me.n, start_us=123, trigger_index=4, slipped=True)]

            def read_segment(self, seg):
                return me.data

        host = types.SimpleNamespace(end=lambda: me.calls.append(("end",)))
        link = types.SimpleNamespace(open_host=lambda t: me.calls.append(("open", t)) or host)
        core = types.SimpleNamespace(take=lambda h, ms, owner: me.calls.append(("take", owner)),
                                     plan_apply=lambda h, a: me.calls.append(("plan", a)),
                                     plan_release=lambda h, f: me.calls.append(("release", f)))
        capture = types.SimpleNamespace(LogicCapture=Cap, ONE_SHOT=0)
        pkg = types.ModuleType("oep_client")
        pkg.link, pkg.core, pkg.capture, pkg.host = link, core, capture, types.SimpleNamespace(OepError=OepError)
        return {"oep_client": pkg, "oep_client.link": link, "oep_client.core": core, "oep_client.capture": capture,
                "oep_client.host": pkg.host}


def test_oep_source_against_a_stand_in(tmp_path, monkeypatch):
    fake = StandIn(n=11)
    for name, mod in fake.modules().items():
        monkeypatch.setitem(sys.modules, name, mod)
    req = sources.Request([("SDA", "47"), ("SCL", "48"), ("INT", "5")], 20_000_000, 11, trigger=("SCL", "fall"),
                          pretrigger=3)
    out = sources.capture("oep:/dev/ttyACM9", req, tmp_path / "o.wsc")
    assert ("plan", [(7, 0, 47), (7, 1, 48), (7, 2, 5)]) in fake.calls
    conf = next(kw for c, *kw in fake.calls if c == "configure")[0]
    assert conf["trigger"] == (2, 1, 1) and conf["pretrigger"] == 3 and conf["samples"] == 11 and conf["rate"] == 20_000_000
    assert [c[0] for c in fake.calls][-2:] == ["release", "end"]                 # plan released, session ended
    head, chans = wsc.read(out)
    assert head["tick_hz"] == [20_000_000, 1]
    assert [wsc.unpack(c) for c in chans] == [row.tobytes() for row in fake.levels]
    meta = head["meta"]
    assert meta["source"] == "oep:/dev/ttyACM9" and meta["start_us"] == 123 and meta["time_base_slipped"] is True
    assert meta["trigger_index"] == 4 and meta["probe_channels"] == {"SDA": 47, "SCL": 48, "INT": 5}


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
