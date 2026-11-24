//! `wireskein_core`: WireSkein's Rust core for Python (PyO3, abi3 for CPython 3.11 and later).
//!
//! Built up beside the Python engine (bench/notes/2026-11-24-rust-plan.ja.md): the bench calls it as one engine of
//! its portfolio, and nothing in the product imports it until a part of it is made the default. The changes of each
//! line are read in place (a C-contiguous int64 array borrowed as a slice; another copied once); the claims come back
//! as the analysis format's JSON. Beside the engine, the parts around it: a capture file read (`read_file`), a recorded
//! run verified (`verify`), a capture file's analysis document (`analyze_file`), a line's states (`states_of_line`), a
//! capture's overview (`survey_file`), a capture written (`write_file`), a file's annotations (`annotations_file`),
//! a decode spec's document (`decode_file`), a part's lines a block at a time (`file_blocks`, `sr_blocks`), verify's
//! markers written (`mark`), each switched on in the wireskein package by WIRESKEIN_RUST (src/wireskein/_rust.py).

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
///             "lines": [{"name", "initial", "edges": int64[], "step", "phase", "reduction", "acquisition"}],
///             "events": [{"name", "step", "phase", "n", "start_level", "quanta": int64[], "levels": uint8[],
///                         "counts": uint8[], "reduction", "acquisition"}],
///             "analog": [{"name", "encoding", "rate_hz": (num, den), "t0_ticks": (num, den), "width", "value_bits",
///                         "zero", "scale_nv", "unit", "values": float64[], "acquisition"}]}]}
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
            ld.set_item("acquisition", from_json(py, &serde_json::Value::Object(l.acquisition))?)?;
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
            ed.set_item("acquisition", from_json(py, &serde_json::Value::Object(e.acquisition))?)?;
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
            ad.set_item("acquisition", from_json(py, &serde_json::Value::Object(a.acquisition))?)?;
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

/// analyze_file(path, hints_json="null", part=None, mode="final", window=None, split=None, pins=None,
/// thresholds_json=None, items=None, items_name=None, segment=None, markers_json=None, dialects_json=None,
/// events=False) -> the analysis document as JSON (wireskein-analysis: what `wireskein analyze` prints), the GIL
/// released while the file is read and analyzed. part: only the part (or the run of parts) holding it; mode: "final"
/// or "all"; window: (from, to) in seconds, only the items inside; split: a long part read in pieces,
/// "key=value,..." (the defaults when None) or "off"; pins: only these channels; thresholds_json: analog channels
/// also read as logic ({name: V | [LOW, HIGH]}); items: a part read in pieces writes its items here (ndjson; named
/// items_name in the document); segment: only the items inside the first marker segment matching this glob (the
/// marker source markers_json {"pin", "dialect"}, the dialects dialects_json, WireSkein's heading when None; events:
/// its text lines too).
#[pyfunction]
#[pyo3(signature = (path, hints_json="null", part=None, mode="final", window=None, split=None, pins=None,
                    thresholds_json=None, items=None, items_name=None, segment=None, markers_json=None,
                    dialects_json=None, events=false))]
#[allow(clippy::too_many_arguments)]
fn analyze_file(
    py: Python<'_>,
    path: std::path::PathBuf,
    hints_json: &str,
    part: Option<usize>,
    mode: &str,
    window: Option<(f64, f64)>,
    split: Option<&str>,
    pins: Option<Vec<String>>,
    thresholds_json: Option<&str>,
    items: Option<std::path::PathBuf>,
    items_name: Option<String>,
    segment: Option<String>,
    markers_json: Option<&str>,
    dialects_json: Option<&str>,
    events: bool,
) -> PyResult<String> {
    let json_of = |what: &str, t: &str| -> PyResult<serde_json::Value> {
        serde_json::from_str(t).map_err(|e| PyValueError::new_err(format!("{what}: {e}")))
    };
    let hints = json_of("hints", hints_json)?;
    if mode != "final" && mode != "all" {
        return Err(PyValueError::new_err(format!("mode {mode:?}: final or all")));
    }
    let opts = match split {
        None => Some(wireskein_analysis::split::Options::default()),
        Some(s) => wireskein_analysis::split::Options::parse(s).map_err(PyValueError::new_err)?,
    };
    let thresholds = match thresholds_json {
        Some(t) => {
            wireskein_analysis::analog::thresholds_of(&json_of("thresholds", t)?).map_err(PyValueError::new_err)?
        }
        None => vec![],
    };
    let markers = markers_json.map(|t| json_of("markers", t)).transpose()?;
    let dialects = dialects_json.map(|t| json_of("dialects", t)).transpose()?;
    let name = items_name
        .or_else(|| items.as_ref().map(|p| p.display().to_string()))
        .unwrap_or_default();
    let doc = py
        .detach(|| {
            let asked = wireskein_analysis::Asked {
                pins: pins.as_deref(),
                thresholds: &thresholds,
                items: items.as_deref().map(|p| (p, name.as_str())),
                segment: segment.as_deref(),
                markers: markers.as_ref(),
                dialects: dialects.as_ref(),
                events,
            };
            wireskein_analysis::analyze_file_asked(&path, &hints, part, mode, window, opts.as_ref(), &asked)
        })
        .map_err(PyValueError::new_err)?;
    Ok(doc.to_string())
}

