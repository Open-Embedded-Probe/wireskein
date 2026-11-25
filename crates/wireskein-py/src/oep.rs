//! Capturing for Python: `capture_source` runs one capture from a source `<kind>:<address>` by its kind's driver
//! (wireskein-sources: `oep`, `demo`), `oep_capture` one from an OEP probe as oep-client-python's `acquire.capture_at`
//! does, `source_list` the devices that can be seen, `oep_assemble` puts a stream's pushes together (for a test that
//! gives the frames).

use pyo3::exceptions::{
    PyConnectionError, PyFileNotFoundError, PyOSError, PyRuntimeError, PyTimeoutError, PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde_json::Value;
use wireskein_capture::{Error, Mode, Request};

/// A data push as a test gives it: (position, data, (segment position, samples, start_ns, uncertainty) or None).
type Push = (u64, Vec<u8>, Option<(u64, u64, u64, u64)>);

fn py_err(e: Error) -> PyErr {
    match e {
        Error::Value(m) => PyValueError::new_err(m),
        Error::NotFound(m) => PyFileNotFoundError::new_err(m),
        Error::Timeout(m) => PyTimeoutError::new_err(m),
        Error::Io(m) => PyOSError::new_err(m),
        Error::Connection(m) => PyConnectionError::new_err(m),
        e => PyRuntimeError::new_err(e.to_string()),
    }
}

fn bad(what: &str) -> PyErr {
    PyValueError::new_err(format!("request: {what}"))
}

/// wireskein.sources.Request (as JSON) -> wireskein_capture::Request.
fn request(v: &Value) -> PyResult<Request> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    let u = |k: &str| v.get(k).and_then(|x| x.as_u64());
    let f = |k: &str| v.get(k).and_then(|x| x.as_f64());
    let pair = |x: &Value| -> Option<(String, String)> {
        let a = x.as_array()?;
        Some((a.first()?.as_str()?.to_string(), a.get(1)?.as_str()?.to_string()))
    };
    let channels = v
        .get("channels")
        .and_then(|c| c.as_array())
        .ok_or_else(|| bad("channels"))?
        .iter()
        .map(|c| pair(c).ok_or_else(|| bad("channels: [name, id]")))
        .collect::<PyResult<Vec<_>>>()?;
    let mut r = Request::new(
        channels,
        u("rate_hz").ok_or_else(|| bad("rate_hz"))?,
        u("samples").unwrap_or(0),
    );
    r.trigger = v
        .get("trigger")
        .filter(|t| !t.is_null())
        .map(|t| pair(t).ok_or_else(|| bad("trigger")))
        .transpose()?;
    // an analog channel's trigger: (name, kind, volts)
    r.trigger_volts = v
        .get("trigger")
        .and_then(|t| t.as_array())
        .and_then(|a| a.get(2))
        .and_then(|x| x.as_f64());
    if let Some(list) = v.get("analog").and_then(|x| x.as_array()) {
        for a in list {
            let a = a.as_array().ok_or_else(|| bad("analog: [name, id, frontend]"))?;
            let name = a.first().and_then(|x| x.as_str()).ok_or_else(|| bad("analog: name"))?;
            let id = a.get(1).and_then(|x| x.as_str()).ok_or_else(|| bad("analog: id"))?;
            let fe = a.get(2).and_then(|x| x.as_u64()).map(|x| x as u8);
            r.analog.push((name.to_string(), id.to_string(), fe));
        }
    }
    r.analog_rate_hz = u("analog_rate_hz");
    r.analog_samples = u("analog_samples");
    r.pretrigger = u("pretrigger");
    r.arm_now = s("arm") == Some("now");
    r.timeout_s = f("timeout_s").unwrap_or(10.0);
    if let Some(res) = v.get("resolution").and_then(|x| x.as_object()) {
        for (name, how) in res {
            let a = how.as_array().ok_or_else(|| bad("resolution"))?;
            let mode = a.first().and_then(|m| m.as_str()).ok_or_else(|| bad("resolution"))?;
            let d = a.get(1).and_then(|d| d.as_u64()).ok_or_else(|| bad("resolution"))?;
            r.resolution.push((name.clone(), (mode.to_string(), d)));
        }
    }
    r.mode = Mode::parse(s("mode").unwrap_or("oneshot")).map_err(py_err)?;
    r.duration_s = f("duration_s");
    r.segments = u("segments");
    r.on_loss_stop = s("on_loss").unwrap_or("stop") == "stop";
    r.spool_dir = s("spool_dir").map(Into::into);
    r.glitch_filter_ns = u("glitch_filter_ns").map(|x| x as u32);
    r.threshold_v = f("threshold_v");
    Ok(r)
}

fn out<'py>(py: Python<'py>, a: &wireskein_capture::Acquired) -> (String, Vec<Bound<'py, PyBytes>>) {
    let (doc, datas) = wireskein_capture::to_doc(a);
    (doc.to_string(), datas.iter().map(|d| PyBytes::new(py, d)).collect())
}

