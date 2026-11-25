//! A capture's data (oep-if-capture §1): format 0, samples of w bits with each channel at its bit (§1.1), and format 1,
//! the logic blocks (§1.3: blocks of L samples, each channel raw or reduced over intervals of D) - oep-client-python's
//! `acquire.from_interleaved` / `_samples_at_once` and `capture_blocks.segment_columns`.
//!
//! Bits are packed one a sample, sample i at bit i % 8 of byte i / 8.

use crate::msg::{Error, Result};

/// Format 1's carriages (capture §1.3).
pub const RAW: u8 = 0;
pub const SAMPLE: u8 = 1;
pub const ANY_HIGH: u8 = 2;
pub const ANY_LOW: u8 = 3;
pub const CHANGE_LATCH: u8 = 4;

pub fn carriage_name(kind: u8) -> &'static str {
    match kind {
        SAMPLE => "sample",
        ANY_HIGH => "any_high",
        ANY_LOW => "any_low",
        CHANGE_LATCH => "change_latch",
        _ => "raw",
    }
}

pub fn carriage_of(name: &str) -> Option<u8> {
    Some(match name {
        "sample" => SAMPLE,
        "any_high" => ANY_HIGH,
        "any_low" => ANY_LOW,
        "change_latch" => CHANGE_LATCH,
        _ => return None,
    })
}

/// An interval's bits for a reduction.
pub fn value_bits(kind: u8) -> usize {
    if kind == CHANGE_LATCH { 2 } else { 1 }
}

/// Bits appended one after another (a part's channel joined from its segments: a segment need not end on a byte).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BitBuf {
    pub bytes: Vec<u8>,
    pub nbits: usize,
}

impl BitBuf {
    /// `n` bits of `src` (packed from bit 0).
    pub fn append(&mut self, src: &[u8], n: usize) {
        if n == 0 {
            return;
        }
        let nb = n.div_ceil(8);
        let sh = self.nbits % 8;
        if sh == 0 {
            self.bytes.extend_from_slice(&src[..nb]);
        } else {
            for &b in &src[..nb] {
                *self.bytes.last_mut().expect("a partial byte") |= b << sh;
                self.bytes.push(b >> (8 - sh));
            }
        }
        self.nbits += n;
        self.bytes.truncate(self.nbits.div_ceil(8));
        if !self.nbits.is_multiple_of(8) {
            let m = (1u8 << (self.nbits % 8)) - 1;
            if let Some(b) = self.bytes.last_mut() {
                *b &= m;
            }
        }
    }

    /// The whole bytes, taken off the front (the bits of a last partial byte stay).
    pub fn take_whole(&mut self) -> Vec<u8> {
        let whole = self.nbits / 8;
        let out: Vec<u8> = self.bytes.drain(..whole).collect();
        self.nbits -= whole * 8;
        out
    }
}

/// Bit `j` of packed `data`.
#[inline]
pub fn bit(data: &[u8], j: usize) -> u8 {
    (data[j >> 3] >> (j & 7)) & 1
}

/// One channel's bits of `n` samples of `width` bits each, the channel at bit `pos` of a sample (§1.1: stream bit j
/// is bit j % 8 of byte j / 8; a sample may cross a byte).
pub fn column(data: &[u8], width: usize, pos: usize, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n.div_ceil(8)];
    if width.is_multiple_of(8) && width > 0 {
        let b = width / 8;
        let (byte, sh) = (pos / 8, (pos % 8) as u32);
        let whole = n / 8;
        if b == 1 {
            // 8 samples a 64-bit word: the channel's bits gathered into a byte by one multiply
            for (k, o) in out.iter_mut().enumerate().take(whole) {
                let w = u64::from_le_bytes(data[k * 8..k * 8 + 8].try_into().expect("8"));
                *o = ((((w >> sh) & 0x0101_0101_0101_0101).wrapping_mul(0x0102_0408_1020_4080)) >> 56) as u8;
            }
        } else {
            for (k, o) in out.iter_mut().enumerate().take(whole) {
                let mut v = 0u8;
                for i in 0..8 {
                    v |= ((data[(k * 8 + i) * b + byte] >> sh) & 1) << i;
                }
                *o = v;
            }
        }
        for i in whole * 8..n {
            out[i / 8] |= ((data[i * b + byte] >> sh) & 1) << (i % 8);
        }
    } else {
        for i in 0..n {
            out[i / 8] |= bit(data, i * width + pos) << (i % 8);
        }
    }
    out
}

/// An 8 x 8 bit matrix transposed: bit 8i + j to bit 8j + i (Hacker's Delight 7-3).
fn transpose8(mut x: u64) -> u64 {
    let t = (x ^ (x >> 7)) & 0x00AA_00AA_00AA_00AA;
    x ^= t ^ (t << 7);
    let t = (x ^ (x >> 14)) & 0x0000_CCCC_0000_CCCC;
    x ^= t ^ (t << 14);
    let t = (x ^ (x >> 28)) & 0x0000_0000_F0F0_F0F0;
    x ^ t ^ (t << 28)
}

