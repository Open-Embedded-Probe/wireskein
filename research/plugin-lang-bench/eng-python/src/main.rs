//! Embedded CPython via pyo3. Samples and kernel outputs are exposed without copying:
//! a #[pyclass] `Buf` implements the buffer protocol over memory it owns (an Arc clone of
//! the mmap, or the moved kernel Vec), and Python gets `memoryview(buf).cast('H'|'B'|'I')`.
//! The memoryview's Py_buffer.obj references the Buf, so the memory lives as long as Python
//! holds any view of it.
use std::ffi::{c_int, CString};
use std::time::Instant;

use common::*;
use pyo3::exceptions::PyBufferError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyMemoryView, PyTuple};

#[pyclass(frozen)]
struct Buf {
    _keep: Box<dyn Send + Sync>,
    ptr: usize,
    len: usize,
}

#[pymethods]
impl Buf {
    unsafe fn __getbuffer__(slf: Bound<'_, Self>, view: *mut pyo3::ffi::Py_buffer, flags: c_int) -> PyResult<()> {
        if flags & pyo3::ffi::PyBUF_WRITABLE != 0 {
            return Err(PyBufferError::new_err("read-only buffer"));
        }
        let (ptr, len) = (slf.get().ptr, slf.get().len);
        // Sets view->obj = slf (new reference) -> keeps the owner alive.
        if pyo3::ffi::PyBuffer_FillInfo(view, slf.as_ptr(), ptr as *mut _, len as _, 1, flags) != 0 {
            return Err(PyErr::fetch(slf.py()));
        }
        Ok(())
    }
    unsafe fn __releasebuffer__(&self, _view: *mut pyo3::ffi::Py_buffer) {}
}

fn view<'py>(py: Python<'py>, keep: Box<dyn Send + Sync>, ptr: *const u8, len: usize, fmt: &str) -> Bound<'py, PyAny> {
    let b = Bound::new(py, Buf { _keep: keep, ptr: ptr as usize, len }).unwrap();
    PyMemoryView::from(&b.into_any()).unwrap().call_method1("cast", (fmt,)).unwrap()
}

