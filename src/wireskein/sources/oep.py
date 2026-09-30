"""oep:<target> - an OEP probe (oep.fixture.capture, one shot) through oep-client-python.

The channel ids are the probe's channel numbers; the capture's channels are
the plan's roles 0..C-1 in the order given. The lock is taken for the
capture and the plan is released afterwards.
"""

from __future__ import annotations

from .. import wsc
from . import Request, Result

TRIGGER = {"high": (1, 1), "low": (1, 0), "rise": (2, 0), "fall": (2, 1), "both": (2, 2)}   # (type, value), oep-if-capture


def capture(target: str, req: Request) -> Result:
    try:
        from oep_client import capture as oc
        from oep_client import core, link
    except ImportError as e:
        raise RuntimeError('the oep source needs oep-client-python: pip install "wireskein[oep]"') from e
    if not target:
        raise ValueError("oep:<target>: a serial port, tcp://HOST:PORT or usb[:VID:PID[:SERIAL]]")
    from oep_client import host as oh
    try:
        ids = [int(cid) for _, cid in req.channels]
    except ValueError:
        raise ValueError(f"oep channel ids are the probe's channel numbers: {[cid for _, cid in req.channels]}") from None
    names = [n for n, _ in req.channels]
    try:
        return _capture(link, core, oc, oh, target, req, ids, names)
    except oh.OepError as e:        # the probe refused or failed: say what, not where
        raise RuntimeError(f"probe {target}: {e}") from e


def _what(tag, oc) -> str:
    return {getattr(oc, "TRIGGER", None): "a trigger", getattr(oc, "PRETRIGGER", None): "a pretrigger"}.get(
        tag, f"configure item {tag}")


def _capture(link, core, oc, oh, target: str, req: Request, ids: list[int], names: list[str]) -> Result:
    hst = link.open_host(target)
    try:
        core.take(hst, 30_000, owner="wireskein capture")
        cap = oc.LogicCapture(hst)
        core.plan_apply(hst, [(cap.fn, k, ch) for k, ch in enumerate(ids)])
        try:
            trigger = None
            if req.trigger:
                kind, value = TRIGGER[req.trigger[1]]
                trigger = (kind, names.index(req.trigger[0]), value)
            # a trigger asked for must be honoured or refused: a probe without one would
            # otherwise ignore it and start at once (P4, oep-probe-arduino 0.0.8)
            asked = {oc.TRIGGER} if trigger else set()
            if req.pretrigger is not None:
                asked.add(oc.PRETRIGGER)
            try:
                cfg = cap.configure(rate=req.rate, mode=oc.ONE_SHOT, samples=req.samples, trigger=trigger,
                                    pretrigger=req.pretrigger, critical=asked)
            except oh.Unsupported as e:
                raise RuntimeError(f"probe {target} cannot capture with {_what(e.tag, oc)}") from e
            ignored = asked & set(cfg.ignored or [])
            if ignored:
                raise RuntimeError(f"probe {target} ignored {', '.join(_what(t, oc) for t in sorted(ignored))}")
            cap.start()
            segments = cap.wait(req.timeout)
            if not segments:
                raise RuntimeError("the probe finished without a segment")
            seg = segments[0]
            data = cap.read_segment(seg)
        finally:
            core.plan_release(hst, [cap.fn])
    finally:
        hst.end()
    meta = {"start_us": seg.start_us, "probe_channels": dict(zip(names, ids))}
    if seg.slipped:
        meta["time_base_slipped"] = True
    if seg.trigger_index is not None:
        meta["trigger_index"] = seg.trigger_index
    if cfg.jitter_ns:
        meta["jitter_ns"] = cfg.jitter_ns
    return Result(cfg.rate, wsc.from_interleaved(data, names, cfg.width, cfg.positions, seg.samples), meta)