/// Every channel's bits of `n` samples of `width` bits (`column` for each position), the samples read once.
pub fn columns(data: &[u8], width: usize, positions: &[usize], n: usize) -> Vec<Vec<u8>> {
    if width != 8 {
        return positions.iter().map(|&p| column(data, width, p, n)).collect();
    }
    let whole = n / 8;
    // each 8 samples' bits transposed at once (byte p of the result: bit p of the 8 samples), then each position's
    // bytes taken out in one pass of its own (a stream's spool spent most of its time here, §42)
    let t: Vec<u64> = data[..whole * 8]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| transpose8(u64::from_le_bytes(*c)))
        .collect();
    let mut out: Vec<Vec<u8>> = positions
        .iter()
        .map(|&p| {
            let mut o: Vec<u8> = t.iter().map(|x| (x >> (8 * p)) as u8).collect();
            o.resize(n.div_ceil(8), 0);
            o
        })
        .collect();
    for (o, &p) in out.iter_mut().zip(positions) {
        for i in whole * 8..n {
            o[i / 8] |= ((data[i] >> p) & 1) << (i % 8);
        }
    }
    out
}

/// A format 1 configuration (§1.3, as configured and answered): the raw channels' roles, the reduced ones'
/// (carriage, D), the block's L and the layout (w, the raw channels' bit positions).
#[derive(Debug, Clone, Default)]
pub struct Blocks {
    pub raw_roles: Vec<usize>,
    pub reduced: Vec<(usize, (u8, usize))>,
    pub block: usize,
    pub width: usize,
    pub positions: Vec<usize>,
}

/// A reduced channel over a segment: its carriage and D, the values packed as on the wire, how many, and change_latch's
/// level before the first.
#[derive(Debug, Clone, PartialEq)]
pub struct ReducedColumn {
    pub kind: u8,
    pub d: usize,
    pub data: Vec<u8>,
    pub n: usize,
    pub start_level: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Column {
    Raw(Vec<u8>),
    Reduced(ReducedColumn),
}

impl Blocks {
    fn sorted_reduced(&self) -> Vec<(usize, (u8, usize))> {
        let mut r = self.reduced.clone();
        r.sort();
        r
    }

    /// The bytes one block of r samples takes.
    pub fn block_bytes(&self, r: usize) -> usize {
        let mut bits = self.reduced.iter().filter(|(_, (k, _))| *k == CHANGE_LATCH).count();
        bits += self
            .reduced
            .iter()
            .map(|(_, (k, d))| r.div_ceil(*d) * value_bits(*k))
            .sum::<usize>();
        (r * self.width).div_ceil(8) + bits.div_ceil(8)
    }

    /// A segment's bytes: whole blocks of L and the last one.
    pub fn segment_bytes(&self, samples: usize) -> usize {
        let (whole, rest) = (samples / self.block, samples % self.block);
        whole * self.block_bytes(self.block) + if rest > 0 { self.block_bytes(rest) } else { 0 }
    }

