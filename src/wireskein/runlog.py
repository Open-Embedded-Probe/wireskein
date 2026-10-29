"""Recording a test run for `wireskein verify` (standard library only).

A run is a directory:

    run.json          log, captures and expectations
    c0001.wireskein   one capture (wireskein.fileformat: each channel at its own rate)

The log is on the host clock (seconds from the recorder's start). It holds heading
markers ("# test", "## step", "##" closes; docs/markers.ja.md), the
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
            rec.capture(armed, 2_000_000, interleaved=data, names=["PA1", "PA0"],
                        start_ns=seg.start_ns, start_uncertainty_ns=seg.start_uncertainty_ns)
    rec.close()

A capture comes either as the probe's sample stream (`interleaved`, with
`width` bits per sample and channel k at bit `positions[k]`, oep-if-capture
§1.1) or as channels already split (`channels`, wireskein.fileformat.Channel, each
with its own step). Test scripts import this module directly, so these calls
and the helpers below keep their names, arguments and meaning; anything added
gets a default that keeps the old meaning. An incompatible change raises
FORMAT. Capture meta "start_ns" is the probe clock (ns since its boot) of the
first sample, "start_uncertainty_ns" how far off that may be; "time_base_slipped": True means the probe knows some samples were taken
late (OEP segment flags bit 2; absent when not).
"""

from __future__ import annotations

import json
import time
from contextlib import contextmanager
from fractions import Fraction
from pathlib import Path

from . import fileformat

