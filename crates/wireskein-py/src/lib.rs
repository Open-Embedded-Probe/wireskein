//! `wireskein_core`: WireSkein's Rust core for Python (PyO3, abi3 for CPython 3.11 and later).
//!
//! Built up beside the Python engine (bench/notes/2026-11-24-rust-plan.ja.md): the bench calls it as one engine of
//! its portfolio, and nothing in the product imports it until a part of it is made the default. The changes of each
//! line are read in place (a C-contiguous int64 array borrowed as a slice; another copied once); the claims come back
//! as the analysis format's JSON. Beside the engine, the parts around it: a capture file read (`read_file`), a recorded
//! run verified (`verify`), a capture file's analysis document (`analyze_file`), each switched on in the wireskein
//! package by WIRESKEIN_RUST (src/wireskein/_rust.py).

use numpy::{PyArray1, PyReadonlyArray1};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
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

/// readings(rate, n_samples, channels, hints_json="null") -> every reading the hints allow, before the claims are
/// chosen, as JSON ([{protocol, roles, score, params, evidence, layers}]): what a decode spec reads a bus from.
#[pyfunction]
#[pyo3(signature = (rate, n_samples, channels, hints_json="null"))]
fn readings(
    py: Python<'_>,
    rate: f64,
    n_samples: i64,
    channels: Vec<(String, u8, PyReadonlyArray1<'_, i64>)>,
    hints_json: &str,
) -> PyResult<String> {
    let hints: serde_json::Value =
        serde_json::from_str(hints_json).map_err(|e| PyValueError::new_err(format!("hints: {e}")))?;
    let edges: Vec<Vec<i64>> = channels.iter().map(|(_, _, a)| a.as_array().to_vec()).collect();
    let chans: Vec<Channel> = channels
        .iter()
        .zip(&edges)
        .map(|((name, initial, _), e)| Channel::new(name, *initial, e))
        .collect();
    let view = View::new(rate, n_samples, chans).map_err(PyValueError::new_err)?;
    let cands = py.detach(|| ws_core::readings(&view, &hints));
    let mut s = String::from("[");
    for (i, c) in cands.iter().enumerate() {
        let head = serde_json::json!({"protocol": c.protocol, "roles": c.roles, "score": c.score,
                                      "params": c.params, "evidence": c.evidence});
        let h = serde_json::to_string(&head).unwrap();
        s.push_str(if i > 0 { "," } else { "" });
        s.push_str(&h[..h.len() - 1]);
        s.push_str(",\"layers\":");
        s.push_str(c.layers.get());
        s.push('}');
    }
    s.push(']');
    Ok(s)
}

/// A JSON value as Python's json.loads gives it.
fn from_json<'py>(py: Python<'py>, v: &serde_json::Value) -> PyResult<Bound<'py, PyAny>> {
    py.import("json")?.getattr("loads")?.call1((v.to_string(),))
}

