"""Recording a test run for `wireskein verify` (standard library only).

A run is a directory:

    run.json          log, captures and expectations
    c0001.wireskein   one capture (wireskein.fileformat: each channel at its own rate)

The log is on the host clock (host_ns: time.monotonic_ns(), the clock a capture's meta.probe.clock reads the host
on). It holds heading
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
            armed = rec.armed()                       # time.monotonic_ns() right after arming
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

from . import _fileformat as fileformat

FORMAT = "wireskein-run/1"          # docs/run-format.ja.md

__all__ = ["FORMAT", "Recorder", "becomes", "can", "qspi", "rs485", "dead_time", "ends", "i2c", "i2c_timing", "i2s", "ir", "level", "lin", "only_moving",
           "onewire", "phase", "protocol", "pulses", "quadrature", "rgb_led", "sd_spi", "skew", "spi", "square", "starts", "trim", "uart",
           "voltage"]


class Recorder:
    def __init__(self, out: str | Path, **meta):
        self.dir = Path(out)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.started_ns = time.monotonic_ns()
        self.doc = {"format": FORMAT, "meta": meta, "log": [], "captures": [], "expect": {}}
        self.path: list[str] = []
        self._occ: list[tuple[str, int]] = []           # the open headings: (name, occurrence under its parent)
        self._seen: dict[tuple, int] = {}               # (parent occurrences, name) -> times opened
        self._expect: dict[tuple, dict] = {}            # expectations by occurrence path, rendered in close()
        self._paths: dict[str, dict] = {}               # expectations given by their path (expect())
        self._clock = None                              # use_clock(): headings on the probe's clock
        self._clock_probe: str | None = None
        self._closed = False

    def now(self) -> int:
        """The host clock the log is on: time.monotonic_ns()."""
        return time.monotonic_ns()

    def _open(self) -> None:
        if self._closed:
            raise RuntimeError("the run is closed (run.json written and verified): nothing more is recorded")

    def _log(self, src: str, text: str) -> None:
        self._open()
        self.doc["log"].append({"host_ns": self.now(), "src": src, "text": text})

    # markers
    def use_clock(self, clock, probe: str | None = None) -> None:
        """Put every heading from now on on the probe's clock too: clock() reads it (oep_client's Host.clock, or
        lambda: hst.clock_best(4)), probe is its place (its OEP address without the route, as the capture's
        meta.probe.address). A section opened and closed so is cut out of a capture taken on that clock, as the
        probe's own lines are (docs/run-format.ja.md §2.4): what a long capture holds is checked section by section.
        use_clock(None) goes back to the host clock only."""
        self._open()
        self._clock, self._clock_probe = clock, probe

    def heading(self, level: int, name: str = "", *, clock=None, probe: str | None = None) -> None:
        """'#' * level + name; an empty name closes that level. clock / probe: this heading on the probe's clock
        (as use_clock(), which they override)."""
        if "\n" in name or name.startswith("#"):
            raise ValueError(f"bad heading name {name!r}")
        clock = clock or self._clock
        text = "#" * level + (" " + name if name else "")
        if clock is None:
            self._log("marker", text)
        else:
            self._open()
            self._clocked(text, clock, probe or (self._clock_probe if clock is self._clock else None))
        del self.path[level - 1:]
        del self._occ[level - 1:]
        if name:
            self.path += [""] * (level - 1 - len(self.path)) + [name]
            self._occ += [("", 0)] * (level - 1 - len(self._occ))
            k = (tuple(self._occ), name)
            self._seen[k] = self._seen.get(k, 0) + 1
            self._occ.append((name, self._seen[k] - 1))

    def _clocked(self, text: str, clock, probe: str | None) -> None:
        """A heading at the moment the probe read its clock: probe_ns is that reading (exact on the probe's clock,
        and before anything the host sends after it), host_ns the host time it stands for. A clock that cannot be
        read leaves the heading on the host clock and fails the run (a lost line): its section is no longer known."""
        try:
            r = clock()
            line = {"host_ns": int(r.host_ns), "src": "marker", "text": text, "probe_ns": int(r.uptime_ns),
                    "boot_id": int(r.boot_id)}
        except Exception as e:  # noqa: BLE001 - recorded as lost, the run goes on
            self._log("marker", text)
            self._log("lost", f"the probe's clock could not be read for the heading {text!r}: {e}")
            if probe:
                self.doc["log"][-1]["probe"] = probe
            return
        if probe:
            line["probe"] = probe
        self.doc["log"].append(line)

    @contextmanager
    def section(self, level: int, name: str, expect: list | None = None, *, clock=None, probe: str | None = None):
        """A heading closed explicitly, so its quiet tail belongs to it. clock / probe put both its headings on the
        probe's clock (heading()).
        expect is stored for this occurrence of the section. The key in run.json
        is the segment path as markers.py writes it: the first occurrence of a
        name under a parent keeps it, repeats get [1], [2], ... ("duty=64",
        "duty=64[1]"), so each repetition keeps its own expectations and adding
        one later does not rename the earlier ones (docs/run-format.ja.md)."""
        self.heading(level, name, clock=clock, probe=probe)
        if expect is not None:
            self._expect.setdefault(tuple(self._occ), {"checks": []})["checks"] += list(expect)
        try:
            yield self
        finally:
            self.heading(level, clock=clock, probe=probe)

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
        "src", "text", "probe_ns", "boot_id", "probe"}, written with host_ns (that host time in ns) and the rest
        kept; a heading among them opens and closes sections like heading(). lost: what the probe says was dropped
        (its log overflowed, marks missing); each fails the run (docs/run-format.ja.md §2)."""
        self._open()
        for r in rows:
            line = {k: v for k, v in r.items() if k != "t"}
            self.doc["log"].append({"host_ns": round(r["t"] * 1e9), **line})
        for why in lost:
            self._log("lost", why)
            self.doc["log"][-1].update({k: v for k, v in (("probe", probe), ("boot_id", boot_id)) if v is not None})

    def result(self, path: str, name: str, status: str, *, measured: dict | None = None, reason: str = "",
               notices: list[dict] | None = None, capture: str | None = None) -> None:
        """A verdict the test made itself (on what verify.sections() and the analyses gave), recorded with the run so
        that wireskein verify reports and counts it as its own: path the segment's, name the check's, status "ok",
        "ng", "unchecked" or "measured", notices each {"kind", "tick", "part"?, ...} at its tick in the capture part
        (written into the capture's markers with verify --mark), capture the file it is about."""
        self._open()
        if status not in ("ok", "ng", "unchecked", "measured"):
            raise ValueError(f"status {status!r}: ok, ng, unchecked or measured")
        for n in notices or []:
            if not isinstance(n.get("kind"), str) or not isinstance(n.get("tick"), int):
                raise ValueError(f"a notice needs a kind (text) and a tick (an int): {n!r}")
        self.doc.setdefault("results", []).append(
            {"path": path, "check": name, "status": status, "measured": dict(measured or {}), "reason": reason,
             "notices": [{"part": 0, **n} for n in notices or []], "capture": capture})

    def expect(self, path: str, checks: list) -> None:
        """Expectations for a section by its path (docs/run-format.ja.md §2.1), e.g. one the probe's lines open."""
        self._open()
        self._paths.setdefault(path, {"checks": []})["checks"] += list(checks)

    # captures
    def armed(self) -> int:
        """time.monotonic_ns() right after the capture was armed; pass it to capture()."""
        return time.monotonic_ns()

    def capture(self, armed: int, tick_hz: int | float | Fraction, *, interleaved: bytes | None = None,
                names: list[str] | None = None, width: int = 8, positions: list[int] | None = None,
                n: int | None = None, channels: list[fileformat.Channel] | None = None,
                attachments: dict | None = None, **meta) -> str:
        """Store one capture as cNNNN.wireskein. armed: time.monotonic_ns() when it was
        armed (Recorder.armed(), or the capture client's own stamp). Give either
        interleaved + names (+ width / positions / n for other sample layouts)
        or channels. meta goes into the capture file (start_ns, start_uncertainty_ns, trigger_tick, time_base_slipped, ...);
        attachments are free-form files stored with it (see wireskein.fileformat.attach)."""
        self._open()
        if not isinstance(armed, int):
            raise TypeError(f"armed is time.monotonic_ns() (an int, Recorder.armed()), not {armed!r}")
        if (interleaved is None) == (channels is None):
            raise ValueError("give either interleaved (with names) or channels")
        if channels is None:
            if not names:
                raise ValueError("interleaved needs names")
            channels = fileformat.from_interleaved(interleaved, names, width, positions, n)
        name = self._next_name()
        fileformat.write(self.dir / name, tick_hz, channels, attachments, **meta)
        return self._entry(name, armed, [c.name for c in channels])

    def add(self, armed: int, cap) -> str:
        """Store a whole capture - a wireskein.capture.Capture (all its parts and what was lost, e.g. a stream or a
        repeat; wireskein.sources.oep.capture_of() makes one of oep_client.acquire's result), or the path of a
        .wireskein already written - as cNNNN.wireskein of this run. armed: time.monotonic_ns() when it was armed."""
        self._open()
        if not isinstance(armed, int):
            raise TypeError(f"armed is time.monotonic_ns() (an int, Recorder.armed()), not {armed!r}")
        from . import capture as wc
        name = self._next_name()
        if isinstance(cap, (str, Path)):
            import shutil
            shutil.copyfile(cap, self.dir / name)
            cap = wc.read(self.dir / name)
        else:
            wc.write(self.dir / name, cap)
        return self._entry(name, armed, cap.parts[0].names if cap.parts else [])

    def _next_name(self) -> str:
        return f"c{len(self.doc['captures']) + 1:04d}{fileformat.SUFFIX}"

    def _entry(self, name: str, armed: int, channels: list[str]) -> str:
        entry = {"file": name, "host_ns": armed, "channels": channels}
        if any(n for n, _ in self._occ):
            entry["path"] = self._key(tuple(self._occ))       # the section open now: verify assigns it there
        self.doc["captures"].append(entry)
        return name

    def _key(self, occ: tuple) -> str:
        parts = []
        for i, (name, k) in enumerate(occ):
            parts.append(name + (f"[{k}]" if k else ""))
        return "/".join(parts)

    def sections(self) -> list[dict]:
        """verify.sections() of the run as recorded so far, before close(): each segment with the captures that hold
        it, cut to it, for a test to analyse before it records its verdict (result()). An open segment ends at the
        last line recorded."""
        self._open()
        from . import verify
        doc = {**self.doc, "log": sorted(self.doc["log"], key=lambda e: e["host_ns"])}
        return verify.sections(self.dir, doc)

    @property
    def is_empty(self) -> bool:
        """Nothing to check: no expectations (by section or by path), no captures, no results and nothing lost."""
        return (not self._expect and not self._paths and not self.doc["captures"] and not self.doc.get("results")
                and not any(x["src"] == "lost" for x in self.doc["log"]))

    def close(self) -> Path:
        """Write run.json; nothing more is recorded after it (a call then raises)."""
        self._open()
        self._closed = True
        self.doc["expect"] = {self._key(occ): spec for occ, spec in self._expect.items()}
        for path, spec in self._paths.items():
            self.doc["expect"].setdefault(path, {"checks": []})["checks"] += spec["checks"]
        self.doc["log"].sort(key=lambda e: e["host_ns"])     # the probe's lines come late, each at its own time
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
    here = (meta.get("probe") or {}).get("address")    # the probe's place, without the route (an OEP address)
    src = meta.get("source")
    if entry.get("probe") and here:
        if entry["probe"] != here:
            return None, 0, f"the line is of probe {entry['probe']!r}, the capture of {here!r}"
    elif entry.get("probe") and src:
        src = src.partition("?")[0]                       # the route is not the probe
        if not (src == entry["probe"] or src.endswith(":" + entry["probe"])):
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


