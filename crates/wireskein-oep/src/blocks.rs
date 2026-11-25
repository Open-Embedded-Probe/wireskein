//! A capture's data (oep-if-capture §1): format 0, samples of w bits with each channel at its bit (§1.1), and format 1,
//! the logic blocks (§1.3: blocks of L samples, each channel raw or reduced over intervals of D) - oep-client-python's
//! `acquire.from_interleaved` / `_samples_at_once` and `capture_blocks.segment_columns`. Format 0 is packed samples,
//! read by wireskein-capture's `packed` as every device's are; format 1 is read here, as a block layout of the
//! spool's.
//!
//! Bits are packed one a sample, sample i at bit i % 8 of byte i / 8.

use crate::msg::{Error, Result};

pub use wireskein_capture::packed::{BitBuf, bit, column, columns};
pub use wireskein_capture::parts::{
    ANY_HIGH, ANY_LOW, CHANGE_LATCH, RAW, SAMPLE, carriage_name, carriage_of, value_bits,
};
pub use wireskein_capture::spool::{Column, ReducedColumn};

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

/// Format 1 as the spool reads it (wireskein-capture's block layout).
impl wireskein_capture::spool::BlockLayout for Blocks {
    fn block(&self) -> usize {
        self.block
    }
    fn width(&self) -> usize {
        self.width
    }
    fn intervals(&self) -> Vec<usize> {
        self.reduced.iter().map(|(_, (_, d))| *d).collect()
    }
    fn columns(&self, data: &[u8], samples: usize) -> wireskein_capture::Result<Vec<Column>> {
        Ok(Blocks::columns(self, data, samples)?
            .into_iter()
            .map(|(_, c)| c)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
