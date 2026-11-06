# Long captures

[日本語](long-captures.ja.md)

A capture soon grows to gigabytes: an OEP probe streams 40 MB/s, so one minute is 2.4 GB, and a USB SuperSpeed
analyzer fills ten times as fast. WireSkein reads such a capture a block at a time and analyses it in pieces, so
memory stays bounded whatever its length. What a piece decoded is kept (or written out); its changes are dropped.

## 1. The flow

1. **Look at it first**: `wireskein survey CAPTURE` reads it once and says how each line moves over the whole (§2).
2. **Keep what is needed**: `wireskein convert IN OUT --from S --to S --pins A,B` writes a stretch of it, or some of
   its lines, as a WireSkein file (§5). A logic `.sr` of any length converts a block at a time.
3. **Decode or analyse it in pieces**: `decode` and `analyze` cut a part of more changes than `max_edges` into pieces
   (§3) and read them one at a time; `--items FILE` writes what they decode to a file as they go (§4).

A capture of `max_edges` changes or fewer is read whole, as always.

## 2. survey

```sh
wireskein survey long.sr                       # lines to read
wireskein survey long.sr --json --plan         # the document (wireskein-survey/1), with the pieces it would be cut into
```

Read once a block at a time (a WireSkein file of one part and a logic `.sr` straight from the file; anything else is
loaded first). Per line:

| Key | Meaning |
| --- | --- |
| `edges` | Its changes |
| `activity` | Its changes in each of `bins` (default 1000) equal stretches of the time |
| `intervals`, `high`, `low` | The intervals between its changes, and the lengths of its highs and lows, in bins of powers of two (ticks) |
| `longest_still` | Its `top` (default 10) longest stills: `s`, `e`, `seconds` |
| `short_share` | The share of its intervals of one or two ticks: high on a floating input trembling, or a line ringing at edges |
| `usual_interval_s` | The median of its intervals |
| `free_running` | For a line that never stops (its longest interval within four times its usual one): `period_s`, `freq_hz`, `duty` |

Over all the lines, `all_still`: the longest stretches where none of them moves. `trembling`: the lines whose
`short_share` is over 0.3. With `--plan`, `pieces`: the pieces §3 would cut it into.

## 3. Pieces

A part of more changes than `max_edges` is cut as it is read. From the start, the changes up to `max_edges` are a
window; inside it the cut goes where the lines are quiet:

- Each line's usual interval is the median of its intervals in the window. A line still for `gap_min` times that is
  quiet; a pulse of `noise_max` ticks or shorter does not count (a reflection ringing, a short tremble).
