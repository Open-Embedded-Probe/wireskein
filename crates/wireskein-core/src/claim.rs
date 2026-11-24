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
    /// the score before rounding, as the upper stage raises it
    #[serde(skip)]
    pub total: f64,
    /// another reading on some of its lines was in the best set without it (the verdict's rule over a rival)
    #[serde(skip)]
    pub rival: bool,
    /// one target of a bus of several (its bus read above it as a whole, not alone)
    #[serde(skip)]
    pub bus: bool,
}

/// Layers written as JSON text.
pub fn raw(json: String) -> Box<RawValue> {
    RawValue::from_string(json).expect("layers are written as JSON")
}

/// Claims as the JSON the analysis format has.
pub fn to_json(claims: &[Claim]) -> String {
    serde_json::to_string(claims).expect("claims are plain data")
}
