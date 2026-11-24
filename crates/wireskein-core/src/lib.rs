//! WireSkein's core in plain Rust: a capture's lines (their changes) to claims in the analysis format
//! (docs/analysis-format.md), with no Python in it, to be shared by a native app, the web (WASM) and Python
//! (`wireskein-py`).
//!
//! Built up beside the Python engine a part at a time (bench/notes/2026-11-24-rust-plan.ja.md): each part is judged on
//! the bench against the Python one (equal or better, not the same), and only then made the product's default.

pub mod claim;
pub mod features;
pub mod i2c;
pub mod kernels;
pub mod rebuild;
pub mod spi;
pub mod sync;
pub mod uart;
pub mod view;

pub use claim::Claim;
pub use view::{Channel, View};

/// A reading of some lines that may become a claim: its score, and the best other reading of the same lines by the
/// same reader that is no same reading (another idle level, another rate), if any.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub protocol: String,
    pub roles: std::collections::BTreeMap<String, String>,
    pub score: f64,
    pub alt: Option<f64>,
    pub params: serde_json::Map<String, serde_json::Value>,
    pub evidence: serde_json::Map<String, serde_json::Value>,
    pub layers: Box<serde_json::value::RawValue>,
}

pub const CLAIM_COST: f64 = 0.3;

fn lines(c: &Candidate) -> std::collections::BTreeSet<&str> {
    c.roles.values().map(|s| s.as_str()).collect()
}

/// The set of candidates using each line once with the most value, by branch and bound over the candidates best
/// first (Python's `stack.explanations`); `order`: the candidates by value, best first. Past a bound on the search,
/// the best set found so far (the greedy one is the first found).
fn pack(cands: &[Candidate], order: &[usize], vals: &[f64]) -> Vec<usize> {
    const STEPS: usize = 200_000;
    let sets: Vec<std::collections::BTreeSet<&str>> = order.iter().map(|&i| lines(&cands[i])).collect();
    let mut rest = vec![0.0; order.len() + 1];
    for k in (0..order.len()).rev() {
        rest[k] = rest[k + 1] + vals[k];
    }
    struct Search<'a> {
        sets: Vec<std::collections::BTreeSet<&'a str>>,
        vals: Vec<f64>,
        rest: Vec<f64>,
        best: f64,
        best_set: Vec<usize>,
        cur: Vec<usize>,
        used: std::collections::BTreeMap<&'a str, usize>,
        steps: usize,
    }
    impl Search<'_> {
        fn go(&mut self, k: usize, v: f64) {
            self.steps += 1;
            if v > self.best + 1e-12 {
                self.best = v;
                self.best_set = self.cur.clone();
            }
            if k == self.sets.len() || v + self.rest[k] <= self.best + 1e-12 || self.steps > STEPS {
                return;
            }
            if self.sets[k].iter().all(|l| !self.used.contains_key(l)) {
                self.cur.push(k);
                for l in self.sets[k].clone() {
                    self.used.insert(l, k);
                }
                self.go(k + 1, v + self.vals[k]);
                for l in self.sets[k].clone() {
                    self.used.remove(l);
                }
                self.cur.pop();
            }
            self.go(k + 1, v);
        }
    }
    let mut s = Search {
        sets,
        vals: vals.to_vec(),
        rest,
        best: 0.0,
        best_set: vec![],
        cur: vec![],
        used: Default::default(),
        steps: 0,
    };
    s.go(0, 0.0);
    s.best_set.into_iter().map(|k| order[k]).collect()
}

/// The claims among the candidates: the set using each line once with the most value (the lines a reading explains
/// times its score past the claim's cost; `pack`); each claim's margin over the best rival (another reading on some of its
/// lines, not the same protocol on fewer of them), its verdict from the two.
pub fn choose(mut cands: Vec<Candidate>) -> Vec<Claim> {
    cands.retain(|c| c.score > CLAIM_COST);
    let value = |c: &Candidate| c.roles.len() as f64 * (c.score - CLAIM_COST);
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| value(&cands[b]).partial_cmp(&value(&cands[a])).unwrap().then(a.cmp(&b)));
    let vals: Vec<f64> = order.iter().map(|&i| value(&cands[i])).collect();
    let chosen = pack(&cands, &order, &vals);
    let mut out = vec![];
    for &i in &chosen {
        let c = &cands[i];
        let ls = lines(c);
        // the margin: the best set without this reading (nor a less complete one of its protocol on its lines), the
        // value lost over the lines whose reading changes; its own alternative (another idle level, another rate)
        // a rival too, as one line read otherwise
        let keep: Vec<usize> = order
            .iter()
            .copied()
            .filter(|&j| j != i && !(cands[j].protocol == c.protocol && lines(&cands[j]).is_subset(&ls)))
            .collect();
        let kv: Vec<f64> = keep.iter().map(|&j| value(&cands[j])).collect();
        let alt = pack(&cands, &keep, &kv);
        let total = |set: &[usize]| set.iter().map(|&j| value(&cands[j])).sum::<f64>();
        let owner = |set: &[usize]| -> std::collections::BTreeMap<String, usize> {
            set.iter()
                .flat_map(|&j| cands[j].roles.values().map(move |l| (l.clone(), j)))
                .collect()
        };
        let (ob, oa) = (owner(&chosen), owner(&alt));
        let changed = ob
            .keys()
            .chain(oa.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|l| ob.get(*l) != oa.get(*l))
            .count();
        let mut margin = (total(&chosen) - total(&alt)) / changed.max(1) as f64;
        let mut rival = alt.iter().any(|&j| !lines(&cands[j]).is_disjoint(&ls));
        if let Some(a) = c.alt {
            margin = margin.min(c.score - a.max(CLAIM_COST));
            rival = true;
        }
        let confirmed = (c.score >= 0.85 && margin >= 0.15)
            || (rival && ((c.score >= 0.80 && margin >= 0.25) || (c.score >= 0.60 && margin >= 0.50)));
        let verdict = if confirmed {
            "confirmed"
        } else if c.score >= 0.6 && margin >= 0.05 {
            "likely"
        } else {
            "ambiguous"
        };
        out.push(Claim {
            protocol: c.protocol.clone(),
            roles: c.roles.clone(),
            verdict: verdict.into(),
            score: (c.score * 1e4).round() / 1e4,
            margin: (margin * 1e4).round() / 1e4,
            params: c.params.clone(),
            evidence: c.evidence.clone(),
            layers: c.layers.clone(),
        });
    }
    out
}

/// Every reading of a part's lines, before the claims are chosen.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut cands = uart::candidates(view);
    cands.extend(i2c::candidates(view));
    cands.extend(spi::candidates(view));
    cands
}

/// The claims on a part's lines (built up protocol by protocol: UART, I2C and SPI so far).
pub fn analyze(view: &View, hints: &serde_json::Value) -> Vec<Claim> {
    let _ = hints;
    choose(candidates(view))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_part_has_no_claims() {
        let v = View::new(1e6, 1000, vec![Channel::new("A", 1, &[])]).unwrap();
        assert!(analyze(&v, &serde_json::Value::Null).is_empty());
    }
}
