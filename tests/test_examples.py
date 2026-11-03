"""The examples in tests/data/examples, as analyze reads them: real RVSWD / SWIO from a WCH-LinkE (wch-protocols) and
generated mixed buses, each with the buses it holds (truth.json). analysis-format §4 points at them."""

import json
from pathlib import Path

import pytest

from wireskein.decode import analyze_file

D = Path(__file__).parent / "data" / "examples"


@pytest.mark.parametrize("name", ["swio-v003-target-info", "rvswd-v203-target-info", "mixed-0", "mixed-1"])
def test_analyze_finds_the_buses_of_an_example(name):
    truth = json.loads((D / f"{name}.truth.json").read_text())["buses"]
    doc = analyze_file(D / f"{name}.wireskein")
    claims = doc["parts"][0]["claims"]
    for bus in truth:                                   # every bus, on its pins (SPI's data lines: data0 / data1)
        assert any(c["protocol"] == bus["protocol"] and set(c["roles"].values()) == set(bus["roles"].values())
                   and c["verdict"] in ("confirmed", "likely") for c in claims), (bus, claims)
    if name.startswith("rvswd"):                        # above the frames: the RISC-V debug module's accesses
        (c,) = [c for c in claims if c["protocol"] == "rvswd"]
        assert c["result"]
