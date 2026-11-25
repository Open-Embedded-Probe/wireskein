//! A capture's data formats 2 to 4 (oep-if-capture §1; oep-client-python's `capture_blocks`): format 2, the analog
//! blocks (§1.4: each channel packed, or per interval of D its min and max and / or its sum), format 3, the timed
//! states of an external clock (§3.8: per sample a delta of timebase ticks and the state; an escape moves time on
//! without a sample), and format 4, the test counter (§1: byte k of the stream is k & 0xFF, at the format 0 volume).
//! Formats 0 and 1 are `blocks`.
//!
//! Bits are read as §1 places them: bit j of a part is bit j % 8 of its byte j / 8.

use crate::msg::{Error, Result};
use std::collections::BTreeMap;

pub const FORMAT_PACKED: u8 = 0;
pub const FORMAT_LOGIC_BLOCKS: u8 = 1;
pub const FORMAT_ANALOG_BLOCKS: u8 = 2;
pub const FORMAT_STATE_TIMED: u8 = 3;
pub const FORMAT_TEST_COUNTER: u8 = 4;

/// analog_carriage (§1.4): a bit set, packed alone or minmax and / or sum
pub const PACKED: u8 = 0x01;
pub const MINMAX: u8 = 0x02;
pub const SUM: u8 = 0x04;

/// format 3: a delta that only moves time on (its state is no sample)
pub const ESCAPE: u32 = 0xFFFF_FFFF;

/// The bits of `n`'s binary form (Python's int.bit_length).
fn bit_length(n: usize) -> u32 {
    usize::BITS - n.leading_zeros()
}

/// Bits read from bit 0 up, as §1 places them.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: u32) -> u64 {
        let mut v = 0u64;
        for k in 0..n as usize {
            let j = self.at + k;
            if (self.data[j >> 3] >> (j & 7)) & 1 == 1 {
                v |= 1 << k;
            }
        }
        self.at += n as usize;
        v
    }
}

/// A format 2 configuration (§1.4, as configured and answered): the layout's order and b, the block's L, and the
/// carriage of each channel not packed (role -> (kind, D)).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnalogBlocks {
    pub order: Vec<usize>,
    pub carriage: BTreeMap<usize, (u8, usize)>,
    pub block: usize,
    pub bits: u32,
}

/// One channel's part of a format 2 block: `values` (packed, lossless), or per interval of D (the last one partial)
/// `mins` / `maxs` and / or `sums` (summaries).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnalogPart {
    pub kind: u8,
    pub d: usize,
    pub values: Vec<u64>,
    pub mins: Vec<u64>,
    pub maxs: Vec<u64>,
    pub sums: Vec<u64>,
}

impl AnalogPart {
    /// The original samples each interval covers in a block of r (a sum over its points is their mean).
    pub fn points(&self, r: usize) -> Vec<usize> {
        (0..r).step_by(self.d.max(1)).map(|lo| self.d.min(r - lo)).collect()
    }
}

/// One block of a format 2 segment: samples `first` .. `first + samples - 1`, each channel's part in the layout's
/// order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnalogBlock {
    pub first: usize,
    pub samples: usize,
    pub channels: Vec<(usize, AnalogPart)>,
}

impl AnalogBlocks {
    fn of(&self, role: usize) -> (u8, usize) {
        self.carriage.get(&role).copied().unwrap_or((PACKED, 1))
    }

    fn part_bits(&self, r: usize, kind: u8, d: usize) -> usize {
        let b = self.bits as usize;
        if kind == PACKED {
            return r * b;
        }
        let per = if kind & MINMAX != 0 { 2 * b } else { 0 }
            + if kind & SUM != 0 {
                b + bit_length(d.saturating_sub(1)) as usize
            } else {
                0
            };
        r.div_ceil(d.max(1)) * per
    }

    /// The bytes of one block of r samples: every channel's part, each ending on a byte.
    pub fn block_bytes(&self, r: usize) -> usize {
        self.order
            .iter()
            .map(|&k| {
                let (kind, d) = self.of(k);
                self.part_bits(r, kind, d).div_ceil(8)
            })
            .sum()
    }

    /// A segment's bytes: whole blocks of L, the last of what is left (a fixed size).
    pub fn segment_bytes(&self, samples: usize) -> usize {
        if self.block == 0 {
            return 0;
        }
        let (whole, rest) = (samples / self.block, samples % self.block);
        whole * self.block_bytes(self.block) + if rest > 0 { self.block_bytes(rest) } else { 0 }
    }

