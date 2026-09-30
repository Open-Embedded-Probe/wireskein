"""oep:<target> - an OEP probe through oep-client-python (one shot).

Logic channels go to oep.fixture.capture, analog ones to oep.fixture.analog;
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
from fractions import Fraction

from .. import wsc
from . import Request, Result

TRIGGER = {"high": (1, 1), "low": (1, 0), "rise": (2, 0), "fall": (2, 1), "both": (2, 2)}   # (type, value), oep-if-capture
TAG_FRONTEND = 0x46          # analog describe: frontend, range_min_mv, range_max_mv, attenuation_mdb
TAG_CHIP, TAG_MODEL, TAG_FIRMWARE = 0x4C, 0x41, 0x40      # core describe


def capture(target: str, req: Request) -> Result:
    try:
        from oep_client import capture as oc
        from oep_client import core, link
        from oep_client import host as oh
    except ImportError as e:
        raise RuntimeError('the oep source needs oep-client-python: pip install "wireskein[oep]"') from e
    if not target:
        raise ValueError("oep:<target>: a serial port, tcp://HOST:PORT or usb[:VID:PID[:SERIAL]]")
    try:
        ids = [int(cid) for _, cid in req.channels]
        aids = [int(cid) for _, cid, _ in req.analog]
    except ValueError:
        raise ValueError("oep channel ids are the probe's channel numbers: "
                         f"{[c for _, c in req.channels] + [c for _, c, _ in req.analog]}") from None
    try:
        return _capture(link, core, oc, oh, target, req, ids, aids)
    except oh.OepError as e:        # the probe refused or failed: say what, not where
        raise RuntimeError(f"probe {target}: {e}") from e


def _what(tag, oc) -> str:
    """A configure item for people. The probe names the TLV it refused with its
    tag as sent, bit 7 (critical) included."""
    base = tag & 0x7F if isinstance(tag, int) else tag
    return {getattr(oc, "TRIGGER", None): "a trigger", getattr(oc, "PRETRIGGER", None): "a pretrigger"}.get(
        base, f"configure item 0x{base:02x}" if isinstance(base, int) else f"configure item {tag}")


def _configure(track, oc, oh, target: str, what: str, **kw):
    """configure with the trigger / pretrigger asked for sent critical (a probe
    without them would otherwise ignore them and start at once)."""
    asked = {oc.TRIGGER} if kw.get("trigger") else set()
    if kw.get("pretrigger") is not None:
        asked.add(oc.PRETRIGGER)
    try:
        cfg = track.configure(mode=oc.ONE_SHOT, critical=asked, **kw)
    except oh.Unsupported as e:
        raise RuntimeError(f"probe {target} cannot capture {what} with {_what(e.tag, oc)}") from e
    ignored = asked & set(cfg.ignored or [])
    if ignored:
        raise RuntimeError(f"probe {target} ignored {', '.join(_what(t, oc) for t in sorted(ignored))}")
    return cfg


def _describe(core, hst, fn: int) -> list[tuple[int, bytes]]:
    try:
        return core.describe(hst, fn)
    except Exception:       # noqa: BLE001 - what the probe tells about itself is a bonus
        return []


def _probe_info(core, hst, opened) -> dict:
    info = {}
    for tag, value in _describe(core, hst, 0):
        key = {TAG_CHIP: "chip", TAG_MODEL: "model", TAG_FIRMWARE: "firmware"}.get(tag)
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


def _capture(link, core, oc, oh, target: str, req: Request, ids: list[int], aids: list[int]) -> Result:
    names = [n for n, _ in req.channels]
    anames = [n for n, _, _ in req.analog]
    hst = link.open_host(target)
    try:
        opened = core.take(hst, 30_000, owner="wireskein capture")
        probe = _probe_info(core, hst, opened)
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
                    q = an.configure(mode=oc.ONE_SHOT, rate=arate, samples=1, query=True,
                                     **({"frontends": fes} if fes else {}))
                    real = Fraction(q.rate) if q.rate else Fraction(arate)
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
                    st = group.wait(req.timeout)
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
                segments = track.wait(req.timeout)
                if not segments:
                    raise RuntimeError("the probe finished without a segment")
                seg = segments[0] if cap else None
                aseg = segments[0] if an else None
            data = cap.read_segment(seg) if cap else None
            adata = an.read_segment(aseg) if an else None
            calib = an.calibration() if an else None
            frontends = _frontends(core, hst, an.fn) if an else {}
        finally:
            core.plan_release(hst, fns)
    finally:
        hst.end()

    meta = {}
    if cap:
        meta["probe_channels"] = dict(zip(names, ids))
        _times(meta, seg)
        if seg.slipped:
            meta["time_base_slipped"] = True
        if seg.trigger_index is not None:
            meta["trigger_index"] = seg.trigger_index
        if cfg.jitter_ns:
            meta["jitter_ns"] = cfg.jitter_ns
        tick = Fraction(cfg.rate)
        chans = wsc.from_interleaved(data, names, cfg.width, cfg.positions, seg.samples)
        for c, ch in zip(chans, ids):
            c.acquisition = {"pin": ch, **_accuracy(cfg)}
        t_ref = getattr(seg, "start_ns", None)
    else:
        tick = Fraction(acfg.rate)
        chans = []
        _times(meta, aseg)
        t_ref = getattr(aseg, "start_ns", None)
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
            if getattr(aseg, "start_uncertainty_ns", None) is not None:
                acq["start_uncertainty_ns"] = aseg.start_uncertainty_ns
            if aseg.trigger_index is not None:
                acq["trigger_index"] = aseg.trigger_index
            width = {8: 8, 16: 16, 32: 32}[acfg.slot]
            chans.append(wsc.analog_raw(name, values, Fraction(acfg.rate), width=width, t0_ticks=t0,
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
    elif getattr(seg, "start_us", None) is not None:
        meta["start_us"] = seg.start_us


def _accuracy(cfg) -> dict:
    out = {}
    if getattr(cfg, "rate_measured", False):
        out["rate_measured"] = True
    if getattr(cfg, "rate_ppm", 0):
        out["rate_ppm"] = cfg.rate_ppm
    return out
