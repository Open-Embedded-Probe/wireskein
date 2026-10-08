# WireSkein file format `.wireskein` (`wireskein/1`)

[日本語](wireskein-format.ja.md)

This is the specification of the WireSkein file. Any language may read and write it, as long as it follows this specification.

In this document, rules marked MUST / MUST NOT apply to both readers and writers.

## 1. Principles

- **WireSkein has its own format.** Existing formats (such as sigrok's `.sr`) use one rate for all channels, so they cannot hold, at the resolution it was taken, a capture from a probe that decimates some channels to fit its bandwidth (§6). `.sr` and VCD are treated as export targets for viewing and as import paths.
- **WireSkein data goes into one container of one format.** It holds waveforms (captures), attachments, notes, markers and decode annotations. Hand over one file, and the waveform and the findings travel together.
- **The container is zip.** Many languages can read and write it with standard features, without special libraries. Adding an entry does not require rewriting the channel data.
- **Identify by content, not by extension.** The extension is `.wireskein`. However, readers do not use the extension to decide (§2.3).
- **Each channel is stored at its own sample rate.** A probe that decimates some channels to fit its bandwidth stores only the samples it took. No padding fills the gaps.
  - Reason: padding does not change the analysis results (edge times), but it loses the channel's true resolution (an edge happened somewhere within up to `step` ticks before), and a viewer would show values that were not taken as points identical to values that were taken. The goal is not to make data that does not exist look as if it exists. The size difference (about 2×) is not the deciding factor.
- **The time base is one tick per capture.** Each channel has "how many ticks per sample" (`step`) and "the tick of the first sample" (`phase`).
- **There is no limit on the number of channels.** Each channel is stored in its own entry.
- **Channels of different kinds can coexist.** Even if logic and analog are in the same file, a reader that reads only logic can read the logic (§3.2).
- **Do not break unknown parts.** Readers ignore unknown parts, and carry them over when rewriting (§2.2).
- **ADC values are stored raw.** The conversion to voltage (a linear formula declared by the probe) is attached as a way of viewing; factory calibration curves and the like are chosen at analysis time (§4.2). Some calibrations, such as the one for the ESP32 internal ADC, cut off a large part of the measurable range at the top and bottom, so they are not forced at the storage stage.
- **Times returned by the probe are recorded as estimates; alignment derived by analysis is kept separately.** The probe's start time has delays inside the driver and hardware (which the probe itself does not know exactly), rate errors and jitter. Corrections derived by matching edges of the same signal go into an attachment (§5.1); the channel data and the recorded times are not rewritten. The reader can choose which to use.

## 2. Container

A zip file. The extension is `.wireskein`. Its contents are:

| Entry | Required | Contents |
| --- | --- | --- |
| `wireskein.json` | Required | Format name (§2.1) |
| `capture.json` | When the file has a capture | Tick, channel list, capture metadata (§3) |
| Channel data (e.g. `ch/0.bits`) | Per channel | The entry pointed to by the channel's `file` in `capture.json` (§4) |
| `attach/<name>` | Optional | Free-form attachments (§5) |
| `notes/<number>.json` | Optional | Append-only notes (§5) |
| `markers/markers.json` | Optional | Markers (§5.2) |
| `decode/annotations.json` | Optional | Decode annotations (§5.3) |
| `verify/`, `view/` | — | Reserved (§2.4) |

- Compression is zip deflate or stored. Writers store `wireskein.json` as stored and everything else as deflate.
- Writers of the current version always include a capture (`capture.json`). Readers treat a file without `capture.json` as "containing no capture" (operations that need a waveform refuse, saying so).

### 2.1 `wireskein.json`

A UTF-8 JSON object.

| Key | Type | Meaning |
| --- | --- | --- |
| `format` | string | `"wireskein/1"`. Readers MUST NOT read a file with an unknown value |

- Writers place `wireskein.json` as the **first entry** of the zip, uncompressed (stored). The name `wireskein.json` then appears at a fixed position at the start of the file (from byte 30), so the file can be identified without extracting the zip (the same trick as `mimetype` in ODF and EPUB).
- Readers MUST NOT rely on this position (in a zip repacked by other tools, the order may change). Identification follows the procedure in §2.3.

### 2.2 Unknown entries

- Readers MUST ignore entries not in this specification (for future additions).
- Operations that rewrite the file (such as replacing an attachment) MUST **carry over unknown entries as they are**.
- Conversion to sigrok `.sr` also puts unknown entries into the `.sr` under the same names, and carries them over when reading back (§6). If a name collides with an `.sr` entry, the conversion refuses.
- Channels in the capture with an unknown `encoding` are different. They are the data itself, so rewriting is refused (§3.2).

### 2.3 Identification

Readers decide the format by content, not by extension.

1. A zip that has `wireskein.json` whose `format` starts with `"wireskein/"`: a WireSkein file. If `format` is not a version the reader supports, refuse, showing that version.
2. A zip that has `version` and `metadata`: a sigrok `.sr` (§6).
3. Neither: refuse as an unreadable file.

Writers choose the output format by extension (`.sr` for sigrok, anything else for WireSkein).

### 2.4 Defined and reserved parts

| Name | Status | Contents |
| --- | --- | --- |
| `markers/markers.json` | Defined (§5.2) | Time markers and segments |
| `decode/annotations.json` | Defined (§5.3) | Decode results turned into rows and segments for display |
| `verify/` | Reserved | Check results (expected and actual, pass/fail) |
| `view/` | Reserved | Viewer display state |

- The contents of a reserved name are decided when it is defined. Writers MUST NOT use these names before they are defined.
- Defining a new part does not change `format` (old readers ignore it and carry it over, as in §2.2).
- Names inside `markers/` and `decode/` that are not in this table are also reserved.

## 3. `capture.json`

A UTF-8 JSON object.

The format name is held by `wireskein.json` (§2.1).

| Key | Type | Meaning |
| --- | --- | --- |
| `id` | string | ID of this capture. A 128-bit random number generated by the writer when the capture is taken, written as 32 lowercase hex characters. Carried over through format conversion (including round trips via `.sr` and VCD). Used to refer to this capture from other files (§5.1.1) |
| `tick_hz` | [integer, integer] | Tick rate as numerator and denominator (e.g. `[160000000, 3]`). The denominator is 1 or more |
| `ticks` | integer | Length of the capture (number of ticks). The maximum of the ends of all channels. For logic, `phase + n × step`; for analog, `t0_ticks + n × tick_hz / rate_hz` rounded up |
| `channels` | array | Channel list (table below). The order has no meaning (it is the default display order) |
| `meta` | object | Small facts about the capture (§3.3) |

- The numerators and denominators of fractions (`tick_hz`, `rate_hz`, `t0_ticks`) are integers with absolute value at most 2⁵³ − 1 (the range exactly representable by a JavaScript number). When a writer builds a fraction from a floating-point number, the denominator is at most 10⁹.

Each element of `channels`:

| Key | Type | Meaning |
| --- | --- | --- |
| `name` | string | Channel name (e.g. `PA5`). MUST NOT be duplicated within the file |
| `file` | string | Name of the data entry (e.g. `ch/0.bits`) |
| `encoding` | string | How the data is stored (§4). Handling of unknown values: §3.2 |
| `n` | integer | Number of samples |
| `step` | integer (1 or more) | How many ticks per sample (required for `bits` and `interval-*`; for `interval-*`, the number of ticks in the interval of one value) |
| `phase` | integer (0 or more) | Tick of the first sample. Sample k is at tick `phase + k × step` (required for `bits` and `interval-*`; for `interval-*`, the start of the first interval) |

### 3.1 Time of channels other than `bits`

An `encoding` other than `bits` may define its own way of expressing sample times. This is because the actual ADC rate is often not an integer multiple of the logic tick. Analog (§4.2, §4.3) uses the following keys.

| Key | Type | Meaning |
| --- | --- | --- |
| `rate_hz` | [integer, integer] | Sample rate of the channel (fraction; the denominator is 1 or more) |
| `t0_ticks` | [integer, integer] | Time of the first sample, in ticks (fraction; may be negative) |

The time of sample k, in ticks, is `t0_ticks + k × tick_hz / rate_hz`.

- `t0_ticks` is the estimate returned by the probe (the track's start offset combined with the offset `skew_ns` of the sequentially switched ADC channel).
- Alignment derived by analysis (offset and time scale) is not written back here; it goes into an attachment (§1, §5.1).

### 3.1.1 Channel acquisition (`acquisition`)

A channel of any `encoding` may have the optional key `acquisition` (an object). It records how the channel was acquired, whether or not analysis uses it. The keys whose meaning WireSkein defines are below. Other keys may be added freely.

| Key | Meaning |
| --- | --- |
| `pin` | The probe's channel number (the channel in the OEP plan). The ADC unit and number can be looked up from the chip in `meta.probe` and this number |
| `attenuation_db` | ADC attenuation (such as 0 / 2.5 / 6 / 11 dB on ESP32). Without it, the meaning of the raw value is undetermined |
| `reference` | ADC reference voltage (an object): `source` (`"vdd"`, `"internal"`, `"external"`), `mv` (that voltage, in mV), and `measured` (`true`: measured; `false` / absent: nominal). On MCUs whose reference voltage is the supply (such as CH32V003 and CH32X035, which run at either 3.3 V or 5 V), the meaning of the raw value is undetermined without it |
| `vrefint_raw` | Raw value of the internal reference voltage (Vrefint), measured at the same time. When the reference is the supply, it can be used to back-calculate the actual supply voltage |
| `vrefint_nominal_mv` | Nominal Vrefint voltage (mV; calibration information from OEP v1). Used together with `vrefint_raw` to back-calculate the supply voltage |
| `start_uncertainty_ns` | Uncertainty (±ns) of the time of the first sample of this channel (track). When several tracks are acquired together, it differs per track |
| `frontend` | The selected input front end (OEP's `frontend_used` and describe's `frontend`: number, measurable range `range_min_mv` / `range_max_mv`, attenuation) |

### 3.2 Unknown `encoding`

- Readers **MUST NOT read a channel with an unknown `encoding` as another `encoding`.**
- A reader that reads for analysis skips that channel and shows the user the names and `encoding` of the skipped channels. The other channels can be used as they are (the case of a logic-only reader reading a file where logic and analog coexist).
- Operations that rewrite the file (format conversion, saving to another file) MUST refuse if any channel was skipped, because data would be lost. Operations that only add entries to the file (attachments, notes) can proceed even if channels were skipped.

### 3.3 `meta` keys

Any keys may be included. The keys whose meaning WireSkein defines are:

| Key | Meaning |
| --- | --- |
| `start_ns` | Time of tick 0 on the probe's clock (ns since boot, integer) (OEP section's `start_ns`; an estimate) |
| `start_uncertainty_ns` | Uncertainty of `start_ns` (±ns; OEP section's `start_uncertainty_ns`; a guide, not a guarantee) |
| `trigger_tick` | Trigger position, in ticks (integer). In OEP, the logic section's `trigger_index` (in multirate, the base sample number; tick 0 is the section's base sample 0, so it is the tick as is). When there is no trigger, the key itself is omitted. The trigger position of an analog track is that channel's `acquisition.trigger_index` (a sample number of that channel) |
| `time_base_slipped` | `true`: the probe knows that sample times were delayed (OEP section flags bit 2). When there is no delay, the key itself is omitted |
| `probe` | Information about the acquiring device (an object). Keys are optional. The ones whose meaning WireSkein defines are: `model`, `firmware` (the probe), `chip`, `chip_revision` (the MCU acquired from), `clock` (correspondence between the probe clock and the host clock read before and after acquisition, OEP core §7.7: `before` / `after` with `host_ns` (host monotonic clock, midpoint of the round trip), `uptime_ns`, `boot_id`, `uncertainty_ns` (half the round trip); for the same boot_id, `rate_ppm` (deviation of the probe clock speed relative to the host) and `rate_ppm_uncertainty`), `generation` (OEP v1 acquisition generation: `{"logic": n, "analog": n}`, to track which start an acquisition came from), `calibration` (factory calibration values; `scheme` holds the scheme name and `raw` the raw values, stored as is without applying them. Example: ESP32 eFuse ADC calibration values) |

Large information, or information added later, goes into an attachment or a note, not `meta` (§5).

## 4. Channel data (`encoding`)

### 4.1 `bits` (logic)

- One bit per sample. Sample i is in bit `i mod 8` of byte `floor(i / 8)` (bit 0 = LSB).
- The length is `ceil(n / 8)` bytes. Bits beyond n in the last byte are 0 (readers ignore them).
- The value of sample k is treated as unchanged from tick `phase + k × step` until the tick of the next sample. Therefore a change of value (an edge) happened somewhere within the `step` ticks before that sample's tick. This is the channel's time resolution.

### 4.2 `analog` (analog, raw value)

A form that holds raw ADC values losslessly. Values of OEP's `oep.fixture.analog` are stored in this form.

| Key | Type | Meaning |
| --- | --- | --- |
| `width` | 8, 16, 32 | Bytes per sample × 8. Unsigned little-endian integer |
| `value_bits` | integer (optional) | Number of valid bits in the value (e.g. 12 for a 12-bit ADC). Values are 0 to 2^value_bits − 1 |
| `zero` | number (optional) | Raw value corresponding to 0 V |
| `scale_nv` | number (optional) | Quantity per raw value step (10⁻⁹ of the unit `unit`; nV for voltage) |
| `unit` | string (optional) | Unit of the converted value. Default `"V"` (e.g. `"A"` for a current sensor) |
| `rate_hz`, `t0_ticks` | §3.1 | Required |

- The data length is `n × width / 8` bytes.
- The converted value is `(value − zero) × scale_nv × 10⁻⁹` (in unit `unit`) (OEP's linear formula).
- **Clipped values**: when `value_bits` is present, values 0 and 2^value_bits − 1 only indicate that the input was at or below the lower end, or at or above the upper end, of the range respectively; they are not voltages (OEP capture §1.2 rule 6). If `scale_nv` is negative (an inverting front end), value 0 is the upper end. The file keeps the raw values, and readers treat them as "≤ lower end" and "≥ upper end" (checks mark as unchecked whatever this leaves undetermined; viewers show them with a mark). For voltage, it is **the voltage at the probe pin** (including ADC attenuation, but not external dividers outside the probe). Values with external dividers undone are derived at analysis time from the `acquisition` information. Without `zero` and `scale_nv`, the conversion to voltage is unknown (raw values only).
- **Values are stored raw.** Other conversions such as factory calibration (`meta.probe.calibration`) are not applied at the storage stage. They are chosen at analysis time (§1).
- The value of sample k is the value at the instant given by the time in §3.1 (it does not carry the "same until the next sample" meaning of logic `bits`).

### 4.3 `analog-f32` (analog, voltage)

float32 voltages (little endian). A form for importing data where only voltages are known, such as sigrok `.sr` or Saleae exports.

| Key | Type | Meaning |
| --- | --- | --- |
| `unit` | string (optional) | Unit of the values. Default `"V"` |
| `rate_hz`, `t0_ticks` | §3.1 | Required |

- The data length is `n × 4` bytes. NaN means "no value".

### 4.4 `interval-any`, `interval-latch` (logic summarized per interval)

A form that holds a logic line not per sample but as one value per interval of `step` ticks. OEP multirate (capture §5) any_active and edge_latch are stored in this form (the sample policy can be held with `bits` `step` and `phase`). Interval k is ticks `[phase + k × step, phase + (k + 1) × step)`.

| Key | Type | Meaning |
| --- | --- | --- |
| `step`, `phase` | §3 | Required. Interval length and start of the first interval |
| `active` | 0 or 1 | The level being watched (0 for active-low). Required |

- **`interval-any`**: 1 bit per value, laid out the same as `bits` (§4.1). If the line was `active` at any tick in the interval, the value is `active`; if never, the opposite.
- **`interval-latch`**: 2 bits per value; value k is in bits `2k` and `2k + 1` (bit numbering as in §4.1; bit 0 is the low bit). The length is `ceil(2n / 8)` bytes.
  - Bit 0: the level at the last tick of the interval.
  - Bit 1: whether the line changed to `active` within the interval (the previous tick was not `active`). The first tick of an interval is compared with the last tick of the previous interval. The first tick of the channel (`phase`) is compared with nothing.
- Values are summaries of intervals. They do not hold the position or number of changes (time uncertainty is `step` ticks).
- Checks and analysis answer only what can be stated with certainty from the summary. For example, an `active` value in `interval-any` means only "was active at some point"; it cannot be said that it was active the whole time. Only a lower bound on the number of pulses is known. Checks that cannot be decided are unchecked.
- `.sr` and VCD have no way to represent this form. A capture with channels in this form cannot be exported to those formats (refused).

### 4.5 Reserved (not yet defined)

The following will be defined as new `encoding` values when needed. **`format` does not change** (readers skip or refuse unknown `encoding`s, so old readers never misread new files; §3.2).

- **Edge lists** (a compact form for logic with few changes).

An ADC with parallel output whose pins are captured as logic and turned back into values is planned to be held as `analog` (§4.2), without creating a new `encoding` ([design decisions](design.md) §5.1).

## 5. Attachments and notes

- **Attachments** (`attach/<name>`): anything can go in, such as acquisition settings, wiring notes or analysis results (text, JSON, binary).
  - Names MUST NOT start with `/` and MUST NOT contain `..`.
  - The content type is indicated by the name's extension (e.g. `.json`, `.txt`, `.png`).
  - Replacing an attachment with the same name rewrites the whole file (a zip MUST NOT contain two entries with the same name).
- **Notes** (`notes/<number>.json`): append-only records.
  - The number is a 4-digit decimal starting at 1 (`0001`), incremented by 1 for each addition.
  - The content is a JSON object that MUST have `time` (ISO 8601, with time zone offset) and `content` (a string or a JSON value). Other keys (e.g. `kind`) are optional.
  - A written note MUST NOT be rewritten or deleted.
- Both can be added to an existing file just by adding entries to the zip. The channel data is not rewritten.

### 5.1 Time alignment (`attach/alignment.json`)

The track start times returned by the probe (`t0_ticks`) have limits (start offset, rate error; §1). Alignment derived by analysis goes into this attachment, without rewriting the channel data or `t0_ticks`. The reader can choose whether to use it.

A UTF-8 JSON object.

| Key | Type | Meaning |
| --- | --- | --- |
| `format` | string | `"wireskein-alignment/0"`. Readers MUST NOT use one with an unknown value |
| `channels` | object | From channel name to its correction (table below) |

Correction:

| Key | Type | Meaning |
| --- | --- | --- |
| `offset_ticks` | number | Offset a (ticks) |
| `scale` | number | Time scale b |
| `reference` | string | The logic channel used as reference |
| `via` | string | The channel matched against the reference (one that captured the same signal). It may be the corrected channel itself |
| `method` | string | How it was derived (currently `"edges"`: matching edge positions) |
| `matched` | integer | Number of matched edges |
| `residual_ticks` | number | Remaining error (root mean square over matched edges, in ticks) |
| Others | — | Conditions of the derivation (threshold, search range, etc.). Optional |

- For a time t (ticks) written in the file, the aligned time is `a + b × t`. For analog sample k, t is the time in §3.1.
- Channels taken by the same ADC run on the same track clock, so the correction derived from one channel (`via`) is written with the same values for the other channels too.
- The corrections in `channels` align analog channels to the logic tick within the same file. The reference is logic; logic channels are not corrected.
- When re-derived, the attachment is replaced (rewriting the whole file, per the rules in §5).

#### 5.1.1 Alignment to another file (`files`)

For viewing captures taken at the same time by different probes on one time axis. Neither original file is rewritten; a correspondence to the reference file is added to the `alignment.json` of the file being aligned.

| Key | Type | Meaning |
| --- | --- | --- |
| `files` | object (optional) | From the reference file's name (without path) to the correspondence in the table below |

Correspondence:

| Key | Type | Meaning |
| --- | --- | --- |
| `capture_id` | string | The `id` (§3) in the reference file's `capture.json`. To avoid confusing it with a different file of the same name |
| `offset_ticks` | number | a (ticks of the reference file) |
| `scale` | number | b (reference tick ÷ this file's tick; includes the drift between the two clocks) |
| `reference` | string | The matched logic channel in the reference file |
| `via` | string | The channel in this file that captured the same signal |
| `method`, `matched`, `residual_ticks`, etc. | — | Same as §5.1 |

- Logic tick t of this file corresponds to tick `a + b × t` of the reference file.
- Analog channels of this file are first aligned to this file's logic tick with the `channels` correction (if any), and then this correspondence is applied.
- Readers MUST NOT use this correspondence if `capture_id` does not match (a different file that only shares the name). `id` is carried over through format conversion, so the correspondence still works even if the reference is converted to `.sr` or similar and back.

### 5.2 Markers (`markers/markers.json`)

Time marks and segments placed by users or tools. Markers placed in a viewer also go here.

A UTF-8 JSON object.

| Key | Type | Meaning |
| --- | --- | --- |
| `format` | string | `"wireskein-markers/0"`. Readers MUST NOT use one with an unknown value |
| `markers` | array | Markers (table below). The order has no meaning |

Marker:

| Key | Type | Meaning |
| --- | --- | --- |
| `t` | number | Time (ticks; the ticks of §3, the value before alignment) |
| `end` | number (optional) | End of the segment (ticks). If present, the segment `t` to `end` |
| `label` | string | Name |
| `note` | string (optional) | Description |
| `by` | string (optional) | Who placed it (`"viewer"`, a tool name, etc.) |
| `time` | string (optional) | When it was placed (ISO 8601, with time zone offset) |

- Unlike notes (§5), markers may be rewritten. Rewriting rewrites the whole file (the same as replacing an attachment in §5).

### 5.3 Decode annotations (`decode/annotations.json`)

Decode results turned into rows and segments for the viewer to draw. They carry no protocol-specific meaning, so the viewer can draw them without knowing the protocol. Even a viewer without a server can display decode results if this entry is present.

A UTF-8 JSON object.

| Key | Type | Meaning |
| --- | --- | --- |
| `format` | string | `"wireskein-annotations/0"`. Readers MUST NOT use one with an unknown value |
| `source` | string (optional) | What produced it (e.g. `"wireskein analyze"`) |
| `rows` | array | Rows (table below) |

Row:

| Key | Type | Meaning |
| --- | --- | --- |
| `name` | string | Row name (e.g. `"i2c transactions"`) |
| `near` | string (optional) | Name of the channel under which this row is placed |
| `verdict`, `score` | string, number (optional) | Certainty of the judgment this row is based on (e.g. `"confirmed"`, `"likely"`, and a score from 0 to 1). Viewers show uncertain rows so that they are recognizable as such |
| `items` | array | Segments: `s` (start tick), `e` (end tick; optional, a point if absent), `text` (string to display), `level` (optional: `"ok"`, `"warn"`, `"error"`), `detail` (optional: the original value, JSON) |

- Ticks are the ticks of §3.
- When decoded again, the entry is replaced.

## 6. Relation to other formats

- **sigrok `.sr`**: one rate for all channels. On export, slow channels are padded to the tick rate. Each channel's `step` and `phase`, the exact `tick_hz`, `id` and `meta` go into `wireskein/sr-extra.json` inside the `.sr` (`"format": "wireskein-sr-extra/1"`). Attachments, notes and unknown entries (§2.2) also go into the `.sr` under the same names (the names of unknown entries are listed in `extras` of `wireskein/sr-extra.json`). sigrok ignores all of them. WireSkein reads them back and restores the original capture.
  - Export supports up to 32 logic channels.
  - The `.sr` is written at one rate because sigrok cannot correctly read an `.sr` with multiple devices (`[device N]`) at different rates (confirmed with sigrok-cli 0.7.2). sigrok ignores unknown entries, so the true rates can travel in the bundled `wireskein/sr-extra.json`.
  - **Analog**: `.sr` analog is float32 voltage at the same rate as logic.
    - On read, it becomes `analog-f32` (`rate_hz` is the `.sr` rate, `t0_ticks` is 0).
    - On write, only channels whose sample times line up with the tick (`tick_hz / rate_hz` and `t0_ticks` are integers) are written, and slow ones are padded to the tick rate. If any channel does not line up, the export refuses.
    - `analog` is written as voltage using `zero` and `scale_nv`. Without them, the export refuses. The raw values and `acquisition` go into `wireskein/sr-extra.json` and are restored as `analog` on read-back.
- **VCD**: writes only change points, so channels at different rates can be written without padding.
  - `$timescale` is the largest unit (1, 10, 100 × s to fs) of which the tick period is an exact integer multiple. Examples: 20 MHz (50 ns) gives 10 ns; 160/3 MHz (18.75 ns) gives 10 ps.
  - Analog is written as `real` variables (voltage; the raw value if there is no conversion).
  - The exact tick, each channel's `step` and `phase`, `acquisition`, `id`, `meta`, and the analog rates and starts go as JSON into `$comment wireskein {...} $end` (`"format": "wireskein-vcd-extra/1"`). Other tools ignore it; WireSkein reads it back and restores them.
  - Attachments and notes cannot go into VCD. `convert` reports this.
  - When reading VCD from other tools:
    - The tick is the greatest common divisor of the change times.
    - 1-bit variables become logic; vectors become one channel per bit (`NAME[k]`). x and z are read as 0, and their count goes into `meta.vcd_x_or_z`.
    - `real` is read as analog only when the change interval is constant (a sampled signal). Others are not read; their names are shown (so as not to create samples that do not exist).
  - sigrok reads `$timescale` as the sample rate (100 MHz in the 20 MHz example above; the change positions are the same).
- **Run (record)**: a test run saves each capture as `cNNNN.wireskein` (run format `wireskein-run/3`, [run format](run-format.md)). If a run kept appending to one zip, a crash midway would leave the zip's central directory unwritten and make the whole thing unreadable. So a run stays a directory, with a separate file for each capture.

## 7. Versioning

- Additions that readers can read under the current rules without misinterpreting them (new `meta` keys, new `encoding` values, new parts, new optional entries) do not change `format`.
- Other changes (changing the meaning of existing keys or the required entries) increment the `format` number. Readers do not read files with an unknown number (§2.1).
- There are no minor numbers within a version. Additive changes are absorbed by the rule that readers ignore what they do not know (§2.2, §3.2).
- Until WireSkein version 1, incompatible changes are allowed, and files with old numbers are not read ([design decisions](design.md) §1).
- The reserved parts (`verify/`, `view/`, §2.4) and edge lists (§4.5) will be defined later as additive changes.
