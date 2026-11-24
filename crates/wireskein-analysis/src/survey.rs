//! An overview of a capture read once a block at a time (Python's `wireskein.survey`, wireskein-survey/1): per line
//! its changes, their count over the time in bins (activity), the intervals between them and the lengths of its highs
//! and lows in bins of powers of two, its longest stills, the share of its intervals of a tick or two, and for a line
//! that never stops its period and duty; over all the lines the longest stretches where none moves; with a plan, the
//! pieces a long capture would be cut into (a second read).

use crate::split;
use std::path::Path;
use wireskein_io::j::J;
use wireskein_io::jd;
use wireskein_io::stream::Source;

pub const FORMAT: &str = "wireskein-survey/1";
/// intervals kept per line for its median (the first ones)
const SAMPLE: usize = 200_000;

/// The power-of-two bin of a length (ticks >= 1): 0 for 1, 1 for 2-3, 2 for 4-7, ... (as numpy's floor(log2)).
fn bin_of(d: i64) -> usize {
    ((d.max(1) as f64).log2().floor() as usize).min(63)
}

fn hist(counts: &[i64; 64]) -> J {
    J::Dict(
        counts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c != 0)
            .map(|(k, c)| {
                let key = if k == 0 {
                    "1".to_string()
                } else {
                    format!("{}-{}", 1u64 << k, (2u64 << k) - 1)
                };
                (key, J::Int(*c))
            })
            .collect(),
    )
}

/// The `n` longest stills seen: (length, start); of equal ones the earliest.
struct Top {
    n: usize,
    v: Vec<(i64, i64)>,
}

impl Top {
    fn new(n: usize) -> Self {
        Top { n, v: vec![] }
    }

    fn add(&mut self, len: i64, start: i64) {
        self.v.push((len, start));
        if self.v.len() >= 2 * self.n.max(64) {
            self.trim();
        }
    }

    fn trim(&mut self) {
        if self.v.len() > self.n {
            let key = |x: &(i64, i64)| (-x.0, x.1);
            if self.n == 0 {
                self.v.clear();
            } else {
                self.v.select_nth_unstable_by_key(self.n - 1, key);
                self.v.truncate(self.n);
            }
        }
    }

    fn doc(&mut self, rate: f64) -> J {
        self.trim();
        self.v.sort_by_key(|x| (-x.0, x.1));
        J::List(
            self.v
                .iter()
                .map(|&(l, s)| jd! {"s" => s, "e" => s + l, "seconds" => l as f64 / rate})
                .collect(),
        )
    }
}

struct LineSurvey {
    name: String,
    initial: u8,
    last: Option<i64>,
    count: i64,
    activity: Vec<i64>,
    ivl: [i64; 64],
    high: [i64; 64],
    low: [i64; 64],
    sample: Vec<i64>,
    hsample: Vec<i64>,
    short: i64,
    top: Top,
    max_ivl: i64,
}

impl LineSurvey {
    fn new(name: &str, initial: u8, bins: usize, top: usize) -> Self {
        LineSurvey {
            name: name.into(),
            initial,
            last: None,
            count: 0,
            activity: vec![0; bins],
            ivl: [0; 64],
            high: [0; 64],
            low: [0; 64],
            sample: vec![],
            hsample: vec![],
            short: 0,
            top: Top::new(top),
            max_ivl: 0,
        }
    }

    fn add(&mut self, e: &[i64], n: i64, bins: usize) {
        if e.is_empty() {
            return;
        }
        let nn = n.max(1) as i128;
        for &x in e {
            let b = ((x as i128 * bins as i128).div_euclid(nn)).min(bins as i128 - 1).max(0) as usize;
            self.activity[b] += 1;
        }
        // the level of each run: the level before the change that ends it (change j of the line, from 0: initial ^
        // (j & 1))
        let mut j = if self.last.is_some() { self.count } else { 1 };
        let mut prev = self.last;
        for &x in e {
            if let Some(p) = prev {
                let d = x - p;
                let high = (self.initial as i64 ^ (j & 1)) != 0;
                let b = bin_of(d);
                self.ivl[b] += 1;
                if high {
                    self.high[b] += 1;
                } else {
                    self.low[b] += 1;
                }
                if d <= 2 {
                    self.short += 1;
                }
                self.top.add(d, p);
                self.max_ivl = self.max_ivl.max(d);
                if self.sample.len() < SAMPLE {
                    self.sample.push(d);
                    if high {
                        self.hsample.push(d);
                    }
                }
                j += 1;
            }
            prev = Some(x);
        }
        self.count += e.len() as i64;
        self.last = prev;
    }

