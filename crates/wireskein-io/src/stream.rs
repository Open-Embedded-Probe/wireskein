//! A part's logic lines read a block of ticks at a time (Python's `split.Source`, `CaptureSource`, `FileSource`):
//! only the block's changes are handed on, so a long capture is read in the memory its piece takes. A WireSkein
//! file's part whose lines are `bits` or `events` at one tick a sample is read straight from the file: each `bits`
//! line decompressed by a thread of its own, a block at a time, through a channel that holds one block (a line kept
//! as `events` is its changes already: read whole, handed on by the block).

use crate::Line;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const BLOCK: i64 = 1 << 24; // ticks a block (a multiple of 8: a bits line's block is whole bytes)

/// Logic lines handed on a block of ticks at a time.
pub trait Source {
    fn names(&self) -> &[String];
    /// The part's ticks.
    fn n(&self) -> i64;
    /// Each line's level before tick 0.
    fn initial(&self) -> &[u8];
    /// The next block: the tick it reaches, each line's changes in it (in the order of names); None past the end.
    fn next_block(&mut self) -> Option<(i64, Vec<Vec<i64>>)>;
}

/// Lines already in memory, handed on by blocks.
pub struct MemSource<'a> {
    lines: &'a [Line],
    names: Vec<String>,
    initial: Vec<u8>,
    n: i64,
    t0: i64,
    block: i64,
}

impl<'a> MemSource<'a> {
    pub fn new(lines: &'a [Line], n: i64) -> Self {
        MemSource {
            lines,
            names: lines.iter().map(|l| l.name.clone()).collect(),
            initial: lines.iter().map(|l| l.initial).collect(),
            n,
            t0: 0,
            block: BLOCK,
        }
    }
}

impl Source for MemSource<'_> {
    fn names(&self) -> &[String] {
        &self.names
    }
    fn n(&self) -> i64 {
        self.n
    }
    fn initial(&self) -> &[u8] {
        &self.initial
    }
    fn next_block(&mut self) -> Option<(i64, Vec<Vec<i64>>)> {
        if self.t0 >= self.n {
            return None;
        }
        let t1 = (self.t0 + self.block).min(self.n);
        let out = self
            .lines
            .iter()
            .map(|l| {
                let a = l.edges.partition_point(|&x| x < self.t0);
                let b = l.edges.partition_point(|&x| x < t1);
                l.edges[a..b].to_vec()
            })
            .collect();
        self.t0 = t1;
        Some((t1, out))
    }
}

enum Feed {
    /// a bits line: its blocks from its thread
    Bits(std::sync::mpsc::Receiver<Result<Vec<i64>, String>>),
    /// a line kept as its changes, whole
    Events(Vec<i64>),
}

/// One part of a WireSkein file read from the file a block at a time.
pub struct FileSource {
    names: Vec<String>,
    initial: Vec<u8>,
    feeds: Vec<Feed>,
    n: i64,
    t0: i64,
}

/// The changes of a bits line's ticks [t0, t0 + bits.len()) against the level before.
fn changes(bits: &[u8], n_ticks: usize, t0: i64, last: &mut u8) -> Vec<i64> {
    let mut out = vec![];
    for i in 0..n_ticks {
        let v = (bits[i / 8] >> (i % 8)) & 1;
        if v != *last {
            out.push(t0 + i as i64);
            *last = v;
        }
    }
    out
}

/// The part's source read from the file, or None where its lines are not all `bits` / `events` at one tick a
/// sample (read whole then).
pub fn file_source(path: &Path, part: usize) -> Result<Option<FileSource>, String> {
    file_source_of(path, part, None)
}

