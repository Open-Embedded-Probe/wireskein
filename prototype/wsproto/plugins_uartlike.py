"""UART-like protocols as small plugins on top of RateBlocks -> Chars.

Each plugin receives characters (with framing flags and break events) that the
common stages already produced; none of them touches edges or bit times.
"""

from __future__ import annotations

import numpy as np

from .gen import lin_checksum, lin_pid


def _split_on_breaks(ch):
    """Characters between consecutive breaks: [(break_time, values, ok)]."""
    out = []
    b = np.concatenate((ch.breaks, [np.iinfo(np.int64).max]))
    for i in range(len(b) - 1):
        sel = (ch.start > b[i]) & (ch.start < b[i + 1])
        out.append((int(b[i]), ch.values[sel] & 0xFF, ch.ok[sel]))
    return out


def lin(ch) -> dict:
    """LIN: break, sync 0x55, PID with parity, data, enhanced checksum."""
    frames, checks = [], {"sync": 0, "pid_parity": 0, "checksum": 0, "frames": 0}
    for _, v, ok in _split_on_breaks(ch):
        if len(v) < 3:
            continue
        checks["frames"] += 1
        sync, pid, data, cs = int(v[0]), int(v[1]), bytes(int(x) for x in v[2:-1]), int(v[-1])
        checks["sync"] += sync == 0x55
        checks["pid_parity"] += lin_pid(pid & 0x3F) == pid
        checks["checksum"] += lin_checksum(data, pid) == cs
        frames.append([pid & 0x3F, data.hex()])
    return {"frames": frames, "checks": checks}


def dmx512(ch) -> dict:
    """DMX512: break, start code, up to 512 slots."""
    packets, checks = [], {"packets": 0, "start_code_0": 0, "framing_ok": 0, "chars": 0}
    for _, v, ok in _split_on_breaks(ch):
        if len(v) < 2:
            continue
        checks["packets"] += 1
        checks["start_code_0"] += int(v[0]) == 0
        checks["framing_ok"] += int(ok.sum())
        checks["chars"] += len(ok)
        packets.append(bytes(int(x) for x in v[1:]).hex())
    return {"packets": packets, "checks": checks}
