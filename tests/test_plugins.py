"""wireskein.plugins: one way in for every addition, one rule for names, and each entry point group read (protocols,
definitions, formats, checks, sources). Objects made here."""

import importlib.metadata as md
from fractions import Fraction

import pytest

from wireskein import plugins, sources


class Proto:
    over = ("uart",)

    def __init__(self, name=None):
        if name is not None:
            self.name = name

    def decode(self, bus):
        return {"messages": [], "checks_passed": 0, "checks_failed": 0}


class Fmt:
    def __init__(self, name=None):
        if name is not None:
            self.name = name

    def sniff(self, head, path):
        return False

    def read(self, path):
        return None


class Src:
    def __init__(self):
        self.asked = []

    def capture(self, target, request):
        self.asked.append(target)
        return sources.Result(Fraction(1000), [])


@pytest.mark.parametrize("name", ["uart", "i2c", "spi", "can", "rgb_led", "nmea", "lines", "frames"])
def test_a_protocol_may_not_take_a_name_of_wireskein(name):
    with pytest.raises(ValueError, match="of WireSkein's own"):
        plugins.register_protocol(Proto(name))


@pytest.mark.parametrize("scheme", ["oep", "sigrok"])
def test_a_source_may_not_take_a_scheme_of_wireskein(scheme):
    with pytest.raises(ValueError, match="of WireSkein's own"):
        plugins.register_source(scheme, Src())


def test_a_source_added_is_run_by_its_scheme():
    s = Src()
    plugins.register_source("acme", s)
    try:
        res = sources.run("acme:bench-3", sources.Request([("A", "0")], 1000, 10))
        assert s.asked == ["bench-3"] and res.meta["source"] == "acme:bench-3"
    finally:
        plugins.unregister_source("acme")
    with pytest.raises(ValueError, match="unknown source 'acme'"):
        sources.run("acme:bench-3", sources.Request([("A", "0")], 1000, 10))


def test_a_check_unregistered_is_gone():
    from wireskein import verify
    plugins.register_check("x_gone", lambda cap, x: (True, {}, ""))
    assert "x_gone" in verify.CHECKS and "x_gone" in plugins.checks()
    plugins.unregister_check("x_gone")
    assert "x_gone" not in verify.CHECKS and "x_gone" not in plugins.checks()


@pytest.fixture
def eps(monkeypatch):
    """Entry points of the groups given, each (name, object), as installed packages would declare them."""
    given = {}

    def entry_points(group=None):
        out = []
        for name, obj in given.get(group, []):
            ep = md.EntryPoint(name, f"irrelevant:{name}", group)
            out.append(ep)
            objs[(group, name)] = obj
        return out

    objs = {}
    monkeypatch.setattr(md, "entry_points", entry_points)
    monkeypatch.setattr(md.EntryPoint, "load", lambda self: objs[(self.group, self.name)])
    monkeypatch.setattr(plugins, "_LOADED", set())
    for t in ("_PROTOCOLS", "_FORMATS", "_SOURCES"):
        monkeypatch.setattr(plugins, t, {})
    monkeypatch.setattr(plugins, "_DIRS", [])
    return given


def test_each_group_is_read(eps, tmp_path):
    s = Src()
    eps.update({"wireskein.protocols": [("acme_link", Proto())], "wireskein.formats": [("acme_csv", Fmt())],
                "wireskein.sources": [("acme", s)], "wireskein.definitions": [("acme", lambda: tmp_path)]})
    assert plugins.protocols()["acme_link"].name == "acme_link"        # named by its entry point
    assert plugins.formats()["acme_csv"].name == "acme_csv"
    assert plugins.sources() == {"acme": s}
    assert plugins.definitions() == [tmp_path.resolve()]


def test_an_object_named_otherwise_than_its_entry_point_is_refused(eps):
    eps["wireskein.protocols"] = [("acme_link", Proto("other_link"))]
    with pytest.raises(ValueError, match="named 'other_link'"):
        plugins.protocols()


def test_an_entry_point_of_a_builtin_source_is_refused(eps):
    eps["wireskein.sources"] = [("oep", Src())]
    with pytest.raises(ValueError, match="of WireSkein's own"):
        plugins.sources()
