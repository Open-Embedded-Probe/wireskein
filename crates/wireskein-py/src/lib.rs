//! `wireskein_core`: WireSkein's Rust core for Python (PyO3, abi3 for CPython 3.11 and later).
//!
//! Built up beside the Python engine (bench/notes/2026-11-24-rust-plan.ja.md): the bench calls it as one engine of
//! its portfolio, and nothing in the product imports it until a part of it is made the default. The changes of each
//! line are read in place (a C-contiguous int64 array borrowed as a slice; another copied once); the claims come back
//! as the analysis format's JSON.

use numpy::PyReadonlyArray1;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use ws_core::{Channel, View};

/// analyze(rate, n_samples, channels, hints_json="null") -> claims as JSON.
/// channels: [(name, initial, edges as an int64 array)], hints_json: the hints as JSON (as wireskein's analyze takes).
/// upper: the Rust upper stage after the engine's choice (false: the engine's claims alone, for an upper stage of the
/// caller's own).
#[pyfunction]
#[pyo3(signature = (rate, n_samples, channels, hints_json="null", upper=true))]
fn analyze(
    py: Python<'_>,
    rate: f64,
    n_samples: i64,
    channels: Vec<(String, u8, PyReadonlyArray1<'_, i64>)>,
    hints_json: &str,
    upper: bool,
) -> PyResult<String> {
    let hints: serde_json::Value =
        serde_json::from_str(hints_json).map_err(|e| PyValueError::new_err(format!("hints: {e}")))?;
    let copies: Vec<Option<Vec<i64>>> = channels
        .iter()
        .map(|(_, _, a)| {
            if a.as_slice().is_ok() {
                None
            } else {
                Some(a.as_array().to_vec())
            }
        })
        .collect();
    let chans: Vec<Channel> = channels
        .iter()
        .zip(&copies)
        .map(|((name, initial, a), copy)| {
            Channel::new(
                name,
                *initial,
                copy.as_deref().unwrap_or_else(|| a.as_slice().expect("contiguous")),
            )
        })
        .collect();
    let view = View::new(rate, n_samples, chans).map_err(PyValueError::new_err)?;
    let claims = py.detach(|| {
        if upper {
            ws_core::analyze(&view, &hints)
        } else {
            ws_core::analyze_engine(&view, &hints)
        }
    });
    Ok(ws_core::claim::to_json(&claims))
}

/// analyze_with_relations(rate, n_samples, channels, hints_json) -> {"claims": [...], "relations": [...]} as JSON.
#[pyfunction]
#[pyo3(signature = (rate, n_samples, channels, hints_json="null"))]
fn analyze_with_relations(
    py: Python<'_>,
    rate: f64,
    n_samples: i64,
    channels: Vec<(String, u8, PyReadonlyArray1<'_, i64>)>,
    hints_json: &str,
) -> PyResult<String> {
    let hints: serde_json::Value =
        serde_json::from_str(hints_json).map_err(|e| PyValueError::new_err(format!("hints: {e}")))?;
    let copies: Vec<Option<Vec<i64>>> = channels
        .iter()
        .map(|(_, _, a)| {
            if a.as_slice().is_ok() {
                None
            } else {
                Some(a.as_array().to_vec())
            }
        })
        .collect();
    let chans: Vec<Channel> = channels
        .iter()
        .zip(&copies)
        .map(|((name, initial, a), copy)| {
            Channel::new(
                name,
                *initial,
                copy.as_deref().unwrap_or_else(|| a.as_slice().expect("contiguous")),
            )
        })
        .collect();
    let view = View::new(rate, n_samples, chans).map_err(PyValueError::new_err)?;
    let (claims, rel) = py.detach(|| ws_core::analyze_full(&view, &hints));
    Ok(format!(
        r#"{{"claims":{},"relations":{}}}"#,
        ws_core::claim::to_json(&claims),
        serde_json::Value::Array(rel)
    ))
}

/// candidates(rate, n_samples, channels) -> every reading before the claims are chosen, as JSON (for development:
/// [{protocol, roles, score, alt, evidence}]).
#[pyfunction]
fn candidates(
    py: Python<'_>,
    rate: f64,
    n_samples: i64,
    channels: Vec<(String, u8, PyReadonlyArray1<'_, i64>)>,
) -> PyResult<String> {
    let edges: Vec<Vec<i64>> = channels.iter().map(|(_, _, a)| a.as_array().to_vec()).collect();
    let chans: Vec<Channel> = channels
        .iter()
        .zip(&edges)
        .map(|((name, initial, _), e)| Channel::new(name, *initial, e))
        .collect();
    let view = View::new(rate, n_samples, chans).map_err(PyValueError::new_err)?;
    let cands = py.detach(|| ws_core::candidates(&view));
    let out: Vec<serde_json::Value> = cands
        .iter()
        .map(|c| {
            serde_json::json!({"protocol": c.protocol, "roles": c.roles, "score": c.score, "alt": c.alt,
                               "evidence": c.evidence})
        })
        .collect();
    Ok(serde_json::to_string(&out).unwrap())
}

#[pymodule]
fn wireskein_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(analyze, m)?)?;
    m.add_function(wrap_pyfunction!(analyze_with_relations, m)?)?;
    m.add_function(wrap_pyfunction!(candidates, m)?)?;
    Ok(())
}
