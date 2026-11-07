"""oep:<address> - an OEP probe, through oep-client-python's acquire.

The capture is oep_client.acquire's (it opens the probe, takes its lease, runs the capture and puts it together:
parts, losses, times and what the probe says about itself); this writes its result as the file's parts and channels,
one to one: `Bits` as samples (`bits`; a line taken by capture format 1's sample at D at step D), `Reduced` as its
intervals (`any_high`, `any_low`, `change_latch`), `Analog` as raw values (`analog`). A part's length is the probe's
(Part.samples) when the client gives it, else that of a line of every sample, else the latest channel end (a
reduced line's last interval may run past the part's end by less than an interval).
<address> is what the client opens (an OEP address such as oep://30eda0ea068b?via=usb, or a short form: a serial
port, tcp://HOST:PORT); the rates a UART link is raised to are this source's own option (Request.fast).
"""

from __future__ import annotations

from .. import _fileformat as fileformat
from . import Request, Result


def capture(target: str, req: Request) -> Result:
    try:
        from oep_client import acquire
    except ImportError as e:
        raise RuntimeError('the oep source needs oep-client-python: pip install "wireskein[oep]"') from e
    if not target:
        raise ValueError("oep:<address>: an OEP address (oep://UNIT_ID[?via=ROUTE]), a serial port or tcp://HOST:PORT")
    areq = acquire.Request(list(req.channels), req.rate, req.samples, trigger=req.trigger, pretrigger=req.pretrigger,
                           arm=req.arm, timeout=req.timeout, analog=list(req.analog), analog_rate=req.analog_rate,
                           analog_samples=req.analog_samples, resolution=dict(req.resolution), mode=req.mode,
                           seconds=req.duration, segments=req.segments, on_loss=req.on_loss, spool_dir=req.spool_dir)
    res = acquire.capture_at(target, areq, fast=req.fast, owner="wireskein capture")
    if res.parts is None and _ticks(res.all_parts[0], res.tick_hz, acquire) is None:
        return Result(res.tick_hz, [_channel(c, acquire) for c in res.channels], dict(res.meta), None, list(res.lost),
                      res.layout, res.temp_dir)
    meta = dict(res.meta) if res.parts is not None else {k: v for k, v in res.meta.items()
                                                         if k not in fileformat.PART_META}
    if res.parts is not None:
        parts = [fileformat.Part([_channel(c, acquire) for c in p.channels], dict(p.meta), p.tick0,
                                 _ticks(p, res.tick_hz, acquire)) for p in res.parts]
    else:
        parts = [fileformat.Part([_channel(c, acquire) for c in res.channels],
                                 {k: v for k, v in res.meta.items() if k in fileformat.PART_META}, 0,
                                 _ticks(res.all_parts[0], res.tick_hz, acquire))]
    return Result(res.tick_hz, [], meta, parts, list(res.lost), res.layout, res.temp_dir)


def capture_of(res):
    """oep_client.acquire's Result as a wireskein.capture.Capture: for Recorder.add() when the capture was taken on a
    session the caller holds (acquire.capture(hst, req)). The probe's facts go to the capture's meta (probe), each
    part's own to the part."""
    from oep_client import acquire
    from .. import capture as wc
    meta = {k: v for k, v in res.meta.items() if k not in fileformat.PART_META}
    parts = [wc.Part(_ticks(p, res.tick_hz, acquire) or _end(p, res.tick_hz, acquire),
                     [_channel(c, acquire) for c in p.channels], dict(p.meta), p.tick0) for p in res.all_parts]
    return wc.Capture(res.tick_hz, parts, res.layout, list(res.lost), meta)


def _end(p, tick_hz, acquire) -> int:
    return max((c.end(tick_hz) if isinstance(c, acquire.Analog) else c.end) for c in p.channels)


def _ticks(p, tick_hz, acquire) -> int | None:
    """The part's length when a reduced line's last interval may run past it: the probe's (Part.samples), else a
    line of every sample's. None when no line is reduced or sampled at a coarser step (the latest end is the length)."""
    coarse = [c for c in p.channels if not isinstance(c, acquire.Analog) and c.step > 1]
    if not coarse:
        return None
    if getattr(p, "samples", None) is not None:
        return int(p.samples)
    whole = [c.end for c in p.channels if isinstance(c, acquire.Bits) and c.step == 1]
    return max(whole) if whole else None


def _channel(c, acquire):
    """The client's channel as the file's (wireskein-format §4): the same data, one to one."""
    if isinstance(c, acquire.Bits):
        bits = fileformat.OnDisk(c.bits.path) if isinstance(c.bits, acquire.OnDisk) else c.bits
        return fileformat.Channel(c.name, bits, c.n, c.step, c.phase, dict(c.acquisition))
    reduced = getattr(acquire, "Reduced", None)
    if reduced is not None and isinstance(c, reduced):
        return fileformat.ReducedChannel(c.name, c.kind, c.data, c.n, c.step, c.phase, c.start_level,
                                         dict(c.acquisition))
    changes = getattr(acquire, "Changes", None)          # a client before capture format 1's reductions
    if changes is not None and isinstance(c, changes):
        return fileformat.EventsChannel(c.name, c.data, c.n, c.step, c.phase, c.start_level, c.count,
                                        dict(c.acquisition))
    if isinstance(c, acquire.Analog):
        return fileformat.AnalogChannel(c.name, c.data, c.n, c.rate_hz, c.t0_ticks, "analog", c.width, c.value_bits,
                                        c.zero, c.scale_nv, c.unit, dict(c.acquisition))
    raise TypeError(f"{c!r}: not a channel the client gives")
