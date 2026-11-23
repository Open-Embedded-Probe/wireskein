"""wireskein.plugins: a definitions directory is the one way in (protocols/, devices/, markers/), one rule for names,
the environment adds them; WireSkein's own names in one ledger. Directories made here."""

import pytest

from wireskein import plugins, sources

DECL = """format = "wireskein-protocol/1"
name = "{name}"
over = ["uart"]

[frame]
fields = [{{ name = "v" }}, {{ name = "c", check = {{ kind = "xor8", over = ["v"] }} }}]
"""


def defs(tmp_path, name):
    (tmp_path / "protocols").mkdir(exist_ok=True)
    (tmp_path / "protocols/x.toml").write_text(DECL.format(name=name))
    return tmp_path


@pytest.mark.parametrize("name", ["uart", "i2c", "nmea", "acme_link", "Acme.link", "acme.link.x", "acme."])
def test_a_protocol_takes_its_owners_namespace(tmp_path, name):
    """owner.name (B13): WireSkein's own names have none, so a name of WireSkein's is refused with the rest."""
    with pytest.raises(ValueError, match="owner.name"):
        plugins.add_definitions(defs(tmp_path, name))
    assert tmp_path.resolve() not in plugins.definitions()


def test_a_declared_protocol_is_there_while_its_directory_is(tmp_path):
    plugins.add_definitions(defs(tmp_path, "acme.link"))
    try:
        assert plugins.protocols()["acme.link"].over == ("uart",)
    finally:
        plugins.remove_definitions(tmp_path)
    assert "acme.link" not in plugins.protocols()


def test_the_registry_holds_wireskeins_names():
    from wireskein import _upper
    from wireskein._engine import staged
    reg = plugins.registry()
    assert {p.name for p in staged.PLUGINS} | {u.name for u in _upper.UPPER} <= set(reg)
    assert all("." not in k for k in reg) and "uart" in plugins.buses() and "nmea" not in plugins.buses()


def test_only_wireskeins_own_sources():
    with pytest.raises(ValueError, match="unknown source 'acme'"):
        sources.run("acme:bench-3", sources.Request([("A", "0")], 1000, 10))


def test_the_environment_adds_definitions(monkeypatch, tmp_path):
    monkeypatch.setattr(plugins, "_LOADED", set())
    monkeypatch.setattr(plugins, "_DIRS", [])
    monkeypatch.setenv("WIRESKEIN_DEFINITIONS", str(defs(tmp_path, "acme.env")))
    assert plugins.definitions() == [tmp_path.resolve()]
    assert "acme.env" in plugins.protocols()


def test_the_cli_takes_a_definitions_directory(tmp_path):
    import subprocess
    import sys
    from pathlib import Path
    ex = Path(__file__).parents[1] / "examples"
    cap = ex / "protocols/demo_link_p4.wireskein"
    run = [sys.executable, "-m", "wireskein"]
    got = subprocess.run([*run, "--definitions", str(ex), "analyze", str(cap)], capture_output=True, text=True)
    assert got.returncode == 0, got.stderr
    assert '"examples.demo_link"' in got.stdout
    got = subprocess.run([*run, "--plugin", "x.py", "analyze", str(cap)], capture_output=True, text=True)
    assert got.returncode != 0                                     # no code is added


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
