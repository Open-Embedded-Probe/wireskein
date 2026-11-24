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

/// The claims among the candidates: the best set using each line once, greedily by the lines a reading explains
/// times its score past the claim's cost; each claim's margin over the best rival (another reading on some of its
/// lines, not the same protocol on fewer of them), its verdict from the two.
pub fn choose(mut cands: Vec<Candidate>) -> Vec<Claim> {
    cands.retain(|c| c.score > CLAIM_COST);
    let value = |c: &Candidate| c.roles.len() as f64 * (c.score - CLAIM_COST);
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| value(&cands[b]).partial_cmp(&value(&cands[a])).unwrap().then(a.cmp(&b)));
    let mut used: std::collections::BTreeSet<String> = Default::default();
    let mut out = vec![];
    for &i in &order {
        let c = &cands[i];
        let ls = lines(c);
        if ls.iter().any(|l| used.contains(*l)) {
            continue;
        }
        used.extend(ls.iter().map(|s| s.to_string()));
        let rival = cands
            .iter()
            .enumerate()
            .filter(|&(j, o)| {
                j != i && !lines(o).is_disjoint(&ls) && !(o.protocol == c.protocol && lines(o).is_subset(&ls))
            })
            .map(|(_, o)| o.score)
            .chain(c.alt)
            .fold(None, |m: Option<f64>, s| Some(m.map_or(s, |m| m.max(s))));
        let margin = c.score - rival.unwrap_or(CLAIM_COST).max(CLAIM_COST);
        let confirmed = (c.score >= 0.85 && margin >= 0.15)
            || (rival.is_some() && ((c.score >= 0.80 && margin >= 0.25) || (c.score >= 0.60 && margin >= 0.50)));
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
    cands
}

/// The claims on a part's lines (built up protocol by protocol: UART and I2C so far).
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
