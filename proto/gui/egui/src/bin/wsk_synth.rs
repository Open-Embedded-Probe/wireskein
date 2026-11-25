//! A made-up capture file for the prototype: an I2C bus of N transactions (a few NACKed) at 100 kHz, a UART at
//! 115200 baud printing lines beside it, and PWM lines, sampled at 25 MHz; written as a WireSkein file
//! (wireskein-io), so the real engine reads it.
//!
//! wsk-synth OUT.wireskein [--tx N] [--pwm K]

use serde_json::Map;
use std::path::PathBuf;
use view_core::synth::{I2cGen, Recorder, mix, uart_bytes};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = PathBuf::from("synth.wireskein");
    let mut n_tx: u64 = 100_000;
    let mut n_pwm = 4usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tx" => {
                i += 1;
                n_tx = args[i].parse::<f64>().unwrap() as u64;
            }
            "--pwm" => {
                i += 1;
                n_pwm = args[i].parse().unwrap();
            }
            f => out = PathBuf::from(f),
        }
        i += 1;
    }
    let hz = 25_000_000i64;
    // lines: SCL, SDA, TX, PWM0..
    let mut names = vec!["SCL".to_string(), "SDA".to_string(), "TX".to_string()];
    for k in 0..n_pwm {
        names.push(format!("PWM{k}"));
    }
    let mut r = Recorder::new(&vec![1u8; 3].into_iter().chain(vec![0u8; n_pwm]).collect::<Vec<_>>());
    let mut g = I2cGen::new(250, 0, 1, 7);
    let mut t: i64 = 1000;
    let mut uart_t: i64 = 5000;
    let uart_bit = hz as f64 / 115200.0;
    let mut k = 0u64;
    for i in 0..n_tx {
        t = g.transaction(&mut r, t, i);
        t += 300 + (mix(i ^ 0x77) % 4000) as i64;
        // a UART line every 20 transactions, where the bus is
        if i % 20 == 0 && uart_t < t {
            let line = format!("t={} temp={}.{}\r\n", i, 20 + mix(i) % 10, mix(i + 1) % 10);
            uart_t = uart_bytes(&mut r, 2, t.max(uart_t), uart_bit, line.as_bytes()) + 200;
            k += 1;
        }
    }
    let n = t.max(uart_t) + 1000;
    // PWM: periods of 40 us to 320 us, duty drifting
    for p in 0..n_pwm {
        let period = 1000i64 << p;
        let mut s = 0i64;
        let mut j = 0u64;
        while s + period < n {
            let duty = 0.1 + 0.8 * (((j as f64) * 0.01 + p as f64).sin() * 0.5 + 0.5);
            r.set(3 + p, s, 1);
            r.set(3 + p, s + (period as f64 * duty) as i64, 0);
            s += period;
            j += 1;
        }
    }
    let lines: Vec<wireskein_io::Line> = names
        .iter()
        .enumerate()
        .map(|(i, name)| wireskein_io::Line {
            name: name.clone(),
            initial: r.initial[i],
            edges: std::mem::take(&mut r.edges[i]),
            step: 1,
            phase: 0,
            reduction: None,
            acquisition: Map::new(),
        })
        .collect();
    let total: usize = lines.iter().map(|l| l.edges.len()).sum();
    let cap = wireskein_io::Capture {
        kind: wireskein_io::Kind::WireSkein,
        tick_hz: (hz, 1),
        id: None,
        complete: true,
        lost: vec![],
        layout: "timeline".into(),
        n_parts: 1,
        meta: Map::new(),
        parts: vec![wireskein_io::Part {
            part: 0,
            parts: None,
            ticks: n,
            tick0: Some(0),
            meta: Map::new(),
            lines,
            events: vec![],
            analog: vec![],
            skipped: vec![],
        }],
    };
    wireskein_io::write::write_wireskein(&out, &cap, &[]).unwrap();
    eprintln!(
        "{}: {n_tx} I2C transactions, {k} UART lines, {n_pwm} PWM, {total} changes, {n} ticks ({:.2} s at 25 MHz)",
        out.display(),
        n as f64 / hz as f64
    );
}
