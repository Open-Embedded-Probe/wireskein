//! WireSkein's core in plain Rust: a capture's lines (their changes) to claims in the analysis format
//! (docs/analysis-format.md), with no Python in it, to be shared by a native app, the web (WASM) and Python
//! (`wireskein-py`).
//!
//! Built up beside the Python engine a part at a time (bench/notes/2026-11-24-rust-plan.ja.md): each part is judged on
//! the bench against the Python one (equal or better, not the same), and only then made the product's default.

pub mod claim;
pub mod view;

pub use claim::Claim;
pub use view::{Channel, View};

/// The claims on a part's lines: none yet (the engine is built up protocol by protocol, R1 on).
pub fn analyze(view: &View, hints: &serde_json::Value) -> Vec<Claim> {
    let _ = (view, hints);
    Vec::new()
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
