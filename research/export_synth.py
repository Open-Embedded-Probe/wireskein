"""Freeze the synthetic evaluation sets as fixtures under corpus/fixtures/synth/.

The generator stays the source (seeded), but the frozen fixtures are what a
reimplementation (any language) runs its regression tests on.

    PYTHONPATH=. uv run python export_synth.py
"""
import json

from wireskein._engine import fixture, synth
from corpus import ROOT

SETS = [("mixed", None, range(0, 200), "tuning"), ("mixed", None, range(1000, 1200), "heldout")] + \
       [("mixed", s, range(0, 50), s) for s in ("glitch", "midstart", "lowrate", "jitter", "freqhop", "baudhop")] + \
       [("uartlike", None, range(60), "uartlike"), ("duplex", None, range(60), "duplex"), ("upper", None, range(80), "upper")]

OUT = ROOT / "corpus/fixtures/synth"
manifest = {"generator": "wireskein._engine.synth (seeded)", "sets": {}}
for profile, stress, seeds, name in SETS:
    ids = []
    for s in seeds:
        cap, truth = synth.scenario(s, profile, stress)
        fid = truth["id"]
        fixture.save(OUT / name / fid, cap, truth)
        ids.append(fid)
    manifest["sets"][name] = {"profile": profile, "stress": stress, "seeds": [seeds.start, seeds.stop], "count": len(ids)}
    print(name, len(ids))
(OUT / "manifest.json").write_text(json.dumps(manifest, indent=1) + "\n")