#[pyfunction]
fn host_inc(x: i64) -> i64 {
    x + 1
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn fmt_err(py: Python<'_>, e: &PyErr) -> String {
    let tb = py.import("traceback").unwrap();
    let lines = tb.call_method1("format_exception", (e.value(py),)).unwrap();
    let parts: Vec<String> = lines.extract().unwrap();
    parts.concat()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cap = load(&a[1], &a[2]);
    let dir = &a[3];
    let mut r = Report::new("python", &cap);
    let mut notes = vec![];
    Python::attach(|py| {
        let path = format!("{dir}/python/plugin.py");
        let code = std::fs::read_to_string(&path).unwrap();
        let m = match PyModule::from_code(py, &cstr(&code), &cstr(&path), c"plugin") {
            Ok(m) => m,
            Err(e) => panic!("{}", fmt_err(py, &e)),
        };
        let t = Instant::now();
        let samples = view(py, Box::new(cap.samples.clone()), cap.samples.as_ptr(), cap.samples.len(), "H");
        r.text("t1_method", "memoryview(#[pyclass] buffer-protocol owner of Arc<Mmap>).cast('H') (zero-copy, read-only)");
        r.num("t1_zero_copy", 1.0);
        r.num("t1_ms", ms(t));
        let t = Instant::now();
        let copy = PyBytes::new(py, &cap.samples[..]);
        let copy_view = PyMemoryView::from(copy.as_any()).unwrap().call_method1("cast", ("H",)).unwrap();
        r.num("t1_copy_ms", ms(t));
        drop(copy_view);
        let ch = cap.busiest();
        let n = cap.n.min(2_000_000);
        let t = Instant::now();
        let list = PyList::new(py, &cap.samples()[..n]).unwrap();
        r.num("t1_list2m_ms", ms(t));

        let want = scan_ref(&cap, ch, n);
        let scan = m.getattr("scan").unwrap();
        let t = Instant::now();
        let got: u32 = scan.call1((&samples, n, ch)).unwrap().extract().unwrap();
        r.num("t2_samples", n as f64);
        r.num("t2_ms", ms(t));
        r.num("t2_ok", (got == want) as u8 as f64);
        let t = Instant::now();
        let got_l: u32 = scan.call1((&list, n, ch)).unwrap().extract().unwrap();
        r.num("t2_list_ms", ms(t));
        drop(list);
        if got_l != want {
            notes.push("scan over list MISMATCH".to_string());
        }

        // numpy variant (optional): prototype venv's site-packages
        let sp = "/home/mt/dev_oep/wireskein/prototype/.venv/lib/python3.13/site-packages";
        py.import("sys").unwrap().getattr("path").unwrap().call_method1("append", (sp,)).unwrap();
        match py.import("numpy") {
            Ok(np) => {
                let ver: String = np.getattr("__version__").unwrap().extract().unwrap();
                let sn = m.getattr("scan_numpy").unwrap();
                let _ = sn.call1((&samples, 1000usize, ch)).unwrap(); // warm up
                let t = Instant::now();
                let g: u32 = sn.call1((&samples, n, ch)).unwrap().extract().unwrap();
                r.num("t2_numpy_ms", ms(t));
                r.num("t2_numpy_ok", (g == want) as u8 as f64);
                notes.push(format!("numpy {ver} imported from prototype venv; np.frombuffer(memoryview) is zero-copy"));
            }
            Err(e) => notes.push(format!("numpy unavailable: {e}")),
        }

        let inp = i2c_input(&cap);
        let want = i2c_ref(&inp);
        r.num("t3_bits", inp.bits.len() as f64);
        let I2cInput { t: tv, bits, bounds } = inp;
        let t = Instant::now();
        let (p, l) = (bits.as_ptr(), bits.len());
        let pb = view(py, Box::new(bits), p, l, "B");
        let (p, l) = (tv.as_ptr() as *const u8, tv.len() * 4);
        let pt = view(py, Box::new(tv), p, l, "I");
        let (p, l) = (bounds.as_ptr() as *const u8, bounds.len() * 4);
        let pbo = view(py, Box::new(bounds), p, l, "I");
        r.num("t3_handoff_ms", ms(t));
        let i2c = m.getattr("i2c").unwrap();
        let t = Instant::now();
        let out = i2c.call1((pb, pt, pbo)).unwrap();
        r.num("t3_ms", ms(t));
        let out = out.cast_into::<PyTuple>().unwrap();
        let recs = out.get_item(0).unwrap().len().unwrap();
        let got: (u64, u64, u64) = (
            out.get_item(1).unwrap().extract().unwrap(),
            out.get_item(2).unwrap().extract().unwrap(),
            out.get_item(3).unwrap().extract().unwrap(),
        );
        r.num("t3_records", recs as f64);
        r.num("t3_ok", (got == want) as u8 as f64);

        m.add_function(wrap_pyfunction!(host_inc, &m).unwrap()).unwrap();
        let g = PyDict::new(py);
        g.set_item("host_inc", m.getattr("host_inc").unwrap()).unwrap();
        py.run(c"def lp(n):\n    x = 0\n    for i in range(n):\n        x = host_inc(x)\n    return x\n", Some(&g), None).unwrap();
        let lp = g.get_item("lp").unwrap().unwrap();
        let t = Instant::now();
        let _: i64 = lp.call1((100_000,)).unwrap().extract().unwrap();
        r.num("t4_host_call_us", ms(t) * 1e3 / 1e5);
        let inc = m.getattr("inc").unwrap();
        let t = Instant::now();
        let mut x = 0i64;
        for _ in 0..100_000 {
            x = inc.call1((x,)).unwrap().extract().unwrap();
        }
        r.num("t4_script_call_us", ms(t) * 1e3 / 1e5);

        let path = format!("{dir}/python/error.py");
        let code = std::fs::read_to_string(&path).unwrap();
        let err = match PyModule::from_code(py, &cstr(&code), &cstr(&path), c"error") {
            Ok(_) => "no error".to_string(),
            Err(e) => fmt_err(py, &e),
        };
        r.text("t5_error", &err);
        let v: String = py.import("sys").unwrap().getattr("version").unwrap().extract().unwrap();
        notes.insert(0, format!("CPython {} via pyo3; GIL held for the whole run", v.split(' ').next().unwrap_or("")));
    });
    r.num("peak_rss_kb", peak_rss_kb() as f64);
    r.text("notes", &notes.join("; "));
    r.print();
}