def square(pin: str, freq_hz: float, duty: float | None = None, freq_rel: float = 0.02, duty_abs: float = 0.01,
           jitter_max: float | None = None, outliers_max: int = 0, grid_abs_s: float | None = None,
           threshold=None) -> dict:
    """A steady square wave (PWM, tone) through the whole window: every whole period within freq_rel (relative) of
    1 / freq_hz and its high time within duty_abs (absolute, of the period) of duty, at most outliers_max periods
    off; the standard deviation of the periods at most jitter_max of their median (a ratio).

    grid_abs_s: every rising edge within this of an ideal grid of period exactly 1 / freq_hz from the first rising
    edge (a wave and a capture on one clock source: errors do not add up; 1 / the capture's rate is one sample).
    Measured either way: grid_max_s, and grid_fit_max_s against the grid of the period that fits best (sources apart)."""
    return _t({"kind": "square", "pin": pin, "freq_hz": freq_hz, "duty": duty, "freq_rel": freq_rel,
               "duty_abs": duty_abs, "jitter_max": jitter_max, "outliers_max": outliers_max, "grid_abs_s": grid_abs_s},
              threshold)


def becomes(pin: str, level: int | None = None, *, freq_hz: float | None = None, period_s: float | None = None,
            duty: float | None = None, high_s: float | None = None, freq_rel: float = 0.02, duty_abs: float = 0.01,
            high_abs_s: float | None = None, high_rel: float | None = None, irregular_max: int = 1, irregular_min: int = 0, min_periods: int = 3,
            within_s: float | None = None, threshold=None) -> dict:
    """The line ends the segment in a state - a level, or a periodic wave (freq_hz or period_s, with duty or high_s)
    - reached by at most one change: before it the state the segment began with, at it at most irregular_max pulses
    of neither (a timer cut mid-period), after it every whole period within freq_rel and its high time within
    duty_abs / high_abs_s or high_rel (a share of the high time: a clock off by 1 % moves both by 1 %), at
    least min_periods of them. The pulses of neither are measured (irregular_pulses: each one's high and period);
    irregular_min asks for some (a change that must cut a period). The change's time from the segment's start is
    measured (change_s); within_s bounds it. The state before is read from the capture just before the segment."""
    if (level is None) == (freq_hz is None and period_s is None):
        raise ValueError("becomes: a level, or freq_hz / period_s")
    return _t({"kind": "becomes", "pin": pin, "level": level, "freq_hz": freq_hz, "period_s": period_s, "duty": duty,
               "high_s": high_s, "freq_rel": freq_rel, "duty_abs": duty_abs, "high_abs_s": high_abs_s, "high_rel": high_rel,
               "irregular_max": irregular_max, "irregular_min": irregular_min, "min_periods": min_periods,
               "within_s": within_s}, threshold)


