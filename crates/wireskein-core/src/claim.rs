//! A claim in the analysis format (docs/analysis-format.md §3): what an engine says a set of lines carries.

use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Claim {
    pub protocol: String,
    /// role -> line
    pub roles: BTreeMap<String, String>,
    /// "confirmed", "likely" or "ambiguous"
    pub verdict: String,
    pub score: f64,
    pub margin: f64,
    pub params: Map<String, Value>,
    pub evidence: Map<String, Value>,
    pub layers: Map<String, Value>,
}

/// Claims as the JSON the analysis format has.
pub fn to_json(claims: &[Claim]) -> String {
    serde_json::to_string(claims).expect("claims are plain data")
}
