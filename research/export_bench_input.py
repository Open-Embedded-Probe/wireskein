"""Write the boundary-bench input: the UART line of a real fixture as raw
little-endian int64 edges plus a small JSON with rate, initial level and the
L2 unit candidates."""

import json
import sys

import numpy as np

import corpus
from wireskein._engine import features, fixture

fid = sys.argv[1] if len(sys.argv) > 1 else "i2cdb-sht30-1b9dbf"
d = corpus.REAL / fid
cap, truth = fixture.load_capture(d), fixture.load_truth(d)
ch = cap.channel(next(b for b in truth["buses"] if b["protocol"] == "uart")["roles"]["data"])
units = [u.samples for u in features.channel_features(cap, ch).units]
out = corpus.ROOT / "corpus/work"
out.mkdir(parents=True, exist_ok=True)
np.asarray(ch.edges, dtype="<i8").tofile(out / "uart_edges.i64")
(out / "uart_edges.json").write_text(json.dumps(
    {"initial": ch.initial, "n_samples": cap.n_samples, "rate": cap.rate, "units": units, "edges": len(ch.edges)}))
print(fid, ch.name, len(ch.edges), "edges ->", out / "uart_edges.i64")