def protocol(name: str, bus: str | None, roles: dict[str, str], messages: list[dict] | None = None,
             checks_failed_max: int = 0, baud: float | None = None, threshold=None) -> dict:
    """A protocol of one's own (wireskein.protocols) on a bus: bus "uart" / "spi" / "i2c" / "uart_pair", roles its
    pins by role ({"data": "TX"}; {"clk", "mosi", "miso", "cs"}; {"scl", "sda"}; two UART lines {"a": "TX", "b":
    "RX"}), baud for a UART. bus None: a protocol declared
    on the wire (a definition, its roles as it names them), its frames the messages. messages: compared in order,
    each on the keys it gives (the others, such as "s", not); without them, at least one message. At most
    checks_failed_max of its checks may fail."""
    return _t({"kind": "protocol", "name": name, "bus": bus, "roles": dict(roles), "messages": messages,
               "checks_failed_max": checks_failed_max, "baud": baud}, threshold)


def trim(check: dict, head_s: float = 0.0, tail_s: float = 0.0, *, head_periods: float = 0.0,
         tail_periods: float = 0.0) -> dict:
    """Any check judged on its segment without its first and last stretch (the change at the start of a segment, a
    stop at its end): in seconds, or in periods of the check's own freq_hz / period_s (both add up)."""
    out = {**check, "trim_s": [float(head_s), float(tail_s)]}
    if head_periods or tail_periods:
        out["trim_periods"] = [float(head_periods), float(tail_periods)]
    return out


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