FORMAT = "wireskein-run/3"          # docs/run-format.ja.md


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
        self._paths: dict[str, dict] = {}               # expectations given by their path (expect())

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
    def section(self, level: int, name: str, expect: list | None = None):
        """A heading closed explicitly, so its quiet tail belongs to it.
        expect is stored for this occurrence of the section. The key in run.json
        is the segment path as markers.py writes it: the first occurrence of a
        name under a parent keeps it, repeats get [1], [2], ... ("duty=64",
        "duty=64[1]"), so each repetition keeps its own expectations and adding
        one later does not rename the earlier ones (docs/run-format.ja.md)."""
        self.heading(level, name)
        if expect is not None:
            self._expect.setdefault(tuple(self._occ), {"checks": []})["checks"] += list(expect)
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

    def probe_lines(self, rows: list[dict], lost: list[str] = (), probe: str | None = None,
                    boot_id: str | None = None) -> None:
        """Lines the probe kept on its own clock (oep_client.probe_log.rows()): each {"t": time.monotonic() seconds,
        "src", "text", "probe_ns", "boot_id", "probe"}, written on the recorder's clock with the rest kept; a
        heading among them opens and closes sections like heading(). lost: what the probe says was dropped (its
        log overflowed, marks missing); each fails the run (docs/run-format.ja.md §2)."""
        for r in rows:
            self.doc["log"].append({**r, "t": r["t"] - self.t0})
        for why in lost:
            self._log("lost", why)
            self.doc["log"][-1].update({k: v for k, v in (("probe", probe), ("boot_id", boot_id)) if v is not None})

    def expect(self, path: str, checks: list) -> None:
        """Expectations for a section by its path (docs/run-format.ja.md §2.1), e.g. one the probe's lines open."""
        self._paths.setdefault(path, {"checks": []})["checks"] += list(checks)

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
        or channels. meta goes into the capture file (start_ns, start_uncertainty_ns, trigger_tick, time_base_slipped, ...);
        attachments are free-form files stored with it (see wireskein.fileformat.attach)."""
        if (interleaved is None) == (channels is None):
            raise ValueError("give either interleaved (with names) or channels")
        if channels is None:
            if not names:
                raise ValueError("interleaved needs names")
            channels = fileformat.from_interleaved(interleaved, names, width, positions, n)
        name = f"c{len(self.doc['captures']) + 1:04d}{fileformat.SUFFIX}"
        fileformat.write(self.dir / name, tick_hz, channels, attachments, **meta)
        entry = {"file": name, "t0": armed - self.t0, "channels": [c.name for c in channels]}
        if any(n for n, _ in self._occ):
            entry["path"] = self._key(tuple(self._occ))       # the section open now: verify assigns it there
        self.doc["captures"].append(entry)
        return name

    def _key(self, occ: tuple) -> str:
        parts = []
        for i, (name, k) in enumerate(occ):
            parts.append(name + (f"[{k}]" if k else ""))
        return "/".join(parts)

    @property
    def is_empty(self) -> bool:
        """Nothing to check: no expectations and no captures."""
        return not self._expect and not self.doc["captures"]

    def close(self) -> Path:
        self.doc["expect"] = {self._key(occ): spec for occ, spec in self._expect.items()}
        for path, spec in self._paths.items():
            self.doc["expect"].setdefault(path, {"checks": []})["checks"] += spec["checks"]
        self.doc["log"].sort(key=lambda e: e["t"])       # the probe's lines come late, each at its own time
        for c in self.doc["captures"]:
            _probe_markers(self.dir / c["file"], self.doc["log"])
        p = self.dir / "run.json"
        p.write_text(json.dumps(self.doc, indent=1))
        return p


# ---- the probe's clock: a log line at its tick in a capture (docs/run-format.ja.md §2.4) ----

def boot_of(meta: dict) -> int | str | None:
    """The boot of the probe clock a capture part's start_ns is on: meta["boot_id"], else the probe's (meta.probe:
    its boot_id, or the clock read around the capture; wireskein-format §3.3)."""
    if meta.get("boot_id") is not None:
        return meta["boot_id"]
    probe = meta.get("probe") or {}
    if probe.get("boot_id") is not None:
        return probe["boot_id"]
    clock = probe.get("clock") or {}
    ids = {clock[k].get("boot_id") for k in ("before", "after") if isinstance(clock.get(k), dict)} - {None}
    return ids.pop() if len(ids) == 1 else None


def place(entry: dict, view: dict) -> tuple[float | None, float, str]:
    """(tick, uncertainty in ticks, why not) of a log line with probe_ns in one part (fileformat.part_view()): its
    tick on that part's clock (float; it may fall outside the part), or None when the line is not on the clock of
    that part's start (another boot or probe, or a part without start_ns)."""
    meta = view.get("meta", {})
    if entry.get("probe_ns") is None:
        return None, 0, "the line has no probe_ns"
    if meta.get("start_ns") is None:
        return None, 0, "the capture has no start_ns"
    boot = boot_of(meta)
    if boot is None:
        return None, 0, "the capture does not say its probe's boot_id (pass boot_id= to Recorder.capture)"
    if entry.get("boot_id") != boot:
        return None, 0, f"the line is of boot {entry.get('boot_id')!r}, the capture of {boot!r}"
    src = meta.get("source")
    if entry.get("probe") and src and not (src == entry["probe"] or src.endswith(":" + entry["probe"])):
        return None, 0, f"the line is of probe {entry['probe']!r}, the capture of {src!r}"
    hz = Fraction(*view["tick_hz"])
    tick = float((entry["probe_ns"] - meta["start_ns"]) * hz / 10**9)
    unc = float((meta.get("start_uncertainty_ns", 0) + entry.get("uncertainty_ns", 0)) * hz / 10**9)
    return tick, unc, ""


def _probe_markers(path: Path, log: list[dict]) -> None:
    """The probe's lines that fall in a capture, as its markers (by "probe-log", wireskein-format §5.2), so a viewer
    shows them on the waveform; markers of others are kept, the probe's earlier ones replaced."""
    if not any(e.get("probe_ns") is not None for e in log):
        return
    head = fileformat.read_header(path)
    out = []
    for k in range(len(head["parts"])):
        view = fileformat.part_view(head, k)
        for e in log:
            if e.get("probe_ns") is None or e["src"] not in ("marker", "dut"):
                continue
            tick, unc, _ = place(e, view)
            if tick is not None and 0 <= tick <= view["ticks"]:
                out.append({"t": tick, "label": e["text"], "by": "probe-log", "part": k, "uncertainty_ticks": unc})
    if out:
        keep = [m for m in fileformat.markers(path) if m.get("by") != "probe-log"]
        fileformat.set_markers(path, keep + out)


# ---- expectation helpers (plain dicts; verify.py is the reference for their meaning) ----
#
# threshold: the logic checks also run on analog channels, read as logic at
# this voltage (one value, or (low, high) for hysteresis). Without it, an
# analog channel in a logic check is left unchecked.

def _t(x: dict, threshold) -> dict:
    if threshold is not None:
        x["threshold"] = list(threshold) if isinstance(threshold, (list, tuple)) else float(threshold)
    return x


