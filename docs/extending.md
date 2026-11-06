# Analyses of your own

[日本語](extending.ja.md)

WireSkein turns a capture into what a reader needs - the buses on it, their messages, whether a test held - so the samples themselves need not be read. When what you need is not built in, add it at the layer it belongs to. This guide says which way fits what, what every addition follows, and how its results reach the viewer.

## 1. Which way

| You want | Use | Details |
| --- | --- | --- |
| A protocol that rides on UART, SPI or I2C (command frames, reports, packets) read in the analysis | A protocol object in Python (`wireskein.protocols`) | [Protocols](protocols.md) §1-§4 |
| Text commands on one UART line and their answers on another (AT commands, a shell) | A protocol object on `"uart_pair"` | [Protocols](protocols.md) §3.1 |
| A higher layer on CAN or LIN frames (CANopen, J1939, OBD-II / UDS, a node's signals), or on a protocol of your own definition | A protocol object on `"can"`, `"lin"` or the definition's name | [Protocols](protocols.md) §3.2 |
| Captures in a file format WireSkein does not read (another analyzer's export, a lab's own log) | A file format (`wireskein.formats`) | §2, `examples/formats/saleae_csv.py` |
| A protocol that is its own bus: its own framing on a clock and data, or on one NRZ line | A TOML definition in a definitions directory | [Protocols](protocols.md) §5 |
| The registers and commands of a device on I2C or SPI named | A device pack under `devices/` of a definitions directory | [Device packs](../src/wireskein/decl/devices/README.md) |
| The marker lines of your own test logs read as segments | A marker dialect under `markers/` of a definitions directory | [Markers](markers.md) §4 |
| A rule tests apply that no check has, judged alike by `wireskein verify`, pytest and the viewer | A check kind (`wireskein.checks`) | [Guide](capture-test-guide.md) §4.10 |
| A judgement only one test makes | `wireskein.verify.sections(run)`, an analysis of the segment, `rec.result(...)` | [Guide](capture-test-guide.md) §3.3 |
| Something to look at, not to judge | Read the capture in Python (`wireskein.capture`, `wireskein.states`, `wireskein.decode`) and put what you find in the file (§3) | [File format](wireskein-format.md) §5 |
| A line coding or a physical layer WireSkein does not read (Manchester, say) | Not an extension point: it needs a bit layer in WireSkein itself | Propose it |

The ways stack: a definition gives the analysis a protocol, a protocol object reads what rides on it, a check judges it in a test, and its messages and notices end up as annotations and markers in the file.

## 2. What every addition follows

- **Read the documented shapes.** A protocol gets the bus as the [analysis format](analysis-format.md) exports it, a check the segment's window ([guide](capture-test-guide.md) §4.10), a script the files of the [file format](wireskein-format.md) and the [run format](run-format.md). Within a format number (`/1`) these shapes do not change, so an addition built on them keeps working; WireSkein's internal types may change and are not used.
- **Show only what evidence carries.** A protocol counts the checks its frames pass and fail (a CRC, a length, a sequence number), and the analysis believes it only above chance; with no check it shows nothing. A check says what it measured whatever its verdict, and what the protocol allows but deserves a look is a notice at its tick, not a failure (guide §4.9).
- **Take the truth from elsewhere than what is tested.** Test an addition on signals made apart from the code that reads them: an encoder of your own in the test, or a probe sending with an established library (an OEP probe's ESP-IDF UART driver, say). The output of a device under test is never the truth: it may be the very thing that is wrong.
- **Names.** A protocol's or a check's name is letters, digits and `_`, and not one of WireSkein's own; WireSkein refuses one that is.
- **Failures stay visible.** An exception in a protocol's `decode` is shown with its bus and the analysis goes on; a check of a kind nobody added is `ng`, never skipped.
- **Packaging.** A package declares its additions as entry points, and they are there whenever it is installed:

| Entry point group | Adds | Value |
| --- | --- | --- |
| `wireskein.protocols` | A protocol on UART, SPI or I2C | An object with `name`, `over`, `decode` |
| `wireskein.definitions` | A definitions directory (protocols, device packs, marker dialects) | A module whose directory holds them, or a callable returning its path |
| `wireskein.checks` | A check kind | A check function, or an object with `.check` (and `.notices`) |
| `wireskein.sources` | A capture source for `wireskein capture` | A module with `capture(target, request)` |
| `wireskein.formats` | A file format read wherever a capture is (analyze, decode, states, convert, the viewer) | An object with `name`, `sniff(head, path)`, `read(path)` returning a `wireskein.capture.Capture` |

```toml
[project.entry-points."wireskein.protocols"]
acme_link = "acme_wireskein:PROTOCOL"

[project.entry-points."wireskein.definitions"]
acme = "acme_wireskein.definitions"
```

## 3. Results in the viewer

- **Annotations:** `wireskein annotate FILE --save` (or `wireskein.annotate.build_file` and `save`) writes the analysis as rows, the messages of an installed protocol among them. A message's `text` is what the viewer draws for it (else its fields), its `level` (`ok`, `warn`, `error`) its colour, its `s` and `e` its span.
- **Markers:** `wireskein.capture.add_markers(path, [{"t": tick, "label": ..., "end"?: tick, "part"?: k}], by="my_tool")` puts markers of your own in the file, in place of the ones your tool put there before; others' stay. `wireskein.capture.markers(path)` reads them all. Verification writes its notices and changes the same way (`by: "verify"`).
- **Notes and attachments:** `wireskein.capture.note(path, ...)` and `attach(path, name, data)` keep anything else with the capture without rewriting its channels.

## 4. From an unknown capture to an analysis of your own

1. `wireskein analyze FILE` says what it finds: a UART `likely`, say, with its bytes but nothing above them.
2. `wireskein states FILE` and the bytes tell the shape: a start byte, a length, a checksum.
3. Write the protocol (§1): `decode` finds the frames in the bytes and counts their checksums.
4. Test it on signals made apart from it: the frames encoded and put on a line by an encoder of your own in the test (a UART frame is a start bit, the data bits least significant first, a stop bit), written as a capture with `wireskein.capture`; or a probe sending them with its UART driver. (WireSkein's own tests use its internal generator; it is not an interface.)
5. `register` it (or install it): `wireskein analyze` now reads the UART `confirmed` with its frames under `result`.
6. In a test, `protocol(NAME, "uart", {"data": PIN}, messages=[...])` checks the frames of a segment.

## 4.1 Which shape is yours

| What is on the wire | Write | Its evidence | Example |
| --- | --- | --- | --- |
| Binary frames on one UART (a start byte, a length, a CRC) | A protocol on `"uart"`, reading `bytes` | The CRC, the length | `demo_link` |
| The same frames in SPI windows, request on MOSI and reply on MISO | The same protocol, `over = ("uart", "spi")`, reading `transfers` | The CRC, the reply's seq | `demo_link` |
| Text commands on one line and answers on the other (AT commands, a shell) | A protocol on `"uart_pair"`, reading both lines' `messages` | An answer that belongs to its command | `demo_cmd` |
| Signals in CAN or LIN frames | A protocol on `"can"` / `"lin"`, reading `frames` | A check byte, a counter | `demo_telemetry` |
| Registers of a chip on I2C or SPI | A device pack | An identity register that reads right | [Device packs](../src/wireskein/decl/devices/README.md) |
| Its own framing on one line or a clock and data | A TOML definition | Expected fields, a CRC | `demo_wire` |
| A rule on any line a test applies (a line that settles, an order of events) | A check kind | Measured values, notices at their ticks | `demo_settle` |

A protocol with no check of its own (plain text on one line, say) shows nothing in the analysis: give it one (an
answer that matches, a field that counts, a fixed shape strict enough not to happen by chance), or read the lines
in a test (`wireskein.verify.sections`, the analysis's `lines`) and judge them there.

## 5. Examples in the repository

| Example | Shows |
| --- | --- |
| `examples/protocols/demo_link.py` | A protocol on UART and SPI: framing, resynchronizing, a CRC-8 as evidence |
| `examples/protocols/demo_link_p4.wireskein` | demo_link sent by an OEP probe's ESP-IDF UART driver and captured, with what it sent as the truth |
| `examples/definitions/demo_wire.toml` | A protocol on one NRZ line: bit stuffing, a variable length, a CRC-8 |
| `tests/test_protocols.py` | Both examples tested on signals made apart from them, the protocol check, annotations and markers |
| `examples/protocols/demo_cmd.py` | Text commands and answers on a UART pair: the pairing as the evidence, events, levels for the viewer |
| `examples/protocols/demo_cmd_p4.wireskein` | demo_cmd sent by an OEP probe's two ESP-IDF UART drivers and captured, with what each sent as the truth |
| `examples/protocols/demo_telemetry.py` | A protocol on CAN and LIN frames: one decode for both, a check byte as evidence, a skipped counter shown |
| `examples/protocols/demo_telemetry_p4_can.wireskein`, `_lin.wireskein` | demo_telemetry sent by an OEP probe's ESP-IDF TWAI and UART drivers, with what it sent as the truth |
| `examples/formats/saleae_csv.py` | A file format: a Saleae Logic 2 digital CSV read as a capture (a tick of 1 ns, the lines by their headers) |
| `examples/checks/demo_settle.py` | A check kind: a line that must settle, its bounces as notices, measuring only without an expected level |
| `tests/test_demo_cmd.py` | demo_cmd in the analysis and in a test's protocol check |
| `tests/test_check_plugins.py` | A check kind added from outside, with its notices; demo_settle on a bouncing contact |
