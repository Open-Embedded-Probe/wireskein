"""--hint takes JSON, @file.json, or protocol names alone ("rvswd", "uart,spi"); anything else says why."""
import pytest

from wireskein.cli import parse_hint


def test_names_and_json(tmp_path):
    assert parse_hint("rvswd") == {"protocols": ["rvswd"]}
    assert parse_hint("uart, spi") == {"protocols": ["uart", "spi"]}
    assert parse_hint('{"protocols": ["i2c"]}') == {"protocols": ["i2c"]}
    f = tmp_path / "h.json"
    f.write_text('{"pins": {"TX": {"protocol": "uart", "baud": 9600}}}')
    assert parse_hint(f"@{f}") == {"pins": {"TX": {"protocol": "uart", "baud": 9600}}}
    assert parse_hint(None) is None


def test_bad_hint_says_why():
    with pytest.raises(SystemExit, match="not JSON"):
        parse_hint('{"protocols": [')
    with pytest.raises(SystemExit, match="neither JSON nor protocol names"):
        parse_hint("rv swd!")
