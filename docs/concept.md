# Purpose and scope of WireSkein

[日本語](concept.ja.md)

## Goal

WireSkein is a tool that extracts the information a question needs from observation records such as logic analyzer captures, and hands it to later steps (test pass/fail judgment, scripts, people, AI). Instead of ending with a person looking at waveforms on a screen, it checks "is this as expected" in code, and outputs "what was transmitted" as structured data.

## Problems to solve

| Problem | WireSkein's approach |
| --- | --- |
| **Locked to hardware and licenses.** The easier an app is to use, the more it assumes its own dedicated hardware, and the harder it is to combine with the logic analyzer at hand or a homemade probe. Extending it requires building the main program, and the license does not fit embedding | Any source works (sigrok `.sr`, VCD, sigrok-supported devices, OEP probes). MIT license. Protocols and devices are added just by adding TOML |
| **Not usable as tests.** There is no way to compare a capture against expectations and output OK / NG | Expectations are written as code inside the test, and `wireskein verify` checks them with measured values ([Run format](run-format.md), [Testing with captures](capture-test-guide.md)) |
| **Too much information, yet important things get dropped.** Reading every waveform every time is heavy. There was a case where summarizing discarded a transfer that sped up midway as noise | Output only the layers, segments and items needed. Uninterpreted segments, the grounds for each candidate, and acquisition quality are not hidden |
| **Every use of the output needs processing.** People re-parse display strings and write conversion scripts per tool | Output is JSON with a stable structure. Times are integer ticks, so you can go back to the original capture |
| **Split by record type.** Logic, analog and other instruments' records are viewed in separate tools and their times are aligned by hand | Logic and analog are kept in one file, and times are aligned from edges of the same signal ([File format](wireskein-format.md)) |

## Properties we value

- **Never treat the uninterpreted as interpreted.** Return what could be read and what could not, and what was checked and what could not be checked, as distinct. In verification, what could not be checked fails by default.
- **Never rewrite the original data.** Alignment and annotations are added as separate items; the samples taken and the times recorded stay as they are. Never create samples that were not taken (do not pad decimated channels).
- **CLI and JSON first.** All functions are usable without a screen. The GUI (viewer) displays the same files and results.
- **The caller decides the target and the processing.** Automatic detection, too, is one processing step that the caller chooses and runs. Test progression and pass/fail decisions are handled by the external test environment.

## Out of scope

- An environment that controls a whole experiment, or a general-purpose notebook.
- A mechanism that always correctly identifies any unknown signal. Estimation returns candidates and grounds, and shows what is uncertain as uncertain.
