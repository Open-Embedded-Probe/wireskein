//! Marker lines on a UART -> a tree of time segments (Python's `wireskein._engine.markers`: docs/markers.md). A
//! dialect gives the line patterns and their action (heading, begin, end, point, attr); a segment starts at the end
//! of its marker line and ends at the start of the closing one. Problems are kept as issues, never fixed silently.
//! The dialects come as JSON ({name: {"dialect": {...}, "marker": [{"pattern", "action", ...}]}}, in their order:
//! Python's `markers.load_dialects()`); `heading()` is WireSkein's own (decl/markers/heading.toml).

use regex::Regex;
use serde_json::{Map, Value, json};
use wireskein_io::j::J;
use wireskein_io::jd;

/// WireSkein's own dialect (decl/markers/heading.toml).
pub fn heading() -> Value {
    json!({"heading": {"format": "wireskein-marker-dialect/1",
                       "dialect": {"name": "heading", "kind": "heading", "prefix": "#"},
                       "marker": [{"pattern": r"^(?P<hashes>#+)[ \t]*(?P<name>.*?)[ \t]*$", "action": "heading"}]}})
}

/// A dialect ready to match.
pub struct Dialect {
    pub name: String,
    heading: bool,
    levels: Vec<String>,
    prefix: Option<String>,
    markers: Vec<(Regex, Map<String, Value>)>,
}

impl Dialect {
    fn matches(&self, text: &str) -> bool {
        self.markers.iter().any(|(r, _)| r.is_match(text))
    }
    fn rank(&self, level: &Value) -> Option<i64> {
        if self.heading {
            level.as_i64() // a heading's level is its depth
        } else {
            let l = level.as_str()?;
            self.levels.iter().position(|x| x == l).map(|i| i as i64)
        }
    }
}

