"""oep:<target> - an OEP probe through oep-client-python (one shot).

Logic channels go to oep.fixture.logic, analog ones to oep.fixture.analog;
with both, oep.fixture.capture-group starts them together (the trigger, if
any, on a logic channel). Channel ids are the probe's channel numbers; a
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
from pathlib import Path
from fractions import Fraction

from .. import fileformat
from . import Request, Result

TRIGGER = {"high": (1, 1), "low": (1, 0), "rise": (2, 0), "fall": (2, 1), "both": (2, 2)}   # (type, value), oep-if-capture
TAG_FRONTEND = 0x46          # analog describe: frontend, range_min_mv, range_max_mv, attenuation_mdb
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
    try:
        return _capture(link, core, oc, oh, message, target, req, ids, aids, fast)
    except oh.OepError as e:        # the probe refused or failed: say what, not where
        hint = ""
        if "(storage)" in str(e):     # capture §2.2: data lost inside a segment stops the track with error 2
            hint = " - the probe lost data inside a segment: try a lower --rate or fewer channels"
        raise RuntimeError(f"probe {target}: {e}{hint}") from e


def _what(tag, oc) -> str:
    """A configure item for people. The probe names the TLV it refused with its
    tag as sent (bit 7, critical, for multirate)."""
    base = tag & 0x7F if isinstance(tag, int) else tag
    return {oc.TRIGGER: "a trigger", oc.PRETRIGGER: "a pretrigger",
            oc.mr.TAG: "these channel reductions (/D, any, latch: none offered, or this combination not kept even at "
                    "its lowest rate)"}.get(
        base, f"configure item 0x{base:02x}" if isinstance(base, int) else f"configure item {tag}")


def _waiting(wait, cap, req):
    """wait(), with a timeout that names a trigger which never came (the probe still waiting, state 2)."""
    try:
        return wait()
    except TimeoutError:
        if req.trigger and cap is not None:
            try:
                waiting = cap.status()[0] == oc_state("waiting")
            except Exception:           # the timeout is the news; a failed status read must not hide it
                waiting = False
            if waiting:
                raise TimeoutError(f"no trigger ({req.trigger[0]}:{req.trigger[1]}) within {req.timeout:g} s "
                                   f"(--timeout)") from None
        raise


def oc_state(name: str) -> int:
    from oep_client.capture import STATE
    return STATE[name]


def _configure(track, oc, oh, target: str, what: str, **kw):
    """configure; a refusal (unsupported, naming the TLV) in one line. Every probe knows the configure TLVs of OEP
    v1 (capture §3.3), so one it cannot honour is refused, not ignored; oep-client sends multirate critical itself."""
    try:
        return track.configure(mode=oc.ONE_SHOT, **kw)
    except oh.Unsupported as e:
        raise RuntimeError(f"probe {target} cannot capture {what} with {_what(e.tag, oc)}") from e
    except ValueError as e:              # oep-client checks multirate against the probe's describe before sending
        if "multirate" not in str(e):
            raise
        raise RuntimeError(f"probe {target}: {e}") from e


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
    out = {}
    for tag, v in _describe(core, hst, fn):
        if tag == TAG_FRONTEND and len(v) >= 13:
            fe, lo, hi, att = struct.unpack_from("<BiiI", v)
            out[fe] = {"frontend": fe, "range_min_mv": lo, "range_max_mv": hi,
                       **({"attenuation_db": att / 1000} if att != 0xFFFFFFFF else {})}
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
    anames = [n for n, _, _ in req.analog]
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
            if cap:
                trigger = None
                if req.trigger:
                    kind, value = TRIGGER[req.trigger[1]]
                    trigger = (kind, names.index(req.trigger[0]), value)
                cfg = _configure(cap, oc, oh, target, "logic", rate=req.rate, samples=req.samples, trigger=trigger,
                                 pretrigger=req.pretrigger,
                                 **({"multirate": _multirate(req, names)} if req.reduce else {}))
            if an:
                arate = req.analog_rate or req.rate
                asamples = req.analog_samples
                fes = {k: fe for k, (_, _, fe) in enumerate(req.analog) if fe is not None}
                if asamples is None and cap:
                    # as long as the logic capture, at the rate the probe will really use
                    # (channels share the ADC, so it may answer less than asked)
                    real = Fraction(arate)
                    try:
                        q = an.configure(mode=oc.ONE_SHOT, rate=arate, samples=1, query=True,
                                         **({"frontends": fes} if fes else {}))
                        real = Fraction(q.rate) if q.rate else real
                    except oh.Rejected as e:       # query is optional (OEP v1): absent = unknown_operation
                        if e.result.detail != message.UNKNOWN_OPERATION:
                            raise
                    asamples = max(1, round(Fraction(req.samples) / Fraction(cfg.rate) * real))
                elif asamples is None:
                    asamples = req.samples
                acfg = _configure(an, oc, oh, target, "analog", rate=arate, samples=asamples,
                                  **({"frontends": fes} if fes else {}))
            group = None
            if cap and an:
                group = oc.CaptureGroup(hst)
                group.bind([cap, an], trigger=cap if req.trigger else None)
                try:
                    _, group_start = group.start([cap, an])
                    st = _waiting(lambda: group.wait(req.timeout), cap, req)
                    seg = _segment(cap)
                    aseg = _segment(an)
                finally:
                    group.bind([])
                probe["group_start_ns"] = group_start
                if st.trigger_ns is not None:
                    probe["trigger_ns"] = st.trigger_ns
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
        if cfg.block is not None:
            chans = _reduced(cap.decode_multirate(data, seg.samples), req, names)
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


def _multirate(req: Request, names: list[str]) -> list:
    """Request.reduce as oep-client's Multirate list (oep-if-capture §5.2): the role is the channel's place."""
    from oep_client import multirate as mr
    policy = {"sample": mr.SAMPLE, "any": mr.ANY_ACTIVE, "latch": mr.EDGE_LATCH}
    return [mr.Multirate(names.index(n), policy[p], d, param) for n, (p, d, param) in req.reduce.items()]


def _reduced(dec, req: Request, names: list[str]) -> list:
    """A multirate segment decoded by oep-client -> the file's channels, in the order asked: the D = 1 channels as
    bits, "sample" as bits every d ticks from its phase, "any" / "latch" as interval channels (the tick is the base
    sample)."""
    d1 = iter(dec.d1)
    out = []
    for role, name in enumerate(names):
        policy, d, param = req.reduce.get(name, ("sample", 1, 0))
        if policy == "sample" and d == 1:
            lv = next(d1)
            out.append(fileformat.Channel(name, fileformat.pack(bytes(lv)), len(lv), 1, 0))
        elif policy == "sample":
            v = dec.reduced[role]
            out.append(fileformat.Channel(name, fileformat.pack(bytes(v)), len(v), d, param))
        else:
            out.append(fileformat.interval(name, dec.reduced[role], d, 0, f"interval-{policy}", param))
    return out


def _segment(track):
    segments = track.segments()
    if not segments:
        raise RuntimeError("the probe finished without a segment")
    return segments[0]


def _times(meta: dict, seg) -> None:
    meta["start_ns"] = seg.start_ns
    meta["start_uncertainty_ns"] = seg.start_uncertainty_ns
