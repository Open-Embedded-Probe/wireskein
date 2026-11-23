"""Engines of other directions (bench/engines), judged as the main engine is: kept side by side to learn from each
other's weak points (bench/notes/2026-11-22-structure-experiments.ja.md). Trials: none is meant to replace the
main engine as it stands."""

from wsbench import algorithm


@algorithm("e2e", "engine", status="trial", about="another engine of bench/engines",
           variants={"light": {"name": "light"}, "views": {"name": "views"},
                     "declarative": {"name": "declarative"}, "learned": {"name": "learned"},
                     "synth": {"name": "synth"}, "synth_gated": {"name": "synth_gated"},
                     "nulled": {"name": "nulled"}, "open": {"name": "open"},
                     "views_upper": {"name": "views_upper"},
                     **{f"{k}_open": {"name": f"{k}_open"} for k in ("light", "declarative", "synth", "learned")}})
def engine(cap, name):
    import engines
    return engines.ENGINES[name](cap)
