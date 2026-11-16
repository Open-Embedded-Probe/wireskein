"""The whole analysis as the installed WireSkein does it, and the same after a clean algorithm of the bench."""

from wsbench import algorithm, registry
from wireskein import decode
from wireskein._engine import methods


def claims(cap):
    return decode.export(decode.analyze(cap), cap, mode="all", alternatives=True)["claims"]


@algorithm("e2e", "wireskein", status="main", about="wireskein.decode.analyze as installed")
def wireskein(cap):
    return claims(cap)


@algorithm("e2e", "preclean", status="trial", about="a clean algorithm of the bench, then wireskein.decode.analyze",
           variants={"deglitch.k2": {"clean": "deglitch.k2"}, "deglitch.k3": {"clean": "deglitch.k3"},
                     "hybrid.r3": {"clean": "hybrid.r3"}})
@algorithm("e2e", "preclean", status="retired", about="a clean algorithm of the bench, then wireskein.decode.analyze",
           variants={"relative.r3": {"clean": "relative.r3"}}, reason="its clean, clean/relative, is retired")
def preclean(cap, clean):
    return claims(registry.get("clean", clean)(cap))


@algorithm("e2e", "inject", status="trial",
           about="WireSkein's analysis with steps of the bench's algorithms put in (wireskein._engine.methods)",
           variants={"rhythm": {"use": ["lineclass/rhythm.g3ns"]},
                     "gated": {"use": ["grouping/gated.w4"]},
                     "rhythm+gated": {"use": ["lineclass/rhythm.g3ns", "grouping/gated.w4"]}})
def inject(cap, use):
    steps = {}
    for ref in use:
        stage, name = ref.split("/", 1)
        steps.update(registry.get(stage, name).engine_steps())
    with methods.use(steps):
        return claims(cap)
