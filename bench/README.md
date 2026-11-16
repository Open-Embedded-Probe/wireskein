# The algorithm bench

[日本語](README.ja.md)

Each stage of the inference (a line's class, a clock's period, a bit time, noise removal, the whole analysis) with
several algorithms side by side, measured on the same data, every result kept in a ledger. The main algorithm (what
WireSkein does today) and the ones being tried are compared in one table, so that what goes into the product is
decided by numbers. Not part of the package; nothing under `src/` uses it.

```sh
uv run python bench/run.py --list                                  # stages, algorithms, suites
uv run python bench/run.py --stage all --suite smoke               # that everything runs (about 30 s)
uv run python bench/run.py --stage clock_period --suite noise --note "comb: candidates from the histogram"
uv run python bench/report.py                                      # algorithms x datasets
uv run python bench/report.py --stage e2e --delta                  # against the main algorithm
uv run python bench/report.py --history clock_period/comb          # the trail of one algorithm
uv run python bench/select.py --stage e2e --suite noise --by noise # choosing between algorithms (1.8)
```

Runs kept in the ledger use the same versions on every machine (`uv run --with-requirements bench/requirements.txt
python bench/...`, 1.9).

## 1. Rules

### 1.1 Separate

- **The bench is wholly apart from the product.** `src/wireskein` never imports `bench/`. The bench reads
  `wireskein` and never changes it (no monkey-patching).
- **Taking an algorithm into the product is a change of its own,** citing the ledger's run ids as its evidence
  (1.5).

### 1.2 Stages

- **A stage fixes the input, the output and the scoring.** An algorithm never scores itself: every algorithm of a
  stage is scored by the same code against the same truth (`wsbench/stages.py`).
- **An algorithm never sees the truth**, only the capture (channels named D0…Dn, in a shuffled order).
- **Metrics are added, not changed.** A metric whose meaning changes cannot be compared with its old numbers: a
  changed one gets a new name. The bench's own hash (`env.bench_sha`) is in the ledger.
- **A stage is measured with the earlier stages' truth given.** `clock_period` is asked only about lines that are
  clocks, `bit_unit` only about asynchronous ones: the stage's own errors, without the earlier stages'. What earlier
  errors do to the result is measured as a whole by `e2e`.