/// The dialects of a JSON document, in its order; each pattern matched at the start of a line, as Python's re.match.
pub fn dialects(v: &Value) -> Result<Vec<Dialect>, String> {
    let mut out = vec![];
    for (name, d) in v.as_object().into_iter().flatten() {
        let head = &d["dialect"];
        let mut markers = vec![];
        for m in d["marker"].as_array().into_iter().flatten() {
            let pat = m["pattern"]
                .as_str()
                .ok_or_else(|| format!("{name}: a marker without a pattern"))?;
            let r = Regex::new(&format!(r"\A(?:{pat})")).map_err(|e| format!("{name}: {pat:?}: {e}"))?;
            markers.push((r, m.as_object().cloned().unwrap_or_default()));
        }
        out.push(Dialect {
            name: name.clone(),
            heading: head["kind"] == "heading",
            levels: head["levels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(String::from))
                .collect(),
            prefix: head["prefix"].as_str().map(String::from),
            markers,
        });
    }
    Ok(out)
}

/// A segment of the tree (`Tree::segs[0]` is the root, the whole capture).
#[derive(Debug, Clone)]
pub struct Segment {
    pub level: Value,
    pub name: String,
    pub begin: i64,
    pub end: Option<i64>,
    pub seq: Option<i64>,
    /// "block" (begin / end), "section" (point, heading) or "root"
    pub kind: &'static str,
    pub raw: String,
    pub attrs: Map<String, Value>,
    pub children: Vec<usize>,
    pub issues: Vec<Value>,
    pub parent: Option<usize>,
}

/// The segments, the root first.
#[derive(Debug, Clone)]
pub struct Tree {
    pub segs: Vec<Segment>,
}

impl Tree {
    /// Its path: the names from the top, a repeated name among its siblings given its index ("step[1]").
    pub fn path(&self, i: usize) -> String {
        let mut parts = vec![];
        let mut s = Some(i);
        while let Some(k) = s {
            let g = &self.segs[k];
            if g.kind == "root" {
                break;
            }
            let idx = g.parent.map_or(0, |p| {
                self.segs[p]
                    .children
                    .iter()
                    .filter(|&&c| self.segs[c].name == g.name)
                    .position(|&c| c == k)
                    .unwrap_or(0)
            });
            parts.push(if idx > 0 {
                format!("{}[{idx}]", g.name)
            } else {
                g.name.clone()
            });
            s = g.parent;
        }
        parts.reverse();
        parts.join("/")
    }

    /// The segments in order (each before its children), the root first.
    pub fn walk(&self) -> Vec<usize> {
        let (mut out, mut stack) = (vec![], vec![0usize]);
        while let Some(i) = stack.pop() {
            out.push(i);
            stack.extend(self.segs[i].children.iter().rev());
        }
        out
    }

    /// The segments whose path matches a glob ("session/pwm/*", "*/1kHz-25[1]"): `*` any run, `?` any one
    /// character; "[" is literal (the sibling index, not a character class).
    pub fn select(&self, pattern: &str) -> Vec<usize> {
        let pat: Vec<char> = pattern.chars().collect();
        self.walk()
            .into_iter()
            .filter(|&i| self.segs[i].kind != "root" && glob(&pat, &self.path(i).chars().collect::<Vec<_>>()))
            .collect()
    }
}

fn glob(p: &[char], s: &[char]) -> bool {
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < s.len() {
        if i < p.len() && (p[i] == '?' || p[i] == s[j]) && p[i] != '*' {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(st) = star {
            i = st + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == '*' {
        i += 1;
    }
    i == p.len()
}

/// A text line: its text, its first tick, the tick after its last character.
pub type TextLine = (String, i64, i64);

/// (text, first tick, the tick after its last character) of each complete line a UART claim's bytes hold (Python's
/// `markers.lines_with_times`). layers: the claim's layers (its "bytes": {"s", "hex"}); params: its "baud", "L".
pub fn lines_with_times(layers: &Value, params: &Map<String, Value>, rate: f64) -> Vec<TextLine> {
    let b = &layers["bytes"];
    let st: Vec<i64> = b["s"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_i64())
        .collect();
    let hex = b["hex"].as_str().unwrap_or("");
    let vals: Vec<u8> = (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok())
        .collect();
    let Some(baud) = params.get("baud").and_then(|x| x.as_f64()) else {
        return vec![];
    };
    let l = params.get("L").and_then(|x| x.as_f64()).unwrap_or(10.0);
    let ch = wireskein_io::pyfmt::round(l * rate / baud);
    let (mut out, mut a) = (vec![], 0usize);
    for i in 0..vals.len().min(st.len()) {
        if vals[i] != 0x0A {
            continue;
        }
        let text: String = vals[a..i].iter().map(|&c| c as char).collect(); // latin-1
        let text = text.trim_end_matches('\r').to_string();
        if a < i || !text.is_empty() {
            out.push((text, if a < st.len() { st[a] } else { st[i] }, st[i] + ch));
        }
        a = i + 1;
    }
    out
}

fn value_of(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| json!(s))
}

/// The segment tree of marker lines (Python's `markers.build`).
pub fn build(lines: &[(String, i64, i64)], d: &Dialect, n: i64) -> Tree {
    let mut t = Tree {
        segs: vec![Segment {
            level: json!("capture"),
            name: String::new(),
            begin: 0,
            end: Some(n),
            seq: None,
            kind: "root",
            raw: String::new(),
            attrs: Map::new(),
            children: vec![],
            issues: vec![],
            parent: None,
        }],
    };
    let mut stack = vec![0usize];
    let mut last_seq: Map<String, Value> = Map::new();
    let new = |t: &mut Tree, level: Value, name: &str, begin: i64, seq, kind, raw: &str, parent: usize| -> usize {
        t.segs.push(Segment {
            level,
            name: name.into(),
            begin,
            end: None,
            seq,
            kind,
            raw: raw.into(),
            attrs: Map::new(),
            children: vec![],
            issues: vec![],
            parent: Some(parent),
        });
        t.segs.len() - 1
    };
    // close the open segments at this rank or deeper (sections silently)
    let close_from = |t: &mut Tree, stack: &mut Vec<usize>, r: i64, at: i64, why: Option<Value>| {
        while stack.len() > 1 && d.rank(&t.segs[*stack.last().unwrap()].level).is_some_and(|x| x >= r) {
            let s = stack.pop().unwrap();
            t.segs[s].end = Some(at);
            if t.segs[s].kind != "section"
                && let Some(w) = &why
            {
                t.segs[s].issues.push(w.clone());
            }
        }
    };
    for (text, t0, t1) in lines {
        let Some((r, mk)) = d.markers.iter().find(|(r, _)| r.is_match(text)) else {
            if d.prefix.as_deref().is_some_and(|p| text.starts_with(p)) {
                let top = *stack.last().unwrap();
                t.segs[top]
                    .issues
                    .push(json!({"kind": "unparsed", "at": t0, "text": text}));
            }
            continue;
        };
        let g = r.captures(text).unwrap();
        let grp = |k: &str| g.name(k).map(|m| m.as_str().to_string());
        let act = mk.get("action").and_then(|x| x.as_str()).unwrap_or("");
        let top = *stack.last().unwrap();
        if act == "heading" {
            let depth = grp("hashes").map_or(0, |h| h.chars().count() as i64);
            let name = grp("name").unwrap_or_default();
            if !name.is_empty() {
                close_from(&mut t, &mut stack, depth, *t0, None);
                let parent = *stack.last().unwrap();
                let pd = if t.segs[parent].kind != "root" {
                    t.segs[parent].level.as_i64().unwrap_or(0)
                } else {
                    0
                };
                let s = new(&mut t, json!(depth), &name, *t1, None, "section", text, parent);
                if depth > pd + 1 {
                    t.segs[s]
                        .issues
                        .push(json!({"kind": "level_skip", "from": pd, "to": depth}));
                }
                t.segs[parent].children.push(s);
                stack.push(s);
            } else if stack
                .iter()
                .any(|&x| t.segs[x].kind != "root" && t.segs[x].level == json!(depth))
            {
                close_from(&mut t, &mut stack, depth, *t0, None);
            } else {
                t.segs[top]
                    .issues
                    .push(json!({"kind": "end_without_begin", "at": t0, "text": text}));
            }
            continue;
        }
        if act == "attr" {
            let key = grp("key")
                .filter(|k| !k.is_empty())
                .or_else(|| mk.get("key").and_then(|x| x.as_str()).map(String::from))
                .unwrap_or_default();
            let v = value_of(&grp("value").unwrap_or_default());
            match t.segs[top].attrs.entry(key).or_insert_with(|| json!([])) {
                Value::Array(a) => a.push(v),
                _ => unreachable!(),
            }
            continue;
        }
        let level = grp("level")
            .filter(|k| !k.is_empty())
            .map(Value::String)
            .or_else(|| mk.get("level").cloned())
            .unwrap_or(Value::Null);
        let Some(rank) = d.rank(&level) else {
            t.segs[top]
                .issues
                .push(json!({"kind": "unknown_level", "at": t0, "text": text}));
            continue;
        };
        let seq = grp("seq")
            .filter(|x| !x.is_empty())
            .and_then(|x| x.trim().parse::<i64>().ok());
        let name = grp("name").unwrap_or_default();
        let lkey = match &level {
            Value::String(s) => s.clone(),
            x => x.to_string(),
        };
        if act == "begin" || act == "point" {
            close_from(
                &mut t,
                &mut stack,
                rank,
                *t0,
                Some(json!({"kind": "implicit_end", "at": t0, "by": text})),
            );
            let parent = *stack.last().unwrap();
            let kind = if act == "begin" { "block" } else { "section" };
            let s = new(&mut t, level.clone(), &name, *t1, seq, kind, text, parent);
            if let Some(q) = seq {
                if let Some(last) = last_seq.get(&lkey).and_then(|x| x.as_i64())
                    && q != last + 1
                {
                    t.segs[s]
                        .issues
                        .push(json!({"kind": "seq_gap", "expected": last + 1, "got": q}));
                }
                last_seq.insert(lkey, json!(q));
            }
            t.segs[parent].children.push(s);
            stack.push(s);
        } else if act == "end" {
            let idx = (1..stack.len())
                .rev()
                .find(|&i| t.segs[stack[i]].level == level && t.segs[stack[i]].kind == "block");
            let Some(idx) = idx else {
                t.segs[top]
                    .issues
                    .push(json!({"kind": "end_without_begin", "at": t0, "text": text}));
                continue;
            };
            let target = stack[idx];
            while stack.len() - 1 > idx {
                let s = stack.pop().unwrap();
                t.segs[s].end = Some(*t0);
                if t.segs[s].kind != "section" {
                    t.segs[s]
                        .issues
                        .push(json!({"kind": "unterminated", "closed_by": text}));
                }
            }
            stack.pop();
            let tg = &t.segs[target];
            let mism = (seq.is_some() && tg.seq.is_some() && seq != tg.seq)
                || (!name.is_empty() && !tg.name.is_empty() && name != tg.name);
            t.segs[target].end = Some(*t0);
            if mism {
                t.segs[target]
                    .issues
                    .push(json!({"kind": "end_mismatch", "text": text}));
            }
        }
    }
    while stack.len() > 1 {
        let s = stack.pop().unwrap();
        t.segs[s].end = Some(n);
        if t.segs[s].kind != "section" {
            t.segs[s]
                .issues
                .push(json!({"kind": "unterminated", "closed_by": "capture end"}));
        }
    }
    t
}

/// The marker stream found (Python's `markers.find`): the UART claim and dialect whose lines match best.
pub struct Found {
    pub pin: String,
    pub dialect: usize,
    pub lines: usize,
    pub markers: usize,
    pub tree: Tree,
}

/// A UART claim's data line, layers and params (the engine's claims as `find` and `events` read them).
pub struct Uart<'a> {
    pub pin: &'a str,
    pub layers: Value,
    pub params: &'a Map<String, Value>,
}

/// The UART claims of the core's.
pub fn uarts(claims: &[wireskein_core::Claim]) -> Vec<Uart<'_>> {
    claims
        .iter()
        .filter(|c| c.protocol == "uart")
        .filter_map(|c| {
            Some(Uart {
                pin: c.roles.get("data")?.as_str(),
                layers: serde_json::from_str(c.layers.get()).ok()?,
                params: &c.params,
            })
        })
        .collect()
}

/// The marker stream: the UART claim + dialect whose lines match best (at least 2); hint {"pin", "dialect"} narrows
/// the search.
pub fn find(us: &[Uart], ds: &[Dialect], rate: f64, n: i64, hint: Option<&Value>) -> Option<Found> {
    let want = |k: &str| {
        hint.and_then(|h| h.get(k))
            .and_then(|x| x.as_str())
            .filter(|x| !x.is_empty())
    };
    let mut best: Option<(usize, &Uart, usize, Vec<TextLine>)> = None;
    for u in us {
        if want("pin").is_some_and(|p| p != u.pin) {
            continue;
        }
        let lines = lines_with_times(&u.layers, u.params, rate);
        if lines.is_empty() {
            continue;
        }
        for (k, d) in ds.iter().enumerate() {
            if want("dialect").is_some_and(|x| x != d.name) {
                continue;
            }
            let hits = lines.iter().filter(|(t, _, _)| d.matches(t)).count();
            if hits >= 2 && best.as_ref().is_none_or(|b| hits > b.0) {
                best = Some((hits, u, k, lines.clone()));
            }
        }
    }
    let (hits, u, k, lines) = best?;
    Some(Found {
        pin: u.pin.to_string(),
        dialect: k,
        lines: lines.len(),
        markers: hits,
        tree: build(&lines, &ds[k], n),
    })
}

/// Every text line of every UART claim on one time line (Python's `markers.events`): the marker channel's marker
/// lines of kind "marker", the rest "event". -> (tick, pin, kind, text), by time.
pub fn events<'a>(us: &'a [Uart], f: &Found, ds: &[Dialect], rate: f64) -> Vec<(i64, &'a str, &'static str, String)> {
    let d = &ds[f.dialect];
    let mut out = vec![];
    for u in us {
        for (text, t0, _) in lines_with_times(&u.layers, u.params, rate) {
            let marker = u.pin == f.pin && d.matches(&text);
            out.push((t0, u.pin, if marker { "marker" } else { "event" }, text));
        }
    }
    out.sort_by_key(|x| x.0);
    out
}

