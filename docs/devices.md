# Adding a capture device

[日本語](devices.ja.md)

WireSkein captures from a device through a **driver**, one for each kind of device. A source names its kind first: `wireskein capture --source oep:oep://30eda0ea068b` uses the `oep` driver, `--source demo:counter` the demo device's. The drivers are WireSkein's own: a new kind of device is a driver added to WireSkein (nothing is plugged in from outside), written in Rust against one interface, `crates/wireskein-capture`. Everything after the device - the budget the received bytes wait in, putting them together, losses, the files - is shared, so a driver only speaks to its device.

This document says what the interface is (§1), how to add a driver, taking the demo device as the example (§2), what a device that keeps its samples in its own memory and a device that needs its firmware sent each time do differently (§3), and how to write a device's protocol by watching it, without reading code whose licence WireSkein cannot take (§4).

## 1. The interface

`crates/wireskein-capture` (module `driver`):

| Part | What it is |
| --- | --- |
| `Driver` | A kind: `kind()` (the source's prefix), `about()`, `list()` (the devices that can be seen now, each as a `Found` with its source), `open(address, &Open)` |
| `Device` | An open device, held until `close()`: `facts()` (model, serial, firmware: the capture's `meta.probe`), `hold()`, `caps()`, `capture(&Request, &Ctl)` |
| `Hold` | How it is held against other users: `Lease` (OEP: the probe keeps it, renewed while the capture runs, lapsing if the host dies), `Lock` (a claimed USB interface, a tty's lock, a lock file), `None` |
| `Caps` | What it can do: its lines, the rates it takes, `stream` and/or `memory` with their limits (`RateLimit`: up to so many lines, up to so many samples a second; a memory's bytes; whether it keeps samples before a trigger), modes, trigger kinds, thresholds, reductions, `firmware` it needs sent. `Caps::check(&Request)` refuses what is beyond them, with the reason |
| `Request` | What to capture, the same for every device (Python's `wireskein.sources.Request`): lines by name and the device's id, rate, samples or duration, trigger and pretrigger, mode (`OneShot`, `Repeat`, `Stream`), on loss stop or continue, where a stream's files go, a threshold |
| `Ctl` | The caller's hold on a capture as it runs: `stop()` (from another thread), and hearing its `Progress` (taking or reading out, bytes so far and of how many, the backlog, losses) |
| `Acquired` | The capture put together: parts (blocks of ticks with no gap inside), each part's lines, what was lost, meta |
| `Registry` | The kinds a program knows; `crates/wireskein-sources::builtin()` is WireSkein's (`demo`, `oep`) |

The shared flow (modules `flow`, `stream`, `spool`, `packed`):

- **The IN side never waits for the host** (`stream::Inlet`). What came in waits in memory within **one budget in bytes** over everything received and not yet written out (512 MiB by default; `WIRESKEIN_CAPTURE_FLOW=budget=MIB`). A disk that stalls or a busy host only grows the backlog. Past the budget a streaming device keeps what it can in its own buffer and drops the rest (`Inlet::try_put`), which shows as a gap in the stream's positions; a device read out at the host's pace waits instead (`Inlet::put`).
- **Gaps are losses** (`spool::Spool`). Bytes come with their position in the stream; a skipped position ends the part and is recorded as a loss. A loss makes the capture a failure, and what was taken is kept (`--on-loss stop`, the default, stops at the first; `continue` goes to the end). A capture may hold several parts in one file.
- **Packed samples into lines** (`packed`). Most logic analysers send N lines packed in 1, 2 or 4 bytes a sample. `Format::Packed { width, positions }` takes each line's bits out (8 x 8 bit transposes; any width works, whole bytes are the fast case), and the spool writes them to disk as they come. A device with a layout of its own (OEP's capture format 1) gives a `spool::BlockLayout`.
- **Written out** by wireskein-io (`save::to_capture`, `save::save`), with the flow's counts in `meta.probe.link.flow`.

## 2. Adding a driver

The demo device (`crates/wireskein-capture/src/demo.rs`) is the example to follow: a device with no hardware that sends a counter (line Dk is bit k of the sample's index) as packed samples. Its options make the shared flow show what it does: `type=stream|memory`, `pace=real|fast`, `drop=K` (drop chunk K: a loss), `stall=MS` (the spool stands still: the budget holds it), `fail=K` (the device stops in error), `budget=MIB`, `firmware=FILE`.

```
wireskein-rs capture --source 'demo:counter?lines=16' --channels A=D0,B=D5 --rate 1M --samples 100k -o demo.wireskein
wireskein-rs capture --source 'demo:?lines=8&drop=3' --channels A=D0 --rate 1M --samples 100k --on-loss continue -o lost.wireskein
wireskein-rs capture --list
```

To add a kind:

1. **A crate of its own** (`crates/wireskein-<kind>`), depending on `wireskein-capture` (and `nusb` for USB: pure Rust, no libusb). Add it to the workspace and to `wireskein-sources::builtin()`.
2. **`Driver::list`**: the devices of this kind that are plugged in (by VID:PID, an interface's class, a serial number), each as `<kind>:<address>`.
3. **`Driver::open`**: parse the address, open the device, hold it (claim the USB interface; `Hold::Lock`), send its firmware if it needs one (§3), ask what it says about itself.
4. **`Device::caps`**: its lines, `stream` and/or `memory` with their limits by line count, triggers, thresholds. `capture` starts with `caps.check(req)`: a request beyond the device is refused before anything is sent.
5. **`Device::capture`**: set the device (rate, lines, threshold, trigger), start it, and take its data through the shared flow - `stream::pipe(flow)` gives the IN side and the reader; the IN side (a thread with many USB IN transfers queued, as `crates/wireskein-oep/src/transport.rs` does) puts each transfer's bytes with its stream position; `stream::collect` reads them into a `SpoolWorker` with `Format::Packed` until the duration, `Ctl::stop`, or a loss with on-loss stop. Then stop the device and return `Acquired` with the device's facts and the flow's report in `meta.probe`. A device that stopped in error adds a loss (`stopped`), keeping what came.
6. **`Device::close`**: stop it, let the interface go.
7. **Tests without the device**: a stand-in IN side that gives the bytes your protocol notes (§4) say the device sends, through `stream::pipe`, as the demo's tests do (a stall within the budget loses nothing, past it is a loss, a drop is a loss, an error stop keeps what came). With the device: the counts in `meta.probe` and a known signal (a probe's own output, a signal generator), never the device under test's own idea of what it sent.

Python reaches every Rust driver through `wireskein_core.capture_source(source, request, owner)`; a kind with no Python implementation (the demo) is a module under `src/wireskein/sources/` that calls it.

## 3. Two other shapes of device

**A device that keeps its samples in its own memory** (fast captures, read out after): its `Caps::memory` says how many bytes, how a sample of so many lines is packed in it (`sample_bits`), the rates by line count, and whether it keeps samples before a trigger. `Caps::check` refuses `samples x segments` beyond `MemoryCaps::samples_for(lines)` and a pretrigger it cannot keep. `capture` arms it, waits for the trigger and the samples after it (telling `Ctl` `Stage::Taking`, and stopping early on `Ctl::stop`), then reads the memory out through the same flow, with `Inlet::put` (the device waits for the host, so nothing is dropped) and `Stage::Reading` progress with `of_bytes` the memory's size. The demo's `type=memory` (with `read=BYTES_A_SECOND` for a slow read-out) does exactly this; its tests check the trigger's sample, the pretrigger and both stages.

**A device that needs its firmware or FPGA configuration sent each time it is opened** (an FX2 or FX3, an FPGA analyser): WireSkein does not ship such files, which are the vendor's. The driver names each one in `Caps::firmware` (`FirmwareNeed`: the file's name, what it is, where a user gets it, where the vendor's application installs it) and finds it in `open` with `driver::find_firmware(kind, need)`, which looks, in order, at `WIRESKEIN_FIRMWARE_<KIND>` (the file or its directory), `WIRESKEIN_FIRMWARE_DIR`, WireSkein's directory for the kind (`~/.config/wireskein/firmware/<kind>/` on Linux, `~/Library/Application Support/...` on macOS, `%APPDATA%\wireskein\firmware\<kind>\` on Windows), then the vendor's own places. When it is nowhere, the error says what the file is, where to get it, every place it looked and the variable to set. Check the vendor's terms on whether a user's copy may be sent by another program before relying on it. The demo's `firmware=FILE` exercises the lookup and the error.

## 4. Writing a protocol by watching the device

Many vendors' host software, and the community drivers for their devices, are under the GPL. WireSkein is MIT: its drivers are written **without reading, copying or writing alongside GPL code** for the device. The protocol is learned by watching the vendor's application talk to the device, and the driver is written from those notes by someone who has not read that code.

1. **Record the traffic** on Linux with usbmon and Wireshark:
   ```
   sudo modprobe usbmon
   lsusb -d 1a86:5537          # the bus and device number (Bus 003 Device 007)
   sudo wireshark -i usbmon3 -k   # or: sudo tshark -i usbmon3 -w run.pcapng
   ```
   Filter on the device (`usb.device_address == 7`). Run the vendor's application (on Linux, or in a Windows VM with the device passed through) and do one thing at a time, noting the time of each: plug in, open, change one setting (the rate, then the lines, then the threshold, then the trigger), start a short capture, stop, close. Record each step in its own file.
2. **Read the records** for:
   - **finding it**: VID:PID, the serial string, the interfaces and endpoints (`lsusb -v`);
   - **opening**: control transfers (`bmRequestType`, `bRequest`, `wValue`, `wIndex`, data) and any firmware upload (a long run of writes after plug-in; the device re-enumerating);
   - **settings**: which bytes change when only the rate changes, only the lines, only the threshold; the rate's encoding (a divider of a base clock?);
   - **start and stop**: the command, and what the device answers;
   - **the data**: the bulk IN endpoint, the transfer sizes, how samples are packed (capture a known signal - a probe's own square wave of known frequency on one line, the others tied low - and find its period in the bytes), any header or counter in each transfer (it shows a dropped transfer), what the end looks like;
   - **errors**: what happens when the host is too slow (unplug a disk, load the CPU) - does the device stop, mark the gap, or drop silently?
3. **Write the notes** in `docs/instruments/<device>-protocol.md`: each message with its fields, what was seen and how sure it is, the records it came from (by name, date, application version). Raw records stay out of the repository (they hold the vendor's firmware when it is uploaded, and serial numbers): keep them with the date and the application's version where your bench keeps its data, and quote only the bytes a note needs.
4. **The driver** is written from the notes alone, by someone who has not read the vendor's or any GPL driver's code; its stand-in tests (§2.7) give the bytes the notes describe. What the notes do not settle is recorded again, not looked up in such code.