/// A C-contiguous int64 array's values (borrowed), or a copy of another's.
fn i64s<'a>(a: &'a PyReadonlyArray1<'_, i64>) -> std::borrow::Cow<'a, [i64]> {
    match a.as_slice() {
        Ok(s) => std::borrow::Cow::Borrowed(s),
        Err(_) => std::borrow::Cow::Owned(a.as_array().to_vec()),
    }
}

/// states_of_line(edges, initial, n, rate, step=1, tol=0.02, min_periods=3, max_odd=3) -> the stretches of one line
/// as JSON (Python's `wireskein.states.of_line`, its keys in Python's order).
#[pyfunction]
#[pyo3(signature = (edges, initial, n, rate, step=1, tol=0.02, min_periods=3, max_odd=Some(3)))]
#[allow(clippy::too_many_arguments)]
fn states_of_line(
    py: Python<'_>,
    edges: PyReadonlyArray1<'_, i64>,
    initial: u8,
    n: i64,
    rate: f64,
    step: i64,
    tol: f64,
    min_periods: usize,
    max_odd: Option<usize>,
) -> String {
    let e = i64s(&edges);
    let o = wireskein_analysis::states::Options {
        tol,
        min_periods,
        max_odd,
    };
    py.detach(|| wireskein_analysis::states::of_line(&e, initial, n, rate, step, &o).dumps())
}

/// survey_file(path, pins=None, bins=1000, top=10, plan=None) -> the overview of a capture file as JSON (Python's
/// `wireskein.survey.survey_file` without its "wireskein"), read a block at a time where the file allows; plan: the
/// split options ("key=value,...", "" for the defaults) to also give the pieces it would be cut into.
#[pyfunction]
#[pyo3(signature = (path, pins=None, bins=1000, top=10, plan=None))]
fn survey_file(
    py: Python<'_>,
    path: std::path::PathBuf,
    pins: Option<Vec<String>>,
    bins: usize,
    top: usize,
    plan: Option<&str>,
) -> PyResult<String> {
    let opts = match plan {
        None => None,
        Some(s) => Some(
            wireskein_analysis::split::Options::parse(s)
                .map_err(PyValueError::new_err)?
                .ok_or_else(|| PyValueError::new_err("plan: split options, not off"))?,
        ),
    };
    py.detach(|| wireskein_analysis::survey::survey_file(&path, pins.as_deref(), bins, top, opts.as_ref()))
        .map(|j| j.dumps())
        .map_err(PyValueError::new_err)
}

/// annotations_file(path, hints_json="null", part=None, source=None) -> the annotations document of a capture file
/// as JSON (Python's `wireskein.annotate.build_file`: each part decoded with the Rust core).
#[pyfunction]
#[pyo3(signature = (path, hints_json="null", part=None, source=None))]
fn annotations_file(
    py: Python<'_>,
    path: std::path::PathBuf,
    hints_json: &str,
    part: Option<usize>,
    source: Option<String>,
) -> PyResult<String> {
    let hints: serde_json::Value =
        serde_json::from_str(hints_json).map_err(|e| PyValueError::new_err(format!("hints: {e}")))?;
    let source = source.unwrap_or_else(|| format!("wireskein-core {} analyze", env!("CARGO_PKG_VERSION")));
    py.detach(|| wireskein_analysis::annotate::annotations_file(&path, &hints, part, &source))
        .map(|j| j.dumps())
        .map_err(PyValueError::new_err)
}

