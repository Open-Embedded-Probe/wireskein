"""What WireSkein does today: taxonomy.groups' clocked groups (clock + partners by data_score + select by boundary),
on the lines as captured."""

from wsbench import algorithm
from wireskein._engine.survey import survey
from wireskein._engine.taxonomy import groups, pin_classes


@algorithm("grouping", "current", status="main", about="taxonomy.groups on the raw lines")
def current(cap):
    sv = survey(cap, asyncs=False)
    return [{"clock": g.clock, "data": list(g.data), "select": g.select, "score": g.score}
            for g in groups(sv, pin_classes(sv, cap)) if g.kind == "sync"]
