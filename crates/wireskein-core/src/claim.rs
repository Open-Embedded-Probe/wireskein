//! A claim in the analysis format (docs/analysis-format.md §3): what an engine says a set of lines carries.

use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
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
    /// the layers as JSON, written once for the claim (a capture's characters are millions: no tree of values)
    pub layers: Box<RawValue>,
}

/// Layers written as JSON text.
pub fn raw(json: String) -> Box<RawValue> {
    RawValue::from_string(json).expect("layers are written as JSON")
}

/// Claims as the JSON the analysis format has.
pub fn to_json(claims: &[Claim]) -> String {
    serde_json::to_string(claims).expect("claims are plain data")
}
