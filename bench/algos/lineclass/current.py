"""What WireSkein does today: the survey's pin classes (_engine/taxonomy.pin_classes), on the lines as captured."""

from wsbench import algorithm
from wireskein._engine.survey import survey
from wireskein._engine.taxonomy import pin_classes

# taxonomy's classes onto the bench's labels (it has no periodic / random: those come out as clock or data)
TO_LABEL = {"clock": "clock", "data": "data", "sparse": "select", "pulse": "data"}


@algorithm("lineclass", "current", status="main", about="taxonomy.pin_classes on the raw lines, best class")
def current(cap):
    sv = survey(cap, asyncs=False)
    pcs = pin_classes(sv, cap)
    out = {}
    for ch in cap.channels:
        pc = pcs.get(ch.name)
        if pc is None:
            out[ch.name] = {"label": "static"}
            continue
        best = max(pc.scores, key=pc.scores.get)
        out[ch.name] = {"label": TO_LABEL[best], "scores": pc.scores}
    return out
