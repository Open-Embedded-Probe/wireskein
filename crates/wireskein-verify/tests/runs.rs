//! Run directories written here as `wireskein.runlog.Recorder` writes them (run.json: the log with its headings,
//! the captures, the expectations by section; each capture a WireSkein file), and the repository's own recorded runs
//! (tests/data/i2c_bench), verified as Python's verify verifies them.

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use wireskein_verify::J;
use zip::write::SimpleFileOptions;

/// A directory of its own under the system's temporary one, emptied.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wireskein-verify-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A WireSkein file of one part at 1 MHz: each line its samples at every tick (bits, LSB first).
fn capture(path: &Path, lines: &[(&str, Vec<u8>)], meta: Value) {
    let ticks = lines[0].1.len();
    let mut chans = vec![];
    let mut files = vec![];
    for (k, (name, s)) in lines.iter().enumerate() {
        let mut b = vec![0u8; s.len().div_ceil(8)];
        for (i, &v) in s.iter().enumerate() {
            b[i / 8] |= v << (i % 8);
        }
        let file = format!("part/0/ch/{k}.bits");
        chans.push(json!({"name": name, "file": file, "encoding": "bits", "n": s.len(), "step": 1, "phase": 0}));
        files.push((file, b));
    }
    let head = json!({"tick_hz": [1000000, 1], "layout": "timeline", "complete": true, "lost": [], "meta": meta,
                      "parts": [{"ticks": ticks, "tick0": 0, "channels": chans}]});
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = SimpleFileOptions::default();
        z.start_file("wireskein.json", o).unwrap();
        z.write_all(br#"{"format": "wireskein/1"}"#).unwrap();
        z.start_file("capture.json", o).unwrap();
        z.write_all(&serde_json::to_vec(&head).unwrap()).unwrap();
        for (f, b) in files {
            z.start_file(f, o).unwrap();
            z.write_all(&b).unwrap();
        }
        z.finish().unwrap();
    }
    std::fs::write(path, buf.into_inner()).unwrap();
}

/// A line's samples: (level, ticks) runs in turn.
fn runs(r: &[(u8, usize)]) -> Vec<u8> {
    r.iter().flat_map(|&(v, n)| std::iter::repeat_n(v, n)).collect()
}

/// UART 8N1 at `bit` ticks a bit: idle, then the bytes back to back, idle to `len`.
fn uart(bytes: &[u8], bit: usize, lead: usize, len: usize) -> Vec<u8> {
    let mut s = vec![1u8; lead];
    for &b in bytes {
        s.extend(std::iter::repeat_n(0, bit));
        for k in 0..8 {
            s.extend(std::iter::repeat_n((b >> k) & 1, bit));
        }
        s.extend(std::iter::repeat_n(1, bit));
    }
    s.resize(len, 1);
    s
}

fn marker(ns: i64, text: &str) -> Value {
    json!({"host_ns": ns, "src": "marker", "text": text})
}

fn result<'a>(rep: &'a J, path: &str, check: &str) -> &'a J {
    rep.get("results")
        .list()
        .iter()
        .find(|r| r.get("path").s() == Some(path) && r.get("check").s() == Some(check))
        .unwrap_or_else(|| panic!("no {check} at {path}"))
}