type EventsArg<'py> = (
    String,
    PyReadonlyArray1<'py, i64>,
    PyReadonlyArray1<'py, u8>,
    PyReadonlyArray1<'py, u8>,
);

/// write_file(path, info_json, lines, events=[], analog=[], extras=[], form=None) -> writes a capture of one part
/// (wireskein-io's writers, the readers' pair): form "wireskein", "sr" or "vcd"; None: by the name, a sigrok session
/// (.sr), a VCD (.vcd) or else a WireSkein file.
/// info_json: {"tick_hz": [num, den], "ticks", "id", "lost", "meta",
///             "lines": [{"step", "phase", "reduction", "acquisition"}],
///             "events": [{"step", "phase", "n", "start_level", "reduction", "acquisition"}],
///             "analog": [{"encoding", "rate_hz", "t0_ticks", "width", "value_bits", "zero", "scale_nv", "unit",
///                         "acquisition"}]}
/// lines: [(name, initial, edges)], events: [(name, quanta, levels, counts)], analog: [(name, values as float64)],
/// extras: [(entry, bytes)] carried as they are. The file is written with the GIL released.
#[pyfunction]
#[pyo3(signature = (path, info_json, lines, events=vec![], analog=vec![], extras=vec![], form=None))]
#[allow(clippy::too_many_arguments)]
fn write_file(
    py: Python<'_>,
    path: std::path::PathBuf,
    info_json: &str,
    lines: Vec<(String, u8, PyReadonlyArray1<'_, i64>)>,
    events: Vec<EventsArg<'_>>,
    analog: Vec<(String, PyReadonlyArray1<'_, f64>)>,
    extras: Vec<(String, Vec<u8>)>,
    form: Option<&str>,
) -> PyResult<()> {
    use serde_json::Value;
    let info: Value = serde_json::from_str(info_json).map_err(|e| PyValueError::new_err(format!("info: {e}")))?;
    let bad = |what: &str| PyValueError::new_err(format!("info: {what}"));
    let pair = |v: &Value| -> Option<(i64, i64)> { Some((v.get(0)?.as_i64()?, v.get(1)?.as_i64()?)) };
    let obj = |v: &Value| v.as_object().cloned().unwrap_or_default();
    let int = |v: &Value, k: &str| v[k].as_i64().ok_or_else(|| bad(k));
    let mut ls = vec![];
    for (k, (name, initial, edges)) in lines.iter().enumerate() {
        let i = &info["lines"][k];
        ls.push(wireskein_io::Line {
            name: name.clone(),
            initial: *initial,
            edges: i64s(edges).into_owned(),
            step: int(i, "step")?,
            phase: int(i, "phase")?,
            reduction: i["reduction"].as_str().map(String::from),
            acquisition: obj(&i["acquisition"]),
        });
    }
    let mut es = vec![];
    for (k, (name, quanta, levels, counts)) in events.iter().enumerate() {
        let i = &info["events"][k];
        es.push(wireskein_io::Events {
            name: name.clone(),
            step: int(i, "step")?,
            phase: int(i, "phase")?,
            n: int(i, "n")?,
            start_level: int(i, "start_level")? as u8,
            quanta: i64s(quanta).into_owned(),
            levels: levels.as_array().to_vec(),
            counts: counts.as_array().to_vec(),
            reduction: i["reduction"].as_str().map(String::from),
            acquisition: obj(&i["acquisition"]),
        });
    }
    let mut an = vec![];
    for (k, (name, values)) in analog.iter().enumerate() {
        let i = &info["analog"][k];
        an.push(wireskein_io::Analog {
            name: name.clone(),
            encoding: i["encoding"].as_str().unwrap_or("analog").into(),
            rate_hz: pair(&i["rate_hz"]).ok_or_else(|| bad("rate_hz"))?,
            t0_ticks: pair(&i["t0_ticks"]).unwrap_or((0, 1)),
            width: i["width"].as_i64().unwrap_or(16),
            value_bits: i["value_bits"].as_i64(),
            zero: i["zero"].as_f64(),
            scale_nv: i["scale_nv"].as_f64(),
            unit: i["unit"].as_str().unwrap_or("V").into(),
            values: values.as_array().to_vec(),
            acquisition: obj(&i["acquisition"]),
        });
    }
    let ticks = int(&info, "ticks")?;
    let cap = wireskein_io::Capture {
        kind: wireskein_io::Kind::WireSkein,
        tick_hz: pair(&info["tick_hz"]).ok_or_else(|| bad("tick_hz"))?,
        id: info["id"].as_str().map(String::from),
        complete: true,
        lost: info["lost"].as_array().cloned().unwrap_or_default(),
        layout: "timeline".into(),
        n_parts: 1,
        meta: obj(&info["meta"]),
        parts: vec![wireskein_io::Part {
            part: 0,
            parts: None,
            ticks,
            tick0: Some(0),
            meta: Default::default(),
            lines: ls,
            events: es,
            analog: an,
            skipped: vec![],
        }],
    };
    py.detach(|| match form {
        None => wireskein_io::write::write(&path, &cap, &extras),
        Some("wireskein") => wireskein_io::write::write_wireskein(&path, &cap, &extras),
        Some("sr") => wireskein_io::write::write_sr(&path, &cap, &extras),
        Some("vcd") => wireskein_io::write::write_vcd(&path, &cap),
        Some(f) => Err(format!("form {f:?}: wireskein, sr or vcd")),
    })
    .map_err(PyValueError::new_err)
}

/// A part's logic lines read a block of ticks at a time (wireskein-io's stream: Python's `split.Source`): `names`,
/// `n` (ticks), `initial` (each line's level before tick 0), `rate` (a .sr's samples a second, else None); iterated,
/// (the tick read up to, [each line's changes in the block as an int64 array]).
#[pyclass]
struct Blocks {
    src: std::sync::Mutex<Box<dyn wireskein_io::stream::Source + Send>>,
    #[pyo3(get)]
    names: Vec<String>,
    #[pyo3(get)]
    initial: Vec<u8>,
    #[pyo3(get)]
    n: i64,
    #[pyo3(get)]
    rate: Option<f64>,
}

impl Blocks {
    fn of(src: Box<dyn wireskein_io::stream::Source + Send>, rate: Option<f64>) -> Blocks {
        Blocks {
            names: src.names().to_vec(),
            initial: src.initial().to_vec(),
            n: src.n(),
            rate,
            src: std::sync::Mutex::new(src),
        }
    }
}

#[pymethods]
impl Blocks {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[allow(clippy::type_complexity)]
    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Option<(i64, Vec<Bound<'py, PyArray1<i64>>>)>> {
        let got = py.detach(|| self.src.lock().map(|mut s| s.next_block()).map_err(|_| ()));
        let got = got.map_err(|_| PyValueError::new_err("a source read after a failure"))?;
        Ok(got.map(|(t1, es)| (t1, es.into_iter().map(|e| PyArray1::from_vec(py, e)).collect())))
    }
}

/// file_blocks(path, part=0, pins=None) -> Blocks, or None where the part's lines are not all bits / events at one
/// tick a sample (read whole then).
#[pyfunction]
#[pyo3(signature = (path, part=0, pins=None))]
fn file_blocks(path: std::path::PathBuf, part: usize, pins: Option<Vec<String>>) -> PyResult<Option<Blocks>> {
    let src = wireskein_io::stream::file_source_of(&path, part, pins.as_deref()).map_err(PyValueError::new_err)?;
    Ok(src.map(|s| Blocks::of(Box::new(s), None)))
}

/// sr_blocks(path, pins=None) -> Blocks of a logic-only sigrok session, or None (analog channels, lines of their own
/// step: read whole then).
#[pyfunction]
#[pyo3(signature = (path, pins=None))]
fn sr_blocks(path: std::path::PathBuf, pins: Option<Vec<String>>) -> PyResult<Option<Blocks>> {
    let src = wireskein_io::stream::sr_source(&path, pins.as_deref()).map_err(PyValueError::new_err)?;
    Ok(src.map(|s| {
        let rate = s.rate;
        Blocks::of(Box::new(s), Some(rate))
    }))
}

/// mark(run_dir, report_json) -> None: what a verify report found written into each capture file's markers (by
/// "verify", its earlier ones replaced; `wireskein verify --mark`).
#[pyfunction]
fn mark(py: Python<'_>, run_dir: std::path::PathBuf, report_json: &str) -> PyResult<()> {
    let rep = wireskein_verify::J::parse(report_json).map_err(PyValueError::new_err)?;
    py.detach(|| wireskein_verify::mark(&run_dir, &rep))
        .map_err(PyValueError::new_err)
}

/// decode_file(path, spec_json, split=None, pins=None, items=None, items_name=None) -> the decode document as JSON
/// (Python's `decode.decode_file`: every part decoded with the spec's roles by the Rust core). split: "key=value,..."
/// or "off" (None: the defaults); items: the file the decoded items are written to (ndjson), named in the document
/// as items_name.
#[pyfunction]
#[pyo3(signature = (path, spec_json, split=None, pins=None, items=None, items_name=None))]
fn decode_file(
    py: Python<'_>,
    path: std::path::PathBuf,
    spec_json: &str,
    split: Option<&str>,
    pins: Option<Vec<String>>,
    items: Option<std::path::PathBuf>,
    items_name: Option<String>,
) -> PyResult<String> {
    let spec = wireskein_analysis::decode::Spec::parse(spec_json).map_err(PyValueError::new_err)?;
    let opts = match split {
        None => Some(wireskein_analysis::split::Options::default()),
        Some(s) => wireskein_analysis::split::Options::parse(s).map_err(PyValueError::new_err)?,
    };
    let name = items_name
        .or_else(|| items.as_ref().map(|p| p.display().to_string()))
        .unwrap_or_default();
    let doc = py
        .detach(|| {
            wireskein_analysis::decode::decode_file(
                &path,
                &spec,
                opts.as_ref(),
                pins.as_deref(),
                items.as_deref().map(|p| (p, name.as_str())),
            )
        })
        .map_err(PyValueError::new_err)?;
    Ok(doc.to_string())
}

/// An analog channel as Python gives it: (name, encoding, values as float64 (raw integers or volts), rate_hz as
/// (num, den), t0_ticks as (num, den), width, value_bits, zero, scale_nv).
type AnalogArgs<'py> = (
    String,
    String,
    PyReadonlyArray1<'py, f64>,
    (i64, i64),
    (i64, i64),
    i64,
    Option<i64>,
    Option<f64>,
    Option<f64>,
);

fn analog_of(a: &AnalogArgs) -> wireskein_io::Analog {
    wireskein_io::Analog {
        name: a.0.clone(),
        encoding: a.1.clone(),
        values: a.2.as_array().to_vec(),
        rate_hz: a.3,
        t0_ticks: a.4,
        width: a.5,
        value_bits: a.6,
        zero: a.7,
        scale_nv: a.8,
        unit: "V".into(),
        acquisition: Default::default(),
    }
}

/// align_find(ref_edges, ref_initial, ref_step, reference, analog, tick_hz, low, high, window_ticks=None,
/// uncertainty_ticks=None, n_samples=0, max_ppm=5000) -> the alignment of the analog channel against the logic
/// channel `reference` as JSON (Python's `wireskein.align.find`, its entry for a channel; an infinite uncertainty as
/// the string "Infinity"). analog: see AnalogArgs; tick_hz: the capture's (num, den).
#[pyfunction]
#[pyo3(signature = (ref_edges, ref_initial, ref_step, reference, analog, tick_hz, low, high, window_ticks=None,
                    uncertainty_ticks=None, n_samples=0, max_ppm=5000.0))]