/// read_file(path, part=None, join=True) -> the capture as a dict.
///
/// A WireSkein file, a sigrok session (.sr) or a VCD, told apart by content (wireskein-io). join: a WireSkein file's
/// runs of timeline parts that follow on without a gap joined into one (as Python's `fileio.load_runs`); False: each
/// part on its own (`fileio.load_parts`); part: that part alone (`fileio.load`; the other parts' channels not read).
/// The file is read with the GIL released; the changes come back as numpy arrays, moved (not copied).
///
/// {"kind": "wireskein"|"sr"|"vcd", "tick_hz": (num, den), "id", "complete", "lost", "layout", "n_parts", "meta",
///  "parts": [{"part", "parts": (first, last)|None, "ticks", "tick0", "meta", "skipped",
///             "lines": [{"name", "initial", "edges": int64[], "step", "phase", "reduction"}],
///             "events": [{"name", "step", "phase", "n", "start_level", "quanta": int64[], "levels": uint8[],
///                         "counts": uint8[], "reduction"}],
///             "analog": [{"name", "encoding", "rate_hz": (num, den), "t0_ticks": (num, den), "width", "value_bits",
///                         "zero", "scale_nv", "unit", "values": float64[]}]}]}
#[pyfunction]
#[pyo3(signature = (path, part=None, join=true))]
fn read_file<'py>(
    py: Python<'py>,
    path: std::path::PathBuf,
    part: Option<usize>,
    join: bool,
) -> PyResult<Bound<'py, PyDict>> {
    let cap = py
        .detach(|| {
            if join && part.is_none() {
                wireskein_io::read(&path)
            } else {
                wireskein_io::read_parts(&path, part)
            }
        })
        .map_err(PyValueError::new_err)?;
    let kind = match cap.kind {
        wireskein_io::Kind::WireSkein => "wireskein",
        wireskein_io::Kind::Sr => "sr",
        wireskein_io::Kind::Vcd => "vcd",
    };
    let d = PyDict::new(py);
    d.set_item("kind", kind)?;
    d.set_item("tick_hz", cap.tick_hz)?;
    d.set_item("id", cap.id)?;
    d.set_item("complete", cap.complete)?;
    d.set_item("lost", from_json(py, &serde_json::Value::Array(cap.lost))?)?;
    d.set_item("layout", cap.layout)?;
    d.set_item("n_parts", cap.n_parts)?;
    d.set_item("meta", from_json(py, &serde_json::Value::Object(cap.meta))?)?;
    let parts = PyList::empty(py);
    for p in cap.parts {
        let pd = PyDict::new(py);
        pd.set_item("part", p.part)?;
        pd.set_item("parts", p.parts)?;
        pd.set_item("ticks", p.ticks)?;
        pd.set_item("tick0", p.tick0)?;
        pd.set_item("meta", from_json(py, &serde_json::Value::Object(p.meta))?)?;
        pd.set_item("skipped", from_json(py, &serde_json::Value::Array(p.skipped))?)?;
        let lines = PyList::empty(py);
        for l in p.lines {
            let ld = PyDict::new(py);
            ld.set_item("name", l.name)?;
            ld.set_item("initial", l.initial)?;
            ld.set_item("edges", PyArray1::from_vec(py, l.edges))?;
            ld.set_item("step", l.step)?;
            ld.set_item("phase", l.phase)?;
            ld.set_item("reduction", l.reduction)?;
            lines.append(ld)?;
        }
        pd.set_item("lines", lines)?;
        let events = PyList::empty(py);
        for e in p.events {
            let ed = PyDict::new(py);
            ed.set_item("name", e.name)?;
            ed.set_item("step", e.step)?;
            ed.set_item("phase", e.phase)?;
            ed.set_item("n", e.n)?;
            ed.set_item("start_level", e.start_level)?;
            ed.set_item("quanta", PyArray1::from_vec(py, e.quanta))?;
            ed.set_item("levels", PyArray1::from_vec(py, e.levels))?;
            ed.set_item("counts", PyArray1::from_vec(py, e.counts))?;
            ed.set_item("reduction", e.reduction)?;
            events.append(ed)?;
        }
        pd.set_item("events", events)?;
        let analog = PyList::empty(py);
        for a in p.analog {
            let ad = PyDict::new(py);
            ad.set_item("name", a.name)?;
            ad.set_item("encoding", a.encoding)?;
            ad.set_item("rate_hz", a.rate_hz)?;
            ad.set_item("t0_ticks", a.t0_ticks)?;
            ad.set_item("width", a.width)?;
            ad.set_item("value_bits", a.value_bits)?;
            ad.set_item("zero", a.zero)?;
            ad.set_item("scale_nv", a.scale_nv)?;
            ad.set_item("unit", a.unit)?;
            ad.set_item("values", PyArray1::from_vec(py, a.values))?;
            analog.append(ad)?;
        }
        pd.set_item("analog", analog)?;
        parts.append(pd)?;
    }
    d.set_item("parts", parts)?;
    Ok(d)
}

/// verify(run_dir) -> the report of a recorded run as JSON (wireskein-verify; the report `wireskein verify --json`
/// writes, its "log" kept), the GIL released while the captures are read and checked.
#[pyfunction]
fn verify(py: Python<'_>, run_dir: std::path::PathBuf) -> PyResult<String> {
    py.detach(|| wireskein_verify::verify(&run_dir).map(|r| r.dumps()))
        .map_err(PyValueError::new_err)
}

/// analyze_file(path, hints_json="null", part=None, mode="final", window=None, split=None) -> the analysis document
/// as JSON (wireskein-analysis: what `wireskein analyze` prints), the GIL released while the file is read and analyzed.
/// part: only the part (or the run of parts) holding it; mode: "final" or "all"; window: (from, to) in seconds, only
/// the items inside; split: a long part read in pieces, "key=value,..." (the defaults when None) or "off".
#[pyfunction]
#[pyo3(signature = (path, hints_json="null", part=None, mode="final", window=None, split=None))]
fn analyze_file(
    py: Python<'_>,
    path: std::path::PathBuf,
    hints_json: &str,
    part: Option<usize>,
    mode: &str,
    window: Option<(f64, f64)>,
    split: Option<&str>,
) -> PyResult<String> {
    let hints: serde_json::Value =
        serde_json::from_str(hints_json).map_err(|e| PyValueError::new_err(format!("hints: {e}")))?;
    if mode != "final" && mode != "all" {
        return Err(PyValueError::new_err(format!("mode {mode:?}: final or all")));
    }
    let opts = match split {
        None => Some(wireskein_analysis::split::Options::default()),
        Some(s) => wireskein_analysis::split::Options::parse(s).map_err(PyValueError::new_err)?,
    };
    let doc = py
        .detach(|| wireskein_analysis::analyze_file(&path, &hints, part, mode, window, opts.as_ref()))
        .map_err(PyValueError::new_err)?;
    Ok(doc.to_string())
}

#[pymodule]
fn wireskein_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(read_file, m)?)?;
    m.add_function(wrap_pyfunction!(verify, m)?)?;
    m.add_function(wrap_pyfunction!(analyze_file, m)?)?;
    m.add_function(wrap_pyfunction!(analyze, m)?)?;
    m.add_function(wrap_pyfunction!(analyze_with_relations, m)?)?;
    m.add_function(wrap_pyfunction!(candidates, m)?)?;
    m.add_function(wrap_pyfunction!(readings, m)?)?;
    Ok(())
}
