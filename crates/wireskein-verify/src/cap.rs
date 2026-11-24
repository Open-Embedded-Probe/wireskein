//! A capture part as the checks read it (Python's `_engine.model.Capture` as `fileio.load_runs` gives it): its lines
//! (Channel: a level before tick 0 and the ticks it changes at, of a step), its lines kept as changes at a coarser
//! step (EventTrace), the names of its analog channels, its meta; a window cut from a longer part keeps that part.

use crate::j::J;
use std::path::Path;
use std::rc::Rc;
pub use wireskein_io::Events;

#[derive(Debug, Clone)]
pub struct Ch {
    pub name: String,
    pub initial: u8,
    pub edges: Vec<i64>,
    pub step: i64,
    pub phase: i64,
    /// any_high / any_low / change_latch (wireskein-format §4.5)
    pub reduction: Option<String>,
}

impl Ch {
    pub fn new(name: &str, initial: u8, edges: Vec<i64>) -> Ch {
        Ch {
            name: name.into(),
            initial,
            edges,
            step: 1,
            phase: 0,
            reduction: None,
        }
    }

    /// The level at a tick (`searchsorted(edges, t, side="right")`).
    pub fn level_at(&self, t: i64) -> u8 {
        self.initial ^ (self.edges.partition_point(|&e| e <= t) as u8 & 1)
    }

    /// The level after edge i.
    pub fn after(&self, i: usize) -> u8 {
        self.initial ^ (((i + 1) & 1) as u8)
    }

    /// (start, length, level) of each run, the cut first and last included (`kernels.runs`).
    pub fn runs(&self, n: i64) -> (Vec<i64>, Vec<i64>, Vec<u8>) {
        let mut b = Vec::with_capacity(self.edges.len() + 2);
        b.push(0);
        b.extend_from_slice(&self.edges);
        b.push(n);
        let start = b[..b.len() - 1].to_vec();
        let length = b.windows(2).map(|w| w[1] - w[0]).collect();
        let level = (0..start.len()).map(|k| self.initial ^ (k as u8 & 1)).collect();
        (start, length, level)
    }

    pub fn rises(&self) -> Vec<i64> {
        let o = if self.initial == 0 { 0 } else { 1 };
        self.edges.iter().skip(o).step_by(2).copied().collect()
    }

    pub fn falls(&self) -> Vec<i64> {
        let o = if self.initial == 0 { 1 } else { 0 };
        self.edges.iter().skip(o).step_by(2).copied().collect()
    }

    /// The core's view of the line.
    pub fn core(&self) -> wireskein_core::Channel<'_> {
        wireskein_core::Channel::new(&self.name, self.initial, &self.edges)
    }
}

#[derive(Debug, Clone)]
pub struct Cap {
    pub rate: f64,
    pub tick_hz: (i64, i64),
    pub n: i64,
    pub channels: Vec<Ch>,
    pub events: Vec<Events>,
    /// the analog channels (not read by this version: named only)
    pub analog: Vec<String>,
    pub meta: J,
    /// a window: the part it was cut from and where
    pub parent: Option<(Rc<Cap>, i64)>,
}

impl Cap {
    pub fn ch(&self, name: &str) -> Option<&Ch> {
        self.channels.iter().find(|c| c.name == name)
    }

    pub fn ev(&self, name: &str) -> Option<&Events> {
        self.events.iter().find(|t| t.name == name)
    }

    /// Every name it holds: lines, lines kept as changes, analog channels.
    pub fn names(&self) -> std::collections::BTreeSet<&str> {
        self.channels
            .iter()
            .map(|c| c.name.as_str())
            .chain(self.events.iter().map(|t| t.name.as_str()))
            .chain(self.analog.iter().map(|s| s.as_str()))
            .collect()
    }

    pub fn label(&self) -> String {
        self.meta.get("label").s().unwrap_or("").to_string()
    }

    /// `meta["window"][0]`, or 0.
    pub fn window_base(&self) -> i64 {
        self.meta.get("window").list().first().and_then(|v| v.i()).unwrap_or(0)
    }
}

/// Ticks [s0, s1) of a capture as a capture of its own, tick 0 at s0 (`verify._window`).
pub fn window(cap: &Rc<Cap>, s0: i64, s1: i64) -> Cap {
    let channels = cap
        .channels
        .iter()
        .map(|ch| Ch {
            name: ch.name.clone(),
            initial: ch.level_at(s0),
            edges: ch
                .edges
                .iter()
                .filter(|&&e| e > s0 && e < s1)
                .map(|&e| e - s0)
                .collect(),
            step: ch.step,
            phase: (ch.phase - s0).rem_euclid(ch.step.max(1)),
            reduction: ch.reduction.clone(),
        })
        .collect();
    let events = cap
        .events
        .iter()
        .map(|t| {
            let k0 = 0.max(-((t.phase - s0).div_euclid(t.step)));
            let k1 = k0.max(t.n.min((s1 - t.phase).div_euclid(t.step)));
            let before = t.quanta.partition_point(|&q| q < k0);
            let sel: Vec<usize> = (0..t.quanta.len())
                .filter(|&i| t.quanta[i] >= k0 && t.quanta[i] < k1)
                .collect();
            Events {
                name: t.name.clone(),
                step: t.step,
                phase: t.phase + k0 * t.step - s0,
                n: k1 - k0,
                start_level: if before > 0 {
                    t.levels[before - 1]
                } else {
                    t.start_level
                },
                quanta: sel.iter().map(|&i| t.quanta[i] - k0).collect(),
                levels: sel.iter().map(|&i| t.levels[i]).collect(),
                counts: sel.iter().map(|&i| t.counts[i]).collect(),
                reduction: t.reduction.clone(),
            }
        })
        .collect();
    let base = cap.window_base();
    let mut meta = cap.meta.clone();
    meta.set("window", J::from(vec![base + s0, base + s1]));
    if let Some(t) = cap.meta.get("trigger_tick").i() {
        meta.set("trigger_tick", t - s0);
    }
    Cap {
        rate: cap.rate,
        tick_hz: cap.tick_hz,
        n: s1 - s0,
        channels,
        events,
        analog: cap.analog.clone(),
        meta,
        parent: Some((cap.clone(), s0)),
    }
}

