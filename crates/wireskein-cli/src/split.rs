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
    fn doc(&self) -> Value {
        let mut d = json!({"s": self.s, "e": self.e, "edges": self.edges, "cut": self.cut});
        if let Some(r) = self.ratio {
            d["ratio"] = json!((r * 10.0).round_ties_even() / 10.0);
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

/// The pieces a part is read in (Python's `split.run` over the part's changes).
pub fn pieces(lines: &[Line], n: i64, o: &Options, noisy: &mut Map<String, Value>) -> Result<Vec<Piece>, String> {
    let names: Vec<&str> = lines.iter().map(|l| l.name.as_str()).collect();
    let unknown: Vec<&String> = o
        .watch
        .iter()
        .chain(&o.ignore)
        .filter(|x| !names.contains(&x.as_str()))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "split: no such channel: {}",
            unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    let es: Vec<&[i64]> = lines.iter().map(|l| l.edges.as_slice()).collect();
    let wes: Vec<(&str, &[i64])> = lines
        .iter()
        .filter(|l| (o.watch.is_empty() || o.watch.contains(&l.name)) && !o.ignore.contains(&l.name))
        .map(|l| (l.name.as_str(), l.edges.as_slice()))
        .collect();
    let mut out = vec![];
    if !o.cuts.is_empty() {
        let mut cuts: Vec<i64> = o.cuts.iter().copied().filter(|&c| 0 < c && c < n).collect();
        cuts.sort_unstable();
        let mut bounds = vec![0];
        bounds.extend(cuts);
        bounds.push(n);
        for w in bounds.windows(2) {
            out.push(Piece {
                s: w[0],
                e: w[1],
                a: w[0],
                b: w[1],
                edges: count(&es, w[0], w[1]),
                cut: if w[1] < n { "given" } else { "end" },
                ratio: None,
            });
        }
        return Ok(out);
    }
    let (mx, mn, ov) = (o.max_edges, o.min_edges, o.overlap_edges);
    let (mut s, mut start, mut prev_cut) = (0i64, 0i64, false);
    loop {
        if count(&es, s, n) <= mx {
            let a = if prev_cut { tick_before(&es, start, s, ov) } else { s };
            out.push(Piece {
                s,
                e: n,
                a,
                b: n,
                edges: count(&es, s, n),
                cut: "end",
                ratio: None,
            });
            return Ok(out);
        }
        let t = tick_after(&es, s, mx, n);
        let t0 = tick_after(&es, s, ((mx as f64 * o.search_from) as usize).max(1), n);
        let (qs, qe, usual, periodic) = quiet(&wes, s, t, o, noisy);
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
                for i in (0..ratio.len()).filter(|&i| ratio[i] >= o.similar * best) {
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
        for (c, r, k) in cuts {
            let a = if prev_cut { tick_before(&es, start, s, ov) } else { s };
            out.push(Piece {
                s,
                e: c,
                a,
                b: tick_after(&es, c, ov, n).min(n),
                edges: count(&es, s, c),
                cut: k,
                ratio: r,
            });
            s = c;
            prev_cut = true;
        }
        start = tick_before(&es, start, s, ov);
    }
}

/// Ticks [a, b) of the lines as lines of their own (tick 0 at a): the changes inside, each line's level at a.
pub fn window(lines: &[Line], a: i64, b: i64) -> Vec<Line> {
    lines
        .iter()
        .map(|l| {
            let i = l.edges.partition_point(|&x| x <= a);
            let j = l.edges.partition_point(|&x| x < b);
            Line {
                name: l.name.clone(),
                initial: l.initial ^ ((i & 1) as u8),
                edges: l.edges[i..j.max(i)].iter().map(|&x| x - a).collect(),
                step: l.step,
            }
        })
        .collect()
}

fn view_of<'a>(lines: &'a [Line], rate: f64, n: i64) -> Result<View<'a>, String> {
    View::new(
        rate,
        n,
        lines
            .iter()
            .map(|l| Channel::new(&l.name, l.initial, &l.edges))
            .collect(),
    )
}

/// A bus the roles name: its protocol, its pins (role -> line), the params carried (baud, mode), the edge sampled.
#[derive(Debug, Clone)]
pub struct Bus {
    pub protocol: String,
    pub pins: std::collections::BTreeMap<String, String>,
    pub params: Map<String, Value>,
    pub edge: Option<String>,
}

/// A bus read in one piece (decode's `_bus`): its result (the final layer and the lower one), its fit, what it
/// did not read.
struct BusRead {
    decoded: bool,
    result: Map<String, Value>,
    fit: Option<f64>,
    uninterpreted: Vec<Value>,
}

/// The hints a piece is read with: the buses' pins, roles and params, only their protocols, the other pins left out.
fn hints_of(buses: &[&Bus], names: &[String], strict: bool) -> Value {
    let mut pins = Map::new();
    for b in buses {
        for (role, pin) in &b.pins {
            let mut h = json!({"protocol": b.protocol, "role": role});
            for (k, v) in &b.params {
                h[k] = v.clone();
            }
            pins.insert(pin.clone(), h);
        }
    }
    let mut out = json!({"pins": pins,
                         "buses": buses.iter().map(|b| json!({"protocol": b.protocol, "pins": b.pins})).collect::<Vec<_>>()});
    if strict && !buses.is_empty() {
        let protos: std::collections::BTreeSet<&str> = buses.iter().map(|b| b.protocol.as_str()).collect();
        out["protocols"] = json!(protos);
        let named: std::collections::BTreeSet<&String> = buses.iter().flat_map(|b| b.pins.values()).collect();
        out["exclude_pins"] = json!(names.iter().filter(|n| !named.contains(n)).collect::<Vec<_>>());
    }
    out
}

/// The buses in sets that name no pin under two protocols (each read on its own).
fn apart(buses: &[Bus]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = vec![];
    'b: for (i, b) in buses.iter().enumerate() {
        for g in groups.iter_mut() {
            let owner: std::collections::BTreeMap<&str, &str> = g
                .iter()
                .flat_map(|&j| {
                    buses[j]
                        .pins
                        .values()
                        .map(move |p| (p.as_str(), buses[j].protocol.as_str()))
                })
                .collect();
            if b.pins
                .values()
                .all(|p| owner.get(p.as_str()).is_none_or(|x| *x == b.protocol))
            {
                g.push(i);
                continue 'b;
            }
        }
        groups.push(vec![i]);
    }
    groups
}