def i2c(scl: str, sda: str, transactions: list[dict] | None = None, freq_hz: float | None = None,
        freq_rel: float = 0.1, released: bool = True, notices: dict | None = None, spike_max_s: float = 50e-9,
        stretch_min: float = 2.0, threshold=None) -> dict:
    """transactions: [{"addr": 0x42, "rw": "write", "bytes": [..], "ack": True}], compared in order.
    The lines are read as an I2C input sees them: pulses shorter than spike_max_s left out (its spike filter; an SCL
    high where a clock is due, between whole lows, kept: a clock a slow rise clipped short), and
    an edge crossing the threshold more than once within a third of SCL's half and 1000 ns (t_r; a capture of 5 MS/s
    or more; a slow rise, ringing) read as
    one at its last change (its hysteresis).
    "ack" is the ACK after the address. The ACK after each data byte is compared as well: by default a write's
    bytes are all ACKed and a read's all but the last (the controller's NACK); "nack_at" (an index from 0, or a
    list) expects a NACK after those bytes and an ACK after the others, "acks" ([True, False, ...]) gives each.
    Optional per transaction: "complete" (default True; False for the last one
    when the window ends before its STOP), "pending_bits" (bits clocked after
    the last whole byte, compared only when given), "start" ("S": a START after a
    STOP or an idle bus, "Sr": a repeated START) and "end" ("P": a STOP, "Sr": the
    next transaction's repeated START), each compared only when given.
    Notices: SCL held low more than stretch_min times its usual low (scl_stretch: a target stretching the clock, or
    the controller pausing), a pulse on either line shorter than spike_max_s (bus_glitch: I2C inputs suppress
    spikes up to 50 ns) and such an edge (slow_edge); notices={"scl_stretch": "ng"} or a count limits them."""
    return _t({"kind": "i2c", "scl": scl, "sda": sda, "transactions": transactions, "freq_hz": freq_hz, "freq_rel": freq_rel,
               "released": released, "notices": notices, "spike_max_s": spike_max_s, "stretch_min": stretch_min},
              threshold)


