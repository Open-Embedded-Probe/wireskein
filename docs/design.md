# Design decisions and reasons

[日本語](design.ja.md)

This document collects the decisions made for the implementation, with their reasons. The rules for the file format and the run format are in [File format](wireskein-format.md) and [Run format](run-format.md).

## 1. Versions and compatibility

- **Until version 1, incompatible changes are allowed. Backward compatibility is not considered.** There is no path for reading files in old formats, and no aliases for old names. The format names (`wireskein/1`, `wireskein-run/3`) are bumped on every incompatible change, and unknown versions are rejected with a clear error.
  - Reason: so that mistakes in the formats and the API can be fixed now, while users are few. Silently reading an old format would misinterpret keys whose meaning has changed.

## 2. Implementation language and location

- **The reference implementation is developed in Python.** Its only dependency is numpy. The file format and the run format are defined as language-independent specifications, and other languages may implement them as long as they are compatible.
  - Reason: for verification in tests (given pins and protocol, read short segments), Python and numpy are enough. The only slow part is Python loops that step through one bit at a time; those are moved to numpy batch processing (kernels).
  - In the Python implementation, run logging and saving (`wireskein.runlog`, `wireskein.fileformat`) work with the standard library only, so that test scripts can import them as they are.
- **A native implementation (Rust etc.), if made, goes in a separate repository.** It is not added to this repository. The shared assets are the TOML definitions (`decl/`) and the evaluation corpus with ground truth (`corpus/fixtures/`).
  - For this reason, TOML expressions are not defined as "Python eval" (they are a restricted syntax, §3.2).

## 3. Decoding (`wireskein analyze`)

### 3.1 Stage structure

An unknown run is read through the following stages (`src/wireskein/_engine/staged.py`).

```text
Edges
 └ Common survey: idle, bit time, character length, local clock period, bursts, pin-to-pin correlation, CS-like boundaries
    └ Pin classification (static pins excluded) → pin groups (clock + highly correlated pins + select line / single pin)
       ├ Synchronous: SyncBits → Frames (CS / START・STOP / gaps) → Words → I²C, SPI, RVSWD, SWD, unknown synchronous serial
       ├ Asynchronous: RateBlocks (blocks per bit time) → Chars (with breaks) → UART, LIN, DMX512
       │    └ Timestamped bytes → lines, NMEA, Modbus RTU, markers / TX・RX pairs → SCPI
       └ Pulse: PulseSymbols → SWIO
                    RVSWD・SWIO → DMI → RISC-V Debug Module
```

Decisions and reasons:

- **Frequency, bit count and pin correlation are estimated in the common survey, not in the protocol analyzers.** Each analyzer does not repeat the same estimation; analyzers do not touch edges and receive typed streams (SyncBits, Chars, etc.). Adding a protocol only requires writing how to delimit and how to interpret.
- **A stage is split out when it reduces later processing or has two or more users.** A stage that does neither is merged. For example, RateBlocks → Chars is kept separate because UART, LIN and DMX512 share it.
- **What a stage cannot decide is passed down as sibling candidates.** The sampling edge, the glitch-filter width, the frame-delimiting threshold and similar choices are not fixed to one option; views for both are built, and the score from the plugin checks selects one. Fixing one choice in an early stage lost the correct answer in some cases.
- **Clocked protocols are not split on rate changes. UART is.** Protocols that sample bits on clock edges do not depend on frequency, so a rate change is only kept as an annotation. UART has no clock, so it is split into blocks per bit time before reading (for example, writes that raise the rate midway).
- **Timing estimation does not depend on frequency.** Clock-likeness, burst boundaries and phase between pins are measured as ratios to the neighboring intervals, not to a period of the whole run.

### 3.2 How protocols are written

- **Protocols that are closed within one frame are written declaratively in TOML** (`src/wireskein/decl/*.toml`: I²C, SPI, RVSWD, SWD, CAN). Word width, bit order, field extraction, fixed values, length rules, repeated words, parity, CRC, bit stuffing and conditional fields can all be written. The same protocol is not also written in Python and kept twice.
  - Reason: definitions cover most common patterns without code, validation is mechanical, and other implementations can use the same definitions.
  - Expressions are limited to integer arithmetic, comparison, logic, the ternary operator and min/max. Loops and variables spanning frames are not included.
  - Searches that a definition cannot express (such as re-framing RVSWD at stop intervals) are core processing called by name (`frame.reframe = "rvswd_stop"`). Exceptions are confined to the core and are not brought into definitions.
  - Scoring is shared in the interpreter; a definition selects evidence items with `[score]`. For example, SPI uses `count_frames`: each frame that matches the rules counts as one piece of evidence, and the probability that all k match by chance (for `8n`, (1/8)^k) is subtracted. This prevents a hypothesis in which a whole other bus fits into one CS window from tying with the true delimiting. I²C does not use this, because its START/STOP conditions are evidence of delimiting.
- **Anything with state or syntax spanning frames is written in Python.** For example, the RISC-V Debug Module (abstract commands on top of DMI) and SCPI.
- Embedded scripting languages (JS, Lua, WASM, etc.) are not adopted. TOML and Python are enough, and this avoids the cost of learning a third language.

### 3.3 Verdicts

