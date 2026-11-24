//! SD commands in SPI mode on an SPI bus's transfers (Python's `_engine/periph.sd_spi` and `_upper.sd_spi`): a
//! command is 6 bytes from the host (01 and the index, a 32-bit argument, CRC7 and the end bit), its R1 the first
//! byte from the card with bit 7 clear within 8 bytes after it (NCR); R7 / R3 / R2 bytes follow; R1b then busy. After
//! a read command the card's blocks (token 0xFE, data, CRC16), after a write the host's blocks each answered by a
//! data response. The evidence: commands with their end bit answered by an R1, the lines idle (0xFF) around them.

use serde_json::{Map, Value, json};

pub fn crc7(data: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in data {
        for k in (0..8).rev() {
            let bit = (b >> k) & 1;
            let top = (c >> 6) & 1;
            c = (c << 1) & 0x7F;
            if top ^ bit == 1 {
                c ^= 0x09;
            }
        }
    }
    c
}

pub fn crc16(data: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in data {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
        }
    }
    c
}

/// One byte of both lines: (tick, mosi, miso).
pub type Row = (i64, u8, u8);

struct Cmd {
    good: bool, // answered by an R1, its end bit set, the lines idle around it
    crc: Option<bool>,
    row: Map<String, Value>,
}

fn extra(cmd: u8, acmd: bool) -> usize {
    if acmd {
        (cmd == 13) as usize
    } else {
        match cmd {
            8 | 58 => 4,
            13 => 1,
            _ => 0,
        }
    }
}

fn commands(rows: &[Row]) -> Vec<Cmd> {
    let mo: Vec<u8> = rows.iter().map(|r| r.1).collect();
    let mi: Vec<u8> = rows.iter().map(|r| r.2).collect();
    let n = rows.len();
    let mut out = vec![];
    let mut blk: usize = 512;
    let mut acmd_next = false;
    let mut i = 0;
    // the card's block from j: (block, next index), or None (with the index) where no token is within reach
    let read_block = |j: usize, size: usize| -> (Option<Value>, usize) {
        let mut k = j;
        while k < n && mi[k] == 0xFF && k - j < 100_000 {
            if mo[k] & 0xC0 == 0x40 {
                return (None, k); // the host gave up waiting: a new command
            }
            k += 1;
        }
        if k >= n {
            return (None, k);
        }
        if mi[k] != 0xFE {
            return (
                Some(json!({"t": rows[k].0, "token": mi[k], "error_token": true})),
                k + 1,
            );
        }
        let d = &mi[(k + 1).min(n)..(k + 1 + size).min(n)];
        let c = &mi[(k + 1 + size).min(n)..(k + 3 + size).min(n)];
        let mut b = json!({"t": rows[k].0, "token": 0xFE, "len": d.len(), "data": crate::pulse::hex(d),
                           "nac_bytes": k - j});
        if d.len() == size && c.len() == 2 {
            b["crc_ok"] = json!(crc16(d) == ((c[0] as u16) << 8 | c[1] as u16));
        } else {
            b["cut"] = json!(true);
        }
        (Some(b), k + 3 + size)
    };
    while i + 6 <= n {
        let b0 = mo[i];
        if b0 & 0xC0 != 0x40 {
            i += 1;
            continue;
        }
        let cmdb = &mo[i..i + 6];
        let cmd = b0 & 0x3F;
        let acmd = acmd_next;
        let arg = u32::from_be_bytes([cmdb[1], cmdb[2], cmdb[3], cmdb[4]]);
        let crc_ok = crc7(&cmdb[..5]) == cmdb[5] >> 1;
        let end_bit = cmdb[5] & 1 == 1;
        let mut row = Map::new();
        row.insert("s".into(), json!(rows[i].0));
        row.insert(
            "cmd".into(),
            json!(format!("{}{cmd}", if acmd { "ACMD" } else { "CMD" })),
        );
        row.insert("arg".into(), json!(arg));
        acmd_next = false;
        let j = i + 6;
        let mut k = j;
        while k < n && k - j < 9 && mi[k] & 0x80 != 0 {
            k += 1;
        }
        if k >= n || mi[k] & 0x80 != 0 {
            row.insert("r1".into(), Value::Null);
            row.insert("crc_ok".into(), json!(crc_ok));
            out.push(Cmd {
                good: false,
                crc: ((cmd == 0 || cmd == 8) && !acmd).then_some(crc_ok),
                row,
            });
            i = j;
            continue;
        }
        let ncr = k - j;
        let r1 = mi[k];
        row.insert("r1".into(), json!(r1));
        k += 1;
        let ex = extra(cmd, acmd);
        if ex > 0 {
            row.insert("resp".into(), json!(crate::pulse::hex(&mi[k.min(n)..(k + ex).min(n)])));
            k += ex;
        }
        row.insert("ncr".into(), json!(ncr));
        row.insert("crc_ok".into(), json!(crc_ok));
        let idle_ok = mi[i..i + 6].iter().all(|&v| v == 0xFF) && mo[j..k.min(n)].iter().all(|&v| v == 0xFF);
        if [12, 28, 29, 38].contains(&cmd) && !acmd {
            while k < n && mi[k] == 0x00 {
                k += 1;
            }
        }
        if cmd == 55 && !acmd && r1 & 0xFE == 0 {
            acmd_next = true;
        }
        if cmd == 16 && !acmd && r1 == 0 {
            blk = arg as usize;
        }
        let mut blocks: Option<Vec<Value>> = None;
        if r1 == 0 {
            let size = if acmd {
                match cmd {
                    13 => Some(64),
                    22 => Some(4),
                    51 => Some(8),
                    _ => None,
                }
            } else {
                match cmd {
                    17 | 18 => Some(blk),
                    9 | 10 => Some(16),
                    _ => None,
                }
            };
            if let Some(size) = size.filter(|&s| s > 0) {
                let mut bs = vec![];
                loop {
                    let (b, k2) = read_block(k, size);
                    k = k2;
                    let Some(b) = b else { break };
                    let err = b.get("error_token").is_some();
                    bs.push(b);
                    if cmd != 18 || acmd || err {
                        break;
                    }
                }
                blocks = Some(bs);
            } else if (cmd == 24 || cmd == 25) && !acmd {
                let mut bs = vec![];
                while k < n {
                    while k < n && mo[k] == 0xFF {
                        k += 1;
                    }
                    if k >= n || !matches!(mo[k], 0xFC..=0xFE) {
                        break;
                    }
                    if mo[k] == 0xFD {
                        k += 2; // stop tran: busy follows
                        while k < n && mi[k] == 0x00 {
                            k += 1;
                        }
                        break;
                    }
                    let d = &mo[(k + 1).min(n)..(k + 1 + blk).min(n)];
                    let cr = &mo[(k + 1 + blk).min(n)..(k + 3 + blk).min(n)];
                    let ok = cr.len() == 2 && crc16(d) == ((cr[0] as u16) << 8 | cr[1] as u16);
                    let mut b = json!({"t": rows[k].0, "token": mo[k], "len": d.len(), "crc_ok": ok});
                    k += 3 + blk;
                    while k < n && mi[k] == 0xFF && mo[k] == 0xFF {
                        k += 1;
                    }
                    let mut resp = 0x05u8;
                    if k < n {
                        resp = mi[k];
                        b["data_response"] = json!(mi[k]);
                        k += 1;
                        while k < n && mi[k] == 0x00 {
                            k += 1;
                        }
                    }
                    bs.push(b);
                    if cmd == 24 || resp & 0x1F != 0x05 {
                        break;
                    }
                }
                blocks = Some(bs);
            }
        }
        if let Some(bs) = blocks {
            let shown: Vec<Value> = bs
                .iter()
                .map(|b| json!({"s": b["t"], "token": b["token"], "len": b.get("len"), "crc_ok": b.get("crc_ok")}))
                .collect();
            row.insert("blocks".into(), json!(shown));
        }
        out.push(Cmd {
            good: end_bit && idle_ok,
            crc: ((cmd == 0 || cmd == 8) && !acmd).then_some(crc_ok),
            row,
        });
        i = k.max(j);
    }
    out
}

