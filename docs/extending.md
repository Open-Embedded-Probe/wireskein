# Analyses of your own

[日本語](extending.ja.md)

WireSkein turns a capture into what a reader needs - the buses on it, their messages, whether a test held - so the samples themselves need not be read. What is not built in is added in one of two ways, and neither runs code of yours inside WireSkein:

- **Tables** in a definitions directory, for what rides on the buses WireSkein reads: protocols of your own, device packs, marker dialects. A table reads what the analysis exports, so it works whatever finds the buses, and in any program that reads the analysis format.
- **Documented formats** in and out, for anything a table cannot say: a capture file goes in, the analysis and the run's verdicts come out as JSON, and your own code reads them.

What neither covers (a bus WireSkein does not read, a line coding, a check kind, a file format) is a request for WireSkein itself: propose it.

## 1. Which way

| You want | Use | Details |
| --- | --- | --- |
| Frames that ride on a UART, SPI or I2C (a start byte, a length, a CRC) | A protocol table, `[frame]` | [Protocols](protocols.md) §2 |
| A higher layer in CAN, LIN or DMX512 frames (a node's readings, a counter, a check byte) | A protocol table, `[frame]` on `"can"` / `"lin"` / `"dmx512"` | [Protocols](protocols.md) §1, §2 |
| Text commands on one UART line and their answers on another (AT commands, a shell) | A protocol table, `[lines]` on `"uart_pair"` | [Protocols](protocols.md) §3 |
| The registers and commands of a device on I2C or SPI named | A device pack under `devices/` | [Device packs](../src/wireskein/decl/devices/README.md) |
| The marker lines of your own test logs read as segments | A marker dialect under `markers/` | [Markers](markers.md) §4 |
| A protocol a table cannot say (state across frames, reassembly, a calculation of its own) | Your code on the analysis's output: `wireskein analyze FILE` (or `wireskein.decode.analyze_file`) and the bus's layers | [Analysis format](analysis-format.md) §4 |
| A judgement only one test makes | `wireskein.verify.sections(run)`, an analysis of the segment, `rec.result(...)` | [Guide](capture-test-guide.md) §3.3 |
| Captures in a file format WireSkein does not read | Convert them to a WireSkein file with `wireskein.capture` (`logic`, `analog_volts`, `Part`, `Capture`, `write`) | [File format](wireskein-format.md) |
| Something to look at, not to judge | Read the capture in Python (`wireskein.capture`, `wireskein.decode`) and put what you find in the file (§3) | [File format](wireskein-format.md) §5 |
| A kind of capture device WireSkein does not have | A driver added to WireSkein itself (Rust), not a plug-in | [Adding a capture device](devices.md) |
| A bus, a line coding, a check kind WireSkein does not have | Not an extension point | Propose it |

The ways stack: the analysis finds the buses, a protocol table, a device pack or a marker dialect reads what rides on them, a test's `protocol(...)` check judges it, and its messages end up as annotations and markers in the file.

## 2. A definitions directory

```
my_definitions/
  protocols/acme_link.toml        protocols of your own (wireskein-protocol/1)
  devices/i2c/acme/sensor.toml    device packs (wireskein-device/1)
  markers/acme.toml               marker dialects (wireskein-marker-dialect/1)
```

- **Adding it:** `wireskein --definitions DIR <command> ...` on the command line (repeatable), `wireskein.plugins.add_definitions(DIR)` in a script or a test (`remove_definitions(DIR)`), or the environment's `WIRESKEIN_DEFINITIONS` (directories separated by the path separator; the command line sets it, so a split read's worker processes have them too).
- **Checked when added:** a table in error (its format, a name, a bus, a check kind, a field it names) is refused then, not when a capture is read. A directory with tables at its top (`*.toml` beside `protocols/`) is refused: a protocol on the wire itself is part of WireSkein's analysis engine ([protocols](protocols.md) §5).
- **Names:** a protocol's is its owner's namespace and its own, `owner.name` (`acme.link`: lowercase letters, digits and `_`). WireSkein's own names have none (`src/wireskein/registry.toml`, the one ledger of them), so a name of yours never meets one of them, now or later. One name is declared once over all the directories added.
- **Read the documented shapes.** A table reads the bus as the [analysis format](analysis-format.md) exports it. Within a format number (`/1`) that does not change, so a table keeps working.
- **Show only what evidence carries.** A protocol counts the checks its frames pass and fail (a CRC, a sum, an answer that belongs to its command); the analysis believes it only above chance, and with no check passed it shows nothing.
- **Take the truth from elsewhere than what is tested.** Test a table on signals made apart from it: an encoder of your own in the test, or a probe sending with an established library (an OEP probe's ESP-IDF UART driver, say). The output of a device under test is never the truth.

## 3. Results in the viewer

- **Annotations:** `wireskein annotate FILE --save` (or `wireskein.decode.annotations_file` and `wireskein.capture.write_annotations`) writes the analysis as rows, the messages of a protocol of your own among them. A message's `text` is what the viewer draws for it (else its fields), its `level` (`ok`, `warn`, `error`) its colour, its `s` and `e` its span.
- **Markers:** `wireskein.capture.add_markers(path, [{"t": tick, "label": ..., "end"?: tick, "part"?: k}], by="my_tool")` puts markers of your own in the file, in place of the ones your tool put there before; others' stay. `wireskein.capture.markers(path)` reads them all. Verification writes its notices and changes the same way (`by: "verify"`).
- **Notes and attachments:** `wireskein.capture.note(path, ...)` and `attach(path, name, data)` keep anything else with the capture without rewriting its channels.

## 4. From an unknown capture to an analysis of your own

1. `wireskein analyze FILE` says what it finds: a UART `likely`, say, with its bytes but nothing above them.
2. `wireskein states FILE` and the bytes tell the shape: a start byte, a length, a checksum.
3. Write the table ([protocols](protocols.md) §2): the fields in order, the check over some of them.
4. Test it on signals made apart from it: the frames encoded and put on a line by an encoder of your own in the test (a UART frame is a start bit, the data bits least significant first, a stop bit), written as a capture with `wireskein.capture`; or a probe sending them with its UART driver.
5. `wireskein --definitions DIR analyze FILE` now reads the UART `confirmed` with its frames under `result`.
6. In a test, `protocol(NAME, "uart", {"data": PIN}, messages=[...])` checks the frames of a segment.

A protocol with no check of its own (plain text on one line, say) shows nothing in the analysis: give it one (an answer that matches, a field that counts, a fixed shape strict enough not to happen by chance), or read the lines in a test (`wireskein.verify.sections`, the analysis's `lines`) and judge them there.

## 5. Examples in the repository

`examples/` is a definitions directory: `wireskein --definitions examples analyze examples/protocols/demo_link_p4.wireskein`.

| Example | Shows |
| --- | --- |
| `examples/protocols/demo_link.toml` | Frames on UART and SPI: a start byte, a length, a flag in a byte, a CRC-8 as evidence |
| `examples/protocols/demo_cmd.toml` | Text commands and answers on a UART pair: the pairing as the evidence, events, levels for the viewer |
| `examples/protocols/demo_telemetry.toml` | Frames in CAN and LIN frames: a check byte seeded by the identifier, a counter per identifier, a text for the viewer |
| `examples/protocols/*_p4.wireskein` | Each sent by an OEP probe's ESP-IDF drivers (UART, TWAI) and captured, with what it sent in `*.truth.json` |
| `tests/test_protocols.py`, `tests/test_demo_cmd.py`, `tests/test_demo_telemetry.py` | The examples tested on signals made apart from them, the protocol check in a test, annotations and markers |
| `tests/test_plugins.py` | The names refused, a definitions directory from the command line and the environment |