/// The events and marker lines inside a window, their times in seconds (Python's `markers.events_in`).
pub fn events_in(us: &[Uart], f: &Found, ds: &[Dialect], rate: f64, window: (i64, i64)) -> Vec<Value> {
    events(us, f, ds, rate)
        .into_iter()
        .filter(|x| window.0 <= x.0 && x.0 < window.1)
        .map(|(t, pin, kind, text)| json!({"s": t as f64 / rate, "pin": pin, "kind": kind, "text": text}))
        .collect()
}

/// Frequency and duty of a line that toggles regularly inside a segment (Python's `markers._periodic`): rising to
/// rising periods, the high time of each; None with fewer than 4 rises or periods that do not agree.
fn periodic(ed: &[i64], level_before: u8, rate: f64, step: i64) -> Option<J> {
    let (rises, falls): (Vec<i64>, Vec<i64>) = {
        let even: Vec<i64> = ed.iter().step_by(2).copied().collect();
        let odd: Vec<i64> = ed.iter().skip(1).step_by(2).copied().collect();
        if level_before == 0 { (even, odd) } else { (odd, even) }
    };
    if rises.len() < 4 {
        return None;
    }
    let per: Vec<f64> = rises.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let med = crate::states::median(&mut per.clone());
    let agree = per.iter().filter(|p| (*p - med).abs() <= 0.05 * med).count() as f64 / per.len() as f64;
    if med <= 0.0 || agree < 0.9 {
        return None;
    }
    let mut high = vec![];
    for (k, r) in rises[..rises.len() - 1].iter().enumerate() {
        let j = falls.partition_point(|f| f < r);
        if j < falls.len() {
            let h = (falls[j] - r) as f64;
            if h < per[k] {
                high.push(h / med);
            }
        }
    }
    let n = per.len() as f64;
    let mean = crate::align::np_sum(&per) / n;
    let dev: Vec<f64> = per.iter().map(|p| (p - mean) * (p - mean)).collect();
    let std = (crate::align::np_sum(&dev) / n).sqrt();
    Some(jd! {
        "freq_hz" => rate * n / (rises[rises.len() - 1] - rises[0]) as f64,
        "duty" => if high.is_empty() { J::Null } else { J::Float(crate::states::median(&mut high)) },
        "periods" => per.len(),
        "period_spread" => std / med,
        "resolution" => step as f64 / med,
    })
}