| Stage | Given → returns | Main metrics |
| --- | --- | --- |
| `lineclass` | `fn(cap)` → `{line: {"label": ...}}` | `accuracy` (clock / data / select / periodic / random / static), `accuracy_ns` (select and data one class: a select line is a role it plays for others, told by grouping, not by the line alone) |
| `clock_period` | `fn(cap, lines)` → `{line: {"period_s", "confidence"}}` | `hit3` (within ±3 %), `octave` (off by a whole factor) |
| `bit_unit` | `fn(cap, lines)` → `{line: {"unit_s", "confidence"}}` | `hit3` |
| `grouping` | `fn(cap)` → `[{"clock", "data", "select", "score"}]` (candidate buses) | `found` (the true clock + data + select among the candidates), `true_score` (its score: how strongly later stages can build on it), `groups_per_case` |
| `segments` | `fn(cap, {line: "clock" / "async"})` → `{line: [(start, end, unit)]}` | `rate_ok` (at the middle of each true segment the rate within 5 %), `blocks_err` (how many rates in time, against the truth) |
| `clean` | `fn(cap)` → `Capture` | `f1_rel`: against the edges before the noise, an edge matching within a quarter of the line's shortest run (a bounce read at its last crossing moves an edge by tens of ns: many samples at a high rate); `f1` within 2 samples; no loss on the column without noise |
| `e2e` | `fn(cap)` → claims (`decode.export`'s) | `ok` (buses confirmed or likely and right), `false_confirmed` (**kept at 0**) |

### 1.3 Algorithms

- **One idea, one file** (`algos/<stage>/<name>.py`); parameter variants as `variants` (`deglitch.k2`,
  `deglitch.k3`).
- **Three statuses:** `main` (what WireSkein does today; one per stage), `trial`, `retired`.
- **Retired, never deleted.** An idea that did not work stays with `status="retired", reason="..."`, so what was
  tried and why it was dropped can be read later. Retired algorithms run only when asked by name.
- **The same input gives the same output.** An algorithm that needs random numbers seeds them from its input.
- **An algorithm that can go into the engine says how** (`@algorithm(..., engine=fn)`: fn takes the algorithm's
  parameters and returns `wireskein._engine.methods` steps by name). e2e's `inject` puts it in and measures the
  whole analysis: winning on a stage's metric does not by itself make the analysis better, so it is measured so
  before it is taken in.
- **Shared helpers go in `wsbench/kit.py`** (algorithm files cannot import each other). The file's and `kit.py`'s
  hash is kept in the ledger (`algo_sha`); a table marks `!` where it changed since (to be measured again).

```python
from wsbench import algorithm

@algorithm("clock_period", "comb", status="trial", about="one line",
           variants={"m2": {"max_mult": 2}, "m4": {"max_mult": 4}})
def comb(cap, lines, max_mult):
    return {n: {"period_s": ..., "confidence": ...} for n in lines}
```

### 1.4 Data

- **A dataset is a source and a chain of transforms,** both strings, kept in the ledger as written.
  - Sources: `set:<name>[a:b]` (a frozen set of `eval/corpus/synth`), `real` (`eval/corpus/real`), `dir:<path>[a:b]`
    (any directory of `<id>.wireskein` beside `<id>.truth.json`), `gen:<profile>[/<stress>][a:b]@<rate>`
    (`eval/synth.py`'s scenarios taken at that rate: the same traffic at any rate, so with `decimate` the rate is
    swept), `i2cbench:h417_slow/stream[a:b]` (real captures of `tests/data/i2c_bench`: I2C through weak pull-ups,
    50 times "S, 0x13 write, NACK, P"; truth made on reading, SCL / SDA renamed D0 / D1, the address's ACK compared
    too: the truth's `expect.compare_ack`, which eval's generated cases do not set)
  - Transforms: `spike:L` (1-tick pulses at random, L per edge), `bounce:L` (a share L of the edges bounce just
    after), `xtalk:L` (a 1-tick pulse at a share L of the next channel's edges), `crop:A:B` (the part from A to B),
    `decimate:K` (taken K times slower), joined with `+` (`crop:0:0.5+spike:0.1`)
  - Open-drain slow rises: `odslow:NS` (an I2C bus's rises NS nanoseconds late, a shorter high gone: the clipped
    clocks of a real bus through weak pull-ups)
  - Noise in time: `pspike:L:NS` (pulses NS nanoseconds wide, L per edge), `pbounce:L:NS` (a share L of the edges
    bounce within NS nanoseconds after). As on a real line, the faster it is taken the more samples they span, and
    taken slowly one may fall between samples and not show. Noise in ticks (`spike` ...) is one tick at any rate
  - **A faster capture must not score lower**: the `rate` suite takes the same traffic at 200 MS/s down to a
    sixteenth of it. An algorithm that does worse with more information has a fault, such as thresholds in samples
  - Noise is seeded from the case id and the transform text: the same data on every run and machine. Each run keeps
    a hash of its exact inputs (`inputs_sha`)
- **Data a capture cannot show is kept, tagged, and counted apart** (`wsbench/data.py`'s `hardness`). Tags come
  from the truth, physics and the records' facts alone, never from an algorithm's result: `undersampled` (a clocked
  bus under 4 samples a period, an asynchronous one under 3 a bit; where the rate changes, at its fastest),
  `noise_over_bit` (noise lasting half a bit or more), `defective_capture` (a real record taken with a probe filter
  its README records locking up), `aliased` (a decoy periodic line faster than the rate / 2.5). Besides its metrics
  so far, e2e gives `ok_readable` / `confirmed_readable` over the untagged buses, `ok_hard` / `confirmed_hard` /
  `decode_hard` over the tagged ones (how far an algorithm gets), `ok_<tag>` per tag, and
  `false_confirmed_aliased` (wrong confirmations using an aliasing line). **False confirmations count in the total
  on any data**: a confident wrong claim harms a user however hard the capture.
- **A suite is not changed once it has results** (`suites.toml`); a changed one gets a new name (`noise-v2`).
- **Tune and holdout are kept apart.** Algorithms are never adjusted on what a holdout suite (the generated heldout
  seeds, the real records) shows.
- **Truth comes from outside WireSkein**: the generator, an established decoder, what a test's firmware sent. Never
  WireSkein's own output.

### 1.5 The ledger, and taking a trial in

- **Every run is added to the ledger** (in git). `results/ledger.jsonl` has one line per (run, stage, algorithm,
  dataset): the summary, the cost, the algorithm's hash and parameters, the dataset, the inputs' hash.
  `results/runs.jsonl` has one line per run: what all its results share (the commit, whether the tree was dirty, a
  hash of `src/wireskein`, the machine and versions, `--note`'s reason, the command). The tools read the two joined.
  Lines are never removed. Scores are rounded to 6 significant digits, costs to 4. Every item's row goes to
  `results/runs/<run>.jsonl` (not in git).
- **Runs worth keeping are made on a commit.** Work in progress writes to another ledger with
  `WSBENCH_RESULTS=<dir>`.
- **A trial becomes main** (goes into the product) when:
  1. it is better than main on the tune suites and no worse on the holdout ones
  2. it is no worse than main on the column without noise
  3. `e2e`'s `false_confirmed` does not grow (stays 0)
  4. its time (`seconds_median`) is acceptable
  The product change makes `current` that algorithm; the trial's file becomes `retired` (reason: the version that
  took it).

### 1.6 Adding from outside

- **Algorithms**: `--algos-dir DIR` (repeatable) or `WSBENCH_ALGOS` (`os.pathsep` separated), written as under
  `algos/`, kept outside the repository if wanted; the ledger keeps their path and hash.
- **Data**: your own records in a directory with eval's truth (`<id>.truth.json`), in a suite as `dir:<path>`.

### 1.7 Cost

`report.py --cost` puts each algorithm's score beside its cost and marks `P` on the Pareto front (no other algorithm
both scores higher and costs less). Times move with other load (a build beside the run), so the costs are read from
the steadiest down (`wsbench/cost.py`):

| Metric | Meaning | Under load |
| --- | --- | --- |
| `mem kB` (`--memory`) | peak allocation during the call (tracemalloc) | the same whatever the load |
| `scaling` | how CPU time grows with the edges (1 linear, 2 square) | constant factors do not move it |
| `vs main` | CPU time against main's, the same case in the same worker one right after the other (geometric mean) | ±15-30 % with every CPU busy |
| `ref` | against a fixed piece of work timed beside each case; machines compare | ±10 % for heavy stages, up to 2x for light ones |
| `cpu ms` | CPU time (a short call made up to 5 times, the least kept) | grows when a physical core is shared |

- **Choose on `vs main` and `scaling`.** Ten times slower, or growing with the square of the data, shows under
  load too; a 10 % difference is measured again on an idle machine.
- The load before a run (loadavg) is kept (`env.load_before`); tables warn about results from a busy machine.
- **Instructions** (`M instr`): on a machine with hardware counters (native Linux) each call's instructions retired
  are recorded, and against-main and scaling are given on them too. Load and clock speed hardly move them: where
  they are counted, decide on them. WSL2 and most cloud VMs (GitHub's runners too) have no PMU and record none;
  `python bench/wsbench/perfcount.py` tells whether a machine counts (and if not, why).

### 1.8 Choosing between algorithms (`select.py`)

No one algorithm need be best on every kind of data. `select.py` compares ways of switching between them from a
run's item rows (nothing is run again).

```sh
uv run python bench/select.py --stage e2e --suite noise --by noise
```

| Strategy | Meaning |
| --- | --- |
| `fixed <algo>` | one algorithm for everything |
| `oracle` | per item, the best one, knowing the truth: the most any switching can reach |
| `confidence` | per item, the one whose `confidence` is highest |
| `dispatch:<key>` | per group (protocol, line class, noise, rate ...), the one best on that group; the choices made on half of the cases and scored on the other half (and the other way round), so fitting the scored cases earns nothing |

Of the `dispatch` groups, a switch can go at run time only by `rate` and what the capture shows of itself
(`wsbench/observe.py`): `obs_noise` (the share of runs under a fifth of their line's unit), `obs_os` (samples per
unit of the most finely taken line), `obs` (both). The groups marked `(truth)` (protocol, line class, dataset, kind
of noise) are known to the bench only: an upper bound.

For e2e it also compares switching on what each output itself says (its claims by verdict, known at run time):
`cascade A->B` (A first, B where A is unsure), `most_confirmed`, `least_unsure`.

If `oracle` is well above the best `fixed` and `dispatch` or `confidence` come close to it, switching is worth
building; if not, one algorithm is enough. `--by <key>` adds a table of which algorithm wins on each group.

### 1.9 Where to measure

Scores are deterministic (the same cases, seeds and algorithm give the same values), so they are the same wherever
they are measured. Costs are not.

| What | Where | How |
| --- | --- | --- |
| Scores of whole suites | GitHub Actions | Start "Bench" by hand (stages, suites, algorithms, jobs, note, whether to commit). The cases are split over parallel jobs (every algorithm of a case in the same job), merged at the end, and the ledger's new lines committed to the branch it ran on (on the branch as it is just before the push, added again; again if another push came first). The items are kept as the artifact `bench-<run id>`; here with `uv run python bench/merge.py --import <unpacked dir>` |
| Costs | a machine that counts instructions (native Ubuntu) | the `cost` suite with `--memory`, one job fewer than the cores |
| Quick tries | here | `smoke`, `rate-smoke` ...; another ledger with `WSBENCH_RESULTS` |

- Costs on a machine that counts instructions:

  ```sh
  git pull                                       # a clean tree (or the ledger's lines say dirty)
  python3 bench/wsbench/perfcount.py             # its second line "counts itself: ok": a worker counts itself
  uv run --with-requirements bench/requirements.txt python bench/run.py --stage all --suite cost --memory -j 3 \
      --note "cost on <machine>"
  uv run python bench/report.py --suite cost --cost
  git add bench/results && git commit -m "bench: cost run on <machine>" && git push
  ```

- Split by hand: `run.py --shard I/N --run-id ID` N times, then `merge.py ID` (what Actions does). Split or not, the
  scores and the inputs' hashes are the same.
- Versions are pinned in `bench/requirements.txt`. Changing one changes what is measured: say so in the next run's
  `--note`. The Python version is kept in the ledger.
- Ubuntu's default (`kernel.perf_event_paranoid=4`) lets no user count instructions;
  `sudo sysctl kernel.perf_event_paranoid=2` lets a user count their own processes' user space.

Notes on what was measured go in `bench/notes/` (e.g. [2026-11-16, the detection reviewed from zero](notes/2026-11-16-detection-review.ja.md), in Japanese).

## 2. Adding a stage

A class of `Stage` in `wsbench/stages.py`, added to `STAGES`: how an algorithm is called (`run`), each item's
scoring (`score`), the summary (`summarize`), the metric tables show first (`headline`), the metrics where lower is
better (`LOWER`). Then WireSkein's way today wrapped as `main` in `algos/<stage>/current.py`: without it a trial has
nothing to be better than.

Stages not here yet, worth adding next: rate segmentation (the spans of a clock or baud rate that changes), clock and
data grouping, frame delimiting.
