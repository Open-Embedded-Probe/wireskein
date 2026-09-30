"""sigrok:<driver> - any device sigrok supports, through the sigrok-cli command.

<driver> is what `sigrok-cli -d` takes, e.g. fx2lafw, demo,
dreamsourcelab-dslogic:conn=1.6. The channel ids are sigrok's channel names:
logic (D0, D1, ...) and analog (A0, ...; stored as volts, analog-f32, at the
same rate, since sigrok gives one rate per device). The capture comes back as
a .sr and keeps the rate sigrok reports.
"""

from __future__ import annotations

import shutil
import subprocess
import tempfile
from fractions import Fraction
from pathlib import Path

from . import Request, Result

TRIGGER = {"rise": "r", "fall": "f", "both": "e", "high": "1", "low": "0"}


def capture(target: str, req: Request) -> Result:
    exe = shutil.which("sigrok-cli")
    if exe is None:
        raise RuntimeError("the sigrok source needs sigrok-cli on PATH")
    if not target:
        raise ValueError("sigrok:<driver>, e.g. sigrok:fx2lafw")
    from .._engine import wscio
    from .._engine.srio import read_sr
    if req.analog:                  # sigrok has one rate per device: analog ids ride with the logic ones
        if req.analog_rate not in (None, req.rate):
            raise ValueError("sigrok gives one rate per device: --analog-rate must be left out or equal --rate")
        req = type(req)(req.channels + [(n, c) for n, c, _ in req.analog], req.rate, req.samples, req.trigger,
                        req.pretrigger, req.timeout)
    ids = {name: cid for name, cid in req.channels}
    with tempfile.TemporaryDirectory() as d:
        out = Path(d) / "c.sr"
        cmd = [exe, "-d", target, "--config", f"samplerate={req.rate}", "--samples", str(req.samples),
               "-C", ",".join(cid for _, cid in req.channels), "-O", "srzip", "-o", str(out)]
        if req.trigger:
            cmd += ["-t", f"{ids[req.trigger[0]]}={TRIGGER[req.trigger[1]]}", "-w"]
            if req.pretrigger is not None:
                cmd[4:4] = ["--config", f"captureratio={max(0, min(100, round(100 * req.pretrigger / req.samples)))}"]
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=req.timeout)
        if r.returncode != 0 or not out.exists():
            raise RuntimeError(f"sigrok-cli failed ({r.returncode}): {r.stderr.strip() or r.stdout.strip()}")
        cap = read_sr(out)
    by_id = {c.name: c for c in cap.channels}
    an_id = {a.name: a for a in cap.analog}
    missing = [cid for _, cid in req.channels if cid not in by_id and cid not in an_id]
    if missing:
        raise RuntimeError(f"sigrok did not return channels {missing} (got {sorted([*by_id, *an_id])})")
    chans = []
    for name, cid in req.channels:
        c = wscio.to_channel(by_id[cid], cap.n_samples) if cid in by_id else wscio.to_analog(an_id[cid])
        c.name = name
        chans.append(c)
    meta = {"driver": target, "device_channels": ids}
    return Result(Fraction(cap.rate).limit_denominator(10**9), chans, meta)
