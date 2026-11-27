//! The bits a clocked reading sampled, as Python's export gives them (`export._sync_layers` and `_words` over
//! `typed.SyncBits`, `Frames`, `Words`): `bits` (every sampling edge of the clock as the reading took it, each data
//! line's level there), `frames` (the reading's frames over those edges), `conditions` (a START / STOP framing's
//! events) and `words` (each frame's bits in words of a size, MSB first, per data line). Ticks as integers, in the
//! order Python writes them. Kept compact while a reader compares its readings and written as JSON for the readings
//! it keeps only (a long capture's sampling edges are millions).

use serde_json::value::RawValue;
use std::fmt::Write;

#[derive(Debug, Clone, Default)]
pub struct Sampled {
    pub clock: String,
    pub data: Vec<String>,
    /// "rise" / "fall"
    pub sample_edge: &'static str,
    /// the width the lines were settled by (the reading's view; 0: as captured)
    pub deglitch: i64,
    /// every sampling edge of the clock
    pub t: Vec<i64>,
    /// per data line, its level at each of `t`
    pub values: Vec<Vec<u8>>,
    /// the frames: [a, b) of `t`
    pub frames: Vec<(usize, usize)>,
    /// START / STOP framing's events ("S", "Sr", "P"; tick): every change of the data line while the clock is high
    pub conditions: Vec<(&'static str, i64)>,
    /// bits a word (I2C 9: the byte and its acknowledge; SPI 8); 0: no words
    pub word: usize,
}

fn list<T: std::fmt::Display>(out: &mut String, v: &[T]) {
    out.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{x}");
    }
    out.push(']');
}

fn string(s: &str) -> String {
    serde_json::to_string(s).expect("a string")
}

impl Sampled {
    /// The index range of `t` holding the ticks of a span's sampling edges [first, last] (both in `t`).
    pub fn span(&self, first: i64, last: i64) -> (usize, usize) {
        (
            self.t.partition_point(|&x| x < first),
            self.t.partition_point(|&x| x <= last),
        )
    }

    /// The layers as JSON members (`"bits": {...}, "frames": [...], ...`), without the braces.
    pub fn members(&self) -> String {
        let mut o = String::with_capacity(16 * self.t.len() * (1 + self.values.len()) + 64);
        let _ = write!(
            o,
            "\"bits\":{{\"clock\":{},\"data\":[{}],\"sample_edge\":{},\"t\":",
            string(&self.clock),
            self.data.iter().map(|d| string(d)).collect::<Vec<_>>().join(","),
            string(self.sample_edge)
        );
        list(&mut o, &self.t);
        o.push_str(",\"values\":[");
        for (i, row) in self.values.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            list(&mut o, row);
        }
        let _ = write!(o, "],\"deglitch\":{}}}", self.deglitch);
        // (a frame of no bit is none: Python's `b > a`)
        o.push_str(",\"frames\":[");
        let mut first = true;
        for &(a, b) in self.frames.iter().filter(|&&(a, b)| b > a) {
            if !first {
                o.push(',');
            }
            first = false;
            let _ = write!(o, "{{\"s\":{},\"e\":{},\"bits\":{}}}", self.t[a], self.t[b - 1], b - a);
        }
        o.push(']');
        if !self.conditions.is_empty() {
            o.push_str(",\"conditions\":[");
            for (i, (k, s)) in self.conditions.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                let _ = write!(o, "{{\"kind\":\"{k}\",\"s\":{s}}}");
            }
            o.push(']');
        }
        if self.word > 0 {
            o.push_str(",\"words\":");
            o.push_str(&self.words_json());
        }
        o
    }

    /// The words (`export._words`): per data line, each frame's whole words of `word` bits, MSB first; a word's
    /// start and end its first and last sampling edge.
    fn words_json(&self) -> String {
        let size = self.word;
        let mut o = String::from("[");
        let mut first = true;
        for (row, pin) in self.data.iter().enumerate() {
            let pin = string(pin);
            let bits = &self.values[row];
            for &(a, b) in &self.frames {
                for k in 0..b.saturating_sub(a) / size {
                    let s = a + k * size;
                    let v = bits[s..s + size].iter().fold(0i64, |acc, &x| (acc << 1) | x as i64);
                    if !first {
                        o.push(',');
                    }
                    first = false;
                    let e = self.t[(s + size).min(self.t.len()) - 1];
                    let _ = write!(o, "{{\"pin\":{pin},\"s\":{},\"e\":{e},\"value\":{v}}}", self.t[s]);
                }
            }
        }
        o.push(']');
        o
    }
}

/// A reading's layers with members put first (`before`: the sampled bits, as Python's export gives them ahead of
/// the reading's own) and after (`after`).
pub fn joined(layers: &RawValue, before: &str, after: &str) -> Box<RawValue> {
    let own = layers.get().trim();
    let inner = own
        .strip_prefix('{')
        .and_then(|x| x.strip_suffix('}'))
        .unwrap_or("")
        .trim();
    let parts: Vec<&str> = [before, inner, after].into_iter().filter(|x| !x.is_empty()).collect();
    crate::claim::raw(format!("{{{}}}", parts.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn layers_as_python_gives_them() {
        let s = Sampled {
            clock: "SCL".into(),
            data: vec!["SDA".into()],
            sample_edge: "rise",
            deglitch: 2,
            t: (0..20).map(|i| 10 * i).collect(),
            values: vec![(0..20).map(|i| (i % 3 == 0) as u8).collect()],
            frames: vec![(1, 11), (11, 11), (12, 20)],
            conditions: vec![("S", 5), ("P", 115)],
            word: 9,
        };
        let raw = joined(&crate::claim::raw("{\"transactions\":[]}".into()), &s.members(), "");
        let v: Value = serde_json::from_str(raw.get()).unwrap();
        assert_eq!(v["bits"]["t"].as_array().unwrap().len(), 20);
        assert_eq!(v["bits"]["deglitch"], json!(2));
        assert_eq!(v["bits"]["values"][0][3], json!(1));
        assert_eq!(
            v["frames"],
            json!([{"s": 10, "e": 100, "bits": 10}, {"s": 120, "e": 190, "bits": 8}])
        );
        assert_eq!(v["conditions"][1], json!({"kind": "P", "s": 115}));
        // one whole word of 9 in the first frame (bits 1..10: 0,0,1,0,0,1,0,0,1), none in the others
        assert_eq!(
            v["words"],
            json!([{"pin": "SDA", "s": 10, "e": 90, "value": 0b001001001}])
        );
        assert_eq!(v["transactions"], json!([]));
        // (the members in Python's order)
        let keys: Vec<&str> = [
            "\"bits\"",
            "\"frames\"",
            "\"conditions\"",
            "\"words\"",
            "\"transactions\"",
        ]
        .into_iter()
        .collect();
        let at: Vec<usize> = keys.iter().map(|k| raw.get().find(k).unwrap()).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
    }
}