    fn doc(&mut self, rate: f64) -> J {
        let mut out = jd! {"name" => self.name.as_str(), "initial" => self.initial as i64, "edges" => self.count,
        "activity" => J::List(self.activity.iter().map(|&x| J::Int(x)).collect()), "intervals" => hist(&self.ivl),
        "high" => hist(&self.high), "low" => hist(&self.low), "longest_still" => self.top.doc(rate)};
        let k = (self.count - 1).max(1);
        if self.count > 2 {
            out.set("short_share", round4(self.short as f64 / k as f64));
            let mut s: Vec<f64> = self.sample.iter().map(|&x| x as f64).collect();
            let usual = crate::states::median(&mut s);
            out.set("usual_interval_s", usual / rate);
            if self.max_ivl as f64 <= 4.0 * usual {
                // never stops: a free-running line
                let period = if self.sample.len() > 1 {
                    let mut p: Vec<f64> = self.sample.windows(2).map(|w| w[0] as f64 + w[1] as f64).collect();
                    crate::states::median(&mut p)
                } else {
                    2.0 * usual
                };
                let hi = if self.hsample.is_empty() {
                    None
                } else {
                    let mut h: Vec<f64> = self.hsample.iter().map(|&x| x as f64).collect();
                    Some(crate::states::median(&mut h))
                };
                let mut fr = jd! {"period_s" => period / rate,
                "freq_hz" => if period != 0.0 { J::Float(rate / period) } else { J::Null }};
                if let Some(h) = hi
                    && h != 0.0
                    && period != 0.0
                {
                    fr.set("duty", round4(h / period));
                }
                out.set("free_running", fr);
            }
        }
        out
    }
}

/// Python's `round(x, 4)`.
fn round4(x: f64) -> f64 {
    wireskein_io::pyfmt::round_n(x, 4)
}

/// The overview of a source (Python's `survey.survey`): rate its ticks a second; plan: the split options to also
/// give the pieces a long capture is cut into, read from `again` (the same lines read a second time).
pub fn survey(
    src: &mut dyn Source,
    rate: f64,
    bins: usize,
    top: usize,
    plan: Option<(&split::Options, &mut dyn Source)>,
) -> Result<J, String> {
    let bins = bins.max(1);
    let names = src.names().to_vec();
    let n = src.n();
    let mut lines: Vec<LineSurvey> = names
        .iter()
        .zip(src.initial())
        .map(|(k, &i)| LineSurvey::new(k, i, bins, top))
        .collect();
    let mut still = Top::new(top);
    let mut last_any: Option<i64> = None;
    let mut all: Vec<i64> = vec![];
    while let Some((_, got)) = src.next_block() {
        all.clear();
        for (l, e) in lines.iter_mut().zip(&got) {
            l.add(e, n, bins);
            all.extend_from_slice(e);
        }
        all.sort_unstable();
        for &x in &all {
            if let Some(p) = last_any {
                still.add(x - p, p);
            }
            last_any = Some(x);
        }
    }
    let mut out = jd! {"format" => FORMAT, "ticks" => n, "rate_hz" => rate, "bins" => bins,
    "bin_seconds" => if n != 0 { J::Float(n as f64 / bins as f64 / rate) } else { J::Int(0) },
    "lines" => J::List(lines.iter_mut().map(|l| l.doc(rate)).collect()), "all_still" => still.doc(rate)};
    let trembling: Vec<(String, J)> = out
        .get("lines")
        .list()
        .iter()
        .filter(|l| l.get("short_share").f().is_some_and(|s| s > 0.3))
        .map(|l| {
            (
                l.get("name").s().unwrap_or("").to_string(),
                l.get("short_share").clone(),
            )
        })
        .collect();
    if !trembling.is_empty() {
        out.set("trembling", J::Dict(trembling));
    }
    if let Some((o, again)) = plan {
        let pieces: Vec<J> = split::Splitter::new(again, o)?
            .map(|(p, _)| {
                let mut d = jd! {"s" => p.s, "e" => p.e, "edges" => p.edges, "cut" => p.cut};
                if let Some(r) = p.ratio {
                    d.set("ratio", wireskein_io::pyfmt::round_n(r, 1));
                }
                if (p.a, p.b) != (p.s, p.e) {
                    d.set("read", vec![p.a, p.b]);
                }
                d
            })
            .collect();
        out.set("pieces", J::List(pieces));
    }
    Ok(out)
}

/// Only the lines named (in their order), of a source.
struct Pick<'a> {
    src: Box<dyn Source + 'a>,
    keep: Vec<usize>,
    names: Vec<String>,
    initial: Vec<u8>,
}

impl Source for Pick<'_> {
    fn names(&self) -> &[String] {
        &self.names
    }
    fn n(&self) -> i64 {
        self.src.n()
    }
    fn initial(&self) -> &[u8] {
        &self.initial
    }
    fn next_block(&mut self) -> Option<(i64, Vec<Vec<i64>>)> {
        let (t, mut got) = self.src.next_block()?;
        Some((t, self.keep.iter().map(|&k| std::mem::take(&mut got[k])).collect()))
    }
}

