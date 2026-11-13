# WeAct's U3 logic analyzer (CH32H417)

[日本語](weact-u3-logic-analyzer.ja.md)

This note is about WeAct Studio's Logic Analyzer V2 (CH32H417, USB 3.0, 200 MHz × 16 lines) and U3LogicAnalyzer, the application that runs it on the PC. It is input for deciding whether WireSkein can read its captures and how the two fit together.

What was read:
- The application: [WeActStudio.U3LogicAnalyzer](https://github.com/WeActStudio/WeActStudio.U3LogicAnalyzer) (commit 54a6d9b, 2026-10-28).
- The hardware: [WeActStudio.LogicAnalyzerV2](https://github.com/WeActStudio/WeActStudio.LogicAnalyzerV2) (commit 2d10d93, 2026-11-10).

Everything here comes from the source and the READMEs.

## In short

- The application is a modified sigrok (libsigrok, libsigrokdecode and PulseView). Its source is public under GPL-3.0-or-later.
- It saves sigrok's ordinary `.sr` (srzip), which WireSkein reads as it is.
  - 16 lines take 2-byte samples and 8 lines 1-byte ones, and `samplerate` is written with a fraction, like `187.5 MHz`. WireSkein's reading handles both.
- Talking to the device is simple.
  - Commands go out as 64-byte packets, and answers come back as 32-byte packets.
  - Samples stream in as raw bulk transfers with no header.
  - So WireSkein could also drive the device itself.
  - The stream has no sequence numbers, though, so a gap inside it cannot be seen in the data.
- Upstream sigrok does not have this driver.
  - No sigrok-cli comes with it, so capturing from the command line means building one.

## Source, languages, layout, licence

- The PC application is public. It is C (libsigrok), C++/Qt (PulseView) and Python (libsigrokdecode's decoders), built with CMake.
  - The executable is called `U3LogicAnalyzer`.
- The licence is GPL-3.0-or-later.
  - The CH375 driver and the parts left alone are copyright Q2H2, whose project this is a modification of.
  - The libusb driver is copyright WeAct Studio.
- The hardware drivers are cut down to four:
  - `wch-ch32h417`: uses Windows' CH375DLL / CH375WDM.sys. Windows only.
  - `wch-ch32h417-libusb`: uses libusb. It runs on Linux, and on Windows after a WinUSB rebind with Zadig. It has not been ported to macOS (the README).
  - `fx2lafw`: the same as upstream.
  - `demo`.
- The changes are in these places:
  - libsigrok's `src/hardware/wch-ch32h417*/`.
  - `src/output/srzip.c`: an 8-line capture is saved 8 lines wide.
  - Configuration keys of its own, such as `SR_CONF_LINK_STOP_REASON`, `SR_CONF_ADC_PRECISION` and `SR_CONF_ADC_CHANNEL`.
  - PulseView's windows, including one for the firmware update (IAP).
- The device's firmware:
  - The logic analyzer app (slot 0) and the USB-serial + CMSIS-DAP app (slot 1) are published as binaries only.
  - Only the example user app (slot 2) has its source published.
  - What the firmware does can only be inferred from the driver's comments.

## Talking to the device

### USB

| Item | Value |
| --- | --- |
| VID:PID (running) | `1a86:5537` (WCH's VID) |
| VID:PID (firmware update) | `1a86:fe17` (HID) |
| Interface | one, number 0; configuration 1 |
| EP 0x01 / 0x81 | commands: 64 bytes out, 32 bytes in |
| EP 0x02 / 0x82 | the logic sample stream (IN is 0x82) |
| EP 0x03 / 0x83 | the ADC sample stream (only on the models that have an ADC) |

- No vendor control requests are used; everything is bulk.
- At SuperSpeed:
  - wMaxPacketSize is 1024 and bMaxBurst is 15, so one burst is 16 kB.
  - The driver keeps 32 transfers of 256 kB (8 MB in all) queued to receive into.
- On Linux, the udev rules in `99-weact-ch32h417.rules` let the device be opened without root.

### Commands

A packet sent is `[cmd, len, data…]`, padded with zeros to 64 bytes. An answer is `[resp, len, status, payload…]`.

- When `len` is above 0 there is a status byte, and 0 means success.
- The payload is `len - 1` bytes.

| cmd | Answer | Contents |
| --- | --- | --- |
| `0xA0` logic settings | `0xB0` | `[width, divider index]`; width `0x08` (1 byte) or `0x10` (2 bytes) |
| `0xA1` threshold | `0xB1` | 2 bytes, high byte first (see below) |
| `0xA2` start | none | |
| `0xA3` stop | none | |
| `0xA4` / `0xA5` | `0xB4` / `0xB5` | read back the settings / the threshold |
| `0xA6`–`0xAB` | `0xB6`–`0xBB` | ADC settings, channel, start, stop and read-back |
| `0xAC` version | `0xBC` | `[software version, hardware version]`; some firmware sends only the first |
| `0xAE` | | enter IAP |

The divider index is the position in the driver's table.
- USB 3.0 has 25 steps: 200, 187.5, 175, 162.5, 156.25, 150, 137.5, 125, 112.5, 100, 93.75, 87.5, 81.25, 78.125, 75, 68.75, 50, 40, 30, 20, 16, 12, 8, 6 and 4 MHz.
- On USB 2.0 the firmware adds an offset to the index (`UHSIF_Clock_Set()`, according to the driver's comment).
  - 16 lines: 6 steps, 20 down to 4 MHz.
  - 8 lines: 8 steps, 40 down to 4 MHz.
- A serial number starting with `b5` is a V2, which has the whole table.
  - A serial number starting with `01` (a CH32H417 board with an ADC) gets only the first 14 steps on USB 3.0 and only one on USB 2.0.

The threshold:
- On a V2 (`b5`), the threshold picked in the window (0.6–1.5 V at the input pins) is turned into an input level (1.0–3.3 V) before it is sent.
  - It is sent in 0.1 V steps (10–33), plus `0x8000`.
  - The conversion is a straight line through 0.6 V → 1.0 V and 1.5 V → 3.3 V (measured, says the comment).
- On a `01` model, a DAC value of 0–1024 is sent: 0 is about 3.3 V and 1024 about 1.2 V.

### How the samples are laid out

- EP 0x82 carries samples one after another, with no header.
  - At width `0x10` a sample is 2 bytes; at `0x08` it is 1 byte.
  - Bit n is Dn (channel n is given bit n).
  - It is libsigrok logic data, so a 2-byte sample has its low byte first.
- The channel count is picked from 2, 4, 8 and 16, but what is sent is 1 byte for up to 8 lines and 2 bytes above that.
  - 2 or 4 lines are still 1 byte.
  - Lines that were not picked are in the data too.
- The terminals D0–D15 are PD10–15, PF0–2, PC6–9 and PA13–15 (the hardware's README).
  - That they are UHSIF's PORT16–31 is from core's reading of the reference manual.

### Triggers and stopping

- The device has no trigger unit.
  - The trigger and the pre-trigger share are done on the PC: libsigrok's software trigger (`soft_trigger_logic`) cuts them out of the stream.
- Samples are held in the PC's RAM.
  - The application works from a ceiling of about 18 GB (about 45 s at 200 MHz and 16 lines).
  - Below 512 MB of free memory it stops and keeps what it has.
- The driver's comments say the device stops sending by itself when the USB link cannot carry the sample rate. The firmware's source is not public, so this cannot be checked.
  - The driver stops after 500 ms without data and records `LINK_STOP_NO_DATA`.
  - The window tells the user, but the `.sr` does not keep the reason.
  - A capture that stopped early looks like a short capture in the file.
- The stream has neither sequence numbers nor times, so if the device threw part of it away the PC could not tell.
  - If the device really stops whenever the link falls short, the middle of a capture is never missing. That cannot be checked while the firmware is closed.

## Saving and decoders

- Save / Save As writes a sigrok session file (`.sr`, a zip).
  - The `samplerate` in `metadata` is written like `187.5 MHz`.
  - An 8-line capture is saved with `total probes=8` and `unitsize=1`.
  - Saving a range only, and saving the setup only, are there too.
- For export it has libsigrok's outputs (VCD, CSV, binary and others).
- For import it has libsigrok's inputs (VCD, CSV, Saleae and others).
- There are 132 libsigrokdecode decoders in `libsigrokdecode/decoders/` (`swd` is there; RVSWD is not).
  - They were not compared with upstream's.

## Ways to use it from WireSkein

1. **Read the `.sr` (works now).**
   - Capture in U3LogicAnalyzer and save a `.sr`, and WireSkein reads it as it reads any sigrok file.
     - It reads the channel names (D0–D15), the width (1 or 2 bytes) and rates with a fraction of a MHz.
   - Long captures go through the existing split reading.
   - WireSkein cannot check the capture for loss.
     - Leave out of the analysis any capture that the window said stopped early.
     - The duration that was set can also be compared with the file's length.
2. **Build sigrok-cli and capture from the command line.**
   - Upstream sigrok-cli built against the libsigrok this repository installs (`install/`) should capture through this driver.
   - Whether sigrok-cli copes with the fork's own keys is not known.
   - This is the way to go for capturing automatically during a test.
3. **Drive the device from WireSkein (a source added to `wireskein.sources`).**
   - As above, the protocol is simple, and it can be written on libusb.
   - Things to watch:
     - 200 MHz × 16 lines is 400 MB/s. Receiving that in Python needs many asynchronous transfers queued.
     - The stream has no numbers, so the only check for loss is whether the expected count arrived without the device stopping.
     - A firmware update could change the commands with nothing to tell (the firmware is closed).
     - The application is GPL. A source that drives the device would be written from the facts of the protocol, without copying code (WireSkein is MIT).

The recommendation is 1. Whether to drive the device directly is best decided once `.sr` captures have been tried on the hardware, and the bandwidth and the loss behaviour have been seen.

## Not found out

- What the firmware does inside (the firmware is closed):
  - The size of its buffer.
  - Whether it stops or discards when the buffer overflows.
  - The divider offset on USB 2.0.
- How accurate the sample clock really is (the crystal's tolerance, the error from dividing the PLL).
  - Nor which base clock the fractional steps such as 187.5 MHz are divided from, or how.
- Where the lines land in the data when 2 or 4 lines are picked.
  - The driver changes only the width and treats bit n as channel n.
- Whether upstream sigrok will take the driver.
- The ADC mode (only on the `01` models; the V2 has no ADC).