def spi(clk: str, mosi: str | None = None, miso: str | None = None, cs: str | None = None, mode: int | None = None,
        mosi_bytes: str | None = None, miso_bytes: str | None = None, freq_hz: float | None = None,
        freq_rel: float = 0.1, bit_order: str = "msb", cs_active: int = 0, bits: int | list[int] | None = None,
        notices: dict | None = None, setup_min_s: float | None = None, hold_min_s: float | None = None,
        edge_sample: str | None = None, threshold=None) -> dict:
    """mode is read from the lines: CPOL from SCK's level as CS becomes active, CPHA from the SCK edge MOSI (else
    MISO) changes after (a leading edge: 1). With cs: the clocks in each window of CS active are measured
    (frame_bits) and bits (one count for every window, or a list) compared; an SCK level inside a window shorter
    than half the median one is a glitch. SCK moving while CS is inactive (on a bus shared with other devices it may)
    and a window with no clock are notices (sck_outside_cs, empty_cs_window): notices={"sck_outside_cs": "ng"} or a
    count limits them. With cs, each data line's setup and hold around the sampling edges are measured (margins;
    other_setup_s / other_hold_s on the other edge: wide on both, the line reads the same in either mode) and
    setup_min_s / hold_min_s bound them; modes_ok lists the modes a receiver could read the lines in, from the lines
    alone (CPOL as SCK's idle level, no data line changing at that mode's sampling edges).

    edge_sample="before": a data line changing in the same sample as a sampling edge is taken as changing after it
    (the edge reads the value before). For a part whose output hold is known (its datasheet) to be shorter than a
    sample of the capture: such a change then still reads, and margins tell its hold as hold_under_sample with the
    sample's length (sample_s). The bytes are read just before each sampling edge either way."""
    if edge_sample not in (None, "before"):
        raise ValueError(f"edge_sample: None or \"before\", not {edge_sample!r}")
    return _t({"kind": "spi", "clk": clk, "mosi": mosi, "miso": miso, "cs": cs, "mode": mode, "mosi_bytes": mosi_bytes,
               "miso_bytes": miso_bytes, "freq_hz": freq_hz, "freq_rel": freq_rel, "bit_order": bit_order,
               "cs_active": cs_active, "bits": bits, "notices": notices, "setup_min_s": setup_min_s,
               "hold_min_s": hold_min_s, **({"edge_sample": edge_sample} if edge_sample else {})}, threshold)


def phase(a: str, b: str, phase_deg: float | None = None, phase_abs_deg: float = 2.0, freq_hz: float | None = None,
          freq_rel: float = 0.02, edge: str = "rise", threshold=None) -> dict:
    """b's edges after a's of the same direction (edge: "rise" / "fall") as a fraction of a's period: every pair
    within phase_abs_deg of phase_deg, both lines' frequency within freq_rel (relative) of freq_hz. Nothing
    expected: measured only."""
    return _t({"kind": "phase", "a": a, "b": b, "phase_deg": phase_deg, "phase_abs_deg": phase_abs_deg,
               "freq_hz": freq_hz, "freq_rel": freq_rel, "edge": edge}, threshold)


def skew(a: str, b: str, delay_abs_s: float, delay_s: float = 0.0, edge: str = "both", threshold=None) -> dict:
    """Two lines that change together (patterns, parallel outputs): every edge of b within delay_abs_s of delay_s after
    a's edge of the same direction (negative: before), and no edge of either without a partner. edge "rise" /
    "fall": only those (two outputs of one timer at different duties rise together)."""
    return _t({"kind": "skew", "a": a, "b": b, "delay_abs_s": delay_abs_s, "delay_s": delay_s, "edge": edge}, threshold)


def dead_time(hi: str, lo: str, dead_min_s: float | None = None, dead_max_s: float | None = None, active: int = 1,
              allow_overlap: bool = False, threshold=None) -> dict:
    """The two outputs of a half bridge (on at `active`): from one turning off to the other turning on, each time
    within [dead_min_s, dead_max_s]; both on at once (shoot-through) fails unless allow_overlap. Nothing expected
    beyond that: measured."""
    return _t({"kind": "dead_time", "hi": hi, "lo": lo, "dead_min_s": dead_min_s, "dead_max_s": dead_max_s,
               "active": int(active),
               "allow_overlap": allow_overlap}, threshold)