fn pick<'a>(src: Box<dyn Source + 'a>, pins: Option<&[String]>) -> Result<Box<dyn Source + 'a>, String> {
    let Some(pins) = pins.filter(|p| !p.is_empty()) else {
        return Ok(src);
    };
    let miss: Vec<&str> = pins
        .iter()
        .filter(|p| !src.names().contains(p))
        .map(|s| s.as_str())
        .collect();
    if !miss.is_empty() {
        return Err(format!("pins not in the capture: {}", miss.join(", ")));
    }
    // in the capture's order, as Python's FileSource and `only` keep them
    let keep: Vec<usize> = (0..src.names().len())
        .filter(|&k| pins.contains(&src.names()[k]))
        .collect();
    let names = keep.iter().map(|&k| src.names()[k].clone()).collect();
    let initial = keep.iter().map(|&k| src.initial()[k]).collect();
    Ok(Box::new(Pick {
        src,
        keep,
        names,
        initial,
    }))
}

/// The overview of a capture file (Python's `survey.survey_file`, without its "wireskein"): a WireSkein file of one
/// part whose lines are `bits` / `events` at one tick a sample read a block at a time from the file; anything else
/// read whole first. "capture" as the analysis document gives it.
pub fn survey_file(
    path: &Path,
    pins: Option<&[String]>,
    bins: usize,
    top: usize,
    plan: Option<&split::Options>,
) -> Result<J, String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let kind = wireskein_io::sniff_path(path);
    let head = if kind == Some(wireskein_io::Kind::WireSkein) {
        let f = std::fs::File::open(path).map_err(|e| at(e.to_string()))?;
        let (c, _) = wireskein_io::wsk::head_from(f).map_err(at)?;
        if c.n_parts > 1 {
            return Err(at("several parts: survey one (convert --part K)".into()));
        }
        Some(c)
    } else {
        None
    };
    let streamed = match &head {
        Some(_) => wireskein_io::stream::file_source(path, 0).map_err(at)?.is_some(),
        None => false,
    };
    let whole = if streamed {
        None
    } else {
        let c = wireskein_io::read(path)?;
        if c.parts.len() != 1 {
            return Err(at(format!("{} parts: survey one", c.parts.len())));
        }
        Some(c)
    };
    let cap = whole.as_ref().or(head.as_ref()).unwrap();
    let rate = cap.rate();
    let open = || -> Result<Box<dyn Source + '_>, String> {
        let s: Box<dyn Source> = match &whole {
            None => Box::new(
                wireskein_io::stream::file_source(path, 0)
                    .map_err(at)?
                    .ok_or_else(|| at("not a part to read a block at a time".into()))?,
            ),
            Some(c) => Box::new(wireskein_io::stream::MemSource::new(
                &c.parts[0].lines,
                c.parts[0].ticks,
            )),
        };
        pick(s, pins).map_err(at)
    };
    let mut src = open()?;
    let mut doc = match plan {
        Some(o) => {
            let mut again = open()?;
            survey(src.as_mut(), rate, bins, top, Some((o, again.as_mut())))?
        }
        None => survey(src.as_mut(), rate, bins, top, None)?,
    };
    let mut capture = jd! {"file" => path.file_name().map(|s| s.to_string_lossy().to_string()),
    "sha256" => crate::sha256_of(path).map_err(at)?, "id" => cap.id.clone(),
    "tick_hz" => vec![cap.tick_hz.0, cap.tick_hz.1], "complete" => cap.complete};
    if !cap.lost.is_empty() {
        capture.set(
            "lost",
            J::parse(&serde_json::Value::Array(cap.lost.clone()).to_string())?,
        );
    }
    capture.set("layout", cap.layout.as_str());
    capture.set("parts", cap.n_parts);
    doc.set("capture", capture);
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wireskein_io::Line;

    #[test]
    fn a_clock_and_a_quiet_line() {
        let clk: Vec<i64> = (1..200).map(|k| 10 * k).collect();
        let lines = vec![
            Line {
                name: "CLK".into(),
                initial: 0,
                edges: clk,
                step: 1,
                phase: 0,
                reduction: None,
                acquisition: Default::default(),
            },
            Line {
                name: "Q".into(),
                initial: 1,
                edges: vec![500, 1500],
                step: 1,
                phase: 0,
                reduction: None,
                acquisition: Default::default(),
            },
        ];
        let mut src = wireskein_io::stream::MemSource::new(&lines, 2000);
        let d = survey(&mut src, 1e6, 10, 3, None).unwrap();
        let clk = &d.get("lines").list()[0];
        assert_eq!(clk.get("edges"), &J::Int(199));
        assert_eq!(clk.get("intervals").dumps(), r#"{"8-15":198}"#);
        assert_eq!(clk.get("free_running").get("duty"), &J::Float(0.5));
        let q = &d.get("lines").list()[1];
        assert_eq!(
            q.get("longest_still").dumps(),
            r#"[{"s":500,"e":1500,"seconds":0.001}]"#
        );
        assert!(q.get("free_running").is_null());
        assert_eq!(d.get("all_still").list().len(), 3);
    }
}
