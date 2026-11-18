"""A selection the capture's end cut (its closing edge never recorded) frames its transfer as the others do: an SPI
bus of four transfers whose last runs into the end is read with its CS (it was confirmed without, a quarter of the
transfers unframed). Lines made here."""

import numpy as np

from wireskein import decode
from wireskein._engine import gen
from wireskein._engine.model import Capture


def test_cut_selection_frames_its_transfer():
    clk, mosi, miso, cs = gen.Wave(0), gen.Wave(0), gen.Wave(0), gen.Wave(1)
    r = np.random.default_rng(5)
    frames = [(bytes(r.integers(0, 256, n).tolist()), bytes(r.integers(0, 256, n).tolist())) for n in (3, 1, 6, 4)]
    t = 1e-4
    for f in frames:
        t = gen.spi(clk, mosi, miso, cs, t, [f], 1e6) + 3e-4
    end = t - 3e-4 - 20e-6 - 3e-6                           # inside the last selection (gen.spi adds a gap of 20 us)
    cap = gen.quantize({"SCK": clk, "MOSI": mosi, "MISO": miso, "CS": cs}, 12e6, end, np.random.default_rng(0))
    assert len(cap.channel("CS").edges) % 2 == 1            # its last selection left open
    claims = decode.export(decode.analyze(cap), cap, mode="all")["claims"]
    spi = [c for c in claims if c["protocol"] == "spi"]
    assert spi and spi[0]["roles"].get("cs") == "CS", [(c["protocol"], c["roles"], c["verdict"]) for c in claims]


def test_selection_cut_by_the_start_frames_its_transfer():
    """The capture begins inside a selection (its opening edge never recorded): the first transfer is framed too."""
    clk, mosi, miso, cs = gen.Wave(0), gen.Wave(0), gen.Wave(0), gen.Wave(1)
    r = np.random.default_rng(6)
    frames = [(bytes(r.integers(0, 256, n).tolist()), bytes(r.integers(0, 256, n).tolist())) for n in (4, 2, 5, 3)]
    t, starts = 1e-4, []
    for f in frames:
        starts.append(t)
        t = gen.spi(clk, mosi, miso, cs, t, [f], 1e6) + 3e-4
    start = starts[0] + 3e-6                                 # inside the first selection, before its first clock
    cap = gen.quantize({"SCK": clk, "MOSI": mosi, "MISO": miso, "CS": cs}, 12e6, t, np.random.default_rng(0),
                       t_start=start)
    assert cap.channel("CS").initial == 0                   # the capture begins selected
    claims = decode.export(decode.analyze(cap), cap, mode="all")["claims"]
    spi = [c for c in claims if c["protocol"] == "spi"]
    assert spi and spi[0]["roles"].get("cs") == "CS", [(c["protocol"], c["roles"], c["verdict"]) for c in claims]
