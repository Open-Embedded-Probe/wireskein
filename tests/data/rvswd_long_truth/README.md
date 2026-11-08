# rvswd_long_truth

WCH-LinkE captures of RVSWD and SWIO (wch-protocols experiments/e173 and captures/fixtures/wire-linke-p4-2026-09-25),
with the truth from ch32rv: what it printed (`<name>.truth.json`, `<name>.json`) or wrote (`<name>.bin`).

## Bad data, kept on purpose

These were captured too slowly for a LinkE at its fastest, or with long wires: the analysis must read what can be
read from them, tell what it cannot (`reading`, `undersampled`, docs/capturing.md) and never misread. Better captures
of the same operations will be taken; these stay as the bad case.

| File | What is wrong |
| --- | --- |
| `linke_v203_flash_pattern4k.sr` | 50 MS/s for a 10 MHz clock with highs of 20-25 ns: about a third of the frames lost a clock between samples (a few read with it restored, the rest unread) |
| `linke_v203_speed_high_read_ram_256.sr`, `linke_v203_speed_high_read_flash_4k.sr` | The same at 50 MS/s, after a slow connect (0.33 MHz): the start reads, the fast part barely |
| `linke_v203_160mhz_flash_pattern4k.sr` | 160 MS/s, every clock seen, but noisy: glitches on SWCLK (about 2.5 % of its changes) and ringing on SWDIO (14 %) |
| `linke_l103_*.sr`, `linke_v203_100mhz_target_info.sr`, `linke_v203_read_flash_4k.sr` | Ringing on SWDIO at the turnarounds (up to 80 % of its changes on an L103), read as it settles |

To make worse data from a good capture (a lower rate, glitches, crosstalk, ringing, jitter), with the seed kept:
`tools/degrade.py`.
