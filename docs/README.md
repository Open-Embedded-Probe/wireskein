# WireSkein documents

[日本語](README.ja.md)

The repository's [README](../README.md) gives an overview of using WireSkein. This folder holds the specifications and the design decisions. Each document has a Japanese version (`*.ja.md`) with the same content.

| Document | Contents |
| --- | --- |
| [Purpose and scope](concept.md) | The problems to solve, the properties that matter, what is out of scope |
| [Design decisions and reasons](design.md) | Versions and compatibility, implementation language, the decoding stages and verdicts, how checks work, evaluation |
| [The WireSkein file format](wireskein-format.md) | The `.wireskein` specification: container, `capture.json`, channel `encoding`s, attachments, notes, markers, annotations, relation to `.sr` and VCD |
| [The run format](run-format.md) | Recorded test runs (`run.json`) and check results |
| [Markers](markers.md) | Heading markers and the segment rules |
| [Checking captures in tests](capture-test-guide.md) | How to record a test run, capture windows, the list of checks, how to choose tolerances |