def i2c_timing(scl: str, sda: str, mode: str | None = "standard", limits: dict | None = None,
               threshold=None) -> dict:
    """UM10204's times on the bus, every instance against the limits of mode ("standard", "fast", "fast-plus";
    None: only `limits`). limits: {"t_low_min_s": 1.5e-6, "t_vd_dat_max_s": 0.9e-6, "f_scl_max_hz": 400e3, ...}
    replaces that side of the mode's limit of that name (times: t_hd_sta, t_low, t_high, t_su_sta, t_hd_dat,
    t_vd_dat, t_su_dat, t_su_sto, t_buf)."""
    for k in limits or {}:
        if k.rsplit("_", 2)[1:2] not in (["min"], ["max"]):
            raise ValueError(f"limits: {k!r} is not <name>_min_<unit> or <name>_max_<unit>")
    return _t({"kind": "i2c_timing", "scl": scl, "sda": sda, "mode": mode, "limits": dict(limits or {})}, threshold)


def uart(pin: str, baud: float | None, data: str | None = None, baud_rel: float = 0.03, idle: int = 1, bits: int = 8,
         parity: str = "none", stop: float = 1, errors: dict | None = None, threshold=None) -> dict:
    """The bit time measured from the edges must be within baud_rel (relative)
    of baud; pass the rate the transmitter should really produce (e.g. F_CPU /
    BRR), not the nominal one. data: expected bytes as hex (the characters after
    the first idle gap; a window opening or ending inside a character is not an
    error). parity: "none" / "even" / "odd"; stop: 1, 1.5 or 2. errors: the
    framing and parity errors expected, exactly, as {"framing": n, "parity": n}
    (a kind not named is 0; None: none at all, as a framing or parity error
    breaks the protocol). A glitch or a break is no error but a notice.
    baud=None measures only: the bit time is found from the edges and the result
    is "measured" (not a failure) with the measured values, unless data / errors / idle fail."""
    return _t({"kind": "uart", "pin": pin, "baud": baud, "data": data, "baud_rel": baud_rel, "idle": idle, "bits": bits,
               "parity": parity, "stop": stop, "errors": dict(errors) if errors is not None else None}, threshold)


def pulses(pin: str, count: int | None = None, period_s: float | None = None, period_rel: float = 0.05,
           width_min_s: float | None = None, width_max_s: float | None = None, level: int = 1,
           threshold=None) -> dict:
    """Rising edges counted, optional period (TOGGLE / MILLIS style tests); period_rel is relative.

    With width_min_s / width_max_s, the pulses at `level` that start and end inside the window: each as wide as
    that, and count (when given) is how many of them (a watchdog's reset pulse: count=1). A pulse cut by the window's
    edge is no whole pulse and fails. Measured: widths_s."""
    return _t({"kind": "pulses", "pin": pin, "count": count, "period_s": period_s, "period_rel": period_rel,
               "width_min_s": width_min_s, "width_max_s": width_max_s, "level": level}, threshold)


def rgb_led(pin: str, data=None, t0h_min_s: float | None = 0.25e-6, t0h_max_s: float | None = 0.55e-6,
            t1h_min_s: float | None = 0.65e-6, t1h_max_s: float | None = 0.95e-6,
            period_min_s: float | None = 0.65e-6, period_max_s: float | None = 1.85e-6,
            reset_min_s: float | None = 50e-6, latch_safe_s: float | None = 280e-6, notices: dict | None = None,
            threshold=None) -> dict:
    """A WS2812-like line: data, the bytes of each frame between resets as hex in the order sent (one string for
    one frame); every T0H, T1H and bit period within its *_min_s / *_max_s (the defaults: WS2812B's 0.4 / 0.8 /
    1.25 us +- 150 / 150 / 600 ns; None: that side not checked), and every low between frames, and the one after
    the last, at least reset_min_s. A low between frames long enough for reset_min_s but shorter than latch_safe_s
    is a notice (latch_short: later parts, WS2812B-V5 and the like, latch only after 280 us; None: not looked
    for); notices={"latch_short": "ng"} or a count limits them."""
    return _t({"kind": "rgb_led", "pin": pin, "data": data, "t0h_min_s": t0h_min_s, "t0h_max_s": t0h_max_s,
               "t1h_min_s": t1h_min_s, "t1h_max_s": t1h_max_s, "period_min_s": period_min_s,
               "period_max_s": period_max_s, "reset_min_s": reset_min_s, "latch_safe_s": latch_safe_s,
               "notices": notices}, threshold)


def onewire(pin: str, data=None, presence: bool | None = True, speed: str = "standard", threshold=None) -> dict:
    """A 1-Wire line: data, the bytes after each reset as hex (one string per reset; read slots read as the bits
    they carried); presence: each reset answered by a presence pulse (False: none; None: not checked); every reset,
    presence, slot and low within the limits of speed ("standard" / "overdrive")."""
    return _t({"kind": "onewire", "pin": pin, "data": data, "presence": presence, "speed": speed}, threshold)


