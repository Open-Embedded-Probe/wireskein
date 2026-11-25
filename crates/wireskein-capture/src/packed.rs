//! Samples packed as a device sends them, into each line's bits and its changes: N lines in a sample of `width` bits
//! (1, 2 or 4 bytes for most devices: 8, 16 or 32 lines; OEP's capture format 0 also any other width, a sample
//! crossing bytes), line k at bit `positions[k]`; the stream's bit j is bit j % 8 of byte j / 8 (little endian).
//!
//! A line's bits come out packed one a sample, sample i at bit i % 8 of byte i / 8 (`column`, `columns`), which is
//! how the spool keeps them on disk; `changes` turns such bits into a line's level and its changes
//! (wireskein-io's `packed_edges`), and `Packing::lines` does both for samples in memory.

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

/// One line's bits of `n` samples of `width` bits each, the line at bit `pos` of a sample (a sample may cross a byte).
pub fn column(data: &[u8], width: usize, pos: usize, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n.div_ceil(8)];
    if width.is_multiple_of(8) && width > 0 {
        let b = width / 8;
        let (byte, sh) = (pos / 8, (pos % 8) as u32);
        let whole = n / 8;
        if b == 1 {
            // 8 samples a 64-bit word: the line's bits gathered into a byte by one multiply
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
#[inline]
fn transpose8(mut x: u64) -> u64 {
    let t = (x ^ (x >> 7)) & 0x00AA_00AA_00AA_00AA;
    x ^= t ^ (t << 7);
    let t = (x ^ (x >> 14)) & 0x0000_CCCC_0000_CCCC;
    x ^= t ^ (t << 14);
    let t = (x ^ (x >> 28)) & 0x0000_0000_F0F0_F0F0;
    x ^ t ^ (t << 28)
}

/// Byte `lane` of 8 samples of `b` bytes each, from `at`, as one word (sample i in byte i).
#[inline]
fn lane_word(data: &[u8], at: usize, b: usize, lane: usize) -> u64 {
    match b {
        1 => u64::from_le_bytes(data[at..at + 8].try_into().expect("8")),
        2 => {
            // the even (or odd) bytes of 16: each 16-bit sample's byte `lane` gathered
            let lo = u64::from_le_bytes(data[at..at + 8].try_into().expect("8"));
            let hi = u64::from_le_bytes(data[at + 8..at + 16].try_into().expect("8"));
            gather16(lo >> (8 * lane)) | (gather16(hi >> (8 * lane)) << 32)
        }
        _ => {
            let mut w = 0u64;
            for i in 0..8 {
                w |= (data[at + i * b + lane] as u64) << (8 * i);
            }
            w
        }
    }
}

/// Bytes 0, 2, 4, 6 of `x` into bytes 0..4.
#[inline]
fn gather16(x: u64) -> u64 {
    let x = x & 0x00FF_00FF_00FF_00FF;
    let x = (x | (x >> 8)) & 0x0000_FFFF_0000_FFFF;
    (x | (x >> 16)) & 0x0000_0000_FFFF_FFFF
}

/// Every line's bits of `n` samples of `width` bits (`column` for each position), the samples read once: for samples
/// of whole bytes each 8 samples' byte lane is transposed at once (byte p of the result: bit p of the 8 samples), then
/// each position's bytes taken out in one pass of its own (a stream's spool spent most of its time here, §42).
pub fn columns(data: &[u8], width: usize, positions: &[usize], n: usize) -> Vec<Vec<u8>> {
    if !(width == 8 || width == 16 || width == 32) || positions.iter().any(|&p| p >= width) {
        return positions.iter().map(|&p| column(data, width, p, n)).collect();
    }
    let b = width / 8;
    let whole = n / 8;
    let mut lanes: Vec<Option<Vec<u64>>> = vec![None; b];
    for &p in positions {
        let lane = p / 8;
        if lanes[lane].is_none() {
            lanes[lane] = Some(
                (0..whole)
                    .map(|k| transpose8(lane_word(data, k * 8 * b, b, lane)))
                    .collect(),
            );
        }
    }
    let mut out: Vec<Vec<u8>> = positions
        .iter()
        .map(|&p| {
            let t = lanes[p / 8].as_ref().expect("lane");
            let sh = 8 * (p % 8);
            let mut o: Vec<u8> = t.iter().map(|x| (x >> sh) as u8).collect();
            o.resize(n.div_ceil(8), 0);
            o
        })
        .collect();
    for (o, &p) in out.iter_mut().zip(positions) {
        for i in whole * 8..n {
            o[i / 8] |= ((data[i * b + p / 8] >> (p % 8)) & 1) << (i % 8);
        }
    }
    out
}

/// A line's bits (as `column` gives them) as its first level and the ticks it changes at (sample i at tick
/// phase + i x step).
pub fn changes(bits: &[u8], n: usize, step: i64, phase: i64) -> (u8, Vec<i64>) {
    wireskein_io::packed_edges(bits, n, step, phase)
}

/// How a device packs its samples: `width` bits a sample, line k at bit `positions[k]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Packing {
    pub width: usize,
    pub positions: Vec<usize>,
}

impl Packing {
    /// `lines` lines in the fewest whole bytes of 1, 2 or 4 (line k at bit k).
    pub fn bytes_for(lines: usize) -> Packing {
        let width = match lines {
            0..=8 => 8,
            9..=16 => 16,
            _ => 32,
        };
        Packing {
            width,
            positions: (0..lines).collect(),
        }
    }

    /// The bytes `n` samples take.
    pub fn bytes(&self, n: u64) -> u64 {
        (n * self.width as u64).div_ceil(8)
    }

    /// The samples whole in `bytes`.
    pub fn samples_in(&self, bytes: u64) -> u64 {
        bytes * 8 / self.width.max(1) as u64
    }

    /// Each line's bits of `n` samples.
    pub fn columns(&self, data: &[u8], n: usize) -> Vec<Vec<u8>> {
        columns(data, self.width, &self.positions, n)
    }

    /// Each line's first level and changes over `n` samples (sample i at tick i).
    pub fn lines(&self, data: &[u8], n: usize) -> Vec<(u8, Vec<i64>)> {
        self.columns(data, n).iter().map(|c| changes(c, n, 1, 0)).collect()
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

    fn noise(n: usize, sh: u32) -> Vec<u8> {
        (0..n as u32)
            .map(|i| (i.wrapping_mul(2654435761) >> sh) as u8)
            .collect()
    }

    #[test]
    fn columns_of_every_width_agree_with_the_bit_by_bit_reading() {
        let data = noise(4096, 13);
        for width in [1, 3, 4, 8, 16, 24, 32] {
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
        let data = noise(4000, 9);
        for width in [8usize, 16, 32] {
            let all_pos: Vec<usize> = (0..width).collect();
            let some: Vec<usize> = vec![width - 1, 0, 9 % width, 3];
            for pos in [all_pos, some] {
                for n in [0, 5, 7, 8, 9, 64, 100, 1000 * 8 / width] {
                    let all = columns(&data, width, &pos, n);
                    for (k, &p) in pos.iter().enumerate() {
                        assert_eq!(all[k], naive(&data, width, p, n), "{width} {p} {n}");
                    }
                }
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
    fn a_counter_packed_in_two_bytes_is_lines_of_halving_rates() {
        let n = 1000usize;
        let data: Vec<u8> = (0..n as u16).flat_map(|i| i.to_le_bytes()).collect();
        let p = Packing::bytes_for(12);
        assert_eq!(p.width, 16);
        for (k, (first, edges)) in p.lines(&data, n).into_iter().enumerate() {
            assert_eq!(first, 0);
            let period = 1i64 << k;
            let want: Vec<i64> = (1..n as i64).filter(|i| i % period == 0).collect();
            assert_eq!(edges, want, "line {k}");
        }
    }

    /// The common conversion's speed: samples of 1, 2 and 4 bytes into every line's bits, and the bits into changes
    /// (cargo test --release -p wireskein-capture -- --ignored --nocapture speed).
    #[test]
    #[ignore]
    fn speed_of_the_conversion() {
        let mb = 400usize;
        let data = noise(mb << 20, 7);
        for width in [8usize, 16, 32] {
            let pos: Vec<usize> = (0..width).collect();
            let n = data.len() * 8 / width;
            let chunk = 1 << 20; // a spool's pieces
            let t = std::time::Instant::now();
            let mut sum = 0usize;
            for c in data.chunks(chunk) {
                let k = c.len() * 8 / width;
                for col in columns(c, width, &pos, k) {
                    sum += col.len();
                }
            }
            let s = t.elapsed().as_secs_f64();
            println!(
                "columns: {width} lines in {} B a sample, {n} samples: {:.0} MB/s ({sum} B out)",
                width / 8,
                data.len() as f64 / s / 1e6
            );
        }
        let bits = noise(64 << 20, 11);
        let t = std::time::Instant::now();
        let (_, e) = changes(&bits, bits.len() * 8, 1, 0);
        let s = t.elapsed().as_secs_f64();
        println!(
            "changes of busy bits: {:.0} MB/s ({} changes)",
            bits.len() as f64 / s / 1e6,
            e.len()
        );
        let quiet = vec![0u8; 64 << 20];
        let t = std::time::Instant::now();
        let _ = changes(&quiet, quiet.len() * 8, 1, 0);
        println!(
            "changes of a quiet line: {:.0} MB/s",
            quiet.len() as f64 / t.elapsed().as_secs_f64() / 1e6
        );
    }
}