/// Which lines moved in a segment, their levels at both ends, the quiet at its head and tail (Python's
/// `markers.activity`); the lines named in `exclude` left out.
fn activity(lines: &[wireskein_io::Line], b: i64, e: i64, rate: f64, exclude: &str) -> J {
    let (mut moved, mut levels, mut periodic_) = (J::dict(), J::dict(), J::dict());
    let (mut first, mut last): (Option<i64>, Option<i64>) = (None, None);
    let mut changed = vec![];
    for l in lines.iter().filter(|l| l.name != exclude) {
        let ed = &l.edges;
        let (i0, i1) = (ed.partition_point(|&x| x < b), ed.partition_point(|&x| x < e));
        let lv = |t: i64| l.initial ^ ((ed.partition_point(|&x| x <= t) & 1) as u8);
        let (a, z) = (lv(b), lv(b.max(e - 1)));
        levels.set(&l.name, J::List(vec![J::from(a), J::from(z)]));
        if a != z {
            changed.push(l.name.clone());
        }
        if i1 > i0 {
            moved.set(&l.name, i1 - i0);
            first = Some(first.map_or(ed[i0], |f| f.min(ed[i0])));
            last = Some(last.map_or(ed[i1 - 1], |x| x.max(ed[i1 - 1])));
            if let Some(p) = periodic(&ed[i0..i1], l.initial ^ ((i0 & 1) as u8), rate, l.step) {
                periodic_.set(&l.name, p);
            }
        }
    }
    changed.sort();
    jd! {
        "begin" => b, "end" => e, "moved" => moved, "levels" => levels,
        "quiet_head_s" => first.map_or(J::Null, |f| J::Float((f - b) as f64 / rate)),
        "quiet_tail_s" => last.map_or(J::Null, |x| J::Float((e - x) as f64 / rate)),
        "changed" => J::List(changed.into_iter().map(J::from).collect()),
        "periodic" => periodic_,
    }
}

