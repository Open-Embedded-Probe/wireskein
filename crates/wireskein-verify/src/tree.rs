//! The run's sections from the heading markers of its log (`verify.segment_tree`, `markers.build` with the heading
//! dialect of docs/markers.ja.md), on the host clock in microseconds; a log line with probe_ns placed in a capture
//! part (`runlog.place`).

use crate::j::J;
use crate::jd;

#[derive(Debug, Clone)]
pub struct Seg {
    pub level: i64,
    pub name: String,
    pub begin: i64,
    pub end: Option<i64>,
    pub root: bool,
    pub issues: Vec<J>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// the log lines that open and close it (indices into the sorted log)
    pub opened: Option<usize>,
    pub closed: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Tree {
    pub segs: Vec<Seg>,
    /// the log sorted by host_ns
    pub log: Vec<J>,
}

impl Tree {
    /// The segments in the order `walk()` gives them (the root first).
    pub fn walk(&self) -> Vec<usize> {
        let mut out = vec![];
        fn go(t: &Tree, i: usize, out: &mut Vec<usize>) {
            out.push(i);
            for &c in &t.segs[i].children {
                go(t, c, out);
            }
        }
        go(self, 0, &mut out);
        out
    }

    /// Its path: each name, a repeat under the same parent given [k].
    pub fn path(&self, mut i: usize) -> String {
        let mut parts = vec![];
        while !self.segs[i].root {
            let s = &self.segs[i];
            let k = match s.parent {
                Some(p) => self.segs[p]
                    .children
                    .iter()
                    .filter(|&&c| self.segs[c].name == s.name)
                    .position(|&c| c == i)
                    .unwrap_or(0),
                None => 0,
            };
            parts.push(if k > 0 {
                format!("{}[{k}]", s.name)
            } else {
                s.name.clone()
            });
            match s.parent {
                Some(p) => i = p,
                None => break,
            }
        }
        parts.reverse();
        parts.join("/")
    }

    pub fn opened(&self, i: usize) -> Option<&J> {
        self.segs[i].opened.map(|k| &self.log[k])
    }

    pub fn closed(&self, i: usize) -> Option<&J> {
        self.segs[i].closed.map(|k| &self.log[k])
    }

