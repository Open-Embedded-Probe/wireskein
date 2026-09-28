"""Recording side of `ws verify`: what a test run writes (standard library only,
so the ArduinoCore-CH32 test scripts can import or copy this file).

A run is a directory:

    run.json      log, captures and expectations
    c0001.bin     one capture: one byte per sample, bit k = bits[k] (as trace_kit reads it)

The log is on the host clock (seconds from the recorder's start). It holds heading
markers ("# test", "## step", "##" closes; docs/workbench-model.ja.md), the
commands sent and the replies received. Commands go over the OEP console, not a
captured line, so the markers live here, not in the waveform; each capture is
placed on the same clock by the time it was armed. The analysis builds the
segment tree from the headings and checks each segment's captures against the
expectations recorded for its path.

    rec = Recorder("out/run1", target="x035", pins={"PA1": 47})
    with rec.section(1, "test_pwm"):
        with rec.section(2, "duty=64", expect=[square("PA1", 1000, 64 / 255)]):
            rec.command("PWM 64"); rec.reply("PWM duty=64")
            t = rec.armed()
            data = capture.read_all(n)
            rec.capture(data, 2_000_000, ["PA1"], t)
    rec.close()
"""

from __future__ import annotations

import json
import time
from contextlib import contextmanager
from pathlib import Path

FORMAT = "wireskein-run/0"


class Recorder:
    def __init__(self, out: str | Path, **meta):
        self.dir = Path(out)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.t0 = time.monotonic()
        self.doc = {"format": FORMAT, "meta": meta, "log": [], "captures": [], "expect": {}}
        self.path: list[str] = []

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
        if name:
            self.path += [""] * (level - 1 - len(self.path)) + [name]

    @contextmanager
    def section(self, level: int, name: str, expect: list | None = None, **rules):
        """A heading closed explicitly, so its quiet tail belongs to it.
        expect / rules are stored for this section's path (see verify.py)."""
        self.heading(level, name)
        if expect is not None or rules:
            key = "/".join(self.path)
            self.doc["expect"].setdefault(key, {"checks": [], **rules})["checks"] += list(expect or [])
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
        """Call right after the capture was armed; pass the value to capture()."""
        return self.now()

    def capture(self, data: bytes, rate: float, bits: list[str], armed_at: float, **meta) -> str:
        name = f"c{len(self.doc['captures']) + 1:04d}.bin"
        (self.dir / name).write_bytes(bytes(data))
        self.doc["captures"].append({"file": name, "t0": armed_at, "rate": float(rate), "bits": list(bits),
                                     "samples": len(data), **meta})
        return name

    def close(self) -> Path:
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
    """transactions: [{"addr": 0x42, "rw": "write", "bytes": [..], "ack": True}], compared in order."""
    return {"kind": "i2c", "scl": scl, "sda": sda, "transactions": transactions, "hz": hz, "tol_hz": tol_hz,
            "released": released}


def spi(clk: str, mosi: str | None = None, miso: str | None = None, cs: str | None = None, mode: int | None = None,
        mosi_bytes: str | None = None, miso_bytes: str | None = None, hz: float | None = None, tol_hz: float = 0.1,
        bit_order: str = "msb") -> dict:
    return {"kind": "spi", "clk": clk, "mosi": mosi, "miso": miso, "cs": cs, "mode": mode, "mosi_bytes": mosi_bytes,
            "miso_bytes": miso_bytes, "hz": hz, "tol_hz": tol_hz, "bit_order": bit_order}


def uart(pin: str, baud: float, data: str | None = None, tol_baud: float = 0.03, idle: int = 1) -> dict:
    """data: expected bytes as hex."""
    return {"kind": "uart", "pin": pin, "baud": baud, "data": data, "tol_baud": tol_baud, "idle": idle}


def pulses(pin: str, count: int | None = None, period_s: float | None = None, tol: float = 0.05) -> dict:
    """Rising edges counted, optional period (TOGGLE / MILLIS style tests)."""
    return {"kind": "pulses", "pin": pin, "count": count, "period_s": period_s, "tol": tol}
