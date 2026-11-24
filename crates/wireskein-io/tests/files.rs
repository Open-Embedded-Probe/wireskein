//! Files written here (a WireSkein file of several encodings and parts, a sigrok session, a VCD) read back as the
//! lines they hold.

use serde_json::json;
use std::io::Write;
use wireskein_io::{Kind, read_bytes};
use zip::write::SimpleFileOptions;

fn zip_of(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, data) in entries {
            z.start_file(*name, SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }
    buf.into_inner()
}

/// Samples packed a bit each, LSB first.
fn bits(s: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; s.len().div_ceil(8)];
    for (i, &b) in s.iter().enumerate() {
        out[i / 8] |= b << (i % 8);
    }
    out
}

/// Events: (quantum, level after, count) each.
fn events(ev: &[(u64, u8, u8)]) -> Vec<u8> {
    let (mut out, mut prev) = (vec![], -1i64);
    for &(k, lv, c) in ev {
        let mut gap = (k as i64 - prev - 1) as u64;
        loop {
            let b = (gap & 0x7F) as u8;
            gap >>= 7;
            out.push(if gap > 0 { b | 0x80 } else { b });
            if gap == 0 {
                break;
            }
        }
        out.push((lv << 7) | c);
        prev = k as i64;
    }
    out
}