    /// A segment's bytes -> its blocks. Bytes left over, or too few: a broken stream.
    pub fn read(&self, data: &[u8], samples: usize) -> Result<Vec<AnalogBlock>> {
        if self.block == 0 {
            return Err(Error::Protocol("format 2: block 0".into()));
        }
        let expect = self.segment_bytes(samples);
        if data.len() != expect {
            return Err(Error::Protocol(format!(
                "format 2: {} bytes for {samples} samples ({expect} expected)",
                data.len()
            )));
        }
        let b = self.bits;
        let (mut out, mut pos) = (vec![], 0usize);
        for first in (0..samples).step_by(self.block) {
            let r = self.block.min(samples - first);
            let mut blk = AnalogBlock {
                first,
                samples: r,
                channels: vec![],
            };
            for &k in &self.order {
                let (kind, d) = self.of(k);
                let n = self.part_bits(r, kind, d).div_ceil(8);
                let mut bits = Bits {
                    data: &data[pos..pos + n],
                    at: 0,
                };
                pos += n;
                let mut c = AnalogPart {
                    kind,
                    d,
                    ..Default::default()
                };
                if kind == PACKED {
                    c.values = (0..r).map(|_| bits.take(b)).collect();
                } else {
                    for _ in 0..r.div_ceil(d.max(1)) {
                        if kind & MINMAX != 0 {
                            c.mins.push(bits.take(b));
                            c.maxs.push(bits.take(b));
                        }
                        if kind & SUM != 0 {
                            c.sums.push(bits.take(b + bit_length(d.saturating_sub(1))));
                        }
                    }
                }
                blk.channels.push((k, c));
            }
            out.push(blk);
        }
        Ok(out)
    }
}

/// A format 3 entry's bytes: the delta (u32) and the state (w bits filled to a byte).
pub fn state_timed_entry(w: usize) -> usize {
    4 + w.div_ceil(8)
}

/// A format 3 segment's bytes -> (ticks from start, state) for each of its `samples` samples: the first delta counts
/// from start, the others from the sample before, an escape moves time on without a sample. Bytes left over, or too
/// few: a broken stream.
pub fn read_state_timed(data: &[u8], samples: usize, w: usize) -> Result<Vec<(u64, u64)>> {
    let size = state_timed_entry(w);
    let mask = if w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
    let (mut out, mut pos, mut ticks) = (Vec::with_capacity(samples), 0usize, 0u64);
    while out.len() < samples {
        if pos + size > data.len() {
            return Err(Error::Protocol(format!(
                "format 3: {} of {samples} samples in {} bytes",
                out.len(),
                data.len()
            )));
        }
        let delta = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4"));
        ticks += delta as u64;
        if delta != ESCAPE {
            let mut state = 0u64;
            for (j, byte) in data[pos + 4..pos + size].iter().take(8).enumerate() {
                state |= (*byte as u64) << (8 * j);
            }
            out.push((ticks, state & mask));
        }
        pos += size;
    }
    if pos != data.len() {
        return Err(Error::Protocol(format!(
            "format 3: {} bytes past the segment's {samples} samples",
            data.len() - pos
        )));
    }
    Ok(out)
}

/// The bytes `samples` samples of format 3 take at the start of `data` (escapes included): a segment's length is not
/// in its record (§3.8).
pub fn state_timed_length(data: &[u8], samples: usize, w: usize) -> Result<usize> {
    let size = state_timed_entry(w);
    let (mut pos, mut n) = (0usize, 0usize);
    while n < samples {
        if pos + size > data.len() {
            return Err(Error::Protocol(
                "format 3: the segment's bytes end inside its entries".into(),
            ));
        }
        n += (u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4")) != ESCAPE) as usize;
        pos += size;
    }
    Ok(pos)
}

/// Format 4 (§1): where bytes from stream position `position` are not the counter (byte k is k & 0xFF) - the
/// offsets into `data` of the first wrong byte of each wrong run and the run's length; empty when all are right.
pub fn test_counter_wrong(data: &[u8], position: u64) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = vec![];
    for (j, &b) in data.iter().enumerate() {
        if b != ((position + j as u64) & 0xFF) as u8 {
            match out.last_mut() {
                Some((at, n)) if *at + *n == j => *n += 1,
                _ => out.push((j, 1)),
            }
        }
    }
    out
}

/// One segment as it came: its record and its bytes.
pub type RawSegment = (crate::capture::Segment, Vec<u8>);