- **Confirmed / likely / ambiguous** (`confirmed` / `likely` / `ambiguous`) is decided by the score and the difference from second place (the margin). It is not decided by the top score alone.
  - The margin is measured against the best explanation that forbids hypotheses equivalent to the claim. Comparing only within the same set of lines makes the margin 0 for hypotheses that differ only in the presence of CS or in bit order.
- **Avoiding false confirmation is the top priority.** Holding back when a correct answer exists is more acceptable than a false confirmation.
- **The amount of evidence is included in the score.** For each check, the "probability of passing by chance" is estimated and counted as its information content. This keeps a run with only a few frames from mistaking a chance match for confirmation.
- **Upper-layer support is returned to the lower layer, but absence of support is not penalized.** This lets raw binary with no upper layer still be confirmed.
- **An "unknown synchronous serial" is based only on what known rules cannot explain.** Hypotheses for known protocols are placed first, and an unknown hypothesis whose pins overlap them does not win over them.

### 3.4 How far to claim

- **Upper-layer interpretations are layered on top of lower-layer results without replacing them.** If the upper layer is wrong, the lower layer stays correct.
- **Automatic promotion goes only up to stages with evidence that can be cross-checked** (checksums, CRC, request/response matching, standard identifiers).
- **A device model is claimed only when there is identification evidence.** An "address-only match" without a matching identifier value or CRC is not claimed, only listed as candidates. Many models share I²C addresses. Models with the same command table that evidence cannot distinguish are combined into one claim (e.g. `SHT30 | SHT31`).
- Device definitions are written in types that follow how datasheets describe devices: command tables, register tables, message frames ([`decl/devices/`](../src/wireskein/decl/devices/README.md)).

### 3.5 Output and hints

- **Output is chosen to fit the next step.** `--mode final` (the top interpretation per claim; for CLI, scripts and AI), `all` (all stages with sample positions; for GUI annotations), `select` (select stages and items by path). Depth is `--depth transport|frames|protocol|device`.
  - Intermediate stages are kept as references to the typed streams the engine passed to plugins, so plugin authors need to do nothing.
- **Times are output as sample (tick) numbers.** Later steps can convert to seconds without reading the original run, and the GUI can go back to the original waveform.
- **Hints (protocol; per-pin protocol, role and baud rate; pins to exclude; choice of device definitions) only narrow the range tried.** Hints alone do not decide a conclusion; plugin checks run as usual.

## 4. Verification (`wireskein verify`)

- **Verification is not estimation.** The test passes pins, roles and expectations; decoding uses the given roles and the results are compared. Because it does not rely on automatic detection, results are stable, and even a single short transfer (weak as evidence for discovery) can be verified.
- **What could not be checked (`unchecked`) fails by default.** Most such cases are mistakes in the test or missing wiring or capture. What was requested as measured only (`measured`) does not fail.
- **Each tolerance's name shows whether it is relative or absolute** ([Run format](run-format.md) §2.3).
- **UART baud rate is measured independently of the expectation.** The bit width is found by least squares from the intervals between same-direction edges within one character; the expected value is used only to choose among candidates. Measuring in a window cut by the expectation would shift the measured value whenever the expectation is off.
- **Disturbances in the capture's time base are not hidden.** When the probe reports a delay (`time_base_slipped`), or when edges are off the grid only in the error characters, the judgment is not changed, but this is attached to the NG reason. This distinguishes receiver errors from stalls of a software-paced sampler.
- **One disturbance counts as one item.** In UART, after a framing error, re-reading waits for one character's worth of idle.
- **For summary channels (`interval-any`, `interval-latch`), answer only what the summary can definitely say.** Anything that cannot be decided is unchecked.

## 5. Files and time

The rules of the file format and the reason for each are in [File format](wireskein-format.md) §1. Key points:

- WireSkein has its own format (`.wireskein`) and keeps each channel at its own rate, with only the samples actually taken.
- ADC values are stored raw; conversion to voltage and correction are chosen at analysis time.
- Times returned by the probe are recorded as estimates, and alignment computed by analysis goes into a separate attachment.

### 5.1 Parallel-output ADC (not implemented yet)

For connecting a high-speed parallel-output ADC to the probe's digital inputs, **the probe does not know about the ADC; converting bits back to values is done on the analysis side.**

- If the probe only captures the data pins and CLK as an ordinary logic analyzer, the same board and the same conversion work with both OEP and sigrok.
- Board knowledge (pin order, code format, pipeline delay, input range) comes with the board; putting it in the probe would mean changing the probe every time the board changes.
- Converting to values on the probe does not reduce bandwidth, and with the raw bits, mistakes in the code format or delay can be fixed at analysis time.
- The converted values can be held with the current `analog` encoding. The board description goes in that channel's `acquisition`.

## 6. Evaluation

- Estimation correctness is measured on the corpus with ground truth (`corpus/`). Channel names are replaced with `D0…Dn` and their order shuffled, and the ground truth (`truth.json`) is kept where the estimator cannot see it.
- Seeds used for tuning and seeds not used, and noise conditions (glitches, mid-stream start, low sampling rate, jitter, rate changes), are measured separately to check for overfitting and weakness to noise.
- The evaluation scripts are in [`research/`](../research/README.md).