def square(pin: str, freq_hz: float, duty: float | None = None, tol_freq: float = 0.02, tol_duty: float = 0.01,
           max_jitter: float | None = None, threshold=None) -> dict:
    """A steady square wave (PWM, tone): frequency within tol_freq (relative),
    duty within tol_duty (absolute), optional period spread limit (relative)."""
    return _t({"kind": "square", "pin": pin, "freq_hz": freq_hz, "duty": duty, "tol_freq": tol_freq,
               "tol_duty": tol_duty, "max_jitter": max_jitter}, threshold)


def level(pin: str, value: int, threshold=None) -> dict:
    """The pin stays at value for the whole capture (duty 0 / 255, idle lines)."""
    return _t({"kind": "level", "pin": pin, "value": int(value)}, threshold)


def ends(levels: dict[str, int], threshold=None) -> dict:
    """Levels at the end of the capture (a bus released, a pin returned)."""
    return _t({"kind": "ends", "levels": {k: int(v) for k, v in levels.items()}}, threshold)


def starts(levels: dict[str, int], threshold=None) -> dict:
    return _t({"kind": "starts", "levels": {k: int(v) for k, v in levels.items()}}, threshold)


def only_moving(pins: list[str]) -> dict:
    """No captured pin other than these may change."""
    return {"kind": "only_moving", "pins": list(pins)}


def i2c(scl: str, sda: str, transactions: list[dict] | None = None, hz: float | None = None, tol_hz: float = 0.1,
        released: bool = True, threshold=None) -> dict:
    """transactions: [{"addr": 0x42, "rw": "write", "bytes": [..], "ack": True}], compared in order.
    Optional per transaction: "complete" (default True; False for the last one
    when the window ends before its STOP) and "pending_bits" (bits clocked after
    the last whole byte, compared only when given)."""
    return _t({"kind": "i2c", "scl": scl, "sda": sda, "transactions": transactions, "hz": hz, "tol_hz": tol_hz,
               "released": released}, threshold)


def spi(clk: str, mosi: str | None = None, miso: str | None = None, cs: str | None = None, mode: int | None = None,
        mosi_bytes: str | None = None, miso_bytes: str | None = None, hz: float | None = None, tol_hz: float = 0.1,
        bit_order: str = "msb", cs_active: int = 0, threshold=None) -> dict:
    """mode is read from the lines: CPOL from SCK's level as CS becomes active, CPHA from the SCK edge MOSI (else
    MISO) changes after (a leading edge: 1)."""
    return _t({"kind": "spi", "clk": clk, "mosi": mosi, "miso": miso, "cs": cs, "mode": mode, "mosi_bytes": mosi_bytes,
               "miso_bytes": miso_bytes, "hz": hz, "tol_hz": tol_hz, "bit_order": bit_order, "cs_active": cs_active},
              threshold)


def phase(a: str, b: str, phase_deg: float | None = None, tol_deg: float = 2.0, freq_hz: float | None = None,
          tol_freq: float = 0.02, edge: str = "rise", threshold=None) -> dict:
    """b's edges after a's of the same direction (edge: "rise" / "fall") as a fraction of a's period: every pair
    within tol_deg (degrees) of phase_deg, both lines' frequency within tol_freq (relative) of freq_hz. Nothing
    expected: measured only."""
    return _t({"kind": "phase", "a": a, "b": b, "phase_deg": phase_deg, "tol_deg": tol_deg, "freq_hz": freq_hz,
               "tol_freq": tol_freq, "edge": edge}, threshold)


def skew(a: str, b: str, tol_s: float, delay_s: float = 0.0, edge: str = "both", threshold=None) -> dict:
    """Two lines that change together (patterns, parallel outputs): every edge of b within tol_s of delay_s after
    a's edge of the same direction (negative: before), and no edge of either without a partner. edge "rise" /
    "fall": only those (two outputs of one timer at different duties rise together)."""
    return _t({"kind": "skew", "a": a, "b": b, "tol_s": tol_s, "delay_s": delay_s, "edge": edge}, threshold)


def dead_time(hi: str, lo: str, min_s: float | None = None, max_s: float | None = None, active: int = 1,
              allow_overlap: bool = False, threshold=None) -> dict:
    """The two outputs of a half bridge (on at `active`): from one turning off to the other turning on, each time
    within [min_s, max_s]; both on at once (shoot-through) fails unless allow_overlap. Nothing expected beyond
    that: measured."""
    return _t({"kind": "dead_time", "hi": hi, "lo": lo, "min_s": min_s, "max_s": max_s, "active": int(active),
               "allow_overlap": allow_overlap}, threshold)


