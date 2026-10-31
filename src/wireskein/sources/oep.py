"""oep:<address> - an OEP probe, through oep-client-python's acquire.

The capture is oep_client.acquire's (it opens the probe, takes its lease, runs the capture and puts it together:
parts, losses, times and what the probe says about itself); this writes its result as the file's parts and channels,
one to one: `Bits` as samples (`bits`), `Changes` as changes (`events`), `Analog` as raw values (`analog`).
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
        raise RuntimeError('the oep source needs oep-client-python with acquire: pip install "wireskein[oep]"') from e
    if not target:
        raise ValueError("oep:<address>: an OEP address (oep://UNIT_ID[?via=ROUTE]), a serial port or tcp://HOST:PORT")
    areq = acquire.Request(list(req.channels), req.rate, req.samples, trigger=req.trigger, pretrigger=req.pretrigger,
                           arm=req.arm, timeout=req.timeout, analog=list(req.analog), analog_rate=req.analog_rate,
                           analog_samples=req.analog_samples, resolution=dict(req.resolution), mode=req.mode,
                           duration=req.duration, segments=req.segments, on_loss=req.on_loss, spool_dir=req.spool_dir)
    res = acquire.capture_at(target, areq, fast=req.fast, owner="wireskein capture")
    parts = None
    if res.parts is not None:
        parts = [fileformat.Part([_channel(c, acquire) for c in p.channels], dict(p.meta), p.tick0) for p in res.parts]
    return Result(res.tick_hz, [_channel(c, acquire) for c in res.channels], dict(res.meta), parts, list(res.lost),
                  res.layout, res.temp_dir)


def _channel(c, acquire):
    """The client's channel as the file's (wireskein-format §4): the same data, one to one."""
    if isinstance(c, acquire.Bits):
        bits = fileformat.OnDisk(c.bits.path) if isinstance(c.bits, acquire.OnDisk) else c.bits
        return fileformat.Channel(c.name, bits, c.n, c.step, c.phase, dict(c.acquisition))
    if isinstance(c, acquire.Changes):
        return fileformat.EventsChannel(c.name, c.data, c.n, c.step, c.phase, c.start_level, c.count,
                                        dict(c.acquisition))
    if isinstance(c, acquire.Analog):
        return fileformat.AnalogChannel(c.name, c.data, c.n, c.rate_hz, c.t0_ticks, "analog", c.width, c.value_bits,
                                        c.zero, c.scale_nv, c.unit, dict(c.acquisition))
    raise TypeError(f"{c!r}: not a channel the client gives")
