//! A long part read in pieces (Python's `split.py` and `decode._analyze_split`; docs/long-captures.md): a part of more
//! changes than `max_edges` is cut where every line is quiet (or, with a free-running line, where the others are:
//! `data_quiet`; with no quiet at all, at the window's end: `forced`), each cut read past by `overlap_edges` changes
//! both ways. The roles come from the first piece that holds a bus (confirmed or likely); every piece is then read
//! with those roles, each item kept by the piece whose own ticks it starts in, the Debug Module read over the pieces'
//! accesses as one; pieces the roles read badly are told (`doubtful`), what they hold analysed on their own.

use crate::report;
use serde_json::{Map, Value, json};
use wireskein_core::{Channel, View};
use wireskein_io::Line;

/// The split's options (names and defaults as Python's `split.DEFAULTS`).
#[derive(Debug, Clone)]
pub struct Options {
    pub max_edges: usize,
    pub search_from: f64,
    pub gap_min: f64,
    pub similar: f64,
    pub min_edges: usize,
    pub overlap_edges: usize,
    pub noise_max: i64,
    pub noise_share: f64,
    pub roles_from: usize,
    pub reanalyze: bool,
    pub watch: Vec<String>,
    pub ignore: Vec<String>,
    pub cuts: Vec<i64>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_edges: 1_000_000,
            search_from: 0.5,
            gap_min: 4.0,
            similar: 0.5,
            min_edges: 50_000,
            overlap_edges: 10_000,
            noise_max: 1,
            noise_share: 0.3,
            roles_from: 3,
            reanalyze: true,
            watch: vec![],
            ignore: vec![],
            cuts: vec![],
        }
    }
}

impl Options {
    /// From "key=value,..." ("off": None).
    pub fn parse(spec: &str) -> Result<Option<Options>, String> {
        if spec.trim() == "off" {
            return Ok(None);
        }
        let mut o = Options::default();
        for kv in spec.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()) {
            let (k, v) = kv.split_once('=').ok_or_else(|| format!("split: {kv:?}: key=value"))?;
            let (k, v) = (k.trim(), v.trim());
            let f = || v.parse::<f64>().map_err(|_| format!("split: {k}={v}: not a number"));
            match k {
                "max_edges" => o.max_edges = f()? as usize,
                "search_from" => o.search_from = f()?,
                "gap_min" => o.gap_min = f()?,
                "similar" => o.similar = f()?,
                "min_edges" => o.min_edges = f()? as usize,
                "overlap_edges" => o.overlap_edges = f()? as usize,
                "noise_max" => o.noise_max = f()? as i64,
                "noise_share" => o.noise_share = f()?,
                "roles_from" => o.roles_from = f()? as usize,
                "reanalyze" => o.reanalyze = f()? != 0.0,
                "workers" => {}
                "watch" => o.watch = v.split(';').map(String::from).collect(),
                "ignore" => o.ignore = v.split(';').map(String::from).collect(),
                "cuts" => {
                    o.cuts = v
                        .split(';')
                        .map(|x| x.parse::<i64>().map_err(|_| format!("split: cuts {x:?}")))
                        .collect::<Result<_, _>>()?
                }
                _ => return Err(format!("split: unknown option {k:?}")),
            }
        }
        Ok(Some(o))
    }

    /// As the document gives them.
    pub fn doc(&self) -> Value {
        let mut m = json!({"max_edges": self.max_edges, "search_from": self.search_from, "gap_min": self.gap_min,
                           "similar": self.similar, "min_edges": self.min_edges, "overlap_edges": self.overlap_edges,
                           "noise_max": self.noise_max, "noise_share": self.noise_share, "workers": 1,
                           "roles_from": self.roles_from, "reanalyze": self.reanalyze as i64});
        for (k, v) in [("watch", &self.watch), ("ignore", &self.ignore)] {
            if !v.is_empty() {
                m[k] = json!(v);
            }
        }
        if !self.cuts.is_empty() {
            m["cuts"] = json!(self.cuts);
        }
        m
    }
}

