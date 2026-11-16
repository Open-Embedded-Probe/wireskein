"""The whole analysis as the installed WireSkein does it, and the same after a clean algorithm of the bench."""

from wsbench import algorithm, registry
from wireskein import decode


def claims(cap):
    return decode.export(decode.analyze(cap), cap, mode="all", alternatives=True)["claims"]


@algorithm("e2e", "wireskein", status="main", about="wireskein.decode.analyze as installed")
def wireskein(cap):
    return claims(cap)


@algorithm("e2e", "preclean", status="trial", about="a clean algorithm of the bench, then wireskein.decode.analyze",
           variants={"deglitch.k2": {"clean": "deglitch.k2"}, "deglitch.k3": {"clean": "deglitch.k3"}})
def preclean(cap, clean):
    return claims(registry.get("clean", clean)(cap))
