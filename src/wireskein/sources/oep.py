"""oep:<target> - an OEP probe through oep-client-python (one shot).

Logic channels go to oep.fixture.logic (oep.fixture.capture before OEP v1), analog ones to oep.fixture.analog;
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
# UART link rates tried by default (port_speed): fastest first. oep-client 0.0.26 refuses a rate that breaks with
# both directions busy and drops back to the boot speed on repeated broken frames, so trying never ends slower
# than the boot speed; `oep:PORT?fast=0` keeps the boot speed, `?fast=RATE,RATE` tries those.
FAST = [1_500_000, 921_600, 500_000]
PER_SESSION = 2          # candidates tried per open: each costs up to ~1 s, plus a wait when it fails (core §3.5)
FLOWS = [("in", 0)]      # a capture reads back: verify probe -> host only, at the link's largest in-flight n


def _candidates(port: str, unit_id: str, record=None) -> tuple[list[int], bool, list[int]]:
    """The default rates to try on this port and probe: what passed there first, then the list (FAST) without what
    failed lately, at most PER_SESSION. The record is oep-client's (speed_record: per port path and unit_id,
    30 days), which raise_speed(record=True) writes back. When the record has every rate failed, the slowest is
    tried again anyway (one short trial): a rate can fail right after a faster one broke down and hold on its own
    (V003 jig's CH340: 500000 marked failed that way, 43 KB/s when tried alone), and giving up would keep the port
    at the boot speed for 30 days. -> (rates, whether the record shaped them, the rates it left out)."""
    if record is None:
        from oep_client.speed_record import SpeedRecord
        record = SpeedRecord()
    passed, failed = record.lookup(port, unit_id)
    order = [r for r in passed if r not in failed] + [r for r in FAST if r not in passed and r not in failed]
    if not order:
        order = [FAST[-1]]                             # every rate failed: the slowest once more (see _capture)
    return order[:PER_SESSION], bool(passed or failed), [r for r in FAST if r in failed and r not in order]


def _takes(link, name: str) -> bool:
    """Whether the client's raise_speed takes this argument."""
    import inspect
    try:
        return name in inspect.signature(link.raise_speed).parameters
    except (AttributeError, TypeError, ValueError):
        return False


def _raise_default(link, hst, port: str | None, unit_id: str, record=None) -> tuple[list[int], bool, list[int]]:
    """The default raise: candidates from the client's record (_candidates), then raise_speed(record=True). When the
    record has every rate failed, the slowest is retried without the record (with it the client would skip it as
    recorded failed) and a pass is written back, so the next capture starts from it.
    -> (rates asked, whether the record shaped them, the rates it left out)."""
    if not port:
        return [], False, []
    if _takes(link, "max_tries"):
        # a client that does it all (oep-client-python with max_tries): it orders and skips by its record, tries at
        # most PER_SESSION, retries the slowest when all failed, and steps down to a slower candidate in use
        link.raise_speed(hst, list(FAST), record=True, flows=FLOWS, max_tries=PER_SESSION)
        rep = getattr(getattr(hst, "link", None), "speed", None)
        return list(FAST), bool(rep is not None and (rep.skipped or getattr(rep, "retried", False))), []
    if record is None:
        from oep_client.speed_record import SpeedRecord
        record = SpeedRecord()
    rates, remembered, left_out = _candidates(port, unit_id, record)
    retry = bool(rates) and rates[0] in record.lookup(port, unit_id)[1]
    rates = _raise(link, hst, rates, False, record=not retry)
    rep = getattr(getattr(hst, "link", None), "speed", None)
    if retry and rep is not None and rep.chosen:
        record.note(port, unit_id, rep.chosen, passed=True)
    return rates, remembered, left_out


def capture(target: str, req: Request) -> Result:
    try:
        from oep_client import capture as oc
        from oep_client import core, link
        from oep_client import host as oh
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
    asked = "fast" in opts
    try:
        ids = [int(cid) for _, cid in req.channels]
        aids = [int(cid) for _, cid, _ in req.analog]
    except ValueError:
        raise ValueError("oep channel ids are the probe's channel numbers: "
                         f"{[c for _, c in req.channels] + [c for _, c, _ in req.analog]}") from None
    try:
        return _capture(link, core, oc, oh, target, req, ids, aids, fast, asked)
    except oh.OepError as e:        # the probe refused or failed: say what, not where
        raise RuntimeError(f"probe {target}: {e}") from e


def _what(tag, oc) -> str:
    """A configure item for people. The probe names the TLV it refused with its
    tag as sent, bit 7 (critical) included."""
    base = tag & 0x7F if isinstance(tag, int) else tag
    return {getattr(oc, "TRIGGER", None): "a trigger", getattr(oc, "PRETRIGGER", None): "a pretrigger"}.get(
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
    """configure with the mode, the rate, and the trigger / pretrigger / frontend asked for, sent critical (a probe
    without them would otherwise ignore them: another mode or rate, or a start at once)."""
    asked = {oc.TRIGGER} if kw.get("trigger") else set()
    if kw.get("pretrigger") is not None:
        asked.add(oc.PRETRIGGER)
    # OEP v1 (core §2.3, capture §3.3): mode, rate and frontend are sent critical too - a probe that cannot honour
    # one must refuse, not quietly run another mode or rate
    for name, given in (("MODE", True), ("RATE", "rate" in kw), ("FRONTEND", bool(kw.get("frontends")))):
        tag = getattr(oc, name, None)
        if tag is not None and given:
            asked.add(tag)
    try:
        cfg = track.configure(mode=oc.ONE_SHOT, critical=asked, **kw)
    except oh.Unsupported as e:
        raise RuntimeError(f"probe {target} cannot capture {what} with {_what(e.tag, oc)}") from e
    ignored = asked & set(getattr(cfg, "ignored", None) or [])     # before OEP v1's rule review: no ignored TLV
    if ignored:
        raise RuntimeError(f"probe {target} ignored {', '.join(_what(t, oc) for t in sorted(ignored))}")
    return cfg


def _clock(hst):
    """The probe's clock against this host's (core §7.7): the reading with the shortest round trip of a few, or None
    when the client or the probe has no clock op."""
    best = getattr(hst, "clock_best", None)
    if best is None:
        return None
    try:
        return best(4)
    except Exception:                  # an older probe (unknown_operation) or a link hiccup: the capture goes on
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


def _unknown_op() -> int | None:
    """The reject detail of an op the probe does not offer (an optional op such as query)."""
    try:
        from oep_client import message
        return message.UNKNOWN_OPERATION
    except (ImportError, AttributeError):
        return None


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
    boot = getattr(opened, "boot_id", None)
    if boot is not None:
        info["boot_id"] = boot          # start_ns of captures with the same boot_id share one clock
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
    """fast=0: [] (the boot speed); fast=1 / absent: None (the remembered default, _candidates);
    fast=1500000,921600: those, in that order, whatever was remembered."""
    if text.lower() in ("", "0", "no", "off", "false"):
        return []
    if text.lower() in ("1", "yes", "on", "true"):
        return None
    try:
        return [int(r) for r in text.split(",")]
    except ValueError:
        raise ValueError(f"fast={text}: 0, 1, or link rates (fast=1500000,921600)") from None


def _raise(link, hst, rates: list[int], asked: bool = True, record: bool = True) -> list[int]:
    """Raise the link of an open host to the first of `rates` that holds, verifying the read-back direction (FLOWS);
    the client keeps the boot speed, or goes back to it, whenever the probe, the adapter or the line cannot.
    record: let the client's record skip what failed and keep what happened. -> the rates asked of the client
    ([] when none were, or the client cannot and they were only the default)."""
    raise_speed = getattr(link, "raise_speed", None)
    if rates and raise_speed is None:
        if asked:
            raise ValueError("fast=...: this oep-client-python cannot raise the link speed (0.0.27 or later can)")
        return []                                      # the default, with a client that cannot: the boot speed
    if rates:
        raise_speed(hst, rates, flows=FLOWS, record=record)
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
                                     "kb_s": round(f.kb_s, 1), "passed": f.passed} for f in getattr(t, "flows", [])]}
                         for t in rep.trials]
        if getattr(rep, "skipped", None):
            out["skipped"] = list(rep.skipped)            # the record says these failed on this port and probe
        if getattr(rep, "retried", None):
            out["retried"] = rep.retried                  # every rate recorded failed: the slowest tried once anyway
        if getattr(rep, "capped", None):
            out["capped"] = list(rep.capped)              # left out by the limit of tries per capture
        downs = getattr(rep, "step_downs", None) or []
        if downs:                                         # in use (or in probation): from a rate to a slower one
            out["step_downs"] = [{"from": d.rate, "to": getattr(d, "to", None), "why": d.why,
                                  "probation": bool(getattr(d, "probation", False))} for d in downs]
        if getattr(rep, "stepped_down", False) or rep.lost:
            out["stepped_down"] = getattr(rep, "down_why", "") or "lost"   # went down while in use
    return out


