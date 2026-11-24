//! USB Power Delivery on a CC line (Python's `_engine/usbpd.py`): biphase mark coding at about 300 kb/s, a preamble
//! of 64 alternating bits, then 4b5b symbols (LSB first): the ordered set (SOP, SOP', SOP'' or a reset's), the
//! header, the data objects, CRC32 and EOP. Bytes and words little-endian.

use crate::Candidate;
use crate::kernels::median;
use crate::pulse::{best_of_views, candidate, evidence, obj};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const PD_RATE: (f64, f64) = (270e3, 330e3);

fn data4b5b(v: u32) -> Option<u8> {
    Some(match v {
        0b11110 => 0x0,
        0b01001 => 0x1,
        0b10100 => 0x2,
        0b10101 => 0x3,
        0b01010 => 0x4,
        0b01011 => 0x5,
        0b01110 => 0x6,
        0b01111 => 0x7,
        0b10010 => 0x8,
        0b10011 => 0x9,
        0b10110 => 0xA,
        0b10111 => 0xB,
        0b11010 => 0xC,
        0b11011 => 0xD,
        0b11100 => 0xE,
        0b11101 => 0xF,
        _ => return None,
    })
}

fn kcode(v: u32) -> Option<&'static str> {
    Some(match v {
        0b11000 => "S1",
        0b10001 => "S2",
        0b00110 => "S3",
        0b00111 => "R1",
        0b11001 => "R2",
        0b01101 => "EOP",
        _ => return None,
    })
}

fn ordered_set(k: [Option<&str>; 4]) -> Option<&'static str> {
    Some(match k {
        [Some("S1"), Some("S1"), Some("S1"), Some("S2")] => "SOP",
        [Some("S1"), Some("S1"), Some("S3"), Some("S3")] => "SOP'",
        [Some("S1"), Some("S3"), Some("S1"), Some("S3")] => "SOP''",
        [Some("R1"), Some("R1"), Some("R1"), Some("R2")] => "Hard Reset",
        [Some("R1"), Some("S1"), Some("R1"), Some("S3")] => "Cable Reset",
        _ => return None,
    })
}

fn control(t: u32) -> Option<&'static str> {
    Some(match t {
        1 => "GoodCRC",
        2 => "GotoMin",
        3 => "Accept",
        4 => "Reject",
        5 => "Ping",
        6 => "PS_RDY",
        7 => "Get_Source_Cap",
        8 => "Get_Sink_Cap",
        9 => "DR_Swap",
        10 => "PR_Swap",
        11 => "VCONN_Swap",
        12 => "Wait",
        13 => "Soft_Reset",
        16 => "Not_Supported",
        17 => "Get_Source_Cap_Extended",
        18 => "Get_Status",
        19 => "FR_Swap",
        20 => "Get_PPS_Status",
        21 => "Get_Country_Codes",
        22 => "Get_Sink_Cap_Extended",
        _ => return None,
    })
}

