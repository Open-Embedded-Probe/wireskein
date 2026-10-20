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
    if req.trigger and aids and ids and not _group_trigger(oc):
        raise ValueError("this oep-client-python starts logic and analog together only at once (capture-group, "
                         "capture §4: the linked trigger is out of the current OEP v1 draft): leave out --trigger, or "
                         "capture the logic alone")
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


def _group_trigger(oc) -> bool:
    """Whether this client's capture-group takes a trigger track (bind(..., trigger=))."""
    import inspect
    return "trigger" in inspect.signature(oc.CaptureGroup.bind).parameters


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


def _configure(track, oc, oh, target: str, what: str, mode=None, **kw):
    """configure; a refusal (unsupported, naming the TLV) in one line. Every probe knows the configure TLVs of OEP
    v1 (capture §3.3), so one it cannot honour is refused, not ignored."""
    try:
        return track.configure(mode=oc.ONE_SHOT if mode is None else mode, **kw)
    except oh.Unsupported as e:
        raise RuntimeError(f"probe {target} cannot capture {what} with {_what(e.tag, oc)}") from e


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
            if cap:
                trigger = None
                if req.trigger:
                    kind, value = TRIGGER[req.trigger[1]]
                    trigger = (kind, names.index(req.trigger[0]), value)
                blocks = {}
                if req.resolution:          # format 1: these lines as their changes, the others raw (§1.3)
                    blocks = dict(data_format=oc.DATA_FORMAT["logic_blocks"], roles=list(range(len(ids))),
                                  events={names.index(n): d for n, d in req.resolution.items()})
                if req.arm == "now" and trigger:     # arm 0 (§3.3): optional, so it goes critical; arm 1 is the default
                    if not hasattr(oc, "TriggerOpts"):
                        raise ValueError("--arm now needs an oep-client-python with trigger_opts")
                    blocks["trigger_opts"] = oc.TriggerOpts(arm=0)
                if req.mode == "stream":
                    cfg = _configure(cap, oc, oh, target, "logic", oc.STREAMING, rate=req.rate, **blocks)
                    return _stream_result(cap, hst, cfg, req, names, ids, probe, fast, open_s, clock_before, oc)
                cfg = _configure(cap, oc, oh, target, "logic", rate=req.rate, samples=req.samples, trigger=trigger,
                                 pretrigger=req.pretrigger, **blocks)
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
                if req.trigger:
                    group.bind([cap, an], trigger=cap)
                else:
                    group.bind([cap, an])
                try:
                    started = group.start([cap, an])
                    group_start = started[1] if isinstance(started, tuple) else started
                    st = _waiting(lambda: group.wait(req.timeout), cap, req)
                    seg = _segment(cap)
                    aseg = _segment(an)
                finally:
                    group.bind([])
                probe["group_start_ns"] = group_start
                if getattr(st, "trigger_ns", None) is not None:
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
        if getattr(cfg, "event_d", None):
            chans = _from_blocks(cap.blocks(data, seg.samples), names, seg.samples, cfg)
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