#[allow(clippy::too_many_arguments)]
fn align_find(
    py: Python<'_>,
    ref_edges: PyReadonlyArray1<'_, i64>,
    ref_initial: u8,
    ref_step: i64,
    reference: &str,
    analog: AnalogArgs<'_>,
    tick_hz: (i64, i64),
    low: f64,
    high: f64,
    window_ticks: Option<f64>,
    uncertainty_ticks: Option<f64>,
    n_samples: i64,
    max_ppm: f64,
) -> PyResult<String> {
    let rf = wireskein_analysis::align::logic_edges(&i64s(&ref_edges), ref_initial, ref_step);
    let a = analog_of(&analog);
    py.detach(|| {
        wireskein_analysis::align::find(
            &rf,
            reference,
            &a,
            tick_hz,
            low,
            high,
            window_ticks,
            uncertainty_ticks,
            n_samples,
            max_ppm,
        )
    })
    .map(|e| serde_json::Value::Object(e).to_string())
    .map_err(PyValueError::new_err)
}

/// align_between(ref_edges, ref_initial, ref_step, reference, via, tick_ref, tick, ref_n, logic=None, analog=None,
/// tick_hz=None, threshold=None, window_ticks=None, max_ppm=200) -> how this capture's ticks map onto the
/// reference capture's as JSON (Python's `wireskein.align.between`). via: the channel of this capture, logic
/// (edges, initial, step) or analog (AnalogArgs, read at threshold (low, high) on this capture's tick_hz (num, den)).
#[pyfunction]
#[pyo3(signature = (ref_edges, ref_initial, ref_step, reference, via, tick_ref, tick, ref_n, logic=None, analog=None,
                    tick_hz=None, threshold=None, window_ticks=None, max_ppm=200.0))]