def i2c_timing(scl: str, sda: str, mode: str | None = "standard", limits: dict | None = None,
               threshold=None) -> dict:
    """UM10204's times on the bus, every instance against the limits of mode ("standard", "fast", "fast-plus";
    None: only `limits`). limits: {"t_low": min_s, "t_vd_dat": (min_s, max_s), "f_scl": (None, max_hz), ...}
    replaces the mode's limit of that name."""
    return _t({"kind": "i2c_timing", "scl": scl, "sda": sda, "mode": mode,
               "limits": {k: list(v) if isinstance(v, (list, tuple)) else v for k, v in (limits or {}).items()}},
              threshold)


def uart(pin: str, baud: float | None, data: str | None = None, tol_baud: float = 0.03, idle: int = 1, bits: int = 8,
         parity: str = "none", stop: float = 1, max_errors: int | None = None, threshold=None) -> dict:
    """The bit time measured from the edges must be within tol_baud (relative)
    of baud; pass the rate the transmitter should really produce (e.g. F_CPU /
    BRR), not the nominal one. data: expected bytes as hex (the characters after
    the first idle gap; a window opening or ending inside a character is not an
    error). parity: "none" / "even" / "odd"; stop: 1, 1.5 or 2. max_errors:
    framing + parity errors allowed (None: not checked, only reported).
    baud=None measures only: the bit time is found from the edges and the result
    is "measured" (not a failure) with the measured values, unless data / max_errors / idle fail."""
    return _t({"kind": "uart", "pin": pin, "baud": baud, "data": data, "tol_baud": tol_baud, "idle": idle, "bits": bits,
               "parity": parity, "stop": stop, "max_errors": max_errors}, threshold)


def pulses(pin: str, count: int | None = None, period_s: float | None = None, tol_period: float = 0.05,
           threshold=None) -> dict:
    """Rising edges counted, optional period (TOGGLE / MILLIS style tests); tol_period is relative."""
    return _t({"kind": "pulses", "pin": pin, "count": count, "period_s": period_s, "tol_period": tol_period},
              threshold)


def rgb_led(pin: str, data=None, t0h=(0.25e-6, 0.55e-6), t1h=(0.65e-6, 0.95e-6), period=(0.65e-6, 1.85e-6),
            reset_min_s: float | None = 50e-6, threshold=None) -> dict:
    """A WS2812-like line: data, the bytes of each frame between resets as hex in the order sent (one string for
    one frame); every T0H, T1H and bit period within its (min_s, max_s) (the defaults: WS2812B's 0.4 / 0.8 /
    1.25 us +- 150 / 150 / 600 ns; None: not checked), and every low between frames, and the one after the last,
    at least reset_min_s."""
    return _t({"kind": "rgb_led", "pin": pin, "data": data, "t0h": t0h and list(t0h), "t1h": t1h and list(t1h),
               "period": period and list(period), "reset_min_s": reset_min_s}, threshold)


def onewire(pin: str, data=None, presence: bool | None = True, speed: str = "standard", threshold=None) -> dict:
    """A 1-Wire line: data, the bytes after each reset as hex (one string per reset; read slots read as the bits
    they carried); presence: each reset answered by a presence pulse (False: none; None: not checked); every reset,
    presence, slot and low within the limits of speed ("standard" / "overdrive")."""
    return _t({"kind": "onewire", "pin": pin, "data": data, "presence": presence, "speed": speed}, threshold)


def ir(pin: str, frames: list[dict] | None = None, carrier_hz: float | None = None, tol_carrier: float = 0.05,
       tol_time: float | None = 0.1, protocol: str = "nec", threshold=None) -> dict:
    """An IR remote line, as the LED is driven (a carrier) or as a receiver gives it (idle high): frames
    [{"addr", "cmd"} or {"repeat": True}] in order (addr above 0xFF: NEC's extended address); every frame's
    command with its inverse; the carrier within tol_carrier (relative) of carrier_hz; every mark and space within
    tol_time (relative) of NEC's lengths (None: not checked)."""
    return _t({"kind": "ir", "pin": pin, "frames": frames, "carrier_hz": carrier_hz, "tol_carrier": tol_carrier,
               "tol_time": tol_time, "protocol": protocol}, threshold)


