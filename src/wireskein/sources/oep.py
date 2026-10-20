"""oep:<target> - an OEP probe through oep-client-python (one shot).

Logic channels go to oep.fixture.logic, analog ones to oep.fixture.analog;
with both, oep.fixture.capture-group starts them together (the trigger, if
any, on a logic channel - only with a client whose group takes one: the
current OEP v1 draft has the immediate group only, capture §4). Channel ids are the probe's channel numbers; a
track's channels are its plan roles 0..C-1 in the order given. The lock is
taken for the capture and the plans are released afterwards.

Times: tick 0 of the file is the first logic sample (start_ns on the probe's
clock); an analog channel starts at t0_ticks = (its segment's start_ns - that)
plus its skew_ns, in ticks. These are the probe's estimates; each track's
uncertainty is kept, and the final alignment is the analysis's job.
ADC values are kept raw with the probe's linear conversion; the input range,
reference, Vrefint reading and factory calibration go along, never applied.
"""

from __future__ import annotations

import struct
from fractions import Fraction

from .. import fileformat
from . import Request, Result

TRIGGER = {"high": (1, 1), "low": (1, 0), "rise": (2, 0), "fall": (2, 1), "both": (2, 2)}   # (type, value), oep-if-capture
TAG_CHIP, TAG_MODEL, TAG_FIRMWARE, TAG_UNIT_ID = 0x4C, 0x41, 0x40, 0x42     # core describe
# UART link rates tried by default (port_speed): fastest first. oep-client refuses a rate that breaks and drops back
# to the boot speed on repeated broken frames, so trying never ends slower than the boot speed; `oep:PORT?fast=0`
# keeps the boot speed, `?fast=RATE,RATE` tries those.
FAST = [1_500_000, 921_600, 500_000]
PER_SESSION = 2          # candidates tried per open: each costs up to ~1 s, plus a wait when it fails (core §3.5)
FLOWS = [("in", 0)]      # a capture reads back: verify probe -> host only, at the link's largest in-flight n


def _raise_default(link, hst, port: str | None) -> tuple[list[int], bool]:
    """The default raise, left to the client: it orders and skips the candidates by its record of this port and
    probe, tries at most PER_SESSION, retries the slowest when all failed, and steps down to a slower one in use.
    -> (rates asked, whether the record shaped them)."""
    if not port:
        return [], False
    link.raise_speed(hst, list(FAST), record=True, flows=FLOWS, max_tries=PER_SESSION)
    rep = hst.link.speed
    return list(FAST), bool(rep is not None and (rep.skipped or rep.retried))


def capture(target: str, req: Request) -> Result:
    try:
        from oep_client import capture as oc
        from oep_client import core, link
        from oep_client import host as oh
        from oep_client import message
    except ImportError as e:
        raise RuntimeError('the oep source needs oep-client-python: pip install "wireskein[oep]"') from e
    target, _, query = target.partition("?")
    if not target:
        raise ValueError("oep:<target>: a serial port, tcp://HOST:PORT or usb[:VID:PID[:SERIAL]]")
    import urllib.parse
    opts = urllib.parse.parse_qs(query)
    unknown = set(opts) - {"fast"}
    if unknown:
        raise ValueError(f"oep:{target}?...: unknown option {', '.join(sorted(unknown))} "
                         f"(fast=1, or fast=RATE,RATE,... raises a UART probe's link)")
    fast = _rates(opts.get("fast", ["1"])[-1])
    try:
        ids = [int(cid) for _, cid in req.channels]
        aids = [int(cid) for _, cid, _ in req.analog]
    except ValueError:
        raise ValueError("oep channel ids are the probe's channel numbers: "
                         f"{[c for _, c in req.channels] + [c for _, c, _ in req.analog]}") from None
    if req.resolution and not hasattr(oc, "DATA_FORMAT"):
        raise ValueError("a resolution (NAME=ID/D) needs an oep-client-python with capture format 1")
    if req.mode == "stream" and (aids or req.trigger or not ids):
        raise ValueError("a stream (--mode stream) takes logic channels only, started at once, for now")
    try:
        res = _capture(link, core, oc, oh, message, target, req, ids, aids, fast)
        if isinstance(res, _Streamed):           # put together after the session closed: it may take a while
            res = res.result()
        return res
    except oh.OepError as e:        # the probe refused or failed: say what, not where
        hint = ""
        if "(storage)" in str(e):     # capture §2.2: data lost inside a segment stops the track with error 2
            hint = " - the probe lost data inside a segment: try a lower --rate or fewer channels"
        raise RuntimeError(f"probe {target}: {e}{hint}") from e


def _trigger_source(oc, req: Request, cap, an, names: list[str], acfg):
    """--trigger as the group's trigger source (capture §4.4): one stage of one condition - a logic channel's edge or
    level, or an analog channel crossing / staying beyond a voltage (in its ADC values by the answer's scale)."""
    name, kind, *volts = req.trigger
    if name in names:
        role = names.index(name)
        cond = (oc.Cond.edge(cap, role, {"rise": 0, "fall": 1, "both": 2}[kind]) if kind in ("rise", "fall", "both")
                else oc.Cond.level(cap, role, 1 if kind == "high" else 0))
    else:
        role = [n for n, *_ in req.analog].index(name)
        zero, scale = acfg.zero.get(role), acfg.scale_nv.get(role)
        if zero is None or not scale:
            raise ValueError(f"trigger {name}: the probe gives no volt conversion for it")
        value = round(volts[0] * 1e9 / scale + zero)
        top = (1 << acfg.bits) - 1 if acfg.bits else None
        if value < 0 or top is not None and value > top:
            raise ValueError(f"trigger {name}:{kind}:{volts[0]}: outside the input's range")
        make = {"rise": oc.Cond.cross_up, "fall": oc.Cond.cross_down, "high": oc.Cond.above, "low": oc.Cond.below}
        if scale < 0:                            # an inverting frontend: up in volts is down in values
            make = {"rise": oc.Cond.cross_down, "fall": oc.Cond.cross_up, "high": oc.Cond.below, "low": oc.Cond.above}
        cond = make[kind](an, role, value)
    return oc.TriggerSource([oc.Stage([[cond]])], arm=0 if req.arm == "now" else 1)


def _check_caps(group, source, req: Request, target: str, probe: dict) -> None:
    """The group's trigger_caps (capture §4.4, a declaration: bind has the last word) kept in meta.probe, and a
    condition type it does not declare refused before bind, saying which it takes."""
    try:
        caps = group.trigger_caps() if hasattr(group, "trigger_caps") else None
    except Exception:              # noqa: BLE001 - a declaration is a bonus: bind still answers
        caps = None
    if caps is None:
        return
    probe["trigger_caps"] = {"stages": caps.max_stages, "terms": caps.max_terms, "conds": caps.max_conds,
                             "cross_track": caps.cross_track, "kinds": caps.kinds()}
    kinds = {c.kind for st in source.stages for term in st.terms for c in term}
    if not all(caps.takes(k) for k in kinds):
        what = ":".join(str(x) for x in req.trigger[:2]) + (f":{req.trigger[2]}V" if len(req.trigger) > 2 else "")
        raise RuntimeError(f"probe {target} cannot start its tracks on {what} together: its capture group triggers on "
                           f"{', '.join(caps.kinds()) or 'nothing'} (trigger_caps, capture §4.4)")


