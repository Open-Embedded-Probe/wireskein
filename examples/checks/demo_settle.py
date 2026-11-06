"""demo_settle: a check kind of WireSkein's examples, added from outside (verify.register_check, or the entry point
group wireskein.checks).

It judges what no check of WireSkein's says: a line that must settle. After its last change in the segment the
line stays at `level` for at least `quiet_s`, and it changes at most `changes_max` times before that (a relay's
contact bounce, a button's debounced output, a supply-good line). Each change but the last is a notice `bounce` at
its tick, so the viewer shows where the line bounced; a test limits them with notices={"bounce": n} or "ng".

    {"kind": "demo_settle", "pins": ["PA0"], "level": 1, "quiet_s": 0.01, "changes_max": 5}
"""

from __future__ import annotations

from wireskein.verify import MEASURED


def check(cap, x):
    """(ok, measured, reason): ok True / False / None (could not be judged) / MEASURED (only measured)."""
    ch = cap.channel(x["pins"][0])
    e = ch.edges
    end_level = ch.initial ^ (len(e) & 1)
    last = int(e[-1]) if len(e) else 0
    quiet = (cap.n_samples - last) / cap.rate
    got = {"changes": int(len(e)), "level": int(end_level), "quiet_s": quiet,
           "notices": [{"kind": "bounce", "tick": int(t)} for t in e[:-1]]}
    if x.get("level") is None:
        return MEASURED, got, f"{len(e)} changes, then {quiet:.4g} s at {end_level}"
    why = []
    if end_level != x["level"]:
        why.append(f"ends at {end_level}, not {x['level']}")
    if quiet < x.get("quiet_s", 0.0):
        why.append(f"quiet {quiet:.4g} s after its last change, less than {x['quiet_s']} s")
    if x.get("changes_max") is not None and len(e) > x["changes_max"]:
        why.append(f"{len(e)} changes, more than {x['changes_max']}")
    return not why, got, "; ".join(why)


class DemoSettle:
    """The shape an entry point's object takes: the check, and the notices it finds (kind -> meaning)."""
    check = staticmethod(check)
    notices = {"bounce": "a change before the line's last one (a contact bounce)"}


CHECK = DemoSettle()
