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
    let chans = p["channels"].as_array().cloned().unwrap_or_default();
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