#[test]
fn waveforms_buses_and_sections() {
    let d = scratch("sections");
    let n = 20_000;
    // a 1 kHz PWM at 25 %, a line held low, "hi" at 10 kBaud
    let mut pwm = vec![];
    while pwm.len() < n {
        pwm.extend(runs(&[(0, 750), (1, 250)]));
    }
    pwm.truncate(n);
    capture(
        &d.join("c0001.wireskein"),
        &[("P", pwm), ("L", vec![0; n]), ("U", uart(b"hi", 100, 1000, n))],
        json!({"start_ns": 0}),
    );
    let run = json!({
        "format": "wireskein-run/1", "meta": {"target": "test"},
        "log": [marker(1000, "# t"), marker(2000, "## pwm"),
                {"host_ns": 2200, "src": "host", "text": "PWM 64"}, marker(3000, "##"),
                marker(4000, "## quiet"), marker(5000, "##"), marker(5500, "### deep"), marker(6000, "#"),
                {"host_ns": 7000, "src": "lost", "text": "the probe's log overflowed"}],
        "captures": [{"file": "c0001.wireskein", "host_ns": 2500, "channels": ["P", "L", "U"], "path": "t/pwm"}],
        "expect": {
            "t/pwm": {"checks": [
                {"kind": "square", "pin": "P", "freq_hz": 1000, "duty": 0.25, "freq_rel": 0.02, "duty_abs": 0.01,
                 "jitter_max": null, "outliers_max": 0, "grid_abs_s": null},
                {"kind": "square", "pin": "P", "freq_hz": 1200, "duty": 0.25, "freq_rel": 0.02, "duty_abs": 0.01,
                 "jitter_max": null, "outliers_max": 0, "grid_abs_s": null},
                {"kind": "level", "pin": "L", "value": 0},
                {"kind": "starts", "levels": {"L": 0, "U": 1}},
                {"kind": "pulses", "pin": "P", "count": 20, "period_s": 1e-3, "period_rel": 0.05,
                 "width_min_s": null, "width_max_s": null, "level": 1},
                {"kind": "uart", "pin": "U", "baud": 10000, "data": "6869", "baud_rel": 0.03, "idle": 1,
                 "bits": 8, "parity": "none", "stop": 1, "errors": null},
                {"kind": "uart", "pin": "U", "baud": null, "data": null, "baud_rel": 0.03, "idle": 1, "bits": 8,
                 "parity": "none", "stop": 1, "errors": null},
                {"kind": "level", "pin": "X", "value": 0},
                {"kind": "only_moving", "pins": ["P"]}]},
            "t/quiet": {"checks": [{"kind": "level", "pin": "L", "value": 0}]},
            "t/missing": {"checks": [{"kind": "level", "pin": "L", "value": 0}]}},
        "results": [{"path": "t/pwm", "check": "mine", "status": "measured", "measured": {"x": 1}, "reason": "",
                     "notices": [], "capture": null}]
    });
    std::fs::write(d.join("run.json"), serde_json::to_vec(&run).unwrap()).unwrap();
    let rep = wireskein_verify::verify(&d).unwrap();
    let st = |p: &str, c: &str| result(&rep, p, c).get("status").s().unwrap().to_string();
    let why = |p: &str, c: &str| result(&rep, p, c).get("reason").s().unwrap().to_string();
    let rs = rep.get("results").list();
    let sq: Vec<&J> = rs.iter().filter(|r| r.get("check").s() == Some("square")).collect();
    assert_eq!(sq[0].get("status").s(), Some("ok"));
    assert!((sq[0].get("measured").get("freq_hz").f().unwrap() - 1000.0).abs() < 1e-6);
    assert_eq!(sq[1].get("status").s(), Some("ng"));
    assert!(
        sq[1]
            .get("reason")
            .s()
            .unwrap()
            .starts_with("frequency 1000.00 Hz is -16.67% off")
    );
    assert_eq!(st("t/pwm", "level"), "ok");
    assert_eq!(st("t/pwm", "starts"), "ok");
    assert_eq!(st("t/pwm", "pulses"), "ok");
    let u: Vec<&J> = rs.iter().filter(|r| r.get("check").s() == Some("uart")).collect();
    assert_eq!(u[0].get("status").s(), Some("ok"));
    assert_eq!(u[0].get("measured").get("data").s(), Some("6869"));
    assert_eq!(u[1].get("status").s(), Some("measured"));
    assert_eq!(
        u[1].get("reason").s(),
        Some("measured 10000 baud, 2 chars, 0 framing / 0 parity errors")
    );
    // a pin in no capture, other lines moving, sections without a capture or not in the log
    let x = rs
        .iter()
        .find(|r| r.get("expected").get("pin").s() == Some("X"))
        .unwrap();
    assert_eq!(x.get("status").s(), Some("unchecked"));
    assert_eq!(x.get("reason").s(), Some("pins not in any capture: X"));
    assert_eq!(why("t/pwm", "only_moving"), "other pins moved: U");
    assert_eq!(why("t/quiet", "capture"), "no capture inside the segment");
    assert_eq!(why("t/missing", "segment"), "no segment with this path in the log");
    // the test's own verdict, the probe's lost lines, a heading skipping a level
    assert_eq!(st("t/pwm", "mine"), "measured");
    assert_eq!(why("", "probe_log"), "the probe's log overflowed");
    assert_eq!(
        why("t/deep", "markers"),
        r#"{"kind": "level_skip", "from": 1, "to": 3}"#
    );
    let s = rep.get("summary");
    assert_eq!(
        (s.get("segments").i(), s.get("captures").i(), s.get("measured").i()),
        (Some(4), Some(1), Some(2))
    );
    assert!(wireskein_verify::failed(&rep, false));
    let lines = wireskein_verify::lines(&rep, true, false);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("OK  t/pwm  square  c0001.wireskein"))
    );
    assert!(wireskein_verify::summary_line(&rep).contains("4 segments, 1 captures"));
    assert!(rep.dumps().starts_with(r#"{"format":"wireskein-report/1","run":"#));
    assert!(
        wireskein_verify::junit(&rep, false).contains("<failure message=\"no segment with this path in the log\">")
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_section_on_the_probes_clock_is_cut_from_the_capture() {
    // the capture starts at probe time 1 ms (1000 ticks at 1 MHz); the section from 3 ms to 5 ms on the probe's
    // clock: the line is low before 4 ms and high after, so in the section it starts 0 and ends 1
    let d = scratch("probe");
    capture(
        &d.join("c0001.wireskein"),
        &[("A", runs(&[(0, 3000), (1, 5000)]))],
        json!({"start_ns": 1_000_000, "start_uncertainty_ns": 0, "boot_id": 7}),
    );
    let run = json!({
        "format": "wireskein-run/1", "meta": {},
        "log": [{"host_ns": 3000, "src": "marker", "text": "# s", "probe_ns": 3_000_000, "boot_id": 7},
                {"host_ns": 5000, "src": "marker", "text": "#", "probe_ns": 5_000_000, "boot_id": 7}],
        "captures": [{"file": "c0001.wireskein", "host_ns": 500, "channels": ["A"]}],
        "expect": {"s": {"checks": [
            {"kind": "starts", "levels": {"A": 0}}, {"kind": "ends", "levels": {"A": 1}},
            {"kind": "becomes", "pin": "A", "level": 1, "freq_hz": null, "period_s": null, "duty": null,
             "high_s": null, "freq_rel": 0.02, "duty_abs": 0.01, "high_abs_s": null, "high_rel": null,
             "irregular_max": 1, "irregular_min": 0, "min_periods": 3, "within_s": null}]}}
    });
    std::fs::write(d.join("run.json"), serde_json::to_vec(&run).unwrap()).unwrap();
    let rep = wireskein_verify::verify(&d).unwrap();
    for r in rep.get("results").list() {
        assert_eq!(r.get("status").s(), Some("ok"), "{}", r.repr());
        assert_eq!(r.get("capture").s(), Some("c0001.wireskein@2000:4000"));
        assert_eq!(r.get("measured").get("window"), &J::from(vec![2000i64, 4000]));
    }
    let b = result(&rep, "s", "becomes").get("measured");
    assert_eq!(b.get("change_s").f(), Some(1e-3));
    assert_eq!(b.get("change_tick").i(), Some(3000));
    let _ = std::fs::remove_dir_all(&d);
}

/// The repository's recorded I2C bench runs: each section of the probe's clock cut from its capture, I2C read as
/// an input does, with every notice; the same verdicts and the same number of notices as Python's verify.
#[test]
fn the_repositorys_bench_runs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/data/i2c_bench");
    // (run, ok, notices) as `wireskein verify` reports them
    for (name, ok, notices) in [
        ("h417_a", 8, 23),
        ("h417_b", 8, 12),
        ("v205_contract", 18, 181),
        ("x035_100k", 8, 8),
        ("x035_12k5", 8, 23),
    ] {
        let rep = wireskein_verify::verify(&root.join(name)).unwrap();
        let s = rep.get("summary");
        assert_eq!(
            (
                s.get("ok").i(),
                s.get("ng").i(),
                s.get("unchecked").i(),
                s.get("notices").i()
            ),
            (Some(ok), Some(0), Some(0), Some(notices)),
            "{name}"
        );
    }
}