def lin(pin: str, baud: float, frames: list[dict] | None = None, tol_baud: float | None = 0.02,
        min_break_bits: float = 13, checksum: str = "auto", threshold=None) -> dict:
    """LIN frames [{"id": 0x10, "data": "01020304"}] in order; every frame's sync 0x55, identifier parity, framing
    and checksum ("enhanced", "classic", or "auto": classic for the diagnostic ids 0x3C / 0x3D, else enhanced);
    every break at least min_break_bits and its delimiter at least one bit at baud; each frame's baud from its
    sync field within tol_baud (relative; None: not checked)."""
    return _t({"kind": "lin", "pin": pin, "baud": baud, "frames": frames, "tol_baud": tol_baud,
               "min_break_bits": min_break_bits, "checksum": checksum}, threshold)


def i2s(bclk: str, ws: str, sd: str, format: str = "i2s", bits: int | None = None, fs_hz: float | None = None,
        tol_fs: float = 0.01, samples: list | None = None, slot_bits: int | None = None, left_ws: int | None = None,
        threshold=None) -> dict:
    """I2S-like audio read on BCLK's rising edges: format "i2s" (Philips), "left" or "right" (-justified); bits,
    the word (default: the slot; needed for "right"); the sample rate (WS) within tol_fs (relative) of fs_hz;
    slot_bits, BCLKs a channel; samples [(left, right), ...] found in a row among the frames read. left_ws: the
    WS level of the left channel when not the format's (i2s 0, else 1). Nothing expected: measured."""
    return _t({"kind": "i2s", "bclk": bclk, "ws": ws, "sd": sd, "format": format, "bits": bits, "fs_hz": fs_hz,
               "tol_fs": tol_fs, "samples": samples and [list(v) for v in samples], "slot_bits": slot_bits,
               "left_ws": left_ws}, threshold)


def quadrature(a: str, b: str, steps: int | None = None, cycles: float | None = None, direction: str | None = None,
               max_errors: int | None = 0, threshold=None) -> dict:
    """A quadrature encoder's lines: the net count in steps (every edge; positive with a leading b) or in cycles
    (steps / 4); direction "forward" / "backward": no step the other way; max_errors: changes of both lines on one
    tick (a skipped state) allowed (None: not checked)."""
    return _t({"kind": "quadrature", "a": a, "b": b, "steps": steps, "cycles": cycles, "direction": direction,
               "max_errors": max_errors}, threshold)


def sd_spi(clk: str, mosi: str, miso: str, cs: str | None = None, commands: list[dict] | None = None,
           mode: int | None = 0, cs_active: int = 0, max_ncr: int = 8, threshold=None) -> dict:
    """SD in SPI mode: commands [{"cmd": 17, "arg": 0, "r1": 0, "data": ["..."]}, {"acmd": 41, ...}] in order,
    each compared on the keys given ("cmd" / "acmd", "arg", "r1", "resp": the R7 / R3 / R2 bytes as hex,
    "data": the blocks as hex); every command answered within max_ncr bytes, with its end bit, CMD0 and CMD8 with
    a right CRC7, the card's blocks with a right CRC16. mode None: read from the lines."""
    return _t({"kind": "sd_spi", "clk": clk, "mosi": mosi, "miso": miso, "cs": cs,
               "commands": None if commands is None else [_sd_command(c) for c in commands], "mode": mode,
               "cs_active": cs_active, "max_ncr": max_ncr}, threshold)


def _sd_command(c: dict) -> dict:
    """{"acmd": 41, ...} as {"cmd": 41, "acmd": True, ...}."""
    c = dict(c)
    if isinstance(c.get("acmd"), int) and not isinstance(c["acmd"], bool):
        c["cmd"], c["acmd"] = c["acmd"], True
    return c


def voltage(pin: str, volts: float | None = None, tol_v: float = 0.05, min_v: float | None = None,
            max_v: float | None = None, ripple: float | None = None) -> dict:
    """An analog channel's level: the mean within volts +- tol_v (absolute, V),
    every sample within [min_v, max_v], peak-to-peak at most ripple (V). Only
    what is given is checked; a channel without a conversion to volts is left
    unchecked."""
    return {"kind": "voltage", "pin": pin, "volts": volts, "tol_v": tol_v, "min_v": min_v, "max_v": max_v,
            "ripple": ripple}