def _what(tag, oc) -> str:
    """A configure item for people. The probe names the TLV it refused with its
    tag as sent (bit 7 is the critical mark)."""
    base = tag & 0x7F if isinstance(tag, int) else tag
    return {oc.TRIGGER: "a trigger", oc.PRETRIGGER: "a pretrigger",
            getattr(oc, "TRIGGER_OPTS", None): "these trigger options (--arm now)"}.get(
        base, f"configure item 0x{base:02x}" if isinstance(base, int) else f"configure item {tag}")


def _waiting(wait, cap, req):
    """wait(), with a timeout that names a trigger which never came (the probe still waiting, state 2)."""
    try:
        return wait()
    except TimeoutError:
        if req.trigger and cap is not None:
            try:
                st = cap.status()
                waiting = getattr(st, "state", None) == oc_state("waiting") if hasattr(st, "state") else \
                    st[0] == oc_state("waiting")
            except Exception:           # the timeout is the news; a failed status read must not hide it
                waiting = False
            if waiting:
                raise TimeoutError(f"no trigger ({req.trigger[0]}:{req.trigger[1]}) within {req.timeout:g} s "
                                   f"(--timeout)") from None
        raise


def oc_state(name: str) -> int:
    from oep_client.capture import STATE
    return STATE[name]


def _configure(track, oc, oh, target: str, what: str, mode=None, **kw):
    """configure; a refusal (unsupported, naming the TLV) in one line. Every probe knows the configure TLVs of OEP
    v1 (capture §3.3), so one it cannot honour is refused, not ignored."""
    try:
        return track.configure(mode=oc.ONE_SHOT if mode is None else mode, **kw)
    except oh.Unsupported as e:
        raise RuntimeError(f"probe {target} cannot capture {what} with {_what(e.tag, oc)}"
                           f"{_most_said(track, oc, oh, e, kw) if mode is None else ''}") from e


def _most_said(track, oc, oh, e, kw: dict) -> str:
    """For a refused pretrigger: the most this setting takes, found by query (nothing set), as words; else ""."""
    if e.tag is None or e.tag & 0x7F != oc.PRETRIGGER or not kw.get("pretrigger"):
        return ""
    most = _most_pretrigger(track, oc, oh, kw)
    return f" (at most {most} samples before the trigger here, of {kw.get('samples')})" if most is not None else ""


def _most_pretrigger(track, oc, oh, kw: dict) -> int | None:
    """The largest pretrigger the probe takes with these settings, by query and bisection (about log2 of it queries;
    capture §3.3: the same rules as configure, nothing set). None: no query, or not even a small one."""
    from oep_client import message
    lo, hi = 0, int(kw["pretrigger"]) - 1
    if hi < 1 or not isinstance(_query(track, oc, oh, message, {**kw, "pretrigger": 1}), oc.Config):
        return None
    lo = 1
    while lo < hi:
        mid = (lo + hi + 1) // 2
        r = _query(track, oc, oh, message, {**kw, "pretrigger": mid})
        if r is None:
            return None
        if isinstance(r, Exception):
            hi = mid - 1
        else:
            lo = mid
    return lo


def _query(track, oc, oh, message, kw: dict):
    """configure's query (capture §3.3: the same rules, nothing set) -> the answer, the refusal (the exception), or None
    when the probe has no query (OEP v1: optional)."""
    try:
        return track.configure(mode=oc.ONE_SHOT, query=True, **kw)
    except oh.Rejected as e:
        if not isinstance(e, oh.Unsupported) and e.result.detail == message.UNKNOWN_OPERATION:
            return None
        return e


def _clock(hst):
    """The probe's clock against this host's (core §7.7): the reading with the shortest round trip of a few, or None
    when it could not be read (the capture goes on without it)."""
    try:
        return hst.clock_best(4)
    except Exception:                  # noqa: BLE001 - the clock is a bonus to the capture
        return None


def _clock_info(before, after) -> dict:
    """meta.probe.clock: each reading as (host monotonic ns at the midpoint, the probe's uptime_ns, +- ns), and the
    probe clock's rate against the host's when both readings are of one boot (ppm: positive = the probe runs fast)."""
    out = {}
    for name, r in (("before", before), ("after", after)):
        if r is not None:
            out[name] = {"host_ns": int(r.host_ns), "uptime_ns": int(r.uptime_ns), "boot_id": int(r.boot_id),
                         "uncertainty_ns": int(r.uncertainty_ns)}
    if before is not None and after is not None and before.boot_id == after.boot_id:
        host_d, probe_d = after.host_ns - before.host_ns, after.uptime_ns - before.uptime_ns
        if host_d > 0:
            out["rate_ppm"] = (probe_d / host_d - 1) * 1e6
            out["rate_ppm_uncertainty"] = (before.uncertainty_ns + after.uncertainty_ns) / host_d * 1e6
    return out


def _describe(core, hst, fn: int) -> list[tuple[int, bytes]]:
    try:
        return core.describe(hst, fn)
    except Exception:       # noqa: BLE001 - what the probe tells about itself is a bonus
        return []


def _probe_info(core, hst, opened) -> dict:
    info = {}
    for tag, value in _describe(core, hst, 0):
        key = {TAG_CHIP: "chip", TAG_MODEL: "model", TAG_FIRMWARE: "firmware", TAG_UNIT_ID: "unit_id"}.get(tag)
        if key:
            info[key] = value.decode("utf-8", "replace").rstrip("\0")
    info["boot_id"] = opened.boot_id    # start_ns of captures with the same boot_id share one clock
    return info


def _frontends(core, hst, fn: int) -> dict[int, dict]:
    """The analog describe's frontends (capture §3.5): number -> its measurable range."""
    from oep_client import registry
    want = registry.FIXTURE_ANALOG.tlv["describe"]["frontend"]
    out = {}
    for tag, v in _describe(core, hst, fn):
        if tag == want and len(v) >= 9:
            fe, lo, hi = struct.unpack_from("<Bii", v)
            out[fe] = {"frontend": fe, "range_min_mv": lo, "range_max_mv": hi}
            if len(v) >= 13:            # the attenuation (milli dB), when the probe gives it (asked back of oep-spec)
                (att,) = struct.unpack_from("<I", v, 9)
                if att != 0xFFFFFFFF:
                    out[fe]["attenuation_db"] = att / 1000
    return out


def _rates(text: str) -> list[int]:
    """fast=0: [] (the boot speed); fast=1 / absent: None (the default, _raise_default);
    fast=1500000,921600: those, in that order, whatever was remembered."""
    if text.lower() in ("", "0", "no", "off", "false"):
        return []
    if text.lower() in ("1", "yes", "on", "true"):
        return None
    try:
        return [int(r) for r in text.split(",")]
    except ValueError:
        raise ValueError(f"fast={text}: 0, 1, or link rates (fast=1500000,921600)") from None


def _raise(link, hst, rates: list[int]) -> list[int]:
    """Raise the link of an open host to the first of `rates` that holds, verifying the read-back direction (FLOWS);
    the client keeps the boot speed, or goes back to it, whenever the probe, the adapter or the line cannot.
    -> the rates asked of the client."""
    if rates:
        link.raise_speed(hst, rates, flows=FLOWS)
    return rates