- A line that never stops in the window (a free-running clock, a PWM, I2S's WS) tells no frame's end and is left out.
  A clock that pauses between frames (SWCLK, SPI's SCK, I2C's SCL) is not left out: its pauses are where cuts go.
- Where every other line is quiet at once, after `search_from` of the window, is a candidate. The longest is cut at
  its middle, and others at least `similar` times as long too, each piece keeping `min_edges` changes or more.
- With a free-running line in the window, quiet may still be inside a frame (data the same for a while): the cut is
  `data_quiet` and the pieces on both sides are read `overlap_edges` changes past it. An item read twice is kept by
  the piece whose own ticks it starts in.
- With no candidate at all (only free-running lines, or a stream that never pauses), the window is cut at its end,
  `forced`, read with the same overlap.

The remainder after the last cut starts the next window. A decode spec's buses and lines are the ones looked at for
quiet; `analyze` looks at every line.

`analyze` takes the roles from the first piece that holds any (its claims confirmed or likely; up to `roles_from`
pieces), then decodes every piece with them; with none, `split.no_roles` says so.
State across frames (the RISC-V Debug Module) is read over the pieces as one.

### 3.1 Options

`--split KEY=VALUE,...` on `analyze` and `decode` (Python: `split=`, or `"split"` in a decode spec), `--split off` to
read whole however long:

| Option | Meaning | Default |
| --- | --- | --- |
| `max_edges` | A part of more changes than this is cut; a window's size | 1 000 000 |
| `search_from` | Cuts looked for after this share of the window | 0.5 |
| `gap_min` | Quiet: still this many times the line's usual interval | 4 |
| `similar` | Others at least this share of the longest quiet are cut too | 0.5 |
| `min_edges` | A piece's fewest changes | 50 000 |
| `overlap_edges` | Changes read past a `data_quiet` or `forced` cut | 10 000 |
| `noise_max` | Pulses this short (ticks) do not make a line busy | 1 |
| `noise_share` | A line with more of its intervals that short is told as trembling | 0.3 |
| `watch=A;B` | Look for quiet on these lines only | the spec's, or all |
| `ignore=A;B` | Not on these | |
| `cuts=T1;T2` | Cut here (ticks), instead of looking | |
| `workers` | Pieces decoded in this many processes at once (at most twice as many in flight) | 1 |
| `roles_from` | `analyze`: pieces tried, from the start, for the roles (the start may be before the bus) | 3 |

The defaults come from 2 s of a WCH-LinkE's SDI monitor (8.5M changes): pieces of 500k to 1M changes were the
quickest and lightest; 250k needed forced cuts.

### 3.2 What the part says

The part has `split`:

| Key | Meaning |
| --- | --- |
| `options` | The options used |
| `pieces` | `[{"s", "e", "edges", "cut", "ratio"?, "read"?}]`: each piece's own ticks, its changes, how it ends (`quiet`, `data_quiet`, `forced`, `given`, `end`), the quiet's length over the usual interval, and the ticks read when past its own |
| `trembling` | Lines with many intervals of a tick or two: name → share (told, not left out) |
| `doubtful` | Pieces the roles read badly (a bus not decoded, or a fit under 0.3), with where they are: the capture may change on the way (another bus, another speed); analyse them on their own |

Each bus gives `uninterpreted_summary`: `count`, `edges`, `sizes` (in bins of powers of two), `largest`. A long
capture has many small unread stretches (a line moving between frames) and a few that matter. `fit` is the worst
piece's, with every piece's score in `fit.pieces`. Claims on pins the spec does not name are not made in pieces.

## 4. Items written out

What a long capture decodes is more than memory holds (1.28 million DMI accesses from 38 s of a LinkE). `--items
FILE` (Python: `items=`) writes them as they are read, one JSON object a line:

```json
{"bus": 0, "layer": "dmi", "s": 5512646, "e": 5524342, "op": "R", "addr": 4, ...}
```

`bus` is the spec's bus (or the claim), `layer` the result's layer (`dmi`, `dm`, `transactions`, `uninterpreted`,
...), the rest the item as in [analysis-format](analysis-format.md) §3.2, its ticks the part's. The document then gives
each bus `result.items`: `{"file", "counts"}`.

## 5. Lines that are not wanted

A floating input trembling, or a line taken along that is nothing to do with the bus, is common. It is not left out
on its own: the survey and the pieces tell it, and what to do with it is the user's.

1. The analysis goes on: pulses of a tick or two do not hide the quiet.
2. It is told: `survey` gives its `short_share` and `trembling`; a part read in pieces gives `split.trembling`. A line
   that trembles without pause is taken as free-running, and the cuts become `data_quiet` (see `split.pieces`).
3. Look at it: `wireskein states CAPTURE --pin X`, or the viewer.
4. Leave it out of the analysis: `--split ignore=X` (not looked at for quiet), `--pins A,B,...` (only these read).
5. Take it out of the data: `wireskein convert IN OUT --pins A,B,...` (only these written), `--from` / `--to` (only
   that stretch).

## 6. Speed

On 38 s of a LinkE's SDI monitor (25 MHz, 160 million changes, 950 MB as a `.sr`), RVSWD decoded with
`--split workers=4 --items`: 6 min 54 s, 530 MB (the main process). Converting the `.sr` to a WireSkein file: 13 s,
180 MB.