/// The segment tree as plain data (Python's `markers.describe`): each segment's times, marker line, issues, the
/// events directly in it (with_events), the lines' activity (the marker line's left out) and, per claim (UART's
/// text lines are the events), how many items were decoded inside (with the items: results).
#[allow(clippy::too_many_arguments)]
pub fn describe(
    claims: &[wireskein_core::Claim],
    lines: &[wireskein_io::Line],
    rate: f64,
    n: i64,
    found: &Found,
    ds: &[Dialect],
    results: bool,
    with_events: bool,
) -> J {
    let us = uarts(claims);
    let evs = events(&us, found, ds, rate);
    let t = &found.tree;
    let ev_out = |x: &(i64, &str, &str, String)| {
        jd! {"s" => x.0 as f64 / rate, "pin" => x.1, "kind" => x.2, "text" => x.3.as_str()}
    };
    let span = |i: usize| (t.segs[i].begin, t.segs[i].end.unwrap_or(n));
    fn node(i: usize, t: &Tree, cx: &dyn Fn(usize) -> J) -> J {
        let mut d = cx(i);
        d.set(
            "children",
            J::List(t.segs[i].children.iter().map(|&c| node(c, t, cx)).collect()),
        );
        d
    }
    let one = |i: usize| -> J {
        let s = &t.segs[i];
        let (b, e) = span(i);
        let mut d = jd! {"path" => t.path(i), "name" => s.name.as_str(), "level" => wireskein_io::write::j_of(&s.level),
        "raw" => s.raw.as_str(), "begin_s" => b as f64 / rate, "end_s" => e as f64 / rate};
        if !s.attrs.is_empty() {
            d.set("attrs", wireskein_io::write::j_of(&Value::Object(s.attrs.clone())));
        }
        if !s.issues.is_empty() {
            d.set(
                "issues",
                J::List(s.issues.iter().map(wireskein_io::write::j_of).collect()),
            );
        }
        let kids: Vec<(i64, i64)> = s.children.iter().map(|&c| span(c)).collect();
        let direct: Vec<J> = evs
            .iter()
            .filter(|x| b <= x.0 && x.0 < e && !kids.iter().any(|(k0, k1)| *k0 <= x.0 && x.0 < *k1))
            .map(ev_out)
            .collect();
        if !direct.is_empty() && with_events {
            d.set("events", J::List(direct));
        }
        d.set("activity", activity(lines, b, e, rate, &found.pin));
        let mut inside = vec![];
        for c in claims.iter().filter(|c| c.protocol != "uart") {
            let doc = crate::claim_doc(c, "final", lines, rate, n, Some((b, e)), false);
            let r = doc
                .get("result")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            let k: usize = r
                .values()
                .map(|v| match v {
                    Value::Array(a) => a.len(),
                    Value::Object(o) => o.get("s").and_then(|s| s.as_array()).map_or(0, |a| a.len()),
                    _ => 0,
                })
                .sum();
            if k > 0 {
                let mut x = jd! {"protocol" => c.protocol.as_str(),
                "roles" => wireskein_io::write::j_of(&json!(c.roles)), "items" => k};
                if results {
                    x.set("result", wireskein_io::write::j_of(&Value::Object(r)));
                }
                inside.push(x);
            }
        }
        d.set("decoded", J::List(inside));
        d
    };
    let root = &t.segs[0];
    let top: Vec<(i64, i64)> = root.children.iter().map(|&c| span(c)).collect();
    let mut doc = jd! {
        "marker" => jd! {"pin" => found.pin.as_str(), "dialect" => ds[found.dialect].name.as_str(),
                         "lines" => found.lines, "markers" => found.markers},
        "root_issues" => J::List(root.issues.iter().map(wireskein_io::write::j_of).collect()),
        "segments" => J::List(root.children.iter().map(|&c| node(c, t, &one)).collect()),
    };
    if with_events {
        doc.set(
            "events_outside",
            J::List(
                evs.iter()
                    .filter(|x| !top.iter().any(|(k0, k1)| *k0 <= x.0 && x.0 < *k1))
                    .map(ev_out)
                    .collect(),
            ),
        );
    }
    doc
}

