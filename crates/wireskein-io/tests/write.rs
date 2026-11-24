//! A capture written by wireskein-io's writers (a WireSkein file, a sigrok session, a VCD) read back by its readers as
//! the lines, the lines kept as changes and the analog channels it was written from.

use serde_json::{Map, Value, json};
use wireskein_io::{Analog, Capture, Events, Kind, Line, Part, read, write};

fn line(name: &str, initial: u8, edges: Vec<i64>, step: i64, reduction: Option<&str>) -> Line {
    Line {
        name: name.into(),
        initial,
        edges,
        step,
        phase: 0,
        reduction: reduction.map(String::from),
        acquisition: Map::new(),
    }
}

fn capture() -> Capture {
    let mut a = line("A", 0, vec![100, 200, 5000, 5003], 1, None);
    a.acquisition.insert("pin".into(), json!("PA0"));
    let clk = line("CLK", 1, (1..3000).map(|k| 3 * k).collect(), 1, None);
    let slow = line("S", 1, vec![64, 320, 9600], 32, None);
    let high = line("H", 0, vec![40, 80], 4, Some("any_high"));
    let ev = Events {
        name: "E".into(),
        step: 8,
        phase: 0,
        n: 2500,
        start_level: 0,
        quanta: vec![3, 10, 11],
        levels: vec![1, 0, 1],
        counts: vec![1, 1, 1],
        reduction: None,
        acquisition: Map::new(),
    };
    let raw = Analog {
        name: "V".into(),
        encoding: "analog".into(),
        rate_hz: (10_000, 1),
        t0_ticks: (0, 1),
        width: 16,
        value_bits: Some(12),
        zero: Some(2048.0),
        scale_nv: Some(805664.0),
        unit: "V".into(),
        values: (0..100).map(|k| (k * 37 % 4096) as f64).collect(),
        acquisition: Map::new(),
    };
    let mut meta = Map::new();
    meta.insert("probe".into(), json!("x"));
    meta.insert("start_ns".into(), json!(12));
    Capture {
        kind: Kind::WireSkein,
        tick_hz: (1_000_000, 1),
        id: Some("ab".repeat(16)),
        complete: true,
        lost: vec![],
        layout: "timeline".into(),
        n_parts: 1,
        meta,
        parts: vec![Part {
            part: 0,
            parts: None,
            ticks: 20_000,
            tick0: Some(0),
            meta: Map::new(),
            lines: vec![a, clk, slow, high],
            events: vec![ev],
            analog: vec![raw],
            skipped: vec![],
        }],
    }
}

fn dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wireskein-io-write-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn every_form_reads_back() {
    let cap = capture();
    let d = dir();
    for name in ["c.wireskein", "c.sr", "c.vcd"] {
        let p = d.join(name);
        write::write(&p, &cap, &[("attach/a.txt".into(), b"hi".to_vec())]).unwrap();
        let got = read(&p).unwrap();
        assert_eq!(got.tick_hz, cap.tick_hz, "{name}");
        assert_eq!(got.id, cap.id, "{name}");
        let (want, back) = (&cap.parts[0], &got.parts[0]);
        assert_eq!(back.ticks, want.ticks, "{name}");
        for l in &want.lines {
            let b = back.lines.iter().find(|x| x.name == l.name).unwrap();
            assert_eq!(
                (b.initial, &b.edges, b.step),
                (l.initial, &l.edges, l.step),
                "{name} {}",
                l.name
            );
        }
        assert_eq!(back.meta.get("probe"), Some(&Value::from("x")), "{name}");
        let v = back.analog.iter().find(|a| a.name == "V").unwrap();
        match name {
            "c.vcd" => {
                // volts, float32
                let want: Vec<f64> = cap.parts[0].analog[0]
                    .volts()
                    .unwrap()
                    .iter()
                    .map(|&x| x as f32 as f64)
                    .collect();
                // (written as 9 digits: within a float32 step)
                assert!(
                    v.values
                        .iter()
                        .zip(&want)
                        .all(|(a, b)| (a - b).abs() <= 1e-6 * b.abs().max(1.0))
                );
                assert_eq!(v.values.len(), want.len());
            }
            _ => assert_eq!(v.values, cap.parts[0].analog[0].values, "{name}"),
        }
        if name == "c.wireskein" {
            assert_eq!(back.events[0].quanta, vec![3, 10, 11]);
            assert_eq!(
                back.lines.iter().find(|x| x.name == "A").unwrap().acquisition["pin"],
                "PA0"
            );
        }
    }
    std::fs::remove_dir_all(d).ok();
}