    /// A segment read as columns: role -> raw bits or a reduced column (in role order: raws and reduced together).
    /// A change_latch block's start other than the level its interval before ended on, bytes left over or too few:
    /// a broken stream.
    pub fn columns(&self, data: &[u8], samples: usize) -> Result<Vec<(usize, Column)>> {
        if self.block == 0 {
            return Err(Error::Protocol("format 1: block 0".into()));
        }
        let expect = self.segment_bytes(samples);
        if data.len() != expect {
            return Err(Error::Protocol(format!(
                "format 1: {} bytes for {samples} samples ({expect} expected)",
                data.len()
            )));
        }
        let mut raws: Vec<usize> = self.raw_roles.clone();
        raws.sort();
        let red = self.sorted_reduced();
        let latches: Vec<usize> = red
            .iter()
            .filter(|(_, (k, _))| *k == CHANGE_LATCH)
            .map(|(r, _)| *r)
            .collect();
        let mut raw_out: Vec<BitBuf> = vec![BitBuf::default(); raws.len()];
        let mut red_out: Vec<BitBuf> = vec![BitBuf::default(); red.len()];
        let mut red_n = vec![0usize; red.len()];
        let mut starts: Vec<Option<u8>> = vec![None; red.len()];
        let mut lasts: Vec<Option<u8>> = vec![None; red.len()];
        let mut at = 0usize;
        let mut first = 0usize;
        while first < samples {
            let r = self.block.min(samples - first);
            let size = self.block_bytes(r);
            let rb = (r * self.width).div_ceil(8);
            let rawp = &data[at..at + rb];
            for (j, buf) in raw_out.iter_mut().enumerate() {
                let pos = self.positions.get(j).copied().unwrap_or(j);
                buf.append(&column(rawp, self.width, pos, r), r);
            }
            let part = &data[at + rb..at + size];
            let mut b = 0usize;
            let mut block_starts = vec![0u8; red.len()];
            for &role in &latches {
                let i = red.iter().position(|(x, _)| *x == role).expect("latch");
                block_starts[i] = bit(part, b);
                b += 1;
            }
            for (i, (role, (kind, d))) in red.iter().enumerate() {
                let nb = value_bits(*kind);
                let count = r.div_ceil(*d);
                let nbits = count * nb;
                let mut vals = vec![0u8; nbits.div_ceil(8)];
                for j in 0..nbits {
                    vals[j / 8] |= bit(part, b + j) << (j % 8);
                }
                if *kind == CHANGE_LATCH {
                    let s = block_starts[i];
                    match lasts[i] {
                        None => starts[i] = Some(s),
                        Some(l) if l != s => {
                            return Err(Error::Protocol(format!(
                                "format 1: role {role}'s block at sample {first} starts at level {s}, its interval \
                                 before ended at {l}"
                            )));
                        }
                        _ => {}
                    }
                    if count > 0 {
                        lasts[i] = Some(bit(&vals, (count - 1) * 2));
                    }
                }
                red_out[i].append(&vals, nbits);
                red_n[i] += count;
                b += nbits;
            }
            at += size;
            first += r;
        }
        let mut out: Vec<(usize, Column)> = raws
            .iter()
            .zip(raw_out)
            .map(|(&role, buf)| (role, Column::Raw(buf.bytes)))
            .collect();
        for (i, (role, (kind, d))) in red.iter().enumerate() {
            out.push((
                *role,
                Column::Reduced(ReducedColumn {
                    kind: *kind,
                    d: *d,
                    data: std::mem::take(&mut red_out[i].bytes),
                    n: red_n[i],
                    start_level: starts[i].unwrap_or(0),
                }),
            ));
        }
        out.sort_by_key(|(r, _)| *r);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(data: &[u8], width: usize, pos: usize, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n.div_ceil(8)];
        for i in 0..n {
            out[i / 8] |= bit(data, i * width + pos) << (i % 8);
        }
        out
    }

    #[test]
    fn columns_of_every_width_agree_with_the_bit_by_bit_reading() {
        let data: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        for width in [1, 3, 4, 8, 16, 24] {
            for pos in [0, width - 1, width / 2] {
                for n in [0, 1, 7, 8, 9, 63, 64, 100] {
                    assert_eq!(
                        column(&data, width, pos, n),
                        naive(&data, width, pos, n),
                        "{width} {pos} {n}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_column_at_once_is_each_column() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i.wrapping_mul(2654435761) >> 9) as u8).collect();
        for n in [0, 5, 7, 8, 9, 64, 1000] {
            let all = columns(&data, 8, &[0, 1, 2, 3, 4, 5, 6, 7], n);
            for (k, p) in (0..8).enumerate() {
                assert_eq!(all[k], column(&data, 8, p, n));
            }
        }
    }

    #[test]
    fn bits_append_across_bytes() {
        let mut b = BitBuf::default();
        b.append(&[0b101], 3);
        b.append(&[0xFF, 0x01], 9);
        b.append(&[0x00], 2);
        assert_eq!(b.nbits, 14);
        let want: Vec<u8> = [1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0].to_vec();
        assert_eq!((0..14).map(|j| bit(&b.bytes, j)).collect::<Vec<_>>(), want);
        assert_eq!(b.bytes.len(), 2);
    }

    #[test]
    fn a_format_1_segment_of_a_raw_and_a_change_latch_channel() {
        // L 8, w 1: role 0 raw, role 1 change_latch over 4; two blocks
        let blk = Blocks {
            raw_roles: vec![0],
            reduced: vec![(1, (CHANGE_LATCH, 4))],
            block: 8,
            width: 1,
            positions: vec![0],
        };
        assert_eq!(blk.block_bytes(8), 1 + 1); // 8 raw bits, 1 start bit + 2 x 2 value bits
        // block 0: raw 0xAA, start 0, values (level 1 changed) (level 1 unchanged): bits s=0, 1,1, 1,0
        let b0 = [0xAA, 0b00_1110];
        // block 1: raw 0x0F, start 1, values (0, changed) (0, unchanged): 1, 0,1, 0,0
        let b1 = [0x0F, 0b0_0101];
        let data = [b0, b1].concat();
        let cols = blk.columns(&data, 16).unwrap();
        assert_eq!(cols[0], (0, Column::Raw(vec![0xAA, 0x0F])));
        let Column::Reduced(c) = &cols[1].1 else { panic!() };
        assert_eq!((c.n, c.start_level), (4, 0));
        assert_eq!(c.data, vec![0b0010_0111]);
        // a broken start
        let bad = [b0, [0x0F, 0b0_0100]].concat();
        assert!(blk.columns(&bad, 16).is_err());
    }
}