fn data_type(t: u32) -> Option<&'static str> {
    Some(match t {
        1 => "Source_Capabilities",
        2 => "Request",
        3 => "BIST",
        4 => "Sink_Capabilities",
        5 => "Battery_Status",
        6 => "Alert",
        7 => "Get_Country_Info",
        8 => "Enter_USB",
        15 => "Vendor_Defined",
        _ => return None,
    })
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &x in data {
        c ^= x as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// Bursts of BMC bits: (the burst's first edge, bits). Intervals of a half bit pair into a 1, of a whole bit are a 0;
/// a gap of several bits ends a burst.
fn bursts(edges: &[f64], rate: f64) -> Vec<(i64, Vec<u8>)> {
    let mut out = vec![];
    if edges.len() < 64 {
        return out;
    }
    let d: Vec<f64> = edges.windows(2).map(|w| w[1] - w[0]).collect();
    let md = median(&d);
    let long: Vec<f64> = d.iter().copied().filter(|&x| x > md * 1.5).collect();
    let ui = if long.is_empty() { md } else { median(&long) };
    if !(rate / PD_RATE.1 * 0.9 <= ui && ui <= rate / PD_RATE.0 * 1.1) {
        return out;
    }
    let (mut i, n) = (0usize, d.len());
    while i < n {
        let (start, mut bits) = (edges[i].round_ties_even() as i64, vec![]);
        while i < n {
            let x = d[i] / ui;
            if (0.75..=1.3).contains(&x) {
                bits.push(0);
                i += 1;
            } else if (0.3..0.75).contains(&x) && i + 1 < n && (0.3..0.75).contains(&(d[i + 1] / ui)) {
                bits.push(1);
                i += 2;
            } else {
                i += 1;
                break;
            }
        }
        if bits.len() >= 64 {
            out.push((start, bits));
        }
    }
    out
}

fn sym(bits: &[u8], k: usize) -> u32 {
    bits[k..(k + 5).min(bits.len())]
        .iter()
        .enumerate()
        .map(|(j, &b)| (b as u32) << j)
        .sum()
}

/// The edges with the line's duty bias taken out: BMC holds each level as long as the other on average, so a
/// threshold off the middle shows as a difference of their means; the falls are moved by half of it.
fn unbias(ch: &Channel) -> Vec<f64> {
    let e: Vec<f64> = ch.edges.iter().map(|&x| x as f64).collect();
    if e.len() < 8 {
        return e;
    }
    let lv = crate::pulse::level_after(ch);
    let d: Vec<f64> = e.windows(2).map(|w| w[1] - w[0]).collect();
    // keep to runs inside packets (a half or a whole bit): the long idles out
    let cap = 3.0 * median(&d);
    let pick = |l: u8| -> Vec<f64> {
        d.iter()
            .zip(&lv)
            .filter(|&(&x, &v)| v == l && x < cap)
            .map(|(&x, _)| x)
            .collect()
    };
    let (hi, lo) = (pick(1), pick(0));
    if hi.is_empty() || lo.is_empty() {
        return e;
    }
    let mean = |x: &[f64]| x.iter().sum::<f64>() / x.len() as f64;
    let bias = (mean(&lo) - mean(&hi)) / 2.0;
    e.iter()
        .zip(&lv)
        .map(|(&x, &v)| if v == 0 { x + bias } else { x })
        .collect()
}

struct Packet {
    ok: bool,
    header: bool,
    row: Map<String, Value>,
}

fn packets(ch: &Channel, rate: f64) -> Vec<Packet> {
    let mut out = vec![];
    let edges = unbias(ch);
    for (start, bits) in bursts(&edges, rate) {
        // the preamble alternates 0 1 ...: the ordered set starts where that stops
        let mut k = 0usize;
        while k + 1 < bits.len() && bits[k] != bits[k + 1] {
            k += 1;
        }
        if k.is_multiple_of(2) {
            k += 1;
        }
        if k >= 1 && bits.get(k - 1) == Some(&0) {
            k -= 1;
        }
        // find the ordered set near the preamble's end (its boundary may be a bit off)
        let mut sop = None;
        for off in -2i64..=2 {
            let jj = k as i64 + off;
            if jj < 0 || jj as usize + 20 > bits.len() {
                continue;
            }
            let jj = jj as usize;
            let os = [0, 1, 2, 3].map(|q| kcode(sym(&bits, jj + 5 * q)));
            if let Some(s) = ordered_set(os) {
                sop = Some(s);
                k = jj + 20;
                break;
            }
        }
        let Some(sop) = sop else { continue };
        let mut row = obj(vec![("s", json!(start)), ("sop", json!(sop))]);
        if sop == "Hard Reset" || sop == "Cable Reset" {
            row.insert("ok".into(), json!(true));
            out.push(Packet {
                ok: true,
                header: false,
                row,
            });
            continue;
        }
        let (mut nib, mut j, mut eop, mut error) = (vec![], k, false, None);
        while j + 5 <= bits.len() {
            let v = sym(&bits, j);
            j += 5;
            if let Some(kc) = kcode(v) {
                eop = kc == "EOP";
                break;
            }
            match data4b5b(v) {
                Some(x) => nib.push(x),
                None => {
                    error = Some("symbol");
                    break;
                }
            }
        }
        if eop {
            row.insert("eop".into(), json!(true));
        }
        if let Some(er) = error {
            row.insert("error".into(), json!(er));
        }
        let data: Vec<u8> = (0..nib.len().saturating_sub(1))
            .step_by(2)
            .map(|q| nib[q] | (nib[q + 1] << 4))
            .collect();
        if data.len() < 6 {
            row.insert("ok".into(), json!(false));
            row.insert("error".into(), json!(error.unwrap_or("short")));
            out.push(Packet {
                ok: false,
                header: false,
                row,
            });
            continue;
        }
        let hdr = u16::from_le_bytes([data[0], data[1]]) as u32;
        let nobj = ((hdr >> 12) & 7) as usize;
        let mtype = hdr & 0x1F;
        let body = &data[..(2 + 4 * nobj).min(data.len())];
        let crc = &data[(2 + 4 * nobj).min(data.len())..(6 + 4 * nobj).min(data.len())];
        let le = |b: &[u8]| b.iter().rev().fold(0u64, |a, &x| (a << 8) | x as u64);
        let objects: Vec<u64> = (0..nobj)
            .map(|q| le(&body[(2 + 4 * q).min(body.len())..(6 + 4 * q).min(body.len())]))
            .collect();
        let name = if nobj > 0 { data_type(mtype) } else { control(mtype) };
        row.insert("header".into(), json!(hdr));
        row.insert(
            "type".into(),
            json!(name.map(String::from).unwrap_or_else(|| format!("type{mtype}"))),
        );
        row.insert("id".into(), json!((hdr >> 9) & 7));
        row.insert("objects".into(), json!(objects));
        row.insert(
            "power_role".into(),
            json!(if hdr & 0x100 != 0 { "source" } else { "sink" }),
        );
        row.insert("data_role".into(), json!(if hdr & 0x20 != 0 { "DFP" } else { "UFP" }));
        row.insert("spec_rev".into(), json!(((hdr >> 6) & 3) + 1));
        let ok = crc.len() == 4 && crc32(body) as u64 == le(crc) && eop;
        row.insert("ok".into(), json!(ok));
        out.push(Packet { ok, header: true, row });
    }
    out
}

fn read(ch: &Channel, rate: f64, k: i64) -> Option<Candidate> {
    let pk = packets(ch, rate);
    let good: Vec<&Packet> = pk.iter().filter(|p| p.ok).collect();
    if good.len() < 2 {
        return None;
    }
    // each packet's ordered set (4 K-codes: 8) and CRC32 (32)
    let info: f64 = good.iter().map(|p| 8.0 + if p.header { 32.0 } else { 0.0 }).sum();
    let score = good.len() as f64 / pk.len() as f64 * evidence(info);
    let ev = obj(vec![("packets", json!(pk.len())), ("ok", json!(good.len()))]);
    let rows: Vec<Map<String, Value>> = pk.into_iter().map(|p| p.row).collect();
    Some(candidate(
        "usb_pd",
        &[("cc", ch.name)],
        score,
        Map::new(),
        ev,
        "packets",
        &rows,
        k,
    ))
}

/// The USB PD readings of a part: each line, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    if view.rate < 1.2e6 {
        return vec![];
    }
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 64)
        .filter_map(|ch| best_of_views(ch, |c, k| read(c, view.rate, k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENC: [u32; 16] = [
        0b11110, 0b01001, 0b10100, 0b10101, 0b01010, 0b01011, 0b01110, 0b01111, 0b10010, 0b10011, 0b10110, 0b10111,
        0b11010, 0b11011, 0b11100, 0b11101,
    ];

    fn sym_bits(v: u32, out: &mut Vec<u8>) {
        out.extend((0..5).map(|j| ((v >> j) & 1) as u8));
    }

    /// A packet's bits: the preamble, SOP, the header and objects, CRC32, EOP.
    fn packet(hdr: u16, objs: &[u32]) -> Vec<u8> {
        let mut b: Vec<u8> = (0..64).map(|i| (i & 1) as u8).collect();
        for s in [0b11000, 0b11000, 0b11000, 0b10001] {
            sym_bits(s, &mut b);
        }
        let mut data = hdr.to_le_bytes().to_vec();
        for o in objs {
            data.extend(o.to_le_bytes());
        }
        let c = crc32(&data);
        data.extend(c.to_le_bytes());
        for x in data {
            sym_bits(ENC[(x & 0xF) as usize], &mut b);
            sym_bits(ENC[(x >> 4) as usize], &mut b);
        }
        sym_bits(0b01101, &mut b);
        b
    }

    #[test]
    fn packets_with_their_crc() {
        let rate = 12e6;
        let ui = 40.0; // 300 kb/s
        let (mut e, mut t) = (vec![], 1000.0f64);
        let msgs: [(u16, Vec<u32>); 3] = [
            (0x0041, vec![]),
            (0x2161, vec![0x2601912C, 0x0A01912C]),
            (0x0243, vec![]),
        ];
        for (h, o) in &msgs {
            for b in packet(*h, o) {
                e.push(t.round() as i64);
                if b == 1 {
                    e.push((t + ui / 2.0).round() as i64);
                }
                t += ui;
            }
            e.push(t.round() as i64);
            if e.len() % 2 == 1 {
                // back to the idle level (low)
                e.push((t + ui).round() as i64);
            }
            t += 3000.0;
        }
        let v = View::new(rate, t as i64, vec![Channel::new("CC", 0, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let p = lay["packets"].as_array().unwrap();
        assert_eq!(p.len(), 3);
        assert!(p.iter().all(|x| x["ok"] == json!(true)), "{p:?}");
        assert_eq!(p[1]["header"], 0x2161);
        assert_eq!(p[1]["objects"], json!([0x2601912Cu32, 0x0A01912C]));
        assert_eq!(p[1]["type"], "Source_Capabilities");
    }
}