#[allow(clippy::too_many_arguments)]
fn align_between(
    py: Python<'_>,
    ref_edges: PyReadonlyArray1<'_, i64>,
    ref_initial: u8,
    ref_step: i64,
    reference: &str,
    via: &str,
    tick_ref: f64,
    tick: f64,
    ref_n: i64,
    logic: Option<(PyReadonlyArray1<'_, i64>, u8, i64)>,
    analog: Option<AnalogArgs<'_>>,
    tick_hz: Option<(i64, i64)>,
    threshold: Option<(f64, f64)>,
    window_ticks: Option<f64>,
    max_ppm: f64,
) -> PyResult<String> {
    use wireskein_analysis::align;
    let rf = align::logic_edges(&i64s(&ref_edges), ref_initial, ref_step);
    let t = match (&logic, &analog) {
        (Some((e, i, s)), _) => align::logic_edges(&i64s(e), *i, *s),
        (None, Some(a)) => {
            let (Some(th), Some(tk)) = (threshold, tick_hz) else {
                return Err(PyValueError::new_err(format!("{via} is analog: give a threshold")));
            };
            align::analog_edges(&analog_of(a), tk, th.0, th.1).map_err(PyValueError::new_err)?
        }
        _ => return Err(PyValueError::new_err(format!("{via}: no such channel"))),
    };
    let th = if analog.is_some() { threshold } else { None };
    py.detach(|| {
        align::between(
            &rf,
            reference,
            &t,
            via,
            tick_ref,
            tick,
            ref_n,
            window_ticks,
            max_ppm,
            th,
        )
    })
    .map(|e| serde_json::Value::Object(e).to_string())
    .map_err(PyValueError::new_err)
}

#[pymodule]
fn wireskein_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(read_file, m)?)?;
    m.add_function(wrap_pyfunction!(verify, m)?)?;
    m.add_function(wrap_pyfunction!(analyze_file, m)?)?;
    m.add_function(wrap_pyfunction!(align_find, m)?)?;
    m.add_function(wrap_pyfunction!(align_between, m)?)?;
    m.add_function(wrap_pyfunction!(decode_file, m)?)?;
    m.add_function(wrap_pyfunction!(mark, m)?)?;
    m.add_class::<Blocks>()?;
    m.add_function(wrap_pyfunction!(file_blocks, m)?)?;
    m.add_function(wrap_pyfunction!(sr_blocks, m)?)?;
    m.add_function(wrap_pyfunction!(states_of_line, m)?)?;
    m.add_function(wrap_pyfunction!(survey_file, m)?)?;
    m.add_function(wrap_pyfunction!(annotations_file, m)?)?;
    m.add_function(wrap_pyfunction!(write_file, m)?)?;
    m.add_function(wrap_pyfunction!(analyze, m)?)?;
    m.add_function(wrap_pyfunction!(analyze_with_relations, m)?)?;
    m.add_function(wrap_pyfunction!(candidates, m)?)?;
    m.add_function(wrap_pyfunction!(readings, m)?)?;
    Ok(())
}