def _link_info(hst, rates: list[int], open_s: float, read_bytes: int, read_s: float) -> dict:
    """How the link went, for meta.probe.link: what was asked, the rate in force, each trial with its flows, what
    the record skipped, steps down while in use, and the read."""
    out = {"open_s": round(open_s, 3), "read_bytes": read_bytes, "read_s": round(read_s, 3)}
    lk = getattr(hst, "link", None)
    reader = getattr(lk, "reader", None)
    if isinstance(reader, str):
        out["reader"] = reader                            # usb-async / usb-sync (a one-read host's pauses drop data)
    rep = getattr(lk, "speed", None)
    if rates:
        out["asked"] = rates
    if rep is not None:
        out["rate"] = rep.rate
        out["raised"] = bool(rep.chosen)
        if not rep.supported:
            out["why"] = rep.why
        out["trials"] = [{"rate": t.rate, "result": "committed" if t.committed else t.why,
                          "flows": [{"flow": f.flow, "n": f.n, "frames": f.frames, "broken": f.broken, "lost": f.lost,
                                     "kb_s": round(f.kb_s, 1), "passed": f.passed} for f in t.flows]}
                         for t in rep.trials]
        if rep.skipped:
            out["skipped"] = list(rep.skipped)            # the record says these failed on this port and probe
        if rep.retried:
            out["retried"] = rep.retried                  # every rate recorded failed: the slowest tried once anyway
        if rep.capped:
            out["capped"] = list(rep.capped)              # left out by the limit of tries per capture
        if rep.step_downs:                                # in use (or in probation): from a rate to a slower one
            out["step_downs"] = [{"from": d.rate, "to": d.to, "why": d.why, "probation": d.probation}
                                 for d in rep.step_downs]
        if rep.stepped_down or rep.lost:
            out["stepped_down"] = rep.down_why or "lost"  # went down while in use
    return out


