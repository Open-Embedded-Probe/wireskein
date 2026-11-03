"""Freeze the generated evaluation sets under corpus/synth/<set>/: <id>.wireskein and <id>.truth.json.

The generator stays the source (seeded); the frozen sets are what a change to the analysis is measured on, and what
a reimplementation (any language) runs its regression tests on.

    uv run python export_synth.py
"""
import json

from wireskein import decode

import synth
from corpus import SETS

SPECS = [("mixed", None, range(0, 200), "tuning"), ("mixed", None, range(1000, 1200), "heldout")] + \
        [("mixed", s, range(0, 50), s) for s in ("glitch", "midstart", "lowrate", "jitter", "freqhop", "baudhop")] + \
        [("uartlike", None, range(60), "uartlike"), ("duplex", None, range(60), "duplex"), ("upper", None, range(80), "upper")]

manifest = {"generator": "eval/synth.py (seeded)", "sets": {}}
for profile, stress, seeds, name in SPECS:
    (SETS / name).mkdir(parents=True, exist_ok=True)
    for s in seeds:
        cap, truth = synth.scenario(s, profile, stress)
        decode.save(SETS / name / f"{truth['id']}.wireskein", cap)
        (SETS / name / f"{truth['id']}.truth.json").write_text(json.dumps(truth, indent=1, ensure_ascii=False) + "\n")
    manifest["sets"][name] = {"profile": profile, "stress": stress, "seeds": [seeds.start, seeds.stop],
                              "count": len(seeds)}
    print(name, len(seeds))
(SETS / "manifest.json").write_text(json.dumps(manifest, indent=1) + "\n")