def ir(pin: str, frames: list[dict] | None = None, carrier_hz: float | None = None, carrier_rel: float = 0.05,
       timing_rel: float | None = 0.1, protocol: str = "nec", threshold=None) -> dict:
    """Experimental (NEC only; other formats belong to a library made for them). An IR remote line, as the LED is driven (a carrier) or as a receiver gives it (idle high): frames
    [{"addr", "cmd"} or {"repeat": True}] in order (addr above 0xFF: NEC's extended address); every frame's
    command with its inverse; the carrier within carrier_rel (relative) of carrier_hz; every mark and space within
    timing_rel (relative) of NEC's lengths (None: not checked)."""
    return _t({"kind": "ir", "pin": pin, "frames": frames, "carrier_hz": carrier_hz, "carrier_rel": carrier_rel,
               "timing_rel": timing_rel, "protocol": protocol}, threshold)


def lin(pin: str, baud: float, frames: list[dict] | None = None, baud_rel: float | None = 0.02,
        break_min_bits: float = 13, checksum: str = "auto", threshold=None) -> dict:
    """LIN frames [{"id": 0x10, "data": "01020304"}] in order; every frame's sync 0x55, identifier parity, framing
    and checksum ("enhanced", "classic", or "auto": classic for the diagnostic ids 0x3C / 0x3D, else enhanced);
    every break at least break_min_bits and its delimiter at least one bit at baud; each frame's baud from its
    sync field within baud_rel (relative; None: not checked)."""
    return _t({"kind": "lin", "pin": pin, "baud": baud, "frames": frames, "baud_rel": baud_rel,
               "break_min_bits": break_min_bits, "checksum": checksum}, threshold)


def can(pin: str, bitrate: float | None, frames: list[dict] | None = None, bitrate_rel: float = 0.01, idle: int = 1,
        ack: bool | None = True, errors: dict | None = None, sample_point: float = 0.75, notices=None,
        threshold=None) -> dict:
    """Classic CAN on one line at the bus level (a controller's RX, or the bus through a transceiver's RXD;
    recessive = idle), read as a receiving node does. The bit rate measured from the edges within bitrate_rel
    (relative) of bitrate; frames [{"id": 0x123, "data": "112233"}] in order, the good ones ("ext": an extended
    identifier, by default when the id is above 0x7FF; "rtr": a remote frame); ack True: every frame acknowledged
    (a dominant ACK slot), False: none (a controller's TX alone), None: not judged. errors: the stuff, form and CRC
    errors expected, exactly, as {"stuff": n, "form": n, "crc": n} (a kind not named is 0; None: none at all).
    bitrate=None measures only, unless frames / errors are given."""
    return _t({"kind": "can", "pin": pin, "bitrate": bitrate, "frames": frames, "bitrate_rel": bitrate_rel,
               "idle": idle, "ack": ack, "errors": dict(errors) if errors is not None else None,
               "sample_point": sample_point, "notices": notices}, threshold)


def rs485(tx: str, de: str, baud: float | None, data: str | None = None, baud_rel: float = 0.03, idle: int = 1,
          bits: int = 8, parity: str = "none", stop: float = 1, errors: dict | None = None, de_active: int = 1,
          lead_min_s: float | None = 0.0, lead_max_s: float | None = None, tail_min_s: float | None = 0.0,
          tail_max_s: float | None = None, notices=None, threshold=None) -> dict:
    """UART on tx, read as uart() does (baud, data, the format, errors), with a half-duplex driver enable de
    (active at de_active) around it: every character inside a window of DE active, each window's lead (DE active
    to its first start bit) within lead_min_s..lead_max_s and tail (its last stop bit's end to DE inactive) within
    tail_min_s..tail_max_s (by default at least 0: DE released before the stop bit ends cuts it on the bus; a
    tail_max_s bounds how soon the bus is free for the answer). A window of DE active with no character is a
    notice (empty_de_window)."""
    return _t({"kind": "rs485", "tx": tx, "de": de, "baud": baud, "data": data, "baud_rel": baud_rel, "idle": idle,
               "bits": bits, "parity": parity, "stop": stop, "errors": dict(errors) if errors is not None else None,
               "de_active": de_active, "lead_min_s": lead_min_s, "lead_max_s": lead_max_s, "tail_min_s": tail_min_s,
               "tail_max_s": tail_max_s, "notices": notices}, threshold)


