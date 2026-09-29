"""Inputs for zerocopy-bench: dense u16 samples (bit k = channel k), per-channel
u32 edge lists, and the reference I2C transactions / roles from truth."""
import json
import sys

import numpy as np

import corpus
from wireskein._engine import fixture

out = corpus.ROOT / "corpus/work/zerocopy"
out.mkdir(parents=True, exist_ok=True)
for fid in sys.argv[1:] or ["i2cdb-sht30-1b9dbf", "wch-l103-flash-pattern4k"]:
    d = corpus.REAL / fid
    cap, truth = fixture.load_capture(d), fixture.load_truth(d)
    dense = np.zeros(cap.n_samples, dtype="<u2")
    for k, ch in enumerate(cap.channels):
        lv = ch.level_at(np.arange(cap.n_samples)) if cap.n_samples < 0 else None  # placeholder (unused)
        # build the level track from edges without per-sample searchsorted
        tr = np.zeros(cap.n_samples + 1, dtype=np.int8)
        tr[ch.edges] ^= 1
        level = (np.cumsum(tr[:-1]) & 1) ^ ch.initial
        dense |= (level.astype("<u2") << k)
    dense.tofile(out / f"{fid}.u16")
    for k, ch in enumerate(cap.channels):
        ch.edges.astype("<u4").tofile(out / f"{fid}.ch{k}.u32")
    meta = {"rate": cap.rate, "n_samples": cap.n_samples, "channels": [c.name for c in cap.channels],
            "initial": [c.initial for c in cap.channels], "buses": truth["buses"]}
    (out / f"{fid}.json").write_text(json.dumps(meta))
    print(fid, cap.n_samples, "samples", dense.nbytes / 1e6, "MB")