/// The segments document of a capture file (Python's `wireskein segments`): the capture (one part) read, the
/// analog channels asked read as logic, analyzed by the core, its marker stream found (markers: {"pin", "dialect"};
/// dialects: as `dialects` takes them, None for WireSkein's own), described.
pub fn segments_file(
    path: &std::path::Path,
    hints: &Value,
    thresholds: &[(String, f64, f64)],
    markers: Option<&Value>,
    dialects_json: Option<&Value>,
    results: bool,
    with_events: bool,
) -> Result<Option<J>, String> {
    let cap = wireskein_io::read(path)?;
    if cap.parts.len() > 1 {
        return Err(format!(
            "{}: {} parts ({}): read one with part=, or all with read_parts()",
            path.display(),
            cap.n_parts,
            cap.layout
        ));
    }
    let p = cap
        .parts
        .first()
        .ok_or_else(|| format!("{}: no part", path.display()))?;
    let asked = crate::Asked {
        thresholds,
        ..Default::default()
    };
    let lines = crate::lines_asked(p, cap.tick_hz, &asked)?;
    let rate = cap.rate();
    let chans: Vec<wireskein_core::Channel> = lines
        .iter()
        .map(|l| wireskein_core::Channel::new(&l.name, l.initial, &l.edges))
        .collect();
    let view = wireskein_core::View::new(rate, p.ticks, chans).map_err(|e| format!("part {}: {e}", p.part))?;
    let (claims, _) = wireskein_core::analyze_full(&view, hints);
    let own = heading();
    let ds = dialects(dialects_json.unwrap_or(&own))?;
    let us = uarts(&claims);
    let Some(found) = find(&us, &ds, rate, p.ticks, markers) else {
        return Ok(None);
    };
    Ok(Some(describe(
        &claims,
        &lines,
        rate,
        p.ticks,
        &found,
        &ds,
        results,
        with_events,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(ls: &[&str]) -> Vec<(String, i64, i64)> {
        ls.iter()
            .enumerate()
            .map(|(i, t)| (t.to_string(), 100 * i as i64, 100 * i as i64 + 50))
            .collect()
    }

    #[test]
    fn headings_make_a_tree() {
        let ds = dialects(&heading()).unwrap();
        let t = build(
            &lines(&[
                "# session",
                "## pwm",
                "PING",
                "## pwm",
                "### deep",
                "##",
                "#### skip",
                "# end",
            ]),
            &ds[0],
            1000,
        );
        let paths: Vec<String> = t.walk().into_iter().skip(1).map(|i| t.path(i)).collect();
        assert_eq!(
            paths,
            [
                "session",
                "session/pwm",
                "session/pwm[1]",
                "session/pwm[1]/deep",
                "session/skip",
                "end"
            ]
        );
        let i = t.select("session/pwm[1]")[0];
        assert_eq!((t.segs[i].begin, t.segs[i].end), (350, Some(500)));
        assert_eq!(t.select("*/pwm*").len(), 3);
        let skip = t.select("*skip")[0];
        assert_eq!(
            t.segs[skip].issues,
            vec![json!({"kind": "level_skip", "from": 1, "to": 4})]
        );
        assert_eq!(t.segs[t.select("end")[0]].end, Some(1000));
    }
}
