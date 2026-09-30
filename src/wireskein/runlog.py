"""Recording a test run for `wireskein verify` (standard library only).

A run is a directory:

    run.json          log, captures and expectations
    c0001.wireskein   one capture (wireskein.fileformat: each channel at its own rate)

The log is on the host clock (seconds from the recorder's start). It holds heading
markers ("# test", "## step", "##" closes; docs/workbench-model.ja.md), the
commands sent and the replies received. Commands go over the probe's console, not
a captured line, so the markers live here, not in the waveform; each capture is
placed on the same clock by the time it was armed. The analysis builds the
segment tree from the headings and checks each segment's captures against the
expectations recorded for its path.

    rec = Recorder("out/run1", target="x035")
    with rec.section(1, "test_pwm"):
        with rec.section(2, "duty=64", expect=[square("PA1", 1000, 64 / 255)]):
            rec.command("PWM 64"); rec.reply("PWM duty=64")
            armed = rec.armed()                       # time.monotonic() right after arming
            data = capture.read_all(n)
            rec.capture(armed, 2_000_000, interleaved=data, names=["PA1", "PA0"], start_us=seg.start_us)
    rec.close()

A capture comes either as the probe's sample stream (`interleaved`, with
`width` bits per sample and channel k at bit `positions[k]`, oep-if-capture
§1.1) or as channels already split (`channels`, wireskein.fileformat.Channel, each
with its own step). Test scripts import this module directly, so these calls
and the helpers below keep their names, arguments and meaning; anything added
gets a default that keeps the old meaning. An incompatible change raises
FORMAT. Capture meta "start_us" is the probe clock (us, integer) of the first
sample; "time_base_slipped": True means the probe knows some samples were taken
late (OEP segment flags bit 2; absent when not).
"""

from __future__ import annotations

import json
import time
from contextlib import contextmanager
from fractions import Fraction
from pathlib import Path

from . import fileformat

FORMAT = "wireskein-run/2"          # 2: captures are .wireskein files


class Recorder:
    def __init__(self, out: str | Path, **meta):
        self.dir = Path(out)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.t0 = time.monotonic()
        self.doc = {"format": FORMAT, "meta": meta, "log": [], "captures": [], "expect": {}}
        self.path: list[str] = []
        self._occ: list[tuple[str, int]] = []           # the open headings: (name, occurrence under its parent)
        self._seen: dict[tuple, int] = {}               # (parent occurrences, name) -> times opened
        self._expect: dict[tuple, dict] = {}            # expectations by occurrence path, rendered in close()

    def now(self) -> float:
        return time.monotonic() - self.t0

    def _log(self, src: str, text: str, t: float | None = None) -> None:
        self.doc["log"].append({"t": self.now() if t is None else t, "src": src, "text": text})

    # markers
    def heading(self, level: int, name: str = "") -> None:
        """'#' * level + name; an empty name closes that level."""
        if "\n" in name or name.startswith("#"):
            raise ValueError(f"bad heading name {name!r}")
        self._log("marker", "#" * level + (" " + name if name else ""))
        del self.path[level - 1:]
        del self._occ[level - 1:]
        if name:
            self.path += [""] * (level - 1 - len(self.path)) + [name]
            self._occ += [("", 0)] * (level - 1 - len(self._occ))
            k = (tuple(self._occ), name)
            self._seen[k] = self._seen.get(k, 0) + 1
            self._occ.append((name, self._seen[k] - 1))

    @contextmanager
    def section(self, level: int, name: str, expect: list | None = None, **rules):
        """A heading closed explicitly, so its quiet tail belongs to it.
        expect / rules are stored for this occurrence of the section. The key in
        run.json is the segment path as markers.py writes it: a name repeated
        under the same parent gets its index ("duty=64[0]", "duty=64[1]"), so
        each repetition keeps its own expectations."""
        self.heading(level, name)
        if expect is not None or rules:
            self._expect.setdefault(tuple(self._occ), {"checks": [], **rules})["checks"] += list(expect or [])
        try:
            yield self
        finally:
            self.heading(level)

    # traffic
    def command(self, text: str) -> None:
        self._log("host", text)

    def reply(self, text: str) -> None:
        self._log("dut", text)

    def note(self, text: str) -> None:
        self._log("note", text)

    # captures
    def armed(self) -> float:
        """time.monotonic() right after the capture was armed; pass it to capture()."""
        return time.monotonic()

    def capture(self, armed: float, tick_hz: int | float | Fraction, *, interleaved: bytes | None = None,
                names: list[str] | None = None, width: int = 8, positions: list[int] | None = None,
                n: int | None = None, channels: list[fileformat.Channel] | None = None,
                attachments: dict | None = None, **meta) -> str:
        """Store one capture as cNNNN.wireskein. armed: time.monotonic() when it was
        armed (Recorder.armed(), or the capture client's own stamp). Give either
        interleaved + names (+ width / positions / n for other sample layouts)
        or channels. meta goes into the capture file (start_us, time_base_slipped, ...);
        attachments are free-form files stored with it (see wireskein.fileformat.attach)."""
        if (interleaved is None) == (channels is None):
            raise ValueError("give either interleaved (with names) or channels")
        if channels is None:
            if not names:
                raise ValueError("interleaved needs names")
            channels = fileformat.from_interleaved(interleaved, names, width, positions, n)
        name = f"c{len(self.doc['captures']) + 1:04d}{fileformat.SUFFIX}"
        fileformat.write(self.dir / name, tick_hz, channels, attachments, **meta)
        self.doc["captures"].append({"file": name, "t0": armed - self.t0, "channels": [c.name for c in channels]})
        return name

    def _key(self, occ: tuple) -> str:
        parts = []
        for i, (name, k) in enumerate(occ):
            parts.append(name + (f"[{k}]" if self._seen[(occ[:i], name)] > 1 else ""))
        return "/".join(parts)

    def close(self) -> Path:
        self.doc["expect"] = {self._key(occ): spec for occ, spec in self._expect.items()}
        p = self.dir / "run.json"
        p.write_text(json.dumps(self.doc, indent=1))
        return p


