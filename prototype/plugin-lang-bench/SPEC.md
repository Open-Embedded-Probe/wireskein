# Plugin language benchmark — task contract

Every engine binary `eng-<name>` is run as

    ./eng-<name> <data_dir> <capture_id> <scripts_dir>

and prints ONE JSON line built with `common::Report` (see common/src/lib.rs):

| field | meaning |
| --- | --- |
| `t1_method` | how the whole sample buffer (mmap'd u16, `Capture::samples()`) is made visible to the script: e.g. "external ArrayBuffer (zero-copy)", "userdata indexer (zero-copy)", "copy into list" |
| `t1_zero_copy` | 1 if no copy of the sample buffer is made, else 0 |
| `t1_ms` | time to make the buffer visible (per hand-off) |
| `t1_copy_ms` | time of the copying alternative, if you implemented one (else omit) |
| `t2_samples` | number of samples scanned = min(n, 2_000_000) |
| `t2_ms` | script scans `t2_samples` samples and counts edges of channel `cap.busiest()` |
| `t2_ok` | 1 if the count equals `common::scan_ref(cap, ch, t2_samples)` |
| `t3_bits` | `inp.bits.len()` of `common::i2c_input(cap)` |
| `t3_handoff_ms` | time to hand bits/t/bounds to the script (zero-copy if possible) |
| `t3_ms` | script I2C plugin run time |
| `t3_records` | number of transaction records the script built |
| `t3_ok` | 1 if (transactions, data bytes, xor of all 9-bit words) equals `common::i2c_ref(&inp)` |
| `t4_host_call_us` | script calls a host function `inc(x) -> x+1` 100_000 times; µs per call |
| `t4_script_call_us` | host calls a script function `inc(x) -> x+1` 100_000 times; µs per call |
| `t5_error` | full error text (message + location/stack as the engine reports it) from running `scripts/<lang>/error.*` |
| `peak_rss_kb` | `common::peak_rss_kb()` at the end |
| `notes` | anything relevant (limits, unsafe used, why zero-copy is/isn't possible) |

Script functions (in `scripts/<lang>/`):

- `scan(samples, n, ch)` → number of positions i in 1..n where bit `ch` of samples[i] differs from samples[i-1].
- `i2c(bits, t, bounds)` → for each frame f (bounds[2f], bounds[2f+1] are indices into bits/t):
  nw = (b - a) div 9; skip if nw == 0; each 9-bit word w = bits[a+9k .. a+9k+8] MSB first;
  byte = w >> 1, ack = (w & 1) == 0. Build one record per frame: {addr: first_byte >> 1,
  rw: "read" if first_byte & 1 else "write", addr_ack, bytes: [remaining bytes], start: t[a], end: t[b-1]}.
  Return (records, transactions, data_bytes (= sum of nw-1), xor of all 9-bit words).
  The record list must be built (it is the realistic part of a plugin), even if the host only reads its length.
- `inc(x)` → x + 1.
- `error.*`: a function two calls deep that reads an undefined variable / nil field on line 4 of the file, called from top level.

Build with a private target dir so parallel builds don't block each other:

    CARGO_TARGET_DIR=target-<name> cargo build --release -p eng-<name>