fn to_j(v: &serde_json::Value) -> J {
    serde_json::from_value(v.clone()).unwrap_or(J::Null)
}

/// A recorded capture's parts (runs of parts that follow on joined), each labelled as `verify.load_capture` does.
pub fn load(run_dir: &Path, file: &str, host_ns: &J) -> Result<Vec<Rc<Cap>>, String> {
    let c = wireskein_io::read(&run_dir.join(file))?;
    let n_parts = c.parts.len();
    let mut out = vec![];
    for p in c.parts {
        let mut meta = J::dict();
        let mut keys: Vec<(&String, &serde_json::Value)> = p.meta.iter().collect();
        keys.sort_by_key(|(k, _)| *k);
        for (k, v) in keys {
            meta.set(k, to_j(v));
        }
        let label = if n_parts == 1 {
            file.to_string()
        } else if let Some((a, b)) = p.parts {
            format!("{file}#part{a}-{b}")
        } else {
            format!("{file}#part{}", p.part)
        };
        meta.set("capture_id", c.id.clone());
        meta.set("complete", c.complete);
        meta.set("lost", J::List(c.lost.iter().map(to_j).collect()));
        meta.set("layout", c.layout.clone());
        meta.set("part", p.part);
        meta.set("n_parts", c.n_parts);
        if let Some((a, b)) = p.parts {
            meta.set("parts", J::from(vec![a, b]));
        }
        let skipped: Vec<J> = p
            .skipped
            .iter()
            .filter(|s| s.get("analog").is_none() && s.get("hidden").is_none())
            .map(|s| crate::jd! {"name" => to_j(&s["name"]), "encoding" => to_j(&s["encoding"])})
            .collect();
        meta.set("skipped_channels", J::List(skipped));
        meta.set("file", file);
        meta.set("host_ns", host_ns.clone());
        meta.set("label", label);
        let analog = p
            .skipped
            .iter()
            .filter(|s| s.get("analog").is_some())
            .filter_map(|s| s["name"].as_str().map(String::from))
            .collect();
        out.push(Rc::new(Cap {
            rate: c.tick_hz.0 as f64 / c.tick_hz.1 as f64,
            tick_hz: c.tick_hz,
            n: p.ticks,
            channels: p
                .lines
                .into_iter()
                .map(|l| Ch {
                    name: l.name,
                    initial: l.initial,
                    edges: l.edges,
                    step: l.step.max(1),
                    phase: l.phase,
                    reduction: l.reduction,
                })
                .collect(),
            events: p.events,
            analog,
            meta,
            parent: None,
        }));
    }
    Ok(out)
}

// ---- what a line kept as changes says for sure (`_engine/events.py`) ----

pub const SATURATED: u8 = 127;

pub fn latched(t: &Events) -> bool {
    t.reduction.as_deref() == Some("change_latch")
}

/// (rising edges, exact).
pub fn ev_rises(t: &Events) -> (i64, bool) {
    let mut n = 0i64;
    let mut b = t.start_level;
    for (k, &c) in t.counts.iter().enumerate() {
        let c = c as i64;
        n += if b == 0 { (c + 1) / 2 } else { c / 2 };
        b = t.levels[k];
    }
    let exact = !t.counts.iter().any(|&c| c >= SATURATED) && !(latched(t) && !t.counts.is_empty());
    (n, exact)
}

pub fn ev_first_level(t: &Events) -> Option<u8> {
    if t.quanta.first() == Some(&0) {
        None
    } else {
        Some(t.start_level)
    }
}

pub fn ev_end_level(t: &Events) -> u8 {
    t.levels.last().copied().unwrap_or(t.start_level)
}

pub fn ev_moved(t: &Events) -> bool {
    !t.quanta.is_empty()
}

pub fn ev_describe(t: &Events) -> String {
    if latched(t) {
        return format!(
            "{} is kept as change_latch at {} ticks an interval (whether it changed in each, not how often or where; \
             {} intervals hold two or more changes)",
            t.name,
            t.step,
            t.hidden()
        );
    }
    format!(
        "{} is kept as changes at {} ticks a quantum ({} quanta hold more than one change, where in them is not kept)",
        t.name,
        t.step,
        t.hidden()
    )
}