# ---- expectation helpers (plain dicts; verify.py is the reference for their meaning) ----

def square(pin: str, freq_hz: float, duty: float | None = None, tol_freq: float = 0.02, tol_duty: float = 0.01,
           max_jitter: float | None = None) -> dict:
    """A steady square wave (PWM, tone): frequency within tol_freq (relative),
    duty within tol_duty (absolute), optional period spread limit (relative)."""
    return {"kind": "square", "pin": pin, "freq_hz": freq_hz, "duty": duty, "tol_freq": tol_freq,
            "tol_duty": tol_duty, "max_jitter": max_jitter}


def level(pin: str, value: int) -> dict:
    """The pin stays at value for the whole capture (duty 0 / 255, idle lines)."""
    return {"kind": "level", "pin": pin, "value": int(value)}


def ends(levels: dict[str, int]) -> dict:
    """Levels at the end of the capture (a bus released, a pin returned)."""
    return {"kind": "ends", "levels": {k: int(v) for k, v in levels.items()}}


def starts(levels: dict[str, int]) -> dict:
    return {"kind": "starts", "levels": {k: int(v) for k, v in levels.items()}}


def only_moving(pins: list[str]) -> dict:
    """No captured pin other than these may change."""
    return {"kind": "only_moving", "pins": list(pins)}


def i2c(scl: str, sda: str, transactions: list[dict] | None = None, hz: float | None = None, tol_hz: float = 0.1,
        released: bool = True) -> dict:
    """transactions: [{"addr": 0x42, "rw": "write", "bytes": [..], "ack": True}], compared in order.
    Optional per transaction: "complete" (default True; False for the last one
    when the window ends before its STOP) and "pending_bits" (bits clocked after
    the last whole byte, compared only when given)."""
    return {"kind": "i2c", "scl": scl, "sda": sda, "transactions": transactions, "hz": hz, "tol_hz": tol_hz,
            "released": released}


def spi(clk: str, mosi: str | None = None, miso: str | None = None, cs: str | None = None, mode: int | None = None,
        mosi_bytes: str | None = None, miso_bytes: str | None = None, hz: float | None = None, tol_hz: float = 0.1,
        bit_order: str = "msb") -> dict:
    return {"kind": "spi", "clk": clk, "mosi": mosi, "miso": miso, "cs": cs, "mode": mode, "mosi_bytes": mosi_bytes,
            "miso_bytes": miso_bytes, "hz": hz, "tol_hz": tol_hz, "bit_order": bit_order}


def uart(pin: str, baud: float | None, data: str | None = None, tol_baud: float = 0.03, idle: int = 1, bits: int = 8,
         parity: str = "none", stop: float = 1, max_errors: int | None = None) -> dict:
    """The bit time measured from the edges must be within tol_baud (relative)
    of baud; pass the rate the transmitter should really produce (e.g. F_CPU /
    BRR), not the nominal one. data: expected bytes as hex (the characters after
    the first idle gap; a window opening or ending inside a character is not an
    error). parity: "none" / "even" / "odd"; stop: 1, 1.5 or 2. max_errors:
    framing + parity errors allowed (None: not checked, only reported).
    baud=None measures only: the bit time is found from the edges and the result
    is "unchecked" with the measured values, unless data / max_errors / idle fail."""
    return {"kind": "uart", "pin": pin, "baud": baud, "data": data, "tol_baud": tol_baud, "idle": idle, "bits": bits,
            "parity": parity, "stop": stop, "max_errors": max_errors}


def pulses(pin: str, count: int | None = None, period_s: float | None = None, tol: float = 0.05) -> dict:
    """Rising edges counted, optional period (TOGGLE / MILLIS style tests)."""
    return {"kind": "pulses", "pin": pin, "count": count, "period_s": period_s, "tol": tol}