/// SD in SPI mode on the bytes of two data lines, each tried as MOSI: (score, the `sd` layer), or None.
pub fn sd_spi(a: &[Row], b: &[Row]) -> Option<(f64, Vec<Value>)> {
    let mut best: Option<(f64, Vec<Value>)> = None;
    for rows in [a, b] {
        let cmds = commands(rows);
        let good: Vec<&Cmd> = cmds.iter().filter(|c| c.good).collect();
        if good.len() < 2 {
            continue;
        }
        let crc_all = good.iter().filter_map(|c| c.crc).all(|x| x);
        let score = good.len() as f64 / cmds.len() as f64
            * (1.0 - 0.5f64.powf(good.len() as f64 / 2.0))
            * if crc_all { 1.0 } else { 0.5 };
        if best.as_ref().is_none_or(|x| score > x.0) {
            best = Some((score, cmds.into_iter().map(|c| Value::Object(c.row)).collect()));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(cmd: u8, arg: u32, r1: u8, rows: &mut Vec<Row>) {
        let mut c = vec![0x40 | cmd];
        c.extend(arg.to_be_bytes());
        c.push((crc7(&c) << 1) | 1);
        for b in c {
            rows.push((rows.len() as i64 * 8, b, 0xFF));
        }
        rows.push((rows.len() as i64 * 8, 0xFF, 0xFF)); // NCR
        rows.push((rows.len() as i64 * 8, 0xFF, r1));
        rows.push((rows.len() as i64 * 8, 0xFF, 0xFF));
    }

    #[test]
    fn commands_answered() {
        let mut rows = vec![];
        command(0, 0, 0x01, &mut rows);
        command(55, 0, 0x01, &mut rows);
        command(41, 0x4000_0000, 0x00, &mut rows);
        command(16, 512, 0x00, &mut rows);
        assert_eq!(crc7(&[0x40, 0, 0, 0, 0]), 0x4A); // CMD0's CRC: 0x95 >> 1
        let swapped: Vec<Row> = rows.iter().map(|&(t, a, b)| (t, b, a)).collect();
        let (score, sd) = sd_spi(&swapped, &rows).unwrap();
        assert!(score > 0.7, "{score}");
        assert_eq!(sd[2]["cmd"], "ACMD41");
        assert_eq!(sd[0]["r1"], 1);
    }
}