/// The final layer of a protocol's result (Python's `export.FINAL`).
fn finals(protocol: &str) -> &'static [&'static str] {
    crate::final_layers(protocol)
}

/// The buses read in one piece's lines.
fn read_buses(lines: &[Line], rate: f64, n: i64, buses: &[Bus], strict: bool) -> Result<Vec<BusRead>, String> {
    let view = view_of(lines, rate, n)?;
    let names: Vec<String> = lines.iter().map(|l| l.name.clone()).collect();
    let mut out: Vec<Option<BusRead>> = (0..buses.len()).map(|_| None).collect();
    for g in apart(buses) {
        let bs: Vec<&Bus> = g.iter().map(|&i| &buses[i]).collect();
        let hints = hints_of(&bs, &names, strict);
        let cands = wireskein_core::readings(&view, &hints);
        let (claims, _) = wireskein_core::upper::stage_with(wireskein_core::choose(cands.clone()), rate);
        for &i in &g {
            let b = &buses[i];
            let pins: std::collections::BTreeSet<&String> = b.pins.values().collect();
            let mut cs: Vec<&wireskein_core::Candidate> = cands
                .iter()
                .filter(|c| {
                    c.protocol == b.protocol && c.roles.values().collect::<std::collections::BTreeSet<_>>() == pins
                })
                .collect();
            if let Some(e) = &b.edge {
                let same: Vec<&wireskein_core::Candidate> = cs
                    .iter()
                    .copied()
                    .filter(|c| c.params.get("sample_edge") == Some(&json!(e)))
                    .collect();
                if !same.is_empty() {
                    cs = same;
                }
            }
            let Some(best) = cs.into_iter().max_by(|a, b| a.score.partial_cmp(&b.score).unwrap()) else {
                out[i] = Some(BusRead {
                    decoded: false,
                    result: Map::new(),
                    fit: None,
                    uninterpreted: unread_of(b, lines, &Map::new(), rate, n),
                });
                continue;
            };
            // its claim (the upper stage's layers and score), else the reading on its own through the upper stage
            let claim = claims
                .iter()
                .find(|c| c.protocol == best.protocol && c.roles == best.roles && !c.bus)
                .cloned()
                .or_else(|| {
                    wireskein_core::upper::stage_with(wireskein_core::choose(vec![best.clone()]), rate)
                        .0
                        .into_iter()
                        .next()
                });
            let (mut lay, fit): (Map<String, Value>, f64) = match &claim {
                Some(c) => (serde_json::from_str(c.layers.get()).unwrap_or_default(), c.total),
                None => (serde_json::from_str(best.layers.get()).unwrap_or_default(), best.score),
            };
            // each frame given its clock (decode's frame_clocks)
            if let Some(clk) = b.pins.get("clk").and_then(|p| lines.iter().find(|l| &l.name == p))
                && let Some(frames) = report::frame_items(&mut lay, &b.protocol)
            {
                report::frame_clocks(frames, clk, rate);
            }
            let mut result = Map::new();
            if let Some(k) = finals(&b.protocol)
                .iter()
                .find(|k| lay.get(**k).is_some_and(crate::nonempty))
            {
                result.insert(k.to_string(), lay[*k].clone());
            }
            let low = report::lower(&b.protocol);
            if let Some(v) = lay.get(low)
                && !result.contains_key(low)
            {
                result.insert(low.into(), v.clone());
            }
            out[i] = Some(BusRead {
                decoded: true,
                uninterpreted: unread_of(b, lines, &lay, rate, n),
                result,
                fit: Some(fit),
            });
        }
    }
    Ok(out.into_iter().map(|x| x.unwrap()).collect())
}

