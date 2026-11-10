"""tools/knowledge_decl.py: the device declarations generated from EmbedBenchDeviceData's knowledge are what that
knowledge makes now. Run when WIRESKEIN_DEVICEDATA names a clone of it; skipped otherwise (CI has none)."""

import importlib.util
import os
from pathlib import Path

import pytest

ROOT = Path(__file__).parent.parent
DATA = os.environ.get("WIRESKEIN_DEVICEDATA")
pytestmark = pytest.mark.skipif(not DATA, reason="WIRESKEIN_DEVICEDATA not set (a clone of EmbedBenchDeviceData)")


def _tool():
    pytest.importorskip("yaml")
    spec = importlib.util.spec_from_file_location("knowledge_decl", ROOT / "tools" / "knowledge_decl.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_the_declarations_have_not_drifted_from_the_knowledge():
    assert _tool().check(Path(DATA).expanduser()) == []


def test_a_change_is_told(tmp_path):
    tool = _tool()
    decl = tmp_path / "devices"
    tool.write(Path(DATA).expanduser(), decl, [])
    one = next(decl.rglob("*.toml"))
    one.write_text(one.read_text() + "\n[[register]]\naddress = 0xEE\n")
    (decl / "i2c" / "x").mkdir(parents=True)
    (decl / "i2c" / "x" / "gone.toml").write_text(f"{tool.MARK} 0000000 knowledge/gone.yaml\n[device]\n")
    got = tool.check(Path(DATA).expanduser(), decl)
    assert any("differs" in g for g in got) and any("gone.toml" in g and "no knowledge" in g for g in got)