/// As `file_source`, only the lines named in `pins` (the others neither read nor asked to be `bits` / `events`).
pub fn file_source_of(path: &Path, part: usize, pins: Option<&[String]>) -> Result<Option<FileSource>, String> {
    if cfg!(target_family = "wasm") {
        return Ok(None); // (no threads to read the lines by: read whole)
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut s = String::new();
    z.by_name(crate::wsk::CAPTURE)
        .map_err(|e| e.to_string())?
        .read_to_string(&mut s)
        .map_err(|e| e.to_string())?;
    let head: Value = serde_json::from_str(&s).map_err(|e| e.to_string())?;
    let Some(p) = head["parts"].get(part) else {
        return Ok(None);
    };
    let n = p["ticks"].as_i64().ok_or("a part without its ticks")?;
    let chans: Vec<Value> = p["channels"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|c| pins.is_none_or(|ps| ps.is_empty() || ps.iter().any(|x| c["name"].as_str() == Some(x))))
        .collect();
    let ok = chans.iter().all(|c| {
        matches!(c["encoding"].as_str(), Some("bits" | "events"))
            && c["step"].as_i64() == Some(1)
            && c["phase"].as_i64().unwrap_or(0) == 0
    });
    if !ok || chans.is_empty() {
        return Ok(None);
    }
    let (mut names, mut initial, mut feeds) = (vec![], vec![], vec![]);
    for c in &chans {
        let name = c["name"].as_str().unwrap_or("").to_string();
        let entry = c["file"].as_str().unwrap_or("").to_string();
        if c["encoding"] == "events" {
            let mut data = vec![];
            z.by_name(&entry)
                .map_err(|e| e.to_string())?
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            let lines = crate::wsk::read_events_line(c, &data)?;
            initial.push(lines.initial);
            feeds.push(Feed::Events(lines.edges));
        } else {
            let mut first = [0u8; 1];
            let got = z
                .by_name(&entry)
                .map_err(|e| e.to_string())?
                .read(&mut first)
                .map_err(|e| e.to_string())?;
            let init = if got > 0 { first[0] & 1 } else { 0 };
            initial.push(init);
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let path: PathBuf = path.to_path_buf();
            std::thread::spawn(move || {
                let run = || -> Result<(), String> {
                    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                    let mut f = z.by_name(&entry).map_err(|e| e.to_string())?;
                    let (mut t0, mut last) = (0i64, init);
                    let mut buf = vec![0u8; (BLOCK / 8) as usize];
                    while t0 < n {
                        let t1 = (t0 + BLOCK).min(n);
                        let want = ((t1 - t0 + 7) / 8) as usize;
                        let mut got = 0;
                        while got < want {
                            let k = f.read(&mut buf[got..want]).map_err(|e| e.to_string())?;
                            if k == 0 {
                                break;
                            }
                            got += k;
                        }
                        let ticks = ((t1 - t0) as usize).min(got * 8);
                        if tx.send(Ok(changes(&buf[..got], ticks, t0, &mut last))).is_err() {
                            return Ok(()); // the reader stopped
                        }
                        t0 = t1;
                    }
                    Ok(())
                };
                if let Err(e) = run() {
                    let _ = tx.send(Err(e));
                }
            });
            feeds.push(Feed::Bits(rx));
        }
        names.push(name);
    }
    Ok(Some(FileSource {
        names,
        initial,
        feeds,
        n,
        t0: 0,
    }))
}

impl Source for FileSource {
    fn names(&self) -> &[String] {
        &self.names
    }
    fn n(&self) -> i64 {
        self.n
    }
    fn initial(&self) -> &[u8] {
        &self.initial
    }
    fn next_block(&mut self) -> Option<(i64, Vec<Vec<i64>>)> {
        if self.t0 >= self.n {
            return None;
        }
        let t1 = (self.t0 + BLOCK).min(self.n);
        let mut out = vec![];
        for f in &self.feeds {
            out.push(match f {
                Feed::Bits(rx) => rx.recv().ok()?.ok()?,
                Feed::Events(e) => {
                    let a = e.partition_point(|&x| x < self.t0);
                    let b = e.partition_point(|&x| x < t1);
                    e[a..b].to_vec()
                }
            });
        }
        self.t0 = t1;
        Some((t1, out))
    }
}

/// A block: the tick it reaches, each line's changes in it.
type Block = (i64, Vec<Vec<i64>>);

/// A logic-only sigrok session (.sr) read a block of samples at a time (Python's `split.SrSource`): its chunks
/// decompressed by a thread as they are read, each line's changes in the block handed on.
pub struct SrSource {
    names: Vec<String>,
    initial: Vec<u8>,
    rx: std::sync::mpsc::Receiver<Result<Block, String>>,
    n: i64,
    /// samples a second
    pub rate: f64,
}

/// A session's source, or None where it has analog channels or lines of their own step (read whole then). pins:
/// only these lines, in this order (an error naming those it does not have).
pub fn sr_source(path: &Path, pins: Option<&[String]>) -> Result<Option<SrSource>, String> {
    if cfg!(target_family = "wasm") {
        return Ok(None);
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut text = String::new();
    z.by_name("metadata")
        .map_err(|e| e.to_string())?
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    let md = crate::sr::metadata(&text);
    let dev = md.get("device 1").ok_or("no [device 1] in the metadata")?;
    if dev.keys().any(|k| {
        k.strip_prefix("analog")
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|c| c.is_ascii_digit()))
    }) {
        return Ok(None);
    }
    if let Ok(mut f) = z.by_name(crate::sr::EXTRA) {
        let mut s = String::new();
        f.read_to_string(&mut s).map_err(|e| e.to_string())?;
        let extra: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
        let own_step = extra["channels"].as_object().is_some_and(|cs| {
            cs.values()
                .any(|c| c.get("step").and_then(|v| v.as_i64()).unwrap_or(1) != 1)
        });
        if extra["analog"].as_object().is_some_and(|a| !a.is_empty()) || own_step {
            return Ok(None);
        }
    }
    let rate = crate::sr::rate_of(dev.get("samplerate").ok_or("the .sr gives no samplerate")?)?;
    let unitsize: usize = dev.get("unitsize").and_then(|v| v.parse().ok()).unwrap_or(1);
    if !(1..=8).contains(&unitsize) {
        return Err(format!("a .sr sample of {unitsize} bytes: up to 8 (64 channels) read"));
    }
    let prefix = dev.get("capturefile").cloned().unwrap_or_else(|| "logic-1".into());
    let names: std::collections::BTreeMap<usize, String> = dev
        .iter()
        .filter_map(|(k, v)| Some((k.strip_prefix("probe")?.parse::<usize>().ok()?, v.clone())))
        .collect();
    let total: usize = dev
        .get("total probes")
        .and_then(|v| v.parse().ok())
        .unwrap_or(unitsize * 8);
    let mut chunks: Vec<(i64, String)> = z
        .file_names()
        .filter(|n| *n == prefix || n.starts_with(&format!("{prefix}-")))
        .map(|n| {
            let k = if n == prefix {
                0
            } else {
                n.rsplit('-').next().and_then(|x| x.parse().ok()).unwrap_or(0)
            };
            (k, n.to_string())
        })
        .collect();
    chunks.sort();
    let bits: Vec<usize> = if names.is_empty() {
        (0..total.min(unitsize * 8)).collect()
    } else {
        names.keys().map(|b| b - 1).collect()
    };
    let mut plan: Vec<(String, usize)> = bits
        .into_iter()
        .filter(|&b| b < unitsize * 8)
        .map(|b| (names.get(&(b + 1)).cloned().unwrap_or_else(|| format!("bit{b}")), b))
        .collect();
    if let Some(ps) = pins.filter(|p| !p.is_empty()) {
        let miss: Vec<&str> = ps
            .iter()
            .filter(|p| !plan.iter().any(|(n, _)| n == *p))
            .map(|s| s.as_str())
            .collect();
        if !miss.is_empty() {
            return Err(format!("pins not in the capture: {}", miss.join(", ")));
        }
        plan = ps
            .iter()
            .map(|p| plan.iter().find(|(n, _)| n == p).cloned().unwrap())
            .collect();
    }
    let mut bytes = 0u64;
    for (_, c) in &chunks {
        bytes += z.by_name(c).map_err(|e| e.to_string())?.size();
    }
    let n = (bytes / unitsize as u64) as i64;
    // the level of each line before tick 0: its first sample's
    let first: u64 = {
        let mut v = 0u64;
        if let Some((_, c)) = chunks.first() {
            let mut b = vec![0u8; unitsize];
            let mut f = z.by_name(c).map_err(|e| e.to_string())?;
            let mut got = 0;
            while got < unitsize {
                let k = f.read(&mut b[got..]).map_err(|e| e.to_string())?;
                if k == 0 {
                    break;
                }
                got += k;
            }
            if got == unitsize {
                v = b.iter().rev().fold(0u64, |a, &x| (a << 8) | x as u64);
            }
        }
        v
    };
    let initial: Vec<u8> = plan.iter().map(|(_, b)| ((first >> b) & 1) as u8).collect();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let path: PathBuf = path.to_path_buf();
    let bits: Vec<usize> = plan.iter().map(|(_, b)| *b).collect();
    let init = initial.clone();
    std::thread::spawn(move || {
        const SAMPLES: usize = 1 << 20;
        let run = || -> Result<(), String> {
            let mut z = zip::ZipArchive::new(std::fs::File::open(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let mut last = init;
            let mut idx: i64 = 0;
            let mut buf = vec![0u8; SAMPLES * unitsize];
            let mut carry: Vec<u8> = vec![];
            for (_, name) in &chunks {
                let mut f = z.by_name(name).map_err(|e| e.to_string())?;
                loop {
                    let got = f.read(&mut buf).map_err(|e| e.to_string())?;
                    if got == 0 {
                        break;
                    }
                    carry.extend_from_slice(&buf[..got]);
                    let whole = carry.len() / unitsize * unitsize;
                    let mut out: Vec<Vec<i64>> = vec![vec![]; bits.len()];
                    let mut prev: Option<u64> = None;
                    for s in carry[..whole].chunks_exact(unitsize) {
                        let v = s.iter().rev().fold(0u64, |a, &x| (a << 8) | x as u64);
                        if prev != Some(v) {
                            prev = Some(v);
                            for (k, &b) in bits.iter().enumerate() {
                                let lv = ((v >> b) & 1) as u8;
                                if lv != last[k] {
                                    out[k].push(idx);
                                    last[k] = lv;
                                }
                            }
                        }
                        idx += 1;
                    }
                    carry.drain(..whole);
                    if tx.send(Ok((idx, out))).is_err() {
                        return Ok(());
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = run() {
            let _ = tx.send(Err(e));
        }
    });
    Ok(Some(SrSource {
        names: plan.into_iter().map(|(n, _)| n).collect(),
        initial,
        rx,
        n,
        rate,
    }))
}

impl Source for SrSource {
    fn names(&self) -> &[String] {
        &self.names
    }
    fn n(&self) -> i64 {
        self.n
    }
    fn initial(&self) -> &[u8] {
        &self.initial
    }
    fn next_block(&mut self) -> Option<(i64, Vec<Vec<i64>>)> {
        self.rx.recv().ok()?.ok()
    }
}