fn wireskein(parts: serde_json::Value, layout: &str, files: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let head = json!({"id": "00112233445566778899aabbccddeeff", "tick_hz": [1000000, 1], "layout": layout,
                      "complete": true, "parts": parts, "meta": {"source": "test"}});
    let mut entries = vec![
        ("wireskein.json", br#"{"format": "wireskein/1"}"#.to_vec()),
        ("capture.json", serde_json::to_vec(&head).unwrap()),
    ];
    entries.extend(files);
    zip_of(&entries)
}

#[test]
fn encodings_of_a_wireskein_file() {
    // A: bits at step 1; B: bits at step 2, phase 1; C: events step 1 (a change at the first tick); D: events at step
    // 4, one change a quantum; E: events at step 4 hiding changes; F: change_latch; G: analog (left out)
    let a = [1u8, 1, 0, 0, 1, 1, 1, 0, 0, 1];
    let b = [0u8, 1, 1, 0, 0];
    let ch = json!([
        {"name": "A", "file": "part/0/ch/0.bits", "encoding": "bits", "n": 10, "step": 1, "phase": 0},
        {"name": "B", "file": "part/0/ch/1.bits", "encoding": "bits", "n": 5, "step": 2, "phase": 1},
        {"name": "C", "file": "part/0/ch/2.ev", "encoding": "events", "n": 10, "step": 1, "phase": 0,
         "start_level": 0, "events": 2},
        {"name": "D", "file": "part/0/ch/3.ev", "encoding": "events", "n": 3, "step": 4, "phase": 0,
         "start_level": 1, "events": 2},
        {"name": "E", "file": "part/0/ch/4.ev", "encoding": "events", "n": 3, "step": 4, "phase": 0,
         "start_level": 0, "events": 1},
        {"name": "F", "file": "part/0/ch/5.cl", "encoding": "change_latch", "n": 3, "step": 4, "phase": 0,
         "start_level": 0},
        {"name": "G", "file": "part/0/ch/6.an", "encoding": "analog", "n": 2, "width": 16,
         "rate_hz": [1000, 1], "t0_ticks": [0, 1]},
        {"name": "H", "file": "part/0/ch/7.x", "encoding": "future", "n": 2},
    ]);
    // change_latch: interval 0 no change (0), interval 1 changed to 1 (0b11), interval 2 no change (0b01)
    let cl = [0u8, 3, 1];
    let packed_cl = vec![cl[0] | (cl[1] << 2) | (cl[2] << 4)];
    let f = wireskein(
        json!([{"ticks": 10, "tick0": 0, "channels": ch, "meta": {"start_ns": 5}}]),
        "timeline",
        vec![
            ("part/0/ch/0.bits", bits(&a)),
            ("part/0/ch/1.bits", bits(&b)),
            ("part/0/ch/2.ev", events(&[(0, 1, 1), (6, 0, 1)])),
            ("part/0/ch/3.ev", events(&[(0, 0, 1), (2, 1, 1)])),
            ("part/0/ch/4.ev", events(&[(1, 0, 2)])),
            ("part/0/ch/5.cl", packed_cl),
            ("part/0/ch/6.an", vec![0, 0, 1, 0]),
        ],
    );
    let cap = read_bytes(&f).unwrap();
    assert_eq!(cap.kind, Kind::WireSkein);
    assert_eq!(
        (cap.tick_hz, cap.id.as_deref()),
        ((1_000_000, 1), Some("00112233445566778899aabbccddeeff"))
    );
    let p = &cap.parts[0];
    assert_eq!(p.meta["start_ns"], 5);
    let line = |n: &str| p.lines.iter().find(|l| l.name == n).unwrap();
    assert_eq!((line("A").initial, line("A").edges.clone()), (1, vec![2, 4, 7, 9]));
    assert_eq!((line("B").initial, line("B").edges.clone()), (0, vec![3, 7]));
    assert_eq!((line("C").initial, line("C").edges.clone()), (1, vec![6]));
    // each change at its quantum's last tick, cut at the part's end
    assert_eq!((line("D").initial, line("D").edges.clone()), (1, vec![3, 9]));
    assert_eq!((line("F").initial, line("F").edges.clone()), (0, vec![7]));
    let skipped: Vec<&str> = p.skipped.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(skipped, ["E", "G", "H"]);
}

#[test]
fn parts_that_follow_on_are_one_stretch() {
    let ch = |k: usize| {
        json!([{"name": "A", "file": format!("part/{k}/ch/0.bits"), "encoding": "bits", "n": 4,
                                 "step": 1, "phase": 0}])
    };
    let files = vec![
        ("part/0/ch/0.bits", bits(&[0, 1, 1, 1])),
        ("part/1/ch/0.bits", bits(&[0, 0, 1, 1])),
        ("part/2/ch/0.bits", bits(&[1, 0, 0, 0])),
    ];
    // parts 0 and 1 follow on (tick0 4 = 0 + 4), part 2 after a gap
    let parts = json!([{"ticks": 4, "tick0": 0, "channels": ch(0)}, {"ticks": 4, "tick0": 4, "channels": ch(1)},
                       {"ticks": 4, "tick0": 20, "channels": ch(2)}]);
    let cap = read_bytes(&wireskein(parts.clone(), "timeline", files.clone())).unwrap();
    assert_eq!(cap.n_parts, 3);
    assert_eq!(cap.parts.len(), 2);
    let p = &cap.parts[0];
    assert_eq!((p.parts, p.ticks), (Some((0, 1)), 8));
    // part 1 starts at 0 where part 0 ended at 1: a change at the boundary
    assert_eq!(p.lines[0].edges, vec![1, 4, 6]);
    assert_eq!(cap.parts[1].part, 2);
    // segments are never joined
    let seg = read_bytes(&wireskein(parts, "segments", files)).unwrap();
    assert_eq!(seg.parts.len(), 3);
    // a capture.json against the rules is refused
    let bad = zip_of(&[
        ("wireskein.json", br#"{"format": "wireskein/1"}"#.to_vec()),
        (
            "capture.json",
            br#"{"tick_hz": [1, 1], "layout": "timeline", "parts": [{"ticks": 1, "tick0": 0, "channels": []}]}"#
                .to_vec(),
        ),
    ]);
    assert!(read_bytes(&bad).unwrap_err().contains("complete"));
}

#[test]
fn a_sigrok_session() {
    // two probes, unitsize 1, the samples split over two chunks
    let md = "[global]\nsigrok version=0.5.2\n\n[device 1]\ncapturefile=logic-1\ntotal probes=2\nsamplerate=2 MHz\n\
              total analog=0\nprobe1=SCL\nprobe2=SDA\nunitsize=1\n";
    let s1 = vec![0b11u8, 0b11, 0b10, 0b00];
    let s2 = vec![0b01u8, 0b11];
    let f = zip_of(&[
        ("version", b"2".to_vec()),
        ("metadata", md.as_bytes().to_vec()),
        ("logic-1-1", s1),
        ("logic-1-2", s2),
    ]);
    let cap = read_bytes(&f).unwrap();
    assert_eq!((cap.kind, cap.tick_hz), (Kind::Sr, (2_000_000, 1)));
    let p = &cap.parts[0];
    assert_eq!(p.ticks, 6);
    assert_eq!(p.lines[0].name, "SCL");
    assert_eq!((p.lines[0].initial, p.lines[0].edges.clone()), (1, vec![2, 4]));
    assert_eq!((p.lines[1].initial, p.lines[1].edges.clone()), (1, vec![3, 5]));
}

#[test]
fn a_vcd() {
    let text = "$date today $end\n$timescale 10 ns $end\n$scope module top $end\n$var wire 1 ! clk $end\n\
                $var wire 2 \" bus $end\n$var real 64 # v $end\n$upscope $end\n$enddefinitions $end\n#0\n$dumpvars\n\
                1!\nb01 \"\nr1.5 #\n$end\n#20\n0!\n#40\n1!\nbx1 \"\n#60\nb10 \"\n";
    let cap = read_bytes(text.as_bytes()).unwrap();
    assert_eq!(cap.kind, Kind::Vcd);
    // the tick: the change times' gcd, 20 units of 10 ns (5 MHz)
    assert_eq!(cap.tick_hz, (5_000_000, 1));
    let p = &cap.parts[0];
    let line = |n: &str| p.lines.iter().find(|l| l.name == n).unwrap();
    assert_eq!((line("clk").initial, line("clk").edges.clone()), (1, vec![1, 2]));
    assert_eq!((line("bus[0]").initial, line("bus[0]").edges.clone()), (1, vec![3]));
    assert_eq!((line("bus[1]").initial, line("bus[1]").edges.clone()), (0, vec![3]));
    assert_eq!(cap.meta["vcd_x_or_z"], 1);
    assert_eq!(p.skipped[0]["name"], "v");
}