def _capture(link, core, oc, oh, message, target: str, req: Request, ids: list[int], aids: list[int],
             fast: list[int] | None = None) -> Result:
    import time
    names = [n for n, _ in req.channels]
    t_open = time.monotonic()
    remembered = False
    hst = link.open_host(target)
    try:
        opened = core.take(hst, 30_000, owner="wireskein capture")
        probe = _probe_info(core, hst, opened)
        # the link's memory is per port and probe (host development guide §7.5): the adapter belongs to the port,
        # the rest of the link to the probe, so either changing starts over
        record = fast is None                          # the default: the client's record of this port and probe
        if fast is None:
            port = getattr(getattr(hst, "link", None), "port_path", None)
            fast, remembered = _raise_default(link, hst, port)
        else:
            fast = _raise(link, hst, fast)
        open_s = time.monotonic() - t_open
        clock_before = _clock(hst)
        cap = oc.LogicCapture(hst) if ids else None
        an = oc.AnalogCapture(hst) if aids else None
        plan = [(cap.fn, k, ch) for k, ch in enumerate(ids)] if cap else []
        plan += [(an.fn, k, ch) for k, ch in enumerate(aids)] if an else []
        core.plan_apply(hst, plan)
        fns = sorted({fn for fn, _, _ in plan})
        try:
            cfg = acfg = None
            grouped = bool(req.trigger and an)        # a trigger with analog: the group's trigger source (§4.4)
            if cap:
                trigger = None
                if req.trigger and grouped:                # the group's event starts it (capture §4.4)
                    trigger = (oc.LINKED, 0, 0)
                elif req.trigger:
                    kind, value = TRIGGER[req.trigger[1]]
                    trigger = (kind, names.index(req.trigger[0]), value)
                blocks = {}
                if req.resolution:          # format 1: these lines as their changes, the others raw (§1.3)
                    blocks = dict(data_format=oc.DATA_FORMAT["logic_blocks"], roles=list(range(len(ids))),
                                  events={names.index(n): d for n, d in req.resolution.items()})
                if req.arm == "now" and trigger and not grouped:   # arm 0 (§3.3): optional, so critical; 1 the default
                    if not hasattr(oc, "TriggerOpts"):
                        raise ValueError("--arm now needs an oep-client-python with trigger_opts")
                    blocks["trigger_opts"] = oc.TriggerOpts(arm=0)
                if req.mode == "stream":
                    cfg = _configure(cap, oc, oh, target, "logic", oc.STREAMING, rate=req.rate, **blocks)
                    return _stream_result(cap, hst, cfg, req, names, ids, probe, fast, open_s, clock_before, oc)
                lkw = dict(rate=req.rate, samples=req.samples, trigger=trigger, pretrigger=req.pretrigger, **blocks)
                if grouped:          # both tracks asked first (query), set only when neither refuses (below)
                    lq = _query(cap, oc, oh, message, lkw)
                    lrate = Fraction(lq.rate) if getattr(lq, "rate", None) else Fraction(req.rate)
                    lsamples = getattr(lq, "samples", None) or req.samples
                else:
                    cfg = _configure(cap, oc, oh, target, "logic", **lkw)
                    lrate, lsamples = Fraction(cfg.rate), getattr(cfg, "samples", None) or req.samples
            if an:
                arate = req.analog_rate or req.rate
                asamples = req.analog_samples
                fes = {k: fe for k, (_, _, fe) in enumerate(req.analog) if fe is not None}
                if asamples is None and cap:
                    # as long as the logic capture the probe answered (it may hold fewer samples than asked), at
                    # the rate it will really use (channels share the ADC, so it may answer less than asked)
                    real = Fraction(arate)
                    try:
                        q = an.configure(mode=oc.ONE_SHOT, rate=arate, samples=1, query=True,
                                         **({"frontends": fes} if fes else {}))
                        real = Fraction(q.rate) if q.rate else real
                    except oh.Rejected as e:       # query is optional (OEP v1): absent = unknown_operation
                        if e.result.detail != message.UNKNOWN_OPERATION:
                            raise
                    asamples = max(1, round(Fraction(lsamples) / lrate * real))
                elif asamples is None:
                    asamples = req.samples
                linked = {}
                if grouped:                             # its own pretrigger, in its own samples (§4.4)
                    pre = req.pretrigger or 0
                    if cap and pre:
                        real = Fraction(an.configure(mode=oc.ONE_SHOT, rate=arate, samples=1, query=True,
                                                     **({"frontends": fes} if fes else {})).rate)
                        pre = round(Fraction(pre) * real / lrate)
                    linked = {"trigger": (oc.LINKED, 0, 0), **({"pretrigger": pre} if pre else {})}
                what = "analog"
                if linked.get("pretrigger") and cap:    # say where the number came from when it is refused
                    what = (f"analog (its pretrigger {linked['pretrigger']} of {asamples} samples is the logic's "
                            f"{req.pretrigger} at {float(lrate):g} Hz, counted at the analog rate; a longer capture "
                            f"or a lower logic rate gives it more)")
                akw = dict(rate=arate, samples=asamples, **({"frontends": fes} if fes else {}), **linked)
                if grouped:          # every refusal at once, before either track is set (capture §3.3 query)
                    asked = ([("logic", lq)] if cap else []) + [(what, _query(an, oc, oh, message, akw))]
                    refused = [(w, r) for w, r in asked if isinstance(r, Exception)]
                    if refused:
                        kws = {"logic": (cap, lkw), what: (an, akw)}
                        raise RuntimeError(f"probe {target} cannot capture these together (each track asked before "
                                           f"either was set): " + "; ".join(
                                               f"{w} with {_what(r.tag, oc)}{_most_said(*kws[w][:1], oc, oh, r, kws[w][1])}"
                                               if isinstance(r, oh.Unsupported) else f"{w}: {r}"
                                               for w, r in refused)) from refused[0][1]
                    if cap:
                        cfg = _configure(cap, oc, oh, target, "logic", **lkw)
                acfg = _configure(an, oc, oh, target, what, **akw)
            group = None
            if an and (cap or grouped):
                group = oc.CaptureGroup(hst)
                tracks = [t for t in (cap, an) if t]
                source = _trigger_source(oc, req, cap, an, names, acfg) if grouped else None
                judge = (cap if req.trigger[0] in names else an) if grouped else None
                latency = getattr(judge.config, "judge_latency_ns", None) if judge is not None else None
                if latency is not None:                 # how long the judging track takes to know the event (§3.3)
                    probe["judge_latency_ns"] = latency
                if source is not None:
                    _check_caps(group, source, req, target, probe)
                try:
                    group.bind(tracks, source)
                except (oh.Unsupported, oh.Rejected) as e:
                    if source is None:
                        raise
                    why = ""
                    if latency is not None:
                        others = ", ".join(f"{'logic' if t is cap else 'analog'} {float(t.config.rate):g} Hz"
                                           for t in tracks if t is not judge)
                        why = (f"; the {'logic' if judge is cap else 'analog'} track takes up to {latency / 1e6:.3g} ms "
                               f"to know the event, and the others ({others}) must hold that long besides their "
                               f"pretrigger - a lower rate or fewer channels on them, or the trigger on the other track")
                    raise RuntimeError(f"probe {target} cannot start its tracks on a trigger together (capture-group "
                                       f"trigger source, capture §4.4: {e}){why}") from e
                try:
                    group_start = group.start(tracks)
                    st = _waiting(lambda: group.wait(req.timeout), group, req)
                    seg = _segment(cap) if cap else None
                    aseg = _segment(an)
                finally:
                    group.bind([])
                probe["group_start_ns"] = group_start
                if st.trigger is not None:              # (fn judging, ns, +-ns) once the event happened
                    probe["trigger_ns"], probe["trigger_uncertainty_ns"] = st.trigger[1], st.trigger[2]
            else:
                track = cap or an
                track.start()
                segments = _waiting(lambda: track.wait(req.timeout), cap, req)
                if not segments:
                    raise RuntimeError("the probe finished without a segment")
                seg = segments[0] if cap else None
                aseg = segments[0] if an else None
            t_read = time.monotonic()
            data = cap.read_segment(seg) if cap else None
            adata = an.read_segment(aseg) if an else None
            read_s = time.monotonic() - t_read
            probe["link"] = _link_info(hst, fast, open_s, len(data or b"") + len(adata or b""), read_s)
            if remembered:
                probe["link"]["remembered"] = True
            if "rate" not in probe["link"] and getattr(getattr(hst, "link", None), "base_baud", None):
                probe["link"].update(rate=hst.link.base_baud, raised=False)   # nothing tried: the boot speed
            if "rate" not in probe["link"] and record and not getattr(getattr(hst, "link", None), "port_path", None):
                probe["link"].update(rate=0, why="the link is not a serial port this host opened")
            calib = an.calibration() if an else None
            clock_after = _clock(hst)
            clock = _clock_info(clock_before, clock_after)
            if clock:
                probe["clock"] = clock
            frontends = _frontends(core, hst, an.fn) if an else {}
        finally:
            core.plan_release(hst, fns)
    finally:
        try:
            hst.end()
        finally:
            close = getattr(getattr(hst, "link", None), "close", None)
            if close is not None:
                close()          # let the port go (a serial port is opened exclusively: the next open must succeed)

    meta = {}
    if cap:
        meta["probe_channels"] = dict(zip(names, ids))
        _times(meta, seg)
        if seg.slipped:
            meta["time_base_slipped"] = True
        if seg.trigger_index is not None:              # tick 0 is the segment's (base) sample 0
            meta["trigger_tick"] = seg.trigger_index
        tick = Fraction(cfg.rate)
        if getattr(cfg, "event_d", None):
            chans = _from_blocks(cap, data, names, seg.samples, cfg)
            probe["data"] = _budget(oc, cfg, len(data), seg.samples, len(names))
        else:
            chans = fileformat.from_interleaved(data, names, cfg.width, cfg.positions, seg.samples)
        for c, ch in zip(chans, ids):
            c.acquisition = {"pin": ch}
        t_ref = seg.start_ns
    else:
        tick = Fraction(acfg.rate)
        chans = []
        _times(meta, aseg)
        t_ref = aseg.start_ns
    gens = {k: t.generation for k, t in (("logic", cap), ("analog", an)) if t is not None and t.generation is not None}
    if gens:
        probe["generation"] = gens
    if an:
        if calib is not None and (calib.factory or calib.vrefint):
            probe["calibration"] = [{"frontend": fe, "scheme": scheme, "raw": bytes(raw).hex()}
                                    for fe, scheme, raw in calib.factory]
        a_ns = aseg.start_ns
        for k, ((name, _, _), ch) in enumerate(zip(req.analog, aids)):
            values = an.values(adata, k, aseg.samples)
            t0 = Fraction(0)
            if t_ref is not None:
                t0 = Fraction(a_ns - t_ref) * tick / 10**9
            t0 += Fraction(acfg.skew_ns.get(k, 0)) * tick / 10**9
            acq = {"pin": ch}
            fe = acfg.frontend.get(k)
            if fe is not None:
                acq["frontend"] = frontends.get(fe, {"frontend": fe})
                if "attenuation_db" in acq["frontend"]:
                    acq["attenuation_db"] = acq["frontend"]["attenuation_db"]
            if acfg.reference:
                source, mv, measured = acfg.reference
                acq["reference"] = {"source": source, "mv": mv, "measured": bool(measured)}
            if calib is not None and calib.vrefint:
                acq["vrefint_raw"], acq["vrefint_ns"] = calib.vrefint
            if calib is not None and calib.vrefint_nominal_mv is not None:
                acq["vrefint_nominal_mv"] = calib.vrefint_nominal_mv
            acq["start_uncertainty_ns"] = aseg.start_uncertainty_ns
            if aseg.trigger_index is not None:
                acq["trigger_index"] = aseg.trigger_index
            width = {8: 8, 16: 16, 32: 32}[acfg.slot]
            chans.append(fileformat.analog_raw(name, values, Fraction(acfg.rate), width=width, t0_ticks=t0,
                                        value_bits=acfg.bits or None, zero=acfg.zero.get(k),
                                        scale_nv=acfg.scale_nv.get(k), **acq))
    if probe:
        meta["probe"] = probe
    return Result(tick, chans, meta)