def _from_blocks(blocks, names: list[str], samples: int, cfg) -> list:
    """A format 1 segment's blocks (oep-client's capture_blocks) -> the file's channels, in the order asked: a raw
    channel as bits (L is a multiple of 8, so the blocks' bytes join as they are), an events channel as `events` at
    its D (each block's ticks as quanta of the segment: L is a multiple of D). A block the probe carried raw
    (fallback) is counted here from its samples and the block's start level, the same way the probe counts (§1.3)."""
    import numpy as np
    out = []
    for role, name in enumerate(names):
        d = cfg.event_d.get(role)
        if d is None:                   # packed already (sample i at bit i % 8), the last block's tail 0 (§1.3)
            bits = b"".join(b.raw[role] for b in blocks)[:(samples + 7) // 8]
            out.append(fileformat.Channel(name, bits, samples))
            continue
        quanta, levels, counts, start, level = [], [], [], None, None
        for b in blocks:
            q0 = b.first // d
            if role in b.fallback:
                lv = np.unpackbits(np.frombuffer(b.raw[role], np.uint8), bitorder="little")[:b.samples].astype(np.int8)
                if start is None:
                    start = level = int(b.start_levels[role])   # the level before the block (§1.3)
                prev = np.concatenate(([level], lv[:-1]))
                change = (lv != prev).astype(np.int64)
                per = np.add.reduceat(change, np.arange(0, b.samples, d))
                for t in np.flatnonzero(per):
                    quanta.append(q0 + int(t))
                    levels.append(int(lv[min((t + 1) * d, b.samples) - 1]))
                    counts.append(int(per[t]))
                level = int(lv[-1])
            else:
                e = b.events[role]
                if start is None:
                    start = level = int(e.start_level)
                quanta += [q0 + int(t) for t in e.ticks]
                levels += [int(x) for x in e.levels]
                counts += [int(x) for x in e.counts]
                if len(e.levels):
                    level = int(e.levels[-1])
        out.append(fileformat.events(name, quanta, levels, counts, -(-samples // d), d, 0, start or 0))
    return out


STREAM_POLL_S = 0.2       # how often a stream looks for a loss while collecting


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
    cap.subscribe()
    cap.start()
    t_read = time.monotonic()
    deadline, got = t_read + req.duration, None
    while True:                                  # stream() renews the lease across calls on the same Received
        now = time.monotonic()
        if now >= deadline:
            break
        got = cap.stream(link, seconds=min(STREAM_POLL_S, deadline - now), into=got)
        if req.on_loss == "stop" and (got.gaps or (got.start or 0) > 0):
            break
    cap.stop()
    got = cap.finish(link, got or oc.Received())
    read_s = time.monotonic() - t_read
    st = cap.status()
    drops = oc.CaptureDrops.of(st) if hasattr(oc, "CaptureDrops") else None   # the reference firmware's own
    if drops is not None:
        import dataclasses
        probe["drops"] = dataclasses.asdict(drops)
    probe["link"] = _link_info(hst, fast, open_s, len(got.data), read_s)
    clock = _clock_info(clock_before, _clock(hst))
    if clock:
        probe["clock"] = clock
    if cap.generation is not None:
        probe["generation"] = {"logic": cap.generation}
    meta = {"probe_channels": dict(zip(names, ids)), "probe": probe,
            "stream": {"duration_s": req.duration, "on_loss": req.on_loss, "frames": got.frames,
                       "bytes": len(got.data), "read_s": round(read_s, 3)}}
    return _Streamed(cap, cfg, got, st, names, ids, req, meta, oc)


class _Streamed:
    """A stream collected, to be put together once the session is closed (decoding takes time; the lease does not
    wait for it). cap.blocks only reads the configure answer kept in the client."""
    def __init__(self, cap, cfg, got, st, names, ids, req, meta, oc):
        self.cap, self.cfg, self.got, self.st, self.names, self.ids, self.req, self.meta, self.oc = (
            cap, cfg, got, st, names, ids, req, meta, oc)

    def result(self) -> Result:
        parts, lost = _stream_parts(self.cap, self.cfg, self.got, self.names, self.ids, self.req)
        st = self.st
        why = _drops_line(self.meta["probe"].get("drops"))
        if st.state == self.oc.STATE["error"]:
            lost.append({"after_part": len(parts) - 1, "reason": f"the probe stopped in error ({st.error_name})",
                         "stopped": True})
        elif st.dropped and not lost:
            lost.append({"after_part": len(parts) - 1, "reason": "the probe dropped data (status flags bit 0)"})
        if why and lost:                         # where the probe says the data went (its own diagnostics; the
            lost[-1]["reason"] += f" - the probe's counts since start: {why}"    # counters are totals: said once)
        return Result(Fraction(self.cfg.rate), parts[0].channels, self.meta, parts, lost, "timeline")


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
                chans = _from_blocks(cap.blocks(data, samples), names, samples, cfg)
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