fn unread_of(b: &Bus, lines: &[Line], lay: &Map<String, Value>, rate: f64, n: i64) -> Vec<Value> {
    let ls: Vec<&Line> = b
        .pins
        .values()
        .filter_map(|p| lines.iter().find(|l| &l.name == p))
        .collect();
    let sel = (b.protocol == "spi")
        .then(|| b.pins.get("cs"))
        .flatten()
        .and_then(|p| lines.iter().find(|l| &l.name == p));
    // a clocked bus: 16 of its bit times (the median interval of its clock's sampling edges); else 1 ms
    let clk = b.pins.get("clk").and_then(|p| lines.iter().find(|l| &l.name == p));
    let idle = match clk {
        Some(c) if c.edges.len() > 2 => {
            let want = if b.edge.as_deref() == Some("fall") { 0 } else { 1 };
            let t: Vec<i64> = (0..c.edges.len())
                .filter(|&i| c.initial ^ (((i + 1) & 1) as u8) == want)
                .map(|i| c.edges[i])
                .collect();
            let mut d: Vec<f64> = t.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
            if d.is_empty() {
                (report::IDLE_S * rate).round_ties_even() as i64
            } else {
                (report::IDLE_BITS as f64 * report::median(&mut d)) as i64
            }
        }
        _ => (report::IDLE_S * rate).round_ties_even() as i64,
    };
    report::unread(&b.protocol, &ls, lay, idle, sel, n, &mut vec![])
}