def _capture(link, core, oc, oh, target: str, req: Request, ids: list[int], aids: list[int],
             fast: list[int] | None = None, asked: bool = False) -> Result:
    import time
    names = [n for n, _ in req.channels]
    anames = [n for n, _, _ in req.analog]
    t_open = time.monotonic()
    remembered = False
    left_out: list[int] = []
    hst = link.open_host(target)
    try:
        opened = core.take(hst, 30_000, owner="wireskein capture")
        probe = _probe_info(core, hst, opened)
        # the link's memory is per port and probe (host development guide §7.5): the adapter belongs to the port,
        # the rest of the link to the probe, so either changing starts over
        record = fast is None                          # the default: the client's record of this port and probe
        if fast is None:
            port = getattr(getattr(hst, "link", None), "port_path", None)
            fast, remembered, left_out = _raise_default(link, hst, port, probe.get("unit_id", "?"))
        else:
            fast = _raise(link, hst, fast, asked and bool(fast), False)
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
                                 pretrigger=req.pretrigger)
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
                        if getattr(getattr(e, "result", None), "detail", None) != _unknown_op():
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
                if left_out and not probe["link"].get("skipped"):
                    probe["link"]["skipped"] = left_out      # left out before asking the client: say so all the same
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
        if seg.trigger_index is not None:
            meta["trigger_index"] = seg.trigger_index
        if getattr(cfg, "jitter_ns", 0):              # gone from OEP v1's configure answer: recorded while given
            meta["jitter_ns"] = cfg.jitter_ns
        tick = Fraction(cfg.rate)
        chans = fileformat.from_interleaved(data, names, cfg.width, cfg.positions, seg.samples)
        for c, ch in zip(chans, ids):
            c.acquisition = {"pin": ch, **_accuracy(cfg)}
        t_ref = getattr(seg, "start_ns", None)
    else:
        tick = Fraction(acfg.rate)
        chans = []
        _times(meta, aseg)
        t_ref = getattr(aseg, "start_ns", None)
    gens = {k: getattr(t, "generation", None) for k, t in (("logic", cap), ("analog", an)) if t is not None}
    gens = {k: g for k, g in gens.items() if g is not None}          # OEP v1 clients: which start this was
    if gens:
        probe["generation"] = gens
    if an:
        if calib is not None and (calib.factory or calib.vrefint):
            probe["calibration"] = [{"frontend": fe, "scheme": scheme, "raw": bytes(raw).hex()}
                                    for fe, scheme, raw in calib.factory]
        a_ns = getattr(aseg, "start_ns", None)
        for k, ((name, _, _), ch) in enumerate(zip(req.analog, aids)):
            values = an.values(adata, k, aseg.samples)
            t0 = Fraction(0)
            if a_ns is not None and t_ref is not None:
                t0 = Fraction(a_ns - t_ref) * tick / 10**9
            t0 += Fraction(acfg.skew_ns.get(k, 0)) * tick / 10**9
            acq = {"pin": ch, **_accuracy(acfg)}
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
            nominal = getattr(calib, "vrefint_nominal_mv", None)     # OEP v1 clients
            if nominal is not None:
                acq["vrefint_nominal_mv"] = nominal
            if getattr(aseg, "start_uncertainty_ns", None) is not None:
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


def _segment(track):
    segments = track.segments()
    if not segments:
        raise RuntimeError("the probe finished without a segment")
    return segments[0]


def _times(meta: dict, seg) -> None:
    if getattr(seg, "start_ns", None) is not None:      # oep-if-capture with ns times (oep-client 0.0.10)
        meta["start_ns"] = seg.start_ns
        if getattr(seg, "start_uncertainty_ns", None) is not None:
            meta["start_uncertainty_ns"] = seg.start_uncertainty_ns
    elif getattr(seg, "start_us", None) is not None:  # an older probe's us time: the file keeps ns (spec §3.3)
        meta["start_ns"] = seg.start_us * 1000


def _accuracy(cfg) -> dict:
    out = {}
    if getattr(cfg, "rate_measured", False):
        out["rate_measured"] = True
    if getattr(cfg, "rate_ppm", 0):
        out["rate_ppm"] = cfg.rate_ppm
    return out