/// A one-shot capture on the probe at `address` with its data in whatever format `s` asks, its segments
/// as they came (the formats a WireSkein file does not hold yet - 2, 3, 4 - read by the functions above): the lease
/// taken as `owner`, `plan` (role, channel) applied to the logic track (or the analog one), configured, started,
/// waited for `timeout`, read, the session ended. A tool's or a test's.
pub fn take_raw(
    address: &str,
    analog: bool,
    plan: &[(u8, u16)],
    s: &crate::capture::Setting,
    owner: &str,
    timeout: std::time::Duration,
) -> Result<(crate::capture::Config, Vec<RawSegment>)> {
    use crate::capture::Logic;
    let addr = crate::acquire::address_of(address)?;
    let mut hst = crate::acquire::open(&addr)?;
    hst.take(30_000, Some(owner), std::time::Duration::from_secs(5))?;
    let run = |hst: &mut crate::host::Host| -> Result<(crate::capture::Config, Vec<RawSegment>)> {
        let mut t = if analog { Logic::analog(hst)? } else { Logic::new(hst)? };
        let assign: Vec<(u16, u8, u16)> = plan.iter().map(|&(r, ch)| (t.fn_, r, ch)).collect();
        hst.plan_apply(&assign)?;
        let cfg = t.configure(hst, s, false)?;
        t.start(hst)?;
        let segs = t.wait(hst, timeout, None)?;
        let mut out = vec![];
        for seg in segs {
            let data = t.read_segment(hst, &seg)?;
            out.push((seg, data));
        }
        Ok((cfg, out))
    };
    let got = run(&mut hst);
    let _ = hst.end();
    got
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bits put as §1 places them (the writer of a test).
    fn put(bits: &mut Vec<bool>, v: u64, n: u32) {
        for k in 0..n {
            bits.push((v >> k) & 1 == 1);
        }
    }

    fn bytes(bits: &[bool]) -> Vec<u8> {
        let mut out = vec![0u8; bits.len().div_ceil(8)];
        for (j, &b) in bits.iter().enumerate() {
            if b {
                out[j / 8] |= 1 << (j % 8);
            }
        }
        out
    }

    #[test]
    fn analog_blocks_read_what_was_put() {
        // 12-bit values; role 0 packed, role 1 minmax and sum over 8, role 2 sum over 3; blocks of 24, 50 samples
        let cfg = AnalogBlocks {
            order: vec![1, 0, 2],
            carriage: [(1, (MINMAX | SUM, 8)), (2, (SUM, 3))].into_iter().collect(),
            block: 24,
            bits: 12,
        };
        let v = |k: usize, i: usize| ((i * 37 + k * 1000) % 4096) as u64;
        let mut data = vec![];
        for first in (0..50).step_by(24) {
            let r = 24.min(50 - first);
            for &k in &cfg.order {
                let mut bits = vec![];
                match k {
                    0 => (0..r).for_each(|i| put(&mut bits, v(0, first + i), 12)),
                    1 => {
                        for lo in (0..r).step_by(8) {
                            let xs: Vec<u64> = (lo..(lo + 8).min(r)).map(|i| v(1, first + i)).collect();
                            put(&mut bits, *xs.iter().min().unwrap(), 12);
                            put(&mut bits, *xs.iter().max().unwrap(), 12);
                            put(&mut bits, xs.iter().sum(), 15);
                        }
                    }
                    _ => {
                        for lo in (0..r).step_by(3) {
                            put(&mut bits, (lo..(lo + 3).min(r)).map(|i| v(2, first + i)).sum(), 14);
                        }
                    }
                }
                data.extend(bytes(&bits));
            }
        }
        assert_eq!(data.len(), cfg.segment_bytes(50));
        let blocks = cfg.read(&data, 50).unwrap();
        assert_eq!(blocks.iter().map(|b| b.samples).collect::<Vec<_>>(), [24, 24, 2]);
        let last = &blocks[2];
        let (_, p1) = &last.channels[0];
        assert_eq!((p1.mins.len(), p1.points(2)), (1, vec![2]));
        assert_eq!(p1.sums[0], v(1, 48) + v(1, 49));
        let (_, p0) = &last.channels[1];
        assert_eq!(p0.values, [v(0, 48), v(0, 49)]);
        let e = cfg.read(&data[1..], 50).unwrap_err().to_string();
        assert_eq!(
            e,
            format!(
                "format 2: {} bytes for 50 samples ({} expected)",
                data.len() - 1,
                data.len()
            )
        );
    }

    #[test]
    fn state_timed_reads_deltas_states_and_escapes() {
        let mut data = vec![];
        for (d, s) in [(5u32, 0x1ABu16), (ESCAPE, 0xFFFF), (3, 0x0F0), (0, 0x2)] {
            data.extend(d.to_le_bytes());
            data.extend(s.to_le_bytes());
        }
        let got = read_state_timed(&data, 3, 9).unwrap();
        assert_eq!(
            got,
            [
                (5, 0x1AB),
                (5 + ESCAPE as u64 + 3, 0x0F0),
                (5 + ESCAPE as u64 + 3, 0x002)
            ]
        );
        assert_eq!(state_timed_length(&data, 2, 9).unwrap(), 18);
        assert_eq!(state_timed_length(&data, 3, 9).unwrap(), 24);
        assert!(
            read_state_timed(&data, 2, 9)
                .unwrap_err()
                .to_string()
                .contains("6 bytes past")
        );
        assert!(
            read_state_timed(&data, 4, 9)
                .unwrap_err()
                .to_string()
                .contains("3 of 4 samples")
        );
    }

    #[test]
    fn the_test_counter_says_where_it_is_wrong() {
        let mut d: Vec<u8> = (0..600u64).map(|k| ((k + 250) & 0xFF) as u8).collect();
        assert!(test_counter_wrong(&d, 250).is_empty());
        d[7] ^= 1;
        d[8] ^= 1;
        d[300] = 0;
        assert_eq!(test_counter_wrong(&d, 250), [(7, 2), (300, 1)]);
    }
}