/// Items shifted to the part's ticks, each kept by the piece whose own ticks it starts in.
fn join_items(per: &[(&Piece, &Value)]) -> Vec<Value> {
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
fn join_columns(per: &[(&Piece, &Value)]) -> Value {
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
fn unread_summary(items: &[Value]) -> Value {
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

const DOUBT: f64 = 0.3;
const REANALYSES: i64 = 16;

/// The claims and split of a long part, or None when it is one piece (read as usual).
#[allow(clippy::too_many_arguments)]
pub fn analyze(
    lines: &[Line],
    n: i64,
    rate: f64,
    hints: &Value,
    o: &Options,
    mode: &str,
) -> Result<Option<(Vec<Value>, Value)>, String> {
    let mut noisy = Map::new();
    let ps = pieces(lines, n, o, &mut noisy)?;
    if ps.len() == 1 && ps[0].a == 0 {
        return Ok(None);
    }
    // the roles from the first piece that holds any (up to roles_from pieces)
    let (mut keep, mut first_claims, mut k0) = (vec![], vec![], 0);
    for (k, p) in ps.iter().enumerate().take(o.roles_from.max(1)) {
        let w = window(lines, p.a, p.b);
        let view = view_of(&w, rate, p.b - p.a)?;
        let (claims, _) = wireskein_core::analyze_full(&view, hints);
        let docs: Vec<Value> = claims
            .iter()
            .map(|c| crate::claim_doc(c, mode, &w, rate, p.b - p.a, None, false))
            .collect();
        k0 = k;
        keep = docs
            .iter()
            .filter(|c| (c["verdict"] == "confirmed" || c["verdict"] == "likely") && c["protocol"] != "sync_unknown")
            .cloned()
            .collect();
        first_claims = docs;
        if !keep.is_empty() {
            break;
        }
    }
    let buses: Vec<Bus> = keep
        .iter()
        .map(|c| Bus {
            protocol: c["protocol"].as_str().unwrap().into(),
            pins: c["roles"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                .collect(),
            params: ["baud", "mode"]
                .iter()
                .filter_map(|k| c["params"].get(*k).map(|v| (k.to_string(), v.clone())))
                .collect(),
            edge: c["params"]["sample_edge"]
                .as_str()
                .filter(|e| *e == "rise" || *e == "fall")
                .map(String::from),
        })
        .collect();
    // every piece read with the roles
    let whole = |p: &Piece| p.cut == "end" && p.a == 0;
    let mut reads: Vec<Vec<BusRead>> = vec![];
    let mut doubtful = vec![];
    for p in &ps {
        let w = window(lines, p.a, p.b);
        let r = read_buses(&w, rate, p.b - p.a, &buses, !whole(p))?;
        // why the roles read a piece badly
        let mut why = vec![];
        for (j, (b, br)) in buses.iter().zip(&r).enumerate() {
            let owned: usize = br
                .result
                .values()
                .map(|v| match v {
                    Value::Array(a) => a
                        .iter()
                        .filter(|x| {
                            x.get("s")
                                .and_then(|s| s.as_i64())
                                .is_some_and(|s| p.s <= s + p.a && s + p.a < p.e)
                                && x.get("ok") != Some(&json!(false))
                        })
                        .count(),
                    Value::Object(o) => o.get("s").and_then(|s| s.as_array()).map_or(0, |a| {
                        a.iter()
                            .filter_map(|s| s.as_i64())
                            .filter(|s| p.s <= s + p.a && s + p.a < p.e)
                            .count()
                    }),
                    _ => 0,
                })
                .sum();
            if !br.decoded {
                why.push(format!("bus {j} ({}) not decoded", b.protocol));
            } else if br.fit.unwrap_or(1.0) < DOUBT {
                why.push(format!(
                    "bus {j} ({}) fits {}",
                    b.protocol,
                    (br.fit.unwrap() * 1e4).round() / 1e4
                ));
            } else if owned == 0 {
                why.push(format!(
                    "bus {j} ({}) read nothing well in this piece's own ticks",
                    b.protocol
                ));
            }
        }
        if !why.is_empty() && p.edges > 0 {
            let mut d = json!({"s": p.s, "e": p.e, "seconds": [p.s as f64 / rate, p.e as f64 / rate],
                               "why": why.join("; ")});
            let sampled = (0..REANALYSES).any(|k| {
                let t = ((k as f64 + 0.5) * n as f64 / REANALYSES as f64) as i64;
                p.s <= t && t < p.e
            });
            if o.reanalyze && !whole(p) && sampled {
                let view = view_of(&w, rate, p.b - p.a)?;
                let (claims, _) = wireskein_core::analyze_full(&view, &Value::Null);
                let holds: Vec<Value> = claims
                    .iter()
                    .filter(|c| c.verdict == "confirmed" || c.verdict == "likely")
                    .map(|c| {
                        let mut h =
                            json!({"protocol": c.protocol, "roles": c.roles, "verdict": c.verdict, "score": c.score});
                        if !c.params.is_empty() {
                            h["params"] = Value::Object(c.params.clone());
                        }
                        h
                    })
                    .collect();
                d["holds"] = json!(holds);
            }
            doubtful.push(d);
        }
        reads.push(r);
    }
    // each bus joined over the pieces
    let mut claims = vec![];
    for (j, (c, b)) in keep.iter().zip(&buses).enumerate() {
        let per: Vec<(&Piece, &BusRead)> = ps.iter().zip(reads.iter().map(|r| &r[j])).collect();
        let mut keys: Vec<String> = vec![];
        for (_, r) in &per {
            for k in r.result.keys() {
                if !keys.contains(k) {
                    keys.push(k.clone());
                }
            }
        }
        let mut res = Map::new();
        for k in keys {
            let vals: Vec<(&Piece, &Value)> = per
                .iter()
                .filter_map(|(p, r)| r.result.get(&k).map(|v| (*p, v)))
                .collect();
            let v0 = vals[0].1;
            let joined = if v0.is_array() {
                Value::Array(join_items(&vals))
            } else if v0
                .as_object()
                .is_some_and(|o| o.values().any(|x| x.is_array()) && (o.contains_key("t") || o.contains_key("s")))
            {
                join_columns(&vals)
            } else {
                v0.clone()
            };
            res.insert(k, joined);
        }
        if (b.protocol == "rvswd" || b.protocol == "swio") && res.contains_key("dmi") {
            // the Debug Module keeps state across frames: read again over the pieces' accesses joined
            let mut m = Map::new();
            m.insert("dmi".into(), res["dmi"].clone());
            if let Some((_, dm)) = wireskein_core::upper::riscv_dm(&m) {
                res.insert("dm".into(), Value::Array(dm));
            }
        }
        let un = join_items(
            &per.iter()
                .map(|(p, r)| (*p, Value::Array(r.uninterpreted.clone())))
                .collect::<Vec<_>>()
                .iter()
                .map(|(p, v)| (*p, v))
                .collect::<Vec<_>>(),
        );
        let mut d = c.as_object().unwrap().clone();
        d.insert("result".into(), Value::Object(res));
        let fits: Vec<f64> = per
            .iter()
            .filter_map(|(_, r)| r.fit)
            .map(|f| (f * 1e4).round() / 1e4)
            .collect();
        d.insert("fit_pieces".into(), json!(fits));
        if !un.is_empty() {
            d.insert("uninterpreted".into(), json!(un));
        }
        d.insert("uninterpreted_summary".into(), unread_summary(&un));
        for k in ["reading", "clock_speeds", "undersampled"] {
            d.remove(k);
        }
        claims.push(Value::Object(d));
    }
    let mut split = json!({"options": o.doc(), "pieces": ps.iter().map(|p| p.doc()).collect::<Vec<_>>()});
    if !noisy.is_empty() {
        split["trembling"] = Value::Object(noisy);
    }
    if !doubtful.is_empty() {
        split["doubtful"] = json!(doubtful);
    }
    split["roles_from"] = json!([ps[k0].s, ps[k0].e]);
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
    Ok(Some((claims, split)))
}