/// A piece: its own ticks [s, e), the ticks read [a, b), its changes, how it ends.
#[derive(Debug, Clone)]
pub struct Piece {
    pub s: i64,
    pub e: i64,
    pub a: i64,
    pub b: i64,
    pub edges: usize,
    pub cut: &'static str,
    pub ratio: Option<f64>,
}

impl Piece {
    pub fn doc(&self) -> Value {
        let mut d = json!({"s": self.s, "e": self.e, "edges": self.edges, "cut": self.cut});
        if let Some(r) = self.ratio {
            d["ratio"] = json!(wireskein_io::pyfmt::round_n(r, 1));
        }
        if (self.a, self.b) != (self.s, self.e) {
            d["read"] = json!([self.a, self.b]);
        }
        d
    }
}

fn count(es: &[&[i64]], s: i64, t: i64) -> usize {
    es.iter()
        .map(|e| {
            e.partition_point(|&x| x < t)
                .saturating_sub(e.partition_point(|&x| x < s))
        })
        .sum()
}

/// The first tick t with k changes in [s, t] (n when there are not so many).
fn tick_after(es: &[&[i64]], s: i64, k: usize, n: i64) -> i64 {
    if count(es, s, n) < k {
        return n;
    }
    let (mut lo, mut hi) = (s, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if count(es, s, mid + 1) >= k {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo + 1
}

/// The last tick t >= start with k changes in [t, e) (start when there are not so many).
fn tick_before(es: &[&[i64]], start: i64, e: i64, k: usize) -> i64 {
    if count(es, start, e) < k {
        return start;
    }
    let (mut lo, mut hi) = (start, e);
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        if count(es, mid, e) >= k {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// The quiet stretches of [s, t): (starts, ends, the fastest usual interval, a periodic line left out).
#[allow(clippy::type_complexity)]
fn quiet(
    es: &[(&str, &[i64])],
    s: i64,
    t: i64,
    o: &Options,
    noisy: &mut Map<String, Value>,
) -> (Vec<i64>, Vec<i64>, Option<f64>, bool) {
    let (mut sa, mut sb, mut usual_all, mut periodic) = (vec![], vec![], vec![], false);
    for &(name, e) in es {
        let mut w: Vec<i64> = e[e.partition_point(|&x| x < s)..e.partition_point(|&x| x < t)].to_vec();
        if w.len() >= 3 {
            let short = w.windows(2).filter(|p| p[1] - p[0] <= o.noise_max).count();
            let share = short as f64 / (w.len() - 1) as f64;
            if share > o.noise_share {
                let r = (share * 1e3).round_ties_even() / 1e3;
                let prev = noisy.get(name).and_then(|v| v.as_f64()).unwrap_or(0.0);
                noisy.insert(name.into(), json!(prev.max(r)));
            }
            if o.noise_max >= 1 {
                w = wireskein_core::kernels::deglitch(&w, o.noise_max + 1);
            }
        }
        if w.len() < 3 {
            sa.extend(&w);
            sb.extend(&w);
            continue;
        }
        let d: Vec<f64> = w.windows(2).map(|p| (p[1] - p[0]) as f64).collect();
        let usual = report::median(&mut d.clone());
        let thr = o.gap_min * usual;
        if d.iter().copied().fold(f64::NEG_INFINITY, f64::max) <= thr {
            periodic = true;
            continue;
        }
        usual_all.push(usual);
        let busy: Vec<bool> = d.iter().map(|&x| x < thr).collect();
        for (i, &b) in busy.iter().enumerate() {
            if b {
                sa.push(w[i]);
                sb.push(w[i + 1]);
            }
        }
        for i in 0..w.len() {
            let left = i > 0 && busy[i - 1];
            let right = i + 1 < w.len() && busy[i];
            if !left && !right {
                sa.push(w[i]);
                sb.push(w[i]);
            }
        }
    }
    let usual = usual_all
        .iter()
        .copied()
        .fold(None, |a: Option<f64>, x| Some(a.map_or(x, |a| a.min(x))));
    if sa.is_empty() {
        let any = es
            .iter()
            .any(|(_, e)| e.partition_point(|&x| x < t) > e.partition_point(|&x| x < s));
        // every line periodic: no span; no change at all: the whole window quiet
        return if usual.is_none() && !periodic && !any {
            (vec![s], vec![t], None, periodic)
        } else {
            (vec![], vec![], usual, periodic)
        };
    }
    let mut idx: Vec<usize> = (0..sa.len()).collect();
    idx.sort_by_key(|&i| sa[i]);
    let a: Vec<i64> = idx.iter().map(|&i| sa[i]).collect();
    let mut b: Vec<i64> = idx.iter().map(|&i| sb[i]).collect();
    for i in 1..b.len() {
        b[i] = b[i].max(b[i - 1]);
    }
    let (mut qs, mut qe) = (vec![], vec![]);
    for i in 0..a.len() - 1 {
        if a[i + 1] > b[i] {
            qs.push(b[i]);
            qe.push(a[i + 1]);
        }
    }
    (qs, qe, usual, periodic)
}

/// The pieces of a source, each with the lines of the ticks it reads, as they are cut (Python's `split.run`): changes
/// are held from the current piece on only.
pub struct Splitter<'a> {
    src: &'a mut dyn wireskein_io::stream::Source,
    o: Options,
    names: Vec<String>,
    watched: Vec<usize>,
    buf: Vec<Vec<i64>>,
    lv0: Vec<u8>,
    n: i64,
    start: i64,
    upto: i64,
    eof: bool,
    s: i64,
    prev_cut: bool,
    done: bool,
    pending: std::collections::VecDeque<(Piece, Vec<Line>)>,
    pub noisy: Map<String, Value>,
}

impl<'a> Splitter<'a> {
    pub fn new(src: &'a mut dyn wireskein_io::stream::Source, o: &Options) -> Result<Self, String> {
        Self::watching(src, o, &[])
    }

    /// As `new`, the quiet looked for on `watch` (those of them the source has) unless the options name lines to
    /// watch (Python's `split.run(..., watch=)`: a decode spec's lines).
    pub fn watching(
        src: &'a mut dyn wireskein_io::stream::Source,
        o: &Options,
        watch: &[String],
    ) -> Result<Self, String> {
        let names: Vec<String> = src.names().to_vec();
        let unknown: Vec<&String> = o.watch.iter().chain(&o.ignore).filter(|x| !names.contains(x)).collect();
        if !unknown.is_empty() {
            return Err(format!(
                "split: no such channel: {}",
                unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        let given: Vec<&String> = watch.iter().filter(|x| names.contains(x)).collect();
        let want = |x: &String| {
            if !o.watch.is_empty() {
                o.watch.contains(x)
            } else {
                given.is_empty() || given.contains(&x)
            }
        };
        let watched = (0..names.len())
            .filter(|&i| want(&names[i]) && !o.ignore.contains(&names[i]))
            .collect();
        let lv0 = src.initial().to_vec();
        let n = src.n();
        Ok(Splitter {
            buf: vec![vec![]; names.len()],
            src,
            o: o.clone(),
            names,
            watched,
            lv0,
            n,
            start: 0,
            upto: 0,
            eof: false,
            s: 0,
            prev_cut: false,
            done: false,
            pending: Default::default(),
            noisy: Map::new(),
        })
    }

    fn es(&self) -> Vec<&[i64]> {
        self.buf.iter().map(|e| e.as_slice()).collect()
    }

    fn more(&mut self) {
        match self.src.next_block() {
            Some((t1, got)) => {
                self.upto = t1;
                for (b, g) in self.buf.iter_mut().zip(got) {
                    b.extend(g);
                }
            }
            None => {
                self.eof = true;
                self.upto = self.n;
            }
        }
    }

    /// The lines of ticks [a, b) (tick 0 at a): each line's changes inside, its level at a.
    fn window(&self, a: i64, b: i64) -> Vec<Line> {
        self.names
            .iter()
            .zip(&self.buf)
            .zip(&self.lv0)
            .map(|((name, e), &lv)| {
                let i = e.partition_point(|&x| x <= a);
                let j = e.partition_point(|&x| x < b);
                Line {
                    name: name.clone(),
                    initial: lv ^ ((i & 1) as u8),
                    edges: e[i..j.max(i)].iter().map(|&x| x - a).collect(),
                    step: 1,
                    phase: 0,
                    reduction: None,
                    acquisition: Default::default(),
                }
            })
            .collect()
    }

    /// Keep the changes from tick t on.
    fn drop_before(&mut self, t: i64) {
        for (e, lv) in self.buf.iter_mut().zip(self.lv0.iter_mut()) {
            let i = e.partition_point(|&x| x <= t);
            *lv ^= (i & 1) as u8;
            e.drain(..i);
        }
        self.start = t;
    }

    fn step(&mut self) {
        let (mx, mn, ov) = (self.o.max_edges, self.o.min_edges, self.o.overlap_edges);
        let s = self.s;
        while !self.eof && count(&self.es(), s, self.upto) < mx + ov {
            self.more();
        }
        let n = self.n;
        if self.eof && count(&self.es(), s, n) <= mx {
            let es = self.es();
            let a = if self.prev_cut {
                tick_before(&es, self.start, s, ov)
            } else {
                s
            };
            let p = Piece {
                s,
                e: n,
                a,
                b: n,
                edges: count(&es, s, n),
                cut: "end",
                ratio: None,
            };
            let w = self.window(p.a, p.b);
            self.pending.push_back((p, w));
            self.done = true;
            return;
        }
        let upto = self.upto;
        let (cuts, start) = {
            let es: Vec<&[i64]> = self.buf.iter().map(|e| e.as_slice()).collect();
            let wes: Vec<(&str, &[i64])> = self
                .watched
                .iter()
                .map(|&i| (self.names[i].as_str(), self.buf[i].as_slice()))
                .collect();
            let t = tick_after(&es, s, mx, upto);
            let t0 = tick_after(&es, s, ((mx as f64 * self.o.search_from) as usize).max(1), upto);
            let (qs, qe, usual, periodic) = quiet(&wes, s, t, &self.o, &mut self.noisy);
            let (mut ks, mut ke) = (vec![], vec![]);
            for (&a, &b) in qs.iter().zip(&qe) {
                if b > t0 && a >= s {
                    ks.push(a.max(t0));
                    ke.push(b);
                }
            }
            let kind = if periodic { "data_quiet" } else { "quiet" };
            let mut cuts: Vec<(i64, Option<f64>, &'static str)> = vec![];
            match usual.filter(|&u| u != 0.0) {
                Some(u) if !ks.is_empty() => {
                    let ratio: Vec<f64> = ks.iter().zip(&ke).map(|(a, b)| (b - a) as f64 / u).collect();
                    let best = ratio.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    let mut last = s;
                    for i in (0..ratio.len()).filter(|&i| ratio[i] >= self.o.similar * best) {
                        let c = (ks[i] + ke[i]).div_euclid(2);
                        if count(&es, last, c) >= mn {
                            cuts.push((c, Some(ratio[i]), kind));
                            last = c;
                        }
                    }
                    if cuts.is_empty() {
                        let i = (0..ratio.len()).fold(0, |b, i| if ratio[i] > ratio[b] { i } else { b });
                        cuts.push(((ks[i] + ke[i]).div_euclid(2), Some(ratio[i]), kind));
                    }
                }
                _ => cuts.push((t, None, "forced")),
            }
            (cuts, self.start)
        };
        let mut s = s;
        for (c, r, k) in cuts {
            let es = self.es();
            let a = if self.prev_cut {
                tick_before(&es, start, s, ov)
            } else {
                s
            };
            let p = Piece {
                s,
                e: c,
                a,
                b: tick_after(&es, c, ov, upto).min(upto),
                edges: count(&es, s, c),
                cut: k,
                ratio: r,
            };
            let w = self.window(p.a, p.b);
            self.pending.push_back((p, w));
            s = c;
            self.prev_cut = true;
        }
        self.s = s;
        let t = tick_before(&self.es(), self.start, s, ov);
        self.drop_before(t);
    }
}

impl Iterator for Splitter<'_> {
    type Item = (Piece, Vec<Line>);
    fn next(&mut self) -> Option<Self::Item> {
        while self.pending.is_empty() && !self.done {
            self.step();
        }
        self.pending.pop_front()
    }
}

pub(crate) fn view_of<'a>(lines: &'a [Line], rate: f64, n: i64) -> Result<View<'a>, String> {
    View::new(
        rate,
        n,
        lines
            .iter()
            .map(|l| Channel::new(&l.name, l.initial, &l.edges))
            .collect(),
    )
}

/// Items shifted to the part's ticks, each kept by the piece whose own ticks it starts in.
pub(crate) fn join_items(per: &[(&Piece, &Value)]) -> Vec<Value> {
    let mut out = vec![];
    for (p, v) in per {
        for x in v.as_array().into_iter().flatten() {
            let Some(s) = x.get("s").and_then(|v| v.as_i64()) else {
                out.push(x.clone());
                continue;
            };
            let mut y = x.clone();
            let o = y.as_object_mut().unwrap();
            for k in ["s", "e"] {
                if let Some(t) = o.get(k).and_then(|v| v.as_i64()) {
                    o.insert(k.into(), json!(t + p.a));
                }
            }
            for k in ["repaired", "restored"] {
                if let Some(Value::Array(a)) = o.get(k) {
                    let v: Vec<Value> = a
                        .iter()
                        .map(|t| t.as_i64().map_or(t.clone(), |t| json!(t + p.a)))
                        .collect();
                    o.insert(k.into(), Value::Array(v));
                }
            }
            if p.s <= s + p.a && s + p.a < p.e {
                out.push(y);
            }
        }
    }
    out
}

/// A layer of parallel lists keyed by time ("t" or "s"), joined the same way; other keys the first piece's.
pub(crate) fn join_columns(per: &[(&Piece, &Value)]) -> Value {
    let first = per[0].1.as_object().unwrap();
    let tkey = if first.contains_key("t") { "t" } else { "s" };
    let mut out: Map<String, Value> = first
        .iter()
        .filter(|(_, v)| !v.is_array())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let cols: Vec<&String> = first.iter().filter(|(_, v)| v.is_array()).map(|(k, _)| k).collect();
    let mut acc: Map<String, Value> = cols.iter().map(|k| ((*k).clone(), json!([]))).collect();
    for (p, lay) in per {
        let t: Vec<i64> = lay[tkey]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_i64())
            .map(|x| x + p.a)
            .collect();
        let keep: Vec<usize> = (0..t.len()).filter(|&i| p.s <= t[i] && t[i] < p.e).collect();
        for k in &cols {
            let v = lay
                .get(k.as_str())
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let a = acc[k.as_str()].as_array_mut().unwrap();
            if k.as_str() == tkey {
                a.extend(keep.iter().map(|&i| json!(t[i])));
            } else if v.len() == t.len() {
                a.extend(keep.iter().map(|&i| v[i].clone()));
            } else if a.is_empty() {
                a.extend(v);
            }
        }
    }
    out.extend(acc);
    Value::Object(out)
}

/// The size bins of the unread stretches ("1", "2", "3-4", "5-8", ...).
fn size_bin(edges: i64) -> String {
    if edges <= 2 {
        return edges.max(1).to_string();
    }
    let k = 64 - ((edges - 1) as u64).leading_zeros();
    format!("{}-{}", (1u64 << (k - 1)) + 1, 1u64 << k)
}

/// What was not read, summed up: how many stretches, their changes, their sizes in bins, the largest.
pub(crate) fn unread_summary(items: &[Value]) -> Value {
    let (mut count, mut edges) = (0, 0i64);
    let mut bins: std::collections::BTreeMap<i64, (String, i64)> = Default::default();
    let mut largest: Option<Value> = None;
    for u in items {
        let k = u["edges"].as_i64().unwrap_or(0);
        count += 1;
        edges += k;
        let b = size_bin(k);
        let lo: i64 = b.split('-').next().unwrap().parse().unwrap();
        bins.entry(lo).or_insert((b, 0)).1 += 1;
        if largest.as_ref().is_none_or(|l| k > l["edges"].as_i64().unwrap_or(0)) {
            largest = Some(json!({"s": u.get("s"), "e": u.get("e"), "edges": k}));
        }
    }
    let sizes: Map<String, Value> = bins.into_values().map(|(b, c)| (b, json!(c))).collect();
    let mut d = json!({"count": count, "edges": edges, "sizes": sizes});
    if let Some(l) = largest {
        d["largest"] = l;
    }
    d
}

/// What a part read in pieces gives: its whole lines when it is one piece (read as usual), else its claims and split.
pub enum Split {
    Whole(Vec<Line>),
    Pieces(Vec<Value>, Value),
}

/// A part read from a source in pieces (Python's `decode._analyze_split`): the claims (confirmed or likely) of the
/// first piece that holds any taken as the buses' roles, every piece decoded with them as decode reads a spec's
/// buses (`decode::piece`), the pieces joined (`decode::join_part`), each claim given its bus's joined result.
/// items: the items written to this file as they are read (with its name in the document); head: the part's (its
/// number); gf: the probe's glitch filter.
#[allow(clippy::too_many_arguments)]
pub fn analyze(
    src: &mut dyn wireskein_io::stream::Source,
    rate: f64,
    hints: &Value,
    o: &Options,
    items: Option<(&std::path::Path, &str)>,
    head: &Map<String, Value>,
    gf: Option<&Value>,
) -> Result<Split, String> {
    use crate::decode;
    let n = src.n();
    let mut sp = Splitter::new(src, o)?;
    let Some(first) = sp.next() else {
        return Ok(Split::Whole(vec![]));
    };
    if first.0.cut == "end" && first.0.a == 0 {
        return Ok(Split::Whole(first.1));
    }
    // the roles from the first piece that holds any (up to roles_from pieces: the start may be before the bus)
    let mut tried = vec![first];
    let (mut keep, mut first_claims);
    loop {
        let (p, w) = tried.last().unwrap();
        let view = view_of(w, rate, p.b - p.a)?;
        let (claims, _) = wireskein_core::analyze_full(&view, hints);
        let docs: Vec<Value> = claims
            .iter()
            .map(|c| crate::claim_doc(c, "final", w, rate, p.b - p.a, None, false))
            .collect();
        keep = docs
            .iter()
            .filter(|c| (c["verdict"] == "confirmed" || c["verdict"] == "likely") && c["protocol"] != "sync_unknown")
            .cloned()
            .collect::<Vec<_>>();
        first_claims = docs;
        if !keep.is_empty() || tried.len() >= o.roles_from.max(1) {
            break;
        }
        match sp.next() {
            Some(x) => tried.push(x),
            None => break,
        }
    }
    let k0 = tried.len() - 1;
    let roles_piece = (tried[k0].0.s, tried[k0].0.e);
    let buses: Vec<Value> = keep
        .iter()
        .map(|c| {
            let params: Map<String, Value> = ["baud", "mode"]
                .iter()
                .filter_map(|k| c["params"].get(*k).map(|v| (k.to_string(), v.clone())))
                .collect();
            json!({"protocol": c["protocol"], "pins": c["roles"], "params": params})
        })
        .collect();
    let spec = decode::Spec::of(&json!({"buses": buses, "depth": "protocol"}))?;
    // every piece on the edge the roles' piece read each bus on
    let edges: std::collections::BTreeMap<usize, String> = keep
        .iter()
        .enumerate()
        .filter_map(|(j, c)| {
            c["params"]["sample_edge"]
                .as_str()
                .filter(|e| *e == "rise" || *e == "fall")
                .map(|e| (j, e.to_string()))
        })
        .collect();
    let part_no = head.get("part").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let mut sink = match items {
        Some((f, _)) => Some(decode::Items::new(f, &spec, part_no)?),
        None => None,
    };
    let mut outs = vec![];
    for (p, w) in tried.into_iter().chain(&mut sp) {
        let mut d = decode::piece(&p, &w, rate, &spec, head, o.reanalyze, &edges, n, gf)?;
        if let Some(s) = sink.as_mut() {
            s.take(&p, &mut d)?;
        }
        outs.push((p, d));
    }
    if let Some(s) = sink.as_mut() {
        s.close()?;
    }
    let noisy = std::mem::take(&mut sp.noisy);
    let mut joined = decode::join_part(outs, &spec, n, rate, Some(o), noisy, sink.is_some());
    if let (Some(s), Some((_, name))) = (&sink, items) {
        decode::sunk(&mut joined, s, name);
    }
    let buses = joined
        .get("buses")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let claims: Vec<Value> = keep
        .iter()
        .zip(&buses)
        .map(|(c, b)| {
            let mut d = c.as_object().unwrap().clone();
            d.insert("result".into(), b.get("result").cloned().unwrap_or(json!({})));
            d.insert(
                "fit_pieces".into(),
                b["fit"].get("pieces").cloned().unwrap_or(Value::Null),
            );
            if let Some(u) = b.get("uninterpreted").filter(|u| crate::nonempty(u)) {
                d.insert("uninterpreted".into(), u.clone());
            }
            d.insert(
                "uninterpreted_summary".into(),
                b.get("uninterpreted_summary").cloned().unwrap_or(Value::Null),
            );
            Value::Object(d)
        })
        .collect();
    let mut split = joined.get("split").cloned().unwrap_or(json!({}));
    split["roles_from"] = json!([roles_piece.0, roles_piece.1]);
    if keep.is_empty() {
        split["no_roles"] = json!(format!(
            "no bus claimed in the first {} pieces (roles_from): analyse a stretch with --from / --to, or decode with a spec",
            k0 + 1
        ));
    }
    split["not_carried"] = json!(
        first_claims
            .iter()
            .filter(|c| !keep.contains(c))
            .map(|c| json!({"protocol": c["protocol"], "roles": c["roles"], "verdict": c["verdict"]}))
            .collect::<Vec<_>>()
    );
    Ok(Split::Pieces(claims, split))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock in bursts of 100 changes 10 ticks apart, 1000 ticks of quiet after each.
    fn bursts(k: usize) -> Line {
        let (mut e, mut t) = (vec![], 100i64);
        for _ in 0..k {
            for _ in 0..100 {
                e.push(t);
                t += 10;
            }
            t += 1000;
        }
        Line {
            name: "CLK".into(),
            initial: 0,
            edges: e,
            step: 1,
            phase: 0,
            reduction: None,
            acquisition: Default::default(),
        }
    }

    #[test]
    fn cut_where_the_lines_are_quiet() {
        let l = vec![bursts(20)];
        let n = l[0].edges.last().unwrap() + 500;
        let o = Options {
            max_edges: 500,
            min_edges: 100,
            overlap_edges: 20,
            ..Default::default()
        };
        let mut src = wireskein_io::stream::MemSource::new(&l, n);
        let ps: Vec<(Piece, Vec<Line>)> = Splitter::new(&mut src, &o).unwrap().collect();
        assert!(ps.len() >= 4, "{}", ps.len());
        let cycle = 2000; // a burst (its first change to its last: 990) and the quiet to the next (1010)
        for (p, w) in &ps[..ps.len() - 1] {
            // each cut in the middle of a quiet stretch: 505 ticks after a burst's last change
            assert_eq!(p.cut, "quiet");
            assert_eq!((p.e - 100 - 990).rem_euclid(cycle), 505, "{p:?}");
            assert!(p.edges <= 500 && p.edges >= 100);
            // read past the cut by the overlap's changes
            assert!(p.b > p.e);
            assert_eq!(w[0].edges.len(), count(&[l[0].edges.as_slice()], p.a + 1, p.b));
        }
        let last = &ps[ps.len() - 1].0;
        assert_eq!((last.cut, last.e), ("end", n));
        // the pieces' own ticks are the part, without gaps
        assert_eq!(ps[0].0.s, 0);
        assert!(ps.windows(2).all(|w| w[0].0.e == w[1].0.s));
    }
}
