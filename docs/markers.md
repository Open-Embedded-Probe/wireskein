# Markers

[日本語](markers.ja.md)

Markers divide a long run or a test run into segments by experimental context (which test, which step). Segmenting is done in `src/wireskein/_engine/markers.py`, and the syntax definitions (dialects) are in `src/wireskein/decl/markers/*.toml`.

## 1. Heading style (default dialect `heading`)

```text
# test_pwm                 ← level 1: test
## duty=64                 ← level 2: step (one condition)
PWM 64                     ← non-marker line: event
PWM duty=64
##                         ← same level with no name: closes the step
## duty=128
...
#                          ← closes the test
```

| Rule | Contents |
| --- | --- |
| Level | The number of `#` at the start of the line. No depth limit (three levels — test, step, phase — is the guideline) |
| Closing | Closed by the next heading at the same or a higher level. A heading with no name (just `##`) only closes and does not create a new segment |
| Segment path | Heading names joined with `/` (e.g. `test_pwm/duty=64`) |
| Same name | When names collide under the same parent, the first keeps the name as is, and from the second on numbers are appended: `duty=64[1]`, `duty=64[2]`. The original string is kept |
| Parent and child | Selecting a parent segment covers the whole range including its children. Child headings appear as events inside the parent |
| Broken structure | A skipped level (`###` after `#`) and a close with no matching heading are recorded as "problems". They are not silently fixed. A segment still open when the run ends is closed at the end of the run |

Reasons:

- **The heading style was chosen because** it is human-readable, the segment tree is determined just by counting levels, and it can be written with the same intuition as Markdown.
- **Numbering starts from the second occurrence so that** adding a segment with the same name later does not change the paths of earlier segments (or the expectations attached to them).
- **Broken structure is reported as a problem because** when the structure breaks, a capture may be checked against another step's expectations. In verification (`wireskein verify`) it is NG (`markers`).

## 2. Non-marker lines are events

Lines that do not match the marker syntax (commands, responses, parameters) are kept inside the segment as timestamped events. This shows, per segment, "which command was sent where". Events are mainly for the GUI and are not included in CLI output by default (`wireskein analyze --events`, `wireskein segments --events`, `wireskein verify --json ... --log`).

## 3. Where markers go

### 3.1 PC-side log (test run)

`wireskein.runlog.Recorder` writes headings to the PC-side log (`run.json`). A capture is assigned to the segment that was open when it was taken ([Run format](run-format.md) §2.2).

In short tests where commands are sent over a debug line or another path and each step's capture is a single short shot, there is no place in the waveform to put markers. So the markers are kept on the PC side, in a form where **the DUT firmware does not need to change**.

### 3.2 In the waveform (sent over UART)

In long runs, the DUT or a test tool sends marker lines over UART, and `wireskein segments` or `wireskein analyze --segment PATH` divides the run into segments.

- A segment runs from the moment the marker line finishes transmitting to the moment the closing line starts transmitting. The marker traffic itself is not part of the segment.
- When markers are sent over the same UART as commands, the DUT's command handling needs these rules:
  - Lines starting with `#` are discarded as comments, with no response and no echo.
  - Command lines do not start with `#`.

## 4. Other dialects

Existing run syntaxes are read by adding a TOML dialect to `decl/markers/`. There are four line kinds.

| Kind | Meaning |
| --- | --- |
| `begin` | Opens a segment at that level (closes segments at the same or a deeper level) |
| `end` | Closes the nearest segment at the same level (matching name or number). A segment closed without an `end` is recorded as a problem |
| `point` | A segment that lasts until the next `point` at the same level, or the end of the parent |
| `attr` | Attaches a key and value to the innermost segment (as JSON if it parses as JSON) |

Example: `i2cdb` (a syntax that wraps with `CASE_BEGIN` / `CASE_END`, treats `PHASE` as segments, and `INPUT` and `RESULT` as attributes).