    /// Opened and closed on the probe's clock.
    pub fn probe_section(&self, i: usize) -> bool {
        [self.opened(i), self.closed(i)]
            .iter()
            .all(|e| e.is_some_and(|e| !e.get("probe_ns").is_null()))
    }
}

/// `^(#+)[ \t]*(.*?)[ \t]*$`: (the depth, the name) of a heading line.
fn heading(text: &str) -> Option<(i64, String)> {
    let depth = text.chars().take_while(|&c| c == '#').count();
    if depth == 0 || text.contains('\n') {
        return None;
    }
    let name = text[depth..].trim_matches(|c| c == ' ' || c == '\t');
    Some((depth as i64, name.to_string()))
}

fn host_ns(e: &J) -> i64 {
    e.get("host_ns").i().unwrap_or(0)
}

/// The segment tree of a run document.
pub fn segment_tree(doc: &J) -> Tree {
    let mut log: Vec<J> = doc.get("log").list().to_vec();
    log.sort_by_key(host_ns);
    let caps_ns = doc.get("captures").list().iter().map(host_ns);
    let end = log
        .iter()
        .map(host_ns)
        .chain(caps_ns)
        .chain([0])
        .max()
        .unwrap()
        .div_euclid(1000)
        + 1;
    let n = log.len();
    let mut segs = vec![Seg {
        level: 0,
        name: String::new(),
        begin: 0,
        end: Some(n as i64),
        root: true,
        issues: vec![],
        parent: None,
        children: vec![],
        opened: None,
        closed: None,
    }];
    let mut stack = vec![0usize];
    let close_from = |segs: &mut Vec<Seg>, stack: &mut Vec<usize>, depth: i64, t: i64| {
        while stack.len() > 1 && segs[*stack.last().unwrap()].level >= depth {
            let s = stack.pop().unwrap();
            segs[s].end = Some(t);
        }
    };
    for (i, e) in log.iter().enumerate() {
        if e.get("src").s() != Some("marker") {
            continue;
        }
        let text = e.get("text").s().unwrap_or("");
        let Some((depth, name)) = heading(text) else {
            continue;
        };
        let t = i as i64;
        if !name.is_empty() {
            close_from(&mut segs, &mut stack, depth, t);
            let parent = *stack.last().unwrap();
            let pd = if segs[parent].root { 0 } else { segs[parent].level };
            let mut s = Seg {
                level: depth,
                name,
                begin: t,
                end: None,
                root: false,
                issues: vec![],
                parent: Some(parent),
                children: vec![],
                opened: None,
                closed: None,
            };
            if depth > pd + 1 {
                s.issues.push(jd! {"kind" => "level_skip", "from" => pd, "to" => depth});
            }
            segs.push(s);
            let k = segs.len() - 1;
            segs[parent].children.push(k);
            stack.push(k);
        } else if stack.iter().any(|&s| !segs[s].root && segs[s].level == depth) {
            close_from(&mut segs, &mut stack, depth, t);
        } else {
            let top = *stack.last().unwrap();
            segs[top]
                .issues
                .push(jd! {"kind" => "end_without_begin", "at" => t, "text" => text});
        }
    }
    while stack.len() > 1 {
        let s = stack.pop().unwrap();
        segs[s].end = Some(n as i64);
    }
    // back to the lines' times
    let us = |i: i64| -> i64 {
        if (i as usize) < n {
            host_ns(&log[i as usize]).div_euclid(1000)
        } else {
            end
        }
    };
    for s in segs.iter_mut() {
        if s.root {
            s.begin = 0;
            s.end = Some(end);
        } else {
            s.opened = Some(s.begin as usize);
            s.closed = s.end.filter(|&e| (e as usize) < n).map(|e| e as usize);
            s.begin = us(s.begin);
            s.end = s.end.map(us);
        }
        for issue in s.issues.iter_mut() {
            if let Some(at) = issue.get("at").i() {
                issue.set("at", us(at));
            }
        }
    }
    Tree { segs, log }
}

/// The boot of the probe clock a part's start_ns is on (`runlog.boot_of`).
pub fn boot_of(meta: &J) -> J {
    if !meta.get("boot_id").is_null() {
        return meta.get("boot_id").clone();
    }
    let probe = meta.get("probe");
    if !probe.get("boot_id").is_null() {
        return probe.get("boot_id").clone();
    }
    let clock = probe.get("clock");
    let mut ids: Vec<J> = vec![];
    for k in ["before", "after"] {
        let b = clock.get(k).get("boot_id");
        if matches!(clock.get(k), J::Dict(_)) && !b.is_null() && !ids.contains(b) {
            ids.push(b.clone());
        }
    }
    if ids.len() == 1 { ids.pop().unwrap() } else { J::Null }
}

fn py_repr_opt(v: &J) -> String {
    v.repr()
}

/// (tick, uncertainty in ticks, why not) of a log line with probe_ns in a part (`runlog.place`).
pub fn place(entry: &J, meta: &J, tick_hz: (i64, i64)) -> (Option<f64>, f64, String) {
    if entry.get("probe_ns").is_null() {
        return (None, 0.0, "the line has no probe_ns".into());
    }
    if meta.get("start_ns").is_null() {
        return (None, 0.0, "the capture has no start_ns".into());
    }
    let boot = boot_of(meta);
    if boot.is_null() {
        return (
            None,
            0.0,
            "the capture does not say its probe's boot_id (pass boot_id= to Recorder.capture)".into(),
        );
    }
    if *entry.get("boot_id") != boot {
        return (
            None,
            0.0,
            format!(
                "the line is of boot {}, the capture of {}",
                py_repr_opt(entry.get("boot_id")),
                boot.repr()
            ),
        );
    }
    let here = meta.get("probe").get("address");
    let src = meta.get("source");
    let ep = entry.get("probe");
    if ep.truthy() && here.truthy() {
        if ep != here {
            return (
                None,
                0.0,
                format!("the line is of probe {}, the capture of {}", ep.repr(), here.repr()),
            );
        }
    } else if ep.truthy() && src.truthy() {
        let s = src.s().unwrap_or("").split('?').next().unwrap_or("").to_string();
        let p = ep.s().unwrap_or("");
        if !(s == p || s.ends_with(&format!(":{p}"))) {
            return (
                None,
                0.0,
                format!(
                    "the line is of probe {}, the capture of {}",
                    ep.repr(),
                    J::from(s).repr()
                ),
            );
        }
    }
    let (hn, hd) = (tick_hz.0 as i128, tick_hz.1 as i128);
    let d = entry.get("probe_ns").i().unwrap_or(0) as i128 - meta.get("start_ns").i().unwrap_or(0) as i128;
    let tick = (d * hn) as f64 / (hd * 1_000_000_000) as f64;
    let u = meta.get("start_uncertainty_ns").f().unwrap_or(0.0) + entry.get("uncertainty_ns").f().unwrap_or(0.0);
    let unc = u * hn as f64 / (hd as f64 * 1e9);
    (Some(tick), unc, String::new())
}