def qspi(clk: str, cs: str, io: list[str], transfers: list[list[dict]] | None = None, mode: int = 0,
         cs_active: int = 0, freq_hz: float | None = None, freq_rel: float = 0.1, threshold=None) -> dict:
    """SPI on several data lines (dual, quad, octal: io = [IO0, IO1, ...]) read per window of CS active, each
    window phase by phase: transfers = one list of phases per window, in order; a phase {"width": 4, "hex": "..."}
    (or "bits": n) reads its clocks on io[width-1] .. io[0], the MSB first ("lines": [1] picks them, as a
    single-line read on IO1), {"dummy": 8} skips clocks, and a last phase with neither reads the rest. Every
    window's clocks must be its phases'. mode 0..3 as SPI's (the sampling edge and SCK's idle level). Without
    transfers, each window is read on all the lines."""
    return _t({"kind": "qspi", "clk": clk, "cs": cs, "io": list(io), "transfers": transfers, "mode": mode,
               "cs_active": cs_active, "freq_hz": freq_hz, "freq_rel": freq_rel}, threshold)


def i2s(bclk: str, ws: str, sd: str, format: str = "i2s", bits: int | None = None, fs_hz: float | None = None,
        fs_rel: float = 0.01, samples: list | None = None, slot_bits: int | None = None, left_ws: int | None = None,
        threshold=None) -> dict:
    """I2S-like audio read on BCLK's rising edges: format "i2s" (Philips), "left" or "right" (-justified); bits,
    the word (default: the slot; needed for "right"); the sample rate (WS) within fs_rel (relative) of fs_hz;
    slot_bits, BCLKs a channel; samples [(left, right), ...] found in a row among the frames read. left_ws: the
    WS level of the left channel when not the format's (i2s 0, else 1). Nothing expected: measured."""
    return _t({"kind": "i2s", "bclk": bclk, "ws": ws, "sd": sd, "format": format, "bits": bits, "fs_hz": fs_hz,
               "fs_rel": fs_rel, "samples": samples and [list(v) for v in samples], "slot_bits": slot_bits,
               "left_ws": left_ws}, threshold)


def quadrature(a: str, b: str, steps: int | None = None, cycles: float | None = None, direction: str | None = None,
               errors_max: int | None = 0, threshold=None) -> dict:
    """A quadrature encoder's lines: the net count in steps (every edge; positive with a leading a) or in cycles
    (steps / 4); direction "forward" / "backward": no step the other way; errors_max: changes of both lines on one
    tick (a skipped state) allowed (None: not checked)."""
    return _t({"kind": "quadrature", "a": a, "b": b, "steps": steps, "cycles": cycles, "direction": direction,
               "errors_max": errors_max}, threshold)


def sd_spi(clk: str, mosi: str, miso: str, cs: str | None = None, commands: list[dict] | None = None,
           mode: int | None = 0, cs_active: int = 0, ncr_max_bytes: int = 8, threshold=None) -> dict:
    """SD in SPI mode: commands [{"cmd": 17, "arg": 0, "r1": 0, "data": ["..."]}, {"acmd": 41, ...}] in order,
    each compared on the keys given ("cmd" / "acmd", "arg", "r1", "resp": the R7 / R3 / R2 bytes as hex,
    "data": the blocks as hex); every command answered within ncr_max_bytes bytes, with its end bit, CMD0 and CMD8 with
    a right CRC7, the card's blocks with a right CRC16. mode None: read from the lines."""
    return _t({"kind": "sd_spi", "clk": clk, "mosi": mosi, "miso": miso, "cs": cs,
               "commands": None if commands is None else [_sd_command(c) for c in commands], "mode": mode,
               "cs_active": cs_active, "ncr_max_bytes": ncr_max_bytes}, threshold)


def _sd_command(c: dict) -> dict:
    """{"acmd": 41, ...} as {"cmd": 41, "acmd": True, ...}."""
    c = dict(c)
    if isinstance(c.get("acmd"), int) and not isinstance(c["acmd"], bool):
        c["cmd"], c["acmd"] = c["acmd"], True
    return c


def voltage(pin: str, level_v: float | None = None, level_abs_v: float = 0.05, level_min_v: float | None = None,
            level_max_v: float | None = None, ripple_max_v: float | None = None) -> dict:
    """An analog channel's level: the mean within level_v +- level_abs_v, every sample within
    [level_min_v, level_max_v], peak-to-peak at most ripple_max_v. Only what is given is checked; a channel without
    a conversion to volts is left unchecked."""
    return {"kind": "voltage", "pin": pin, "level_v": level_v, "level_abs_v": level_abs_v, "level_min_v": level_min_v,
            "level_max_v": level_max_v, "ripple_max_v": ripple_max_v}