/// oep_capture(address, request_json, owner) -> (doc_json, [bytes]): one capture from the probe at `address`
/// (oep://UNIT_ID[?via=usb|bulk|tcp:HOST:PORT] or tcp://HOST:PORT), its lease taken as `owner`. The doc: tick_hz,
/// meta, one_part, samples, layout, lost, temp_dir and the parts, each channel's data an index into the list or a
/// file's path (a stream's, under temp_dir: the caller removes it once written).
#[pyfunction]
pub fn oep_capture<'py>(
    py: Python<'py>,
    address: &str,
    request_json: &str,
    owner: &str,
) -> PyResult<(String, Vec<Bound<'py, PyBytes>>)> {
    let v: Value = serde_json::from_str(request_json).map_err(|e| bad(&e.to_string()))?;
    let req = request(&v)?;
    let got = py
        .detach(|| wireskein_oep::capture_at(address, &req, owner))
        .map_err(|e| py_err(e.into()))?;
    Ok(out(py, &got))
}

/// capture_source(source, request_json, owner) -> (doc_json, [bytes]): one capture from `source` (`<kind>:<address>`,
/// the kind choosing the driver: oep, demo), held as `owner`; the doc as oep_capture's.
#[pyfunction]
pub fn capture_source<'py>(
    py: Python<'py>,
    source: &str,
    request_json: &str,
    owner: &str,
) -> PyResult<(String, Vec<Bound<'py, PyBytes>>)> {
    let v: Value = serde_json::from_str(request_json).map_err(|e| bad(&e.to_string()))?;
    let req = request(&v)?;
    let got = py
        .detach(|| wireskein_sources::capture(source, &req, owner, &wireskein_capture::Ctl::new()))
        .map_err(py_err)?;
    Ok(out(py, &got))
}

/// source_kinds() -> [kind]: the kinds of device known (each a driver).
#[pyfunction]
pub fn source_kinds() -> Vec<String> {
    wireskein_sources::builtin()
        .kinds()
        .iter()
        .map(|k| k.to_string())
        .collect()
}

/// source_list(kind=None) -> JSON: {"found": [{source, label, facts}], "failed": [[kind, why]]}, the devices that
/// can be seen now.
#[pyfunction]
#[pyo3(signature = (kind=None))]
pub fn source_list(py: Python<'_>, kind: Option<&str>) -> PyResult<String> {
    let (found, failed) = py.detach(|| wireskein_sources::builtin().list(kind)).map_err(py_err)?;
    let doc = serde_json::json!({
        "found": found.iter().map(|f| f.to_json()).collect::<Vec<_>>(),
        "failed": failed.iter().map(|(k, e)| serde_json::json!([k, e.to_string()])).collect::<Vec<_>>(),
    });
    Ok(doc.to_string())
}

/// oep_assemble(config_json, pushes, write_pos=None) -> (doc_json, [bytes]): a stream's data pushes put together as
/// oep_capture does - config: names, ids, rate [num, den], format 0 (width, positions) or 1 (also block, raw_roles,
/// reduced [[role, carriage, D]]), on_loss; pushes: (position, data, (segment position, samples, start_ns,
/// uncertainty) or None) in the order they came; write_pos: an error stop's.
#[pyfunction]
#[pyo3(signature = (config_json, pushes, write_pos=None))]
pub fn oep_assemble<'py>(
    py: Python<'py>,
    config_json: &str,
    pushes: Vec<Push>,
    write_pos: Option<u64>,
) -> PyResult<(String, Vec<Bound<'py, PyBytes>>)> {
    use wireskein_capture::spool::Format;
    use wireskein_oep::blocks::Blocks;
    let c: Value = serde_json::from_str(config_json).map_err(|e| bad(&e.to_string()))?;
    let list = |k: &str| -> Vec<u64> {
        c.get(k)
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
            .unwrap_or_default()
    };
    let names: Vec<String> = c
        .get("names")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let ids: Vec<u16> = list("ids").into_iter().map(|x| x as u16).collect();
    let rate = list("rate");
    let rate = (*rate.first().unwrap_or(&1), *rate.get(1).unwrap_or(&1));
    let width = c.get("width").and_then(|x| x.as_u64()).unwrap_or(8) as usize;
    let positions: Vec<usize> = list("positions").into_iter().map(|x| x as usize).collect();
    let format = if c.get("format").and_then(|x| x.as_u64()) == Some(1) {
        let reduced = c
            .get("reduced")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        let r = r.as_array()?;
                        Some((
                            r.first()?.as_u64()? as usize,
                            (r.get(1)?.as_u64()? as u8, r.get(2)?.as_u64()? as usize),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Format::Blocks(std::sync::Arc::new(Blocks {
            raw_roles: list("raw_roles").into_iter().map(|x| x as usize).collect(),
            reduced,
            block: c.get("block").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
            width,
            positions,
        }))
    } else {
        Format::Packed { width, positions }
    };
    let stop = c.get("on_loss").and_then(|x| x.as_str()).unwrap_or("stop") == "stop";
    let pushes: Vec<_> = pushes
        .into_iter()
        .map(|(p, d, i)| {
            (
                p,
                d,
                i.map(
                    |(position, samples, start_ns, unc)| wireskein_capture::spool::SegmentInfo {
                        position,
                        samples,
                        start_ns,
                        start_uncertainty_ns: unc,
                    },
                ),
            )
        })
        .collect();
    let (parts, lost) = py
        .detach(|| wireskein_capture::save::assemble_pushes(names, ids, rate, format, stop, &pushes, write_pos))
        .map_err(py_err)?;
    let channels = parts.first().map(|p| p.channels.clone()).unwrap_or_default();
    let a = wireskein_capture::Acquired {
        tick_hz: rate,
        channels,
        meta: Default::default(),
        parts: Some(parts),
        lost,
        layout: "timeline".into(),
        temp_dir: None,
        samples: None,
    };
    Ok(out(py, &a))
}