/// A session's lines read a block at a time (stream::sr_source) as its reader reads them whole; pins in their order.
#[test]
fn a_session_read_a_block_at_a_time() {
    let mut cap = capture();
    cap.parts[0].lines.retain(|l| l.step == 1 && l.reduction.is_none());
    cap.parts[0].events.clear();
    cap.parts[0].analog.clear();
    let p = std::env::temp_dir().join(format!("wireskein-io-sr-blocks-{}.sr", std::process::id()));
    write::write(&p, &cap, &[]).unwrap();
    let whole = read(&p).unwrap();
    let pins = vec!["CLK".to_string(), "A".to_string()];
    for pins in [None, Some(pins.as_slice())] {
        let mut src = wireskein_io::stream::sr_source(&p, pins).unwrap().unwrap();
        use wireskein_io::stream::Source;
        let names = src.names().to_vec();
        let init = src.initial().to_vec();
        assert_eq!(src.n(), whole.parts[0].ticks);
        let mut got: Vec<Vec<i64>> = vec![vec![]; names.len()];
        while let Some((_, es)) = src.next_block() {
            for (g, e) in got.iter_mut().zip(es) {
                g.extend(e);
            }
        }
        for (k, n) in names.iter().enumerate() {
            let l = whole.parts[0].lines.iter().find(|l| &l.name == n).unwrap();
            assert_eq!((init[k], &got[k]), (l.initial, &l.edges), "{n}");
        }
        if let Some(ps) = pins {
            assert_eq!(&names, ps);
        }
    }
    let nope = ["X".to_string()];
    assert!(wireskein_io::stream::sr_source(&p, Some(&nope)).is_err());
    std::fs::remove_file(p).ok();
}

/// Markers set, read back in their keys' order, another tool's kept when one's own are replaced; the capture's own
/// entries not to be written.
#[test]
fn markers_written_in_place_of_the_earlier_ones() {
    use wireskein_io::j::J;
    use wireskein_io::markers::{add_markers, markers, put};
    let p = std::env::temp_dir().join(format!("wireskein-io-markers-{}.wireskein", std::process::id()));
    write::write(&p, &capture(), &[("attach/a.txt".into(), b"hi".to_vec())]).unwrap();
    let before = read(&p).unwrap();
    let m = |t: i64, label: &str| J::Dict(vec![("t".into(), J::Int(t)), ("label".into(), J::from(label))]);
    add_markers(&p, &[m(5, "a")], "me").unwrap();
    add_markers(&p, &[m(7, "b"), m(9, "c")], "verify").unwrap();
    add_markers(&p, &[m(8, "d")], "verify").unwrap();
    let got: Vec<(i64, String, String)> = markers(&p)
        .unwrap()
        .iter()
        .map(|x| {
            (
                x.get("t").i().unwrap(),
                x.get("label").s().unwrap().into(),
                x.get("by").s().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![(5, "a".into(), "me".into()), (8, "d".into(), "verify".into())]
    );
    assert!(put(&p, "capture.json", b"{}").is_err());
    // the capture and what was carried with it read as before
    let back = read(&p).unwrap();
    assert_eq!(back.parts[0].lines, before.parts[0].lines);
    assert!(
        wireskein_io::wsk::names(&std::fs::read(&p).unwrap())
            .unwrap()
            .contains(&"attach/a.txt".to_string())
    );
    std::fs::remove_file(p).ok();
}