def _from_blocks(cap, data: bytes, names: list[str], samples: int, cfg) -> list:
    """A format 1 segment's bytes -> the file's channels, in the order asked: a raw channel as bits, an events channel
    as `events` at its D (_segment_columns)."""
    out = []
    for name, col in zip(names, _segment_columns(cap, data, samples, len(names), cfg)):
        if isinstance(col, bytes):
            out.append(fileformat.Channel(name, col[:(samples + 7) // 8], samples))
        else:
            d = col[4]
            out.append(_events_np(name, *col[:4], -(-samples // d), d))
    return out


def _block_columns(blocks, roles: int, cfg) -> list:
    """A format 1 segment's blocks -> per role, a raw channel's bits (L is a multiple of 8, so the blocks' bytes join
    as they are; the last block's tail 0, §1.3), or an events channel's (quanta of the segment, levels after, counts,
    start level, D) as numpy arrays (L is a multiple of D). A block the probe carried raw (fallback) is counted here
    from its samples and the block's start level, the same way the probe counts (§1.3)."""
    import numpy as np
    out = []
    for role in range(roles):
        d = cfg.event_d.get(role)
        if d is None:
            out.append(b"".join(b.raw[role] for b in blocks))
            continue
        qs, ls, cs = [], [], []
        for b in blocks:
            q0 = b.first // d
            if role in b.fallback:
                lv = np.unpackbits(np.frombuffer(b.raw[role], np.uint8), bitorder="little")[:b.samples]
                change = np.empty(b.samples, np.uint8)
                change[0] = lv[0] != b.start_levels[role]       # the level before the block (§1.3)
                np.not_equal(lv[1:], lv[:-1], out=change[1:])
                per = np.add.reduceat(change.astype(np.int64), np.arange(0, b.samples, d))
                t = np.flatnonzero(per)
                qs.append(q0 + t)
                ls.append(lv[np.minimum((t + 1) * d, b.samples) - 1])
                cs.append(per[t])
            else:
                e = b.events[role]
                qs.append(q0 + np.asarray(e.ticks, np.int64))
                ls.append(np.asarray(e.levels, np.uint8))
                cs.append(np.asarray(e.counts, np.int64))
        start = int(blocks[0].start_levels[role]) if blocks else 0
        out.append((np.concatenate(qs).astype(np.int64) if qs else np.zeros(0, np.int64),
                    np.concatenate(ls).astype(np.uint8) if ls else np.zeros(0, np.uint8),
                    np.concatenate(cs).astype(np.int64) if cs else np.zeros(0, np.int64), start, d))
    return out


def _segment_columns(cap, data: bytes, samples: int, roles: int, cfg) -> list:
    """_block_columns of a format 1 segment's bytes; read by columns when the client can (no block objects)."""
    columns = getattr(cap, "columns", None)
    if columns is None:
        return _block_columns(cap.blocks(data, samples), roles, cfg)
    got = columns(data, samples)
    return [got[r] if r not in cfg.event_d else (*got[r], cfg.event_d[r]) for r in range(roles)]


def _events_np(name: str, quanta, levels, counts, start_level: int, n: int, step: int, **acquisition):
    """fileformat.events() over numpy arrays, at once: the same checks and the same bytes (wireskein-format §4.4)."""
    import numpy as np
    q = np.asarray(quanta, np.int64)
    lv = np.asarray(levels, np.int64)
    c = np.asarray(counts, np.int64)
    if len(q):
        before = np.concatenate(([start_level], lv[:-1]))
        sat = np.minimum(c, 127)
        bad = (np.diff(q, prepend=-1) <= 0) | (q >= n) | ((lv != 0) & (lv != 1)) | (c < 1) | \
            ((sat < 127) & (lv != before ^ (sat & 1)))
        if step == 1:
            bad |= c != 1
        if bad.any():
            k = int(np.flatnonzero(bad)[0])
            raise ValueError(f"{name}: event (quantum {q[k]}, level {lv[k]}, count {c[k]}) out of order or impossible")
        gap = np.diff(q, prepend=-1) - 1
        nb = np.ones(len(q), np.int64)
        for j in range(1, 10):
            nb += gap >= (1 << (7 * j))
        at = np.concatenate(([0], np.cumsum(nb + 1)[:-1]))
        out = np.empty(int((nb + 1).sum()), np.uint8)
        for j in range(int(nb.max())):
            m = nb > j
            out[at[m] + j] = ((gap[m] >> (7 * j)) & 0x7F) | np.where(nb[m] > j + 1, 0x80, 0)
        out[at + nb] = (lv << 7) | sat
        data = out.tobytes()
    else:
        data = b""
    return fileformat.EventsChannel(name, data, n, step, 0, start_level, len(q), acquisition)


STREAM_POLL_S = 0.2       # how often a stream looks for a loss while collecting
STREAM_MIN_BYTES = 16384  # a stream's notifications (core §10.3): at least this much a frame ...
STREAM_MAX_DELAY_MS = 20  # ... or after this long. With (0, 0) the P4X sent each format 1 block (tens of bytes on
                          # quiet lines) as a transfer of its own, 2000 a second, and dropped 35 % it could not send


def _stream_result(cap, hst, cfg, req: Request, names, ids, probe, fast, open_s, clock_before, oc) -> Result:
    """A stream (capture §2.1 mode 3): subscribe, start, collect for req.duration, stop and collect the rest. A loss
    (a skipped position: the probe dropped a segment it had no room to send) stops it at once with on_loss "stop"
    (the default), else it goes on; either way the capture is incomplete, a failure (wireskein-format §3.4)."""
    import time
    link = hst.link
    if getattr(cfg, "stream_buffer", None) is not None:   # how long this host may pause (§3.3), before it starts
        worst = None
        if getattr(cfg, "event_d", None):                 # format 1: over its worst bandwidth, a lower bound
            from oep_client import capture_blocks
            worst = capture_blocks.worst_bytes(len(cfg.raw_roles), list(cfg.event_d.values()), cfg.block) \
                * float(cfg.rate) / cfg.block
        probe["stream_buffer"] = {"bytes": cfg.stream_buffer, "tolerable_delay_s": cfg.tolerable_delay_s(worst)}
    spool = _Spool(cap, cfg, names, ids, req) if _spools(cfg) else None
    cap.subscribe(STREAM_MIN_BYTES, STREAM_MAX_DELAY_MS)   # batched: a capture wants bandwidth, not a frame per block
    cap.start()
    t_read = time.monotonic()
    deadline, got = t_read + req.duration, oc.Received()
    while True:                                  # stream() renews the lease across calls on the same Received
        now = time.monotonic()
        if now >= deadline:
            break
        got = cap.stream(link, seconds=min(STREAM_POLL_S, deadline - now), into=got)
        if spool is not None:                    # put together as it comes: memory holds a poll's worth, not the stream
            spool.take(got)
            if req.on_loss == "stop" and spool.lost:
                break
        elif req.on_loss == "stop" and (got.gaps or (got.start or 0) > 0):
            break
    cap.stop()
    got = cap.finish(link, got)
    read_s = time.monotonic() - t_read
    st = cap.status()
    if spool is not None:                        # handed over now; waited for once the session is closed (the worker
        spool.give(got, write_pos=st.write_pos if st.state == oc.STATE["error"] else None)   # may be behind)
    drops = oc.CaptureDrops.of(st) if hasattr(oc, "CaptureDrops") else None   # the reference firmware's own
    if drops is not None:
        import dataclasses
        probe["drops"] = dataclasses.asdict(drops)
    received = spool.received if spool is not None else len(got.data)
    tail = None
    if st.state != oc.STATE["error"]:            # stopped: every byte up to write_pos should have come (§2.1 rule 2)
        end = spool.end if spool is not None else (got.start or 0) + len(got.data) + sum(n for _, n in got.gaps)
        if st.write_pos > end:
            tail = st.write_pos - end
    probe["link"] = _link_info(hst, fast, open_s, received, read_s)
    clock = _clock_info(clock_before, _clock(hst))
    if clock:
        probe["clock"] = clock
    if cap.generation is not None:
        probe["generation"] = {"logic": cap.generation}
    meta = {"probe_channels": dict(zip(names, ids)), "probe": probe,
            "stream": {"duration_s": req.duration, "on_loss": req.on_loss, "frames": got.frames,
                       "bytes": received, "read_s": round(read_s, 3)}}
    return _Streamed(cap, cfg, got, st, names, ids, req, meta, oc, spool, tail)


class _Streamed:
    """A stream collected, to be put together once the session is closed (decoding takes time; the lease does not
    wait for it). cap.blocks only reads the configure answer kept in the client."""
    def __init__(self, cap, cfg, got, st, names, ids, req, meta, oc, spool=None, tail=None):
        self.cap, self.cfg, self.got, self.st, self.names, self.ids, self.req, self.meta, self.oc = (
            cap, cfg, got, st, names, ids, req, meta, oc)
        self.spool, self.tail = spool, tail

    def result(self) -> Result:
        if self.spool is not None:
            self.spool.finish()                  # after the session closed: the lease does not wait for it
            parts, lost = self.spool.parts, self.spool.done_lost
            if not parts:
                raise RuntimeError("the stream brought no data" + (" (the probe dropped it all)" if lost else ""))
        else:
            parts, lost = _stream_parts(self.cap, self.cfg, self.got, self.names, self.ids, self.req)
        st = self.st
        why = _drops_line(self.meta["probe"].get("drops"))
        if st.state == self.oc.STATE["error"]:
            lost.append({"after_part": len(parts) - 1, "reason": f"the probe stopped in error ({st.error_name})",
                         "stopped": True})
        elif st.dropped and not lost:
            lost.append({"after_part": len(parts) - 1, "reason": "the probe dropped data (status flags bit 0)"})
        if self.tail:                            # the end did not come before finish() gave up: a loss too
            lost.append({"after_part": len(parts) - 1, "stopped": True,
                         "reason": f"the stream's last {self.tail} bytes did not arrive after the stop"})
        if why and lost:                         # where the probe says the data went (its own diagnostics; the
            lost[-1]["reason"] += f" - the probe's counts since start: {why}"    # counters are totals: said once)
        return Result(Fraction(self.cfg.rate), parts[0].channels, self.meta, parts, lost, "timeline",
                      self.spool.dir if self.spool is not None else None)


def _spools(cfg) -> bool:
    """Whether a stream is put together on disk as it comes (_Spool): format 0 with whole bytes a sample, and format
    1 (segment by segment). Format 0 with a sample across bytes is put together at the end, in memory."""
    return bool(getattr(cfg, "event_d", None)) or cfg.width % 8 == 0


class _Spool:
    """A stream put together while it comes (wireskein-format §3.4 parts on a timeline): after each poll the bytes
    that arrived are taken out of the client's Received (its start moved on, so it goes on counting positions), the
    channels' bits appended to files by the output and the events kept; a skipped position ends the part (a loss)
    and the next part starts where the bytes do again. Memory holds about a poll's worth, whatever the length."""

    def __init__(self, cap, cfg, names, ids, req: Request):
        import tempfile
        self.cap, self.cfg, self.names, self.ids, self.req = cap, cfg, names, ids, req
        self.dir = tempfile.mkdtemp(prefix=".wireskein-stream-", dir=req.spool_dir)
        self.blocks = bool(getattr(cfg, "event_d", None))
        self.ds = list(getattr(cfg, "event_d", {}).values())
        self.rate = Fraction(cfg.rate)
        self.parts, self.lost = [], []
        self.pending = bytearray()      # bytes from position `at` not put together yet (a gapless stretch)
        self.at = None                  # stream position of pending[0]
        self.bytes = 0
        self.received = 0               # bytes handed over (the caller's count; `bytes` is the worker's)
        self.end = 0                    # the stream position just past what arrived
        self.segs = {}                  # segment position -> SegmentInfo (the latest)
        self.first_ns = None
        self.cur = None                 # the part being put together: dict (tick0, meta, samples, files, events, ...)
        self.stopped = False
        import queue
        import threading
        self._queue, self._error = queue.Queue(), None
        self._worker = threading.Thread(target=self._drain, name="wireskein-spool", daemon=True)
        self._worker.start()

    # -- taking what arrived (the caller's thread: O(1), so its polls keep coming) --
    def give(self, got, write_pos: int | None = None) -> None:
        """The last of the stream: handed over with the end mark, not waited for (finish() waits)."""
        self.take(got)
        self._queue.put(("final", write_pos))
        self._given = True

    def finish(self) -> None:
        """Wait for the worker to put everything together (its error raised here)."""
        self._worker.join()
        if self._error is not None:
            raise self._error

    _given = False

    def take(self, got, final: bool = False, write_pos: int | None = None) -> None:
        """Hand what arrived to the worker: the client's buffers are swapped for empty ones (no copy) and its start
        moved on, so it goes on counting positions. final: wait for the worker to finish (its error raised here)."""
        segs, got.segments = got.segments, {}
        data, gaps, start = got.data, sorted(got.gaps), got.start
        if start is not None:
            got.data, got.gaps = bytearray(), []
            got.start = start + len(data) + sum(n for _, n in gaps)
            self.received += len(data)
            self.end = got.start                 # the position just past what arrived
        self._queue.put((segs, start, data, gaps))
        if final:
            self._queue.put(("final", write_pos))
            self._worker.join()
            if self._error is not None:
                raise self._error

    def _drain(self) -> None:
        try:
            while True:
                item = self._queue.get()
                if item[0] == "final":
                    self._work(True, item[1])
                    self._close()
                    return
                segs, start, data, gaps = item
                self.segs.update(segs)
                if start is not None and data:
                    pos, i = start, 0
                    for index, skipped in gaps + [(len(data), 0)]:
                        self._run(pos, memoryview(data)[i:index])
                        pos += index - i + skipped
                        i = index
                self._work(False, None)
        except BaseException as e:               # noqa: BLE001 - raised in the caller's thread at the end
            self._error = e

    def _run(self, p0: int, data: bytes) -> None:
        if self.stopped or not data:
            return
        self.bytes += len(data)
        if self.at is None and not self.parts and self.cur is None:
            if p0 != 0:                          # positions count from the generation's start: the first is gone
                if self.req.on_loss == "stop":
                    raise RuntimeError(f"the probe dropped the stream's first {p0} bytes before any arrived: nothing "
                                       f"to keep (a lower --rate or fewer channels, or --on-loss continue)")
                self.lost.append({"after_part": -1, "reason": f"the probe dropped the first {p0} bytes (no room to send)"})
            self.at = p0
        elif p0 != self.at + len(self.pending):  # a skipped position: the probe dropped a segment
            self._work(True, None)
            self._close()
            stop = self.req.on_loss == "stop"
            self.lost.append({"after_part": len(self.parts) - 1,
                              "reason": "the probe dropped a segment (no room to send)", "_next": p0,
                              **({"stopped": True} if stop else {})})
            if stop:
                self.stopped = True
                return
            self.at, self.pending = p0, bytearray()
        self.pending += data

    # -- putting together --
    def _work(self, final: bool, write_pos: int | None) -> None:
        if self.at is None or not self.pending:
            return
        if self.blocks:
            self._segments(final, write_pos)
        else:
            self._samples(final, write_pos)

    def _samples(self, final: bool, write_pos: int | None) -> None:
        b = self.cfg.width // 8
        n = len(self.pending) // b
        if write_pos is not None:                # an error stop: nothing at or past write_pos is data (§2.2)
            n = min(n, max(0, (write_pos - self.at) // b))
        if not final:
            n = n // 8 * 8                       # whole bytes of bits; the rest waits for the next poll
        if not n:
            return
        self._open(self.at)
        chans = _samples_at_once(memoryview(self.pending), self.names, self.ids, self.cfg, n)
        for c, f in zip(chans, self.cur["files"]):
            f.write(c.bits)
        self.cur["samples"] += n
        del self.pending[:n * b]
        self.at += n * b
        for q in [q for q in self.segs if q < self.at and q != self.cur.get("first", self.at)]:
            del self.segs[q]                     # put together: only a part's start needs its record
        if final:
            self.pending = bytearray()

    def _segments(self, final: bool, write_pos: int | None) -> None:
        end = self.at + len(self.pending)
        starts = sorted(p for p in self.segs if self.at <= p < end)
        ready = []
        for k, p in enumerate(starts):
            nxt = starts[k + 1] if k + 1 < len(starts) else (end if final else None)
            if nxt is None or (write_pos is not None and p >= write_pos):
                break
            ready.append((p, nxt))
        L = self.cfg.block
        k = 0
        while k < len(ready):                    # segments of whole blocks run on: their blocks read in one go
            j = k
            while j + 1 < len(ready) and self.segs[ready[j][0]].samples % L == 0:
                j += 1
            p, nxt = ready[k][0], ready[j][1]
            x = self.segs[p]
            samples = sum(self.segs.pop(q).samples for q, _ in ready[k:j + 1])
            if self.cur is not None and (self.cur["samples"] % 8 or any(self.cur["samples"] % d for d in self.ds)):
                self._close(follows=True)      # a grid restart (format 1) is not a loss: the next part follows on
            self._open(p, x)
            data = bytes(self.pending[p - self.at:nxt - self.at])
            cols = _segment_columns(self.cap, data, samples, len(self.names), self.cfg)
            for col, f, ev in zip(cols, self.cur["files"], self.cur["events"]):
                if isinstance(col, bytes):
                    f.write(col[:(samples + 7) // 8])
                else:
                    ev[0].append(col[0] + self.cur["samples"] // col[4])
                    ev[1].append(col[1])
                    ev[2].append(col[2])
                    if ev[3] is None:
                        ev[3] = col[3]
            self.cur["samples"] += samples
            del self.pending[:nxt - self.at]
            self.at = nxt
            k = j + 1
        if final:
            self.pending = bytearray()

    def _open(self, position: int, seg=None) -> None:
        if self.cur is not None:
            return
        import os
        seg = seg or self.segs.get(position)
        if self.first_ns is None and seg is not None:
            self.first_ns = seg.start_ns
        if self.parts and self.parts[-1] is not None and getattr(self, "_follows", False):
            tick0 = self.parts[-1].tick0 + self._last_samples
        elif seg is not None and self.first_ns is not None:
            tick0 = round(Fraction(seg.start_ns - self.first_ns, 10**9) * self.rate)
        else:                                    # format 0 without the segment's record: from the bytes
            tick0 = (self.parts[-1].tick0 + self._last_samples if self.parts else 0) + \
                (position - (self._last_end or 0)) * 8 // self.cfg.width
        k = len(self.parts)
        files = [open(os.path.join(self.dir, f"p{k}-{j}.bits"), "wb") for j in range(len(self.names))]
        self.cur = {"tick0": tick0, "meta": _seg_meta(seg) if seg is not None else {}, "samples": 0, "files": files,
                    "events": [[[], [], [], None] for _ in self.names]}
        self._follows = False

    _last_samples, _last_end, _follows = 0, None, False

    def _close(self, follows: bool = False) -> None:
        if self.cur is None:
            return
        import os
        cur, n = self.cur, self.cur["samples"]
        chans = []
        for role, (name, ch, f, ev) in enumerate(zip(self.names, self.ids, cur["files"], cur["events"])):
            f.close()
            d = self.cfg.event_d.get(role) if self.blocks else None
            if d is not None:
                os.unlink(f.name)
                import numpy as np
                cat = [np.concatenate(x) if x else np.zeros(0, np.int64) for x in ev[:3]]
                chans.append(_events_np(name, *cat, ev[3] or 0, -(-n // d), d, pin=ch))
            else:
                os.truncate(f.name, (n + 7) // 8)
                chans.append(fileformat.Channel(name, fileformat.OnDisk(f.name), n, 1, 0, {"pin": ch}))
        if n:
            self.parts.append(fileformat.Part(chans, dict(cur["meta"]), int(cur["tick0"])))
        self._last_samples, self._last_end = n, self.at
        self.cur = None
        self._follows = follows
        for x in self.lost:                      # how long the loss before this part was, when the times say
            if x.get("_next") is not None and x["after_part"] == len(self.parts) - 2 and len(self.parts) >= 2:
                gap = self.parts[-1].tick0 - self.parts[-2].tick0 - self.parts[-2].channels[0].end
                if gap > 0:
                    x["ticks"] = int(gap)
                x.pop("_next")

    @property
    def done_lost(self):
        return [{k: v for k, v in x.items() if k != "_next"} for x in self.lost]


DROP_COUNTS = ("no_stage_bytes", "unqueued_bytes", "dma_overruns", "queue_overflows")


def _drops_line(drops: dict | None) -> str:
    """The reference firmware's capture-drops counters that are not 0, in one line."""
    if not drops:
        return ""
    return ", ".join(f"{k} {drops[k]}" for k in DROP_COUNTS if drops.get(k))


def _stream_parts(cap, cfg, got, names, ids, req: Request) -> tuple[list, list]:
    """What a stream collected -> parts on one timeline and the losses (wireskein-format §3.4). Segments run in
    order; a skipped position is a loss, and a part ends there (with on_loss "stop", the capture too). Each segment
    is read as a one-shot one is and joined to the part; tick0 within a part is exact (samples), across a loss from
    the segments' start_ns. A format 1 segment whose samples are not a multiple of every D starts a new part
    without a loss (the next segment's ticks restart, capture §1.3)."""
    import numpy as np
    if not got.data or not got.segments:
        raise RuntimeError("the stream brought no data" + (" (the probe dropped it all)" if got.gaps else ""))
    runs, pos, at = [], got.start, 0            # (stream position, data index, length) of each gapless stretch
    marks = sorted(got.gaps) + [(len(got.data), 0)]
    for index, skipped in marks:
        runs.append((pos, at, index - at))
        pos += index - at + skipped
        at = index
    segs = sorted(got.segments.values(), key=lambda x: x.position)
    rate = Fraction(cfg.rate)
    first_ns = segs[0].start_ns
    ds = [d for d in getattr(cfg, "event_d", {}).values()]
    parts, lost = [], []
    if got.start != 0:                           # positions count from the generation's start: the first is gone
        if req.on_loss == "stop":
            raise RuntimeError(f"the probe dropped the stream's first {got.start} bytes before any arrived: nothing "
                               f"to keep (a lower --rate or fewer channels, or --on-loss continue)")
        lost.append({"after_part": -1, "reason": f"the probe dropped the first {got.start} bytes (no room to send)"})
    for r, (p0, i0, n) in enumerate(runs):
        if r and req.on_loss == "stop":
            lost.append({"after_part": len(parts) - 1, "reason": "the probe dropped a segment (no room to send)",
                         "stopped": True})
            break
        if r:
            lost.append({"after_part": len(parts) - 1, "reason": "the probe dropped a segment (no room to send)"})
        inside = [x for x in segs if p0 <= x.position < p0 + n]
        run = memoryview(got.data)[i0:i0 + n]   # no copy: a 30 s stream at 40 MB/s is 1.2 GB
        if not ds and cfg.width % 8 == 0:        # format 0, whole bytes a sample: the stretch at once (no padding)
            samples = min(sum(x.samples for x in inside), len(run) * 8 // cfg.width)
            parts.append(_part(_samples_at_once(run, names, ids, cfg, samples), _seg_meta(inside[0]),
                               round(Fraction(inside[0].start_ns - first_ns, 10**9) * rate)))
            continue
        acc, acc_samples, acc_meta, tick0 = None, 0, None, None
        for k, x in enumerate(inside):
            end_ = inside[k + 1].position if k + 1 < len(inside) else p0 + n
            data = bytes(run[x.position - p0:end_ - p0])
            samples = x.samples
            if not ds:
                samples = min(samples, len(data) * 8 // cfg.width)
                chans = fileformat.from_interleaved(data, names, cfg.width, cfg.positions, samples)
            else:
                chans = _from_blocks(cap, data, names, samples, cfg)
            restart = acc is not None and (acc_samples % 8 or any(acc_samples % d for d in ds))
            if acc is None or restart:           # a grid restart (format 1) is not a loss: the next part follows on
                if acc is not None:
                    parts.append(_part(_built(acc, acc_samples, ids), acc_meta, tick0))
                    tick0 += acc_samples
                else:
                    tick0 = round(Fraction(x.start_ns - first_ns, 10**9) * rate)
                acc, acc_samples, acc_meta = [_Acc(c) for c in chans], 0, _seg_meta(x)
            for a_, c in zip(acc, chans):
                a_.add(c, acc_samples)
            acc_samples += samples
        if acc is not None:
            parts.append(_part(_built(acc, acc_samples, ids), acc_meta, tick0))
    for x in lost:                               # how long, when the segments' times say
        k = x["after_part"]
        if 0 <= k < len(parts) - 1:
            gap = parts[k + 1].tick0 - parts[k].tick0 - parts[k].channels[0].end
            if gap > 0:
                x["ticks"] = int(gap)
    return parts, lost


def _part(chans, meta, tick0):
    return fileformat.Part(chans, dict(meta), int(tick0))


def _seg_meta(x) -> dict:
    return {"start_ns": x.start_ns, "start_uncertainty_ns": x.start_uncertainty_ns}


AT_ONCE = 1 << 22         # samples unpacked at a time (a sample's byte per channel would be GBs for a long stream)
WORDS_AT_ONCE = 1 << 15   # 64-bit words of samples worked on at a time (256 KiB: stays in the cache)


def _samples_at_once(run, names, ids, cfg, samples: int) -> list:
    """Format 0 with whole bytes a sample: channel k is bit pos[k] of each little-endian sample, packed a chunk of
    AT_ONCE samples at a time (a multiple of 8, so the chunks' bytes join as they are)."""
    import numpy as np
    b = cfg.width // 8
    if b == 1:                   # 8 samples a 64-bit word: each channel's bits gathered into a byte by one multiply
        whole = samples // 8 * 8
        words = np.frombuffer(run, "<u8", whole // 8)
        planes = np.empty((len(names), whole // 8), np.uint8)
        lo, mul, sh = np.uint64(0x0101010101010101), np.uint64(0x0102040810204080), np.uint64(56)
        tmp = np.empty(WORDS_AT_ONCE, np.uint64)
        for at in range(0, len(words), WORDS_AT_ONCE):       # a cache-sized run of words at a time
            w = words[at:at + WORDS_AT_ONCE]
            t = tmp[:len(w)]
            for k, pos in enumerate(cfg.positions):
                np.right_shift(w, np.uint64(pos), out=t)
                np.bitwise_and(t, lo, out=t)
                np.multiply(t, mul, out=t)
                np.right_shift(t, sh, out=t)
                planes[k, at:at + len(w)] = t
        out = []
        for k, (name, ch, pos) in enumerate(zip(names, ids, cfg.positions)):
            bits = planes[k].tobytes()
            if whole < samples:              # the last few samples
                tail = np.frombuffer(run, np.uint8, samples - whole, whole)
                bits += np.packbits((tail >> pos) & 1, bitorder="little").tobytes()
            out.append(fileformat.Channel(name, bits, samples, 1, 0, {"pin": ch}))
        return out
    a = np.frombuffer(run, np.uint8, samples * b).reshape(samples, b)
    pos = np.array(cfg.positions)
    packed = [bytearray() for _ in names]
    for at in range(0, samples, AT_ONCE):
        bits = np.unpackbits(a[at:at + AT_ONCE], axis=1, bitorder="little")   # (samples, w): bit j of each sample
        planes = np.packbits(np.ascontiguousarray(bits[:, pos].T), axis=1, bitorder="little")   # (channels, bytes)
        for k in range(len(names)):
            packed[k] += planes[k].tobytes()
    return [fileformat.Channel(name, bytes(bits), samples, 1, 0, {"pin": ch})
            for name, ch, bits in zip(names, ids, packed)]


class _Acc:
    """One channel of a part being joined from its segments (bits: whole bytes, the part's samples so far a multiple
    of 8; events: quanta offset by the samples so far, a multiple of D). A segment that follows on carries the level
    before it as its start (§1.3), so its changes join the part's as they are."""
    def __init__(self, c):
        self.first, self.pieces = c, []
        self.start = getattr(c, "start_level", None)

    def add(self, c, offset: int) -> None:
        if isinstance(c, fileformat.EventsChannel):
            ks, lv, ct = c.decode()
            q = offset // c.step
            self.pieces.append(([q + k for k in ks], lv, ct, c.n))
        else:
            self.pieces.append((c.bits, c.n))


def _built(acc: list, samples: int, ids) -> list:
    out = []
    for a, ch in zip(acc, ids):
        c = a.first
        if isinstance(c, fileformat.EventsChannel):
            ks, lv, ct = [], [], []
            for k_, l_, c_, _ in a.pieces:
                ks += k_
                lv += l_
                ct += c_
            out.append(fileformat.events(c.name, ks, lv, ct, -(-samples // c.step), c.step, 0, a.start, pin=ch))
        else:
            out.append(fileformat.Channel(c.name, b"".join(b for b, _ in a.pieces)[:(samples + 7) // 8], samples, 1, 0,
                                          {"pin": ch}))
    return out


def _budget(oc, cfg, got: int, samples: int, channels: int) -> dict:
    """meta.probe.data: the bytes the probe sent for the segment, the most §1.3's formula allows for it, and what
    format 0 would have taken (1 bit a channel a sample, at least)."""
    from oep_client import capture_blocks
    L = cfg.block
    worst_block = capture_blocks.worst_bytes(len(cfg.raw_roles), list(cfg.event_d.values()), L)
    return {"format": int(cfg.format), "block": L, "bytes": got, "worst_bytes": -(-samples // L) * worst_block,
            "format0_bytes": -(-samples * channels // 8)}


def _segment(track):
    segments = track.segments()
    if not segments:
        raise RuntimeError("the probe finished without a segment")
    return segments[0]


def _times(meta: dict, seg) -> None:
    meta["start_ns"] = seg.start_ns
    meta["start_uncertainty_ns"] = seg.start_uncertainty_ns
