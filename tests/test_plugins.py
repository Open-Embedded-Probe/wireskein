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


@pytest.mark.parametrize("name", ["uart", "i2c", "nmea", "acme_link", "Acme.link", "acme.link.x", "acme."])
def test_a_protocol_takes_its_owners_namespace(name):
    """owner.name (B13): WireSkein's own names have none, so a name of WireSkein's is refused with the rest."""
    with pytest.raises(ValueError, match="owner.name"):
        plugins.register_protocol(Proto(name))


def test_the_registry_holds_wireskeins_names():
    from wireskein._engine import staged
    reg = plugins.registry()
    assert {p.name for p in staged.PLUGINS} | {u.name for u in staged.UPPER} <= set(reg)
    assert all("." not in k for k in reg) and "uart" in plugins.buses() and "nmea" not in plugins.buses()


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
    plugins.register_check("tests.x_gone", lambda cap, x: (True, {}, ""))
    assert "tests.x_gone" in verify.CHECKS and "tests.x_gone" in plugins.checks()
    plugins.unregister_check("tests.x_gone")
    assert "tests.x_gone" not in verify.CHECKS and "tests.x_gone" not in plugins.checks()


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
    eps.update({"wireskein.protocols": [("acme.link", Proto())], "wireskein.formats": [("acme.csv", Fmt())],
                "wireskein.sources": [("acme", s)], "wireskein.definitions": [("acme", lambda: tmp_path)]})
    assert plugins.protocols()["acme.link"].name == "acme.link"        # named by its entry point
    assert plugins.formats()["acme.csv"].name == "acme.csv"
    assert plugins.sources() == {"acme": s}
    assert plugins.definitions() == [tmp_path.resolve()]


def test_an_object_named_otherwise_than_its_entry_point_is_refused(eps):
    eps["wireskein.protocols"] = [("acme.link", Proto("acme.other"))]
    with pytest.raises(ValueError, match="named 'acme.other'"):
        plugins.protocols()


def test_an_entry_point_of_a_builtin_source_is_refused(eps):
    eps["wireskein.sources"] = [("oep", Src())]
    with pytest.raises(ValueError, match="of WireSkein's own"):
        plugins.sources()


def test_the_environment_adds_definitions_and_modules(eps, monkeypatch, tmp_path):
    (tmp_path / "acme_plug.py").write_text("from wireskein import plugins\n"
                                           "class P:\n    over = ('uart',)\n    name = 'acme.env'\n"
                                           "    def decode(self, bus): return {}\n"
                                           "plugins.register_protocol(P())\n")
    monkeypatch.setenv("WIRESKEIN_DEFINITIONS", str(tmp_path))
    monkeypatch.setenv("WIRESKEIN_PLUGINS", str(tmp_path / "acme_plug.py"))
    assert plugins.definitions() == [tmp_path.resolve()]
    assert "acme.env" in plugins.protocols()


def _roles_fit(spec, roles) -> bool:
    """A claim's role names against the registry's: a list of names ("x+": x0, x1, ...; "x?": may be absent), or a
    list of such lists (alternatives)."""
    sets = spec if spec and isinstance(spec[0], list) else [spec]
    for names in sets:
        need = {n for n in names if not n.endswith(("+", "?"))}
        multi = {n[:-1] for n in names if n.endswith("+")}
        opt = {n[:-1] for n in names if n.endswith("?")}
        got = set(roles)
        many = {k for k in got if any(k.startswith(m) and k[len(m):].isdigit() for m in multi)}
        if need <= got and all(any(k.startswith(m) for k in many) for m in multi) and got - many - need <= opt:
            return True
    return False


def test_the_engines_output_keeps_to_the_registry():
    """What the analysis emits on the example captures: every protocol a registry name, its roles and layers the
    registry's."""
    from pathlib import Path
    from wireskein import capture, decode
    reg = plugins.registry()
    seen = set()
    for f in sorted((Path(__file__).parent / "data/examples").glob("*.wireskein")):
        cap = capture.read(f)
        for part in cap.parts:
            doc = decode.export(decode.analyze(part), part, mode="all")
            for c in doc["claims"]:
                p = c["protocol"]
                seen.add(p)
                assert p in reg and reg[p]["kind"] == "bus", p
                assert _roles_fit(reg[p]["roles"], c["roles"]), (p, c["roles"])
                # the bus's own layers and those of the upper ones that ride on it (lines, NMEA on a UART)
                allowed = set(reg[p]["layers"]) | {x for u in reg.values() if p in u.get("over", ()) for x in u["layers"]}
                assert set(c.get("layers", {})) <= allowed, (p, set(c.get("layers", {})) - allowed)
    assert seen
