//! Device packs (Python's `_engine/devices.py`): declarative descriptions of devices (`decl/devices/<bus>/...toml`)
//! matched against an I2C bus's transactions and an SPI bus's transfers.
//!
//! - register_map: a pointer write then a read; identity registers whose value names the chip.
//! - command_table: command codes with their response lengths and checks (SHT30's CRC, SPI NOR's JEDEC ID).
//!
//! Evidence levels: identified (an identity matched, or checks passed), consistent (the traffic fits the table),
//! address_only (only the bus address: never a claim, a candidate list). Only the packs' [device] headers are read up
//! front; a pack's tables are parsed when its bus address (or its bus) is first seen.

use serde_json::{Map, Value, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// WireSkein's own packs (src/wireskein/decl/devices), built in.
const BUILTIN: [(&str, &str); 4] = [
    (
        "i2c/qst/qmp6988.toml",
        include_str!("../../../src/wireskein/decl/devices/i2c/qst/qmp6988.toml"),
    ),
    (
        "i2c/sensirion/sht30.toml",
        include_str!("../../../src/wireskein/decl/devices/i2c/sensirion/sht30.toml"),
    ),
    (
        "spi/flash/spi_nor.toml",
        include_str!("../../../src/wireskein/decl/devices/spi/flash/spi_nor.toml"),
    ),
    (
        "spi/radio/cc1101.toml",
        include_str!("../../../src/wireskein/decl/devices/spi/radio/cc1101.toml"),
    ),
];

const FORMAT: &str = "wireskein-device/1";

#[derive(Debug, Clone)]
struct Header {
    bus: String,
    addresses: Vec<i64>,
}

enum Source {
    Text(&'static str),
    File(PathBuf),
}

struct Entry {
    rel: String,
    header: Header,
    source: Source,
}

/// The packs selected (by path patterns relative to their root: "i2c/**", "!i2c/qst/**"), their tables parsed when
/// first needed.
pub struct PackSet {
    entries: Vec<Entry>,
    full: RefCell<BTreeMap<usize, toml::Table>>,
}

fn int(v: &toml::Value) -> Option<i64> {
    v.as_integer()
}

/// The [device] header of a pack's text, read alone (the tables after it are not parsed).
fn header_of(text: &str) -> Option<Header> {
    let mut sect = String::new();
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if inside {
                break;
            }
            inside = t == "[device]";
        }
        if inside || t.starts_with("format") {
            sect.push_str(line);
            sect.push('\n');
        }
    }
    let d: toml::Table = toml::from_str(&sect).ok()?;
    if d.get("format").and_then(|v| v.as_str()) != Some(FORMAT) {
        return None;
    }
    let dev = d.get("device")?.as_table()?;
    Some(Header {
        bus: dev.get("bus")?.as_str()?.to_string(),
        addresses: dev
            .get("addresses")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(int).collect())
            .unwrap_or_default(),
    })
}

/// A path pattern matching a relative path: "**" any number of folders, "*" and "?" inside one name; "x/**"
/// everything below x.
fn glob(pat: &str, path: &str) -> bool {
    fn seg(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => seg(&p[1..], s) || (!s.is_empty() && s[0] != b'/' && seg(p, &s[1..])),
            (Some(b'?'), Some(&c)) if c != b'/' => seg(&p[1..], &s[1..]),
            (Some(&a), Some(&b)) if a == b => seg(&p[1..], &s[1..]),
            _ => false,
        }
    }
    fn parts(p: &[&str], s: &[&str]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&"**") => (0..=s.len()).any(|k| parts(&p[1..], &s[k..])),
            Some(x) => !s.is_empty() && seg(x.as_bytes(), s[0].as_bytes()) && parts(&p[1..], &s[1..]),
        }
    }
    if let Some(dir) = pat.strip_suffix("/**")
        && path.starts_with(&format!("{dir}/"))
    {
        return true;
    }
    let p: Vec<&str> = pat.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    parts(&p, &s)
}

fn selected(rel: &str, select: &[String]) -> bool {
    let inc: Vec<&str> = select
        .iter()
        .filter(|x| !x.starts_with('!'))
        .map(|x| x.as_str())
        .collect();
    let inc = if inc.is_empty() { vec!["**"] } else { inc };
    inc.iter().any(|g| glob(g, rel)) && !select.iter().filter_map(|x| x.strip_prefix('!')).any(|g| glob(g, rel))
}

impl PackSet {
    /// WireSkein's own packs, selected.
    pub fn builtin(select: &[String]) -> PackSet {
        let entries = BUILTIN
            .iter()
            .filter(|(rel, _)| selected(rel, select))
            .filter_map(|&(rel, text)| {
                Some(Entry {
                    rel: rel.into(),
                    header: header_of(text)?,
                    source: Source::Text(text),
                })
            })
            .collect();
        PackSet::in_order(entries)
    }

    /// One root's packs in the order Python's `PackSet` tries them: by their paths, folder by folder (Python's
    /// `sorted` of `Path`s: "i2c/a/x.toml" before "i2c/a-b/x.toml").
    fn in_order(mut entries: Vec<Entry>) -> PackSet {
        entries.sort_by(|a, b| a.rel.split('/').cmp(b.rel.split('/')));
        PackSet {
            entries,
            full: Default::default(),
        }
    }

    /// The packs under a directory (<bus>/.../<pack>.toml), selected: their headers read now, their tables later.
    pub fn from_dir(root: &Path, select: &[String]) -> PackSet {
        fn walk(d: &Path, out: &mut Vec<PathBuf>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            let mut es: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
            es.sort();
            for p in es {
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "toml") {
                    out.push(p);
                }
            }
        }
        let mut files = vec![];
        walk(root, &mut files);
        let entries = files
            .into_iter()
            .filter_map(|p| {
                let rel = p.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
                if !selected(&rel, select) {
                    return None;
                }
                let header = header_of(&std::fs::read_to_string(&p).ok()?)?;
                // the bus must be the top folder's
                (rel.split('/').next() == Some(header.bus.as_str())).then_some(Entry {
                    rel,
                    header,
                    source: Source::File(p),
                })
            })
            .collect();
        PackSet::in_order(entries)
    }

    /// The packs a caller picks (Python's `_upper._device_packs`): WireSkein's own and those under each directory
    /// (a definitions directory's `devices/`), all selected by the same patterns; no patterns (or none given) is
    /// "**", every pack. They are tried as Python's `PackSets` tries them: WireSkein's first, then each directory's
    /// in the order given, each root's by its paths.
    pub fn chosen<P: AsRef<Path>>(select: &[String], dirs: &[P]) -> PackSet {
        let all = ["**".to_string()];
        let select = if select.is_empty() { &all[..] } else { select };
        let mut set = PackSet::builtin(select);
        for d in dirs {
            set.entries.extend(PackSet::from_dir(d.as_ref(), select).entries);
        }
        set
    }

    /// The packs a hints object names (`analyze`'s): `devices` the patterns (Python's `hints["devices"]`),
    /// `device_dirs` the directories of packs beside WireSkein's (each definitions directory's `devices/`). Neither
    /// given: every built-in pack, as before.
    pub fn from_hints(hints: &Value) -> PackSet {
        let strs = |x: &Value| -> Vec<String> {
            x.as_array()
                .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        PackSet::chosen(&strs(&hints["devices"]), &strs(&hints["device_dirs"]))
    }

    /// The packs' paths (relative to their roots), in the order they are tried.
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.rel.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn pack(&self, k: usize) -> Option<toml::Table> {
        let e = &self.entries[k];
        if let Some(t) = self.full.borrow().get(&k) {
            return Some(t.clone());
        }
        let text = match &e.source {
            Source::Text(t) => t.to_string(),
            Source::File(p) => std::fs::read_to_string(p).ok()?,
        };
        let mut t: toml::Table = toml::from_str(&text).ok()?;
        t.insert("_file".into(), toml::Value::String(e.rel.clone()));
        self.full.borrow_mut().insert(k, t.clone());
        Some(t)
    }

    /// The I2C packs of any of these addresses, in the order the packs are tried.
    fn i2c(&self, addresses: &BTreeSet<i64>) -> Vec<toml::Table> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.header.bus == "i2c" && e.header.addresses.iter().any(|a| addresses.contains(a)))
            .filter_map(|(k, _)| self.pack(k))
            .collect()
    }

    fn bus(&self, bus: &str) -> Vec<toml::Table> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.header.bus == bus)
            .filter_map(|(k, _)| self.pack(k))
            .collect()
    }
}

/// A device pack's match.
#[derive(Debug, Clone)]
pub struct Match {
    pub device: String,
    pub level: &'static str,
    pub evidence: Map<String, Value>,
    pub messages: Vec<Value>,
}

fn crc8(data: &[u8], poly: u8, init: u8) -> u8 {
    let mut c = init;
    for &b in data {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ poly } else { c << 1 };
        }
    }
    c
}

/// An I2C transaction as the packs read it.
pub struct Tx {
    pub s: Value,
    pub addr: i64,
    pub read: Option<bool>,
    pub ack: bool,
    pub bytes: Vec<u8>,
}

fn device(p: &toml::Table) -> &toml::Table {
    p.get("device").and_then(|v| v.as_table()).expect("a pack's [device]")
}

fn list<'a>(p: &'a toml::Table, k: &str) -> Vec<&'a toml::Table> {
    p.get(k)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_table()).collect())
        .unwrap_or_default()
}

fn get_i(t: &toml::Table, k: &str, d: i64) -> i64 {
    t.get(k).and_then(int).unwrap_or(d)
}

fn be(b: &[u8]) -> i64 {
    b.iter().fold(0i64, |a, &x| (a << 8) | x as i64)
}

fn addresses(d: &toml::Table) -> Vec<i64> {
    d.get("addresses")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(int).collect())
        .unwrap_or_default()
}

fn name(d: &toml::Table) -> String {
    d.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn i2c_command_table(p: &toml::Table, txs: &[Tx]) -> Option<Match> {
    let d = device(p);
    let cb = get_i(d, "command_bytes", 2) as usize;
    let table: BTreeMap<i64, &toml::Table> = list(p, "command")
        .into_iter()
        .map(|c| (get_i(c, "code", -1), c))
        .collect();
    let crc = d.get("crc").and_then(|v| v.as_table());
    let addrs = addresses(d);
    let (mut known, mut unknown, mut passed, mut failed) = (0, 0, 0, 0);
    let mut msgs = vec![];
    let mut pending: Option<&toml::Table> = None;
    for t in txs {
        if !addrs.contains(&t.addr) {
            continue;
        }
        let b = &t.bytes;
        if t.read == Some(false) && b.len() >= cb {
            let code = be(&b[..cb]);
            match table.get(&code) {
                None => {
                    unknown += 1;
                    msgs.push(json!({"s": t.s, "command": format!("0x{code:0w$x}", w = 2 * cb), "known": false}));
                    pending = None;
                }
                Some(c) => {
                    known += 1;
                    msgs.push(json!({"s": t.s, "command": c.get("name").and_then(|v| v.as_str())}));
                    pending = (get_i(c, "response", 0) > 0).then_some(*c);
                }
            }
        } else if t.read == Some(true)
            && !b.is_empty()
            && let Some(c) = pending
        {
            let want = get_i(c, "response", 0) as usize;
            let ok_len = b.len() == want;
            let mut crc_ok = Value::Null;
            if let Some(cr) = crc
                && ok_len
            {
                let w = get_i(cr, "word", 2) as usize;
                let (poly, init) = (get_i(cr, "poly", 0x31) as u8, get_i(cr, "init", 0xFF) as u8);
                let oks: Vec<bool> = (0..b.len())
                    .step_by(w + 1)
                    .map(|i| {
                        b.get(i + w)
                            .is_some_and(|&x| crc8(&b[i..(i + w).min(b.len())], poly, init) == x)
                    })
                    .collect();
                let n = oks.iter().filter(|&&x| x).count();
                passed += n;
                failed += oks.len() - n;
                crc_ok = json!(oks.iter().all(|&x| x));
            }
            msgs.push(json!({"s": t.s, "response_to": c.get("name").and_then(|v| v.as_str()),
                             "bytes": crate::pulse::hex(b), "crc_ok": crc_ok}));
            pending = None;
        }
    }
    if known + unknown == 0 {
        return None;
    }
    let level = if passed >= 2 && failed == 0 {
        "identified"
    } else if known >= 3 && known as f64 / (known + unknown) as f64 >= 0.8 {
        "consistent"
    } else {
        "address_only"
    };
    let ev = json!({"known_commands": known, "unknown_commands": unknown, "crc_passed": passed, "crc_failed": failed});
    Some(Match {
        device: name(d),
        level,
        evidence: ev.as_object().unwrap().clone(),
        messages: msgs,
    })
}

fn i2c_register_map(p: &toml::Table, txs: &[Tx]) -> Option<Match> {
    let d = device(p);
    let pb = get_i(d, "pointer_bytes", 1) as usize;
    let ident: BTreeSet<(i64, i64)> = list(p, "identify")
        .iter()
        .map(|i| (get_i(i, "register", -1), get_i(i, "value", -1)))
        .collect();
    let regs: BTreeMap<i64, String> = list(p, "register")
        .iter()
        .map(|r| {
            (
                get_i(r, "address", -1),
                r.get("name").and_then(|v| v.as_str()).unwrap_or("").into(),
            )
        })
        .collect();
    let reg_name = |r: i64| regs.get(&r).cloned().unwrap_or_else(|| format!("0x{r:02x}"));
    let addrs = addresses(d);
    let (mut hits, mut misses, mut accesses) = (0, 0, 0);
    let mut msgs = vec![];
    let mut ptr: Option<i64> = None;
    for t in txs {
        if !addrs.contains(&t.addr) {
            continue;
        }
        let b = &t.bytes;
        if t.read == Some(false) && b.len() == pb {
            ptr = Some(be(b));
        } else if t.read == Some(false) && b.len() > pb {
            let reg = be(&b[..pb]);
            accesses += 1;
            msgs.push(json!({"s": t.s, "write": reg_name(reg), "value": crate::pulse::hex(&b[pb..])}));
        } else if t.read == Some(true)
            && !b.is_empty()
            && let Some(pt) = ptr
        {
            accesses += 1;
            for &(reg, val) in &ident {
                if reg == pt {
                    if b[0] as i64 == val {
                        hits += 1;
                    } else {
                        misses += 1;
                    }
                }
            }
            msgs.push(json!({"s": t.s, "read": reg_name(pt), "value": crate::pulse::hex(b)}));
            ptr = None;
        }
    }
    if accesses == 0 {
        return None;
    }
    let level = if hits >= 1 && misses == 0 {
        "identified"
    } else {
        "address_only"
    };
    let ev = json!({"identity_hits": hits, "identity_misses": misses, "accesses": accesses});
    Some(Match {
        device: name(d),
        level,
        evidence: ev.as_object().unwrap().clone(),
        messages: msgs,
    })
}

/// Identified packs whose evidence is the same (SHT30 / SHT31) are one claim, "one of these".
fn merge_indistinguishable(ms: Vec<Match>) -> Vec<Match> {
    let (ident, mut keep): (Vec<Match>, Vec<Match>) = ms.into_iter().partition(|m| m.level == "identified");
    let mut groups: Vec<(String, Vec<Match>)> = vec![];
    for m in ident {
        let key: Vec<(String, String)> = m
            .evidence
            .iter()
            .filter(|(k, _)| *k != "pack")
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect();
        let key = format!("{key:?}");
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1.push(m),
            None => groups.push((key, vec![m])),
        }
    }
    for (_, g) in groups {
        if g.len() == 1 {
            keep.extend(g);
        } else {
            let mut names: Vec<String> = g.iter().map(|m| m.device.clone()).collect();
            names.sort();
            let mut ev = g[0].evidence.clone();
            ev.insert("indistinguishable".into(), json!(names));
            ev.insert(
                "pack".into(),
                json!(g.iter().map(|m| m.evidence["pack"].clone()).collect::<Vec<_>>()),
            );
            keep.push(Match {
                device: names.join(" | "),
                level: "identified",
                evidence: ev,
                messages: g[0].messages.clone(),
            });
        }
    }
    keep
}

/// The packs on an I2C bus's transactions: identified / consistent ones are claims; packs that only share an address
/// one candidate list per address.
pub fn match_i2c(txs: &[Tx], packs: &PackSet) -> Vec<Match> {
    let seen: BTreeSet<i64> = txs.iter().filter(|t| t.ack).map(|t| t.addr).collect();
    let mut claims = vec![];
    let mut cands: BTreeMap<i64, BTreeSet<String>> = seen.iter().map(|&a| (a, BTreeSet::new())).collect();
    for p in packs.i2c(&seen) {
        let d = device(&p);
        let kind = d.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let m = if kind == "command_table" {
            i2c_command_table(&p, txs)
        } else {
            i2c_register_map(&p, txs)
        };
        let Some(mut m) = m else { continue };
        if m.level == "address_only" {
            for a in addresses(d) {
                if let Some(c) = cands.get_mut(&a) {
                    c.insert(m.device.clone());
                }
            }
            continue;
        }
        m.evidence.insert("pack".into(), json!(p["_file"].as_str()));
        claims.push(m);
    }
    let mut out = merge_indistinguishable(claims);
    let claimed: BTreeSet<String> = out
        .iter()
        .flat_map(
            |m| match m.evidence.get("indistinguishable").and_then(|v| v.as_array()) {
                Some(a) => a
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect::<Vec<_>>(),
                None => vec![m.device.clone()],
            },
        )
        .collect();
    for a in seen {
        let names: Vec<String> = cands[&a].difference(&claimed).cloned().collect();
        if !names.is_empty() {
            out.push(Match {
                device: "candidates".into(),
                level: "address_only",
                evidence: json!({"address": a, "chips": names}).as_object().unwrap().clone(),
                messages: vec![],
            });
        }
    }
    out
}

const SPI_TABLE_SELECTIVE: f64 = 0.25; // a command table fitting by chance at most this for "consistent"

/// The packs on an SPI bus's transfers ({pin: bytes of each transfer}), each pin tried as the command line.
pub fn match_spi(lines: &BTreeMap<String, Vec<Vec<u8>>>, packs: &PackSet) -> Vec<Match> {
    let mut out = vec![];
    for p in packs.bus("spi") {
        let d = device(&p);
        let cmds = list(&p, "command");
        let makers: BTreeMap<i64, String> = p
            .get("jedec_manufacturers")
            .and_then(|v| v.as_table())
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| {
                        Some((
                            i64::from_str_radix(k.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()?,
                            v.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let lookup = |b0: u8| -> Option<&toml::Table> {
            cmds.iter()
                .copied()
                .find(|c| (b0 as i64 & get_i(c, "mask", 0xFF)) == get_i(c, "code", -1))
        };
        let cover = (0..=255u8).filter(|&b| lookup(b).is_some()).count() as f64 / 256.0;
        let pins: Vec<&String> = lines.keys().collect();
        let mut best: Option<((u8, usize), Match)> = None;
        for &cmd_pin in &pins {
            let resp: Vec<&String> = pins.iter().copied().filter(|p| *p != cmd_pin).collect();
            let (mut known, mut unknown, mut consistent) = (0usize, 0usize, 0usize);
            let mut ident: BTreeSet<String> = BTreeSet::new();
            let mut msgs = vec![];
            for (i, f) in lines[cmd_pin].iter().enumerate() {
                if f.is_empty() {
                    continue;
                }
                let Some(c) = lookup(f[0]) else {
                    unknown += 1;
                    continue;
                };
                known += 1;
                let ab = get_i(c, "address_bytes", 0) as usize;
                let need = 1 + ab + get_i(c, "dummy_bytes", 0) as usize;
                let ok = match c.get("length").and_then(int) {
                    Some(l) => f.len() == l as usize,
                    None => f.len() >= need + c.get("response").is_some() as usize,
                };
                consistent += ok as usize;
                let mut rec = json!({"frame": i, "command": c.get("name").and_then(|v| v.as_str()), "length_ok": ok});
                if let Some(m) = c.get("mask").and_then(int) {
                    let arg = c.get("arg").and_then(|v| v.as_str()).unwrap_or("arg");
                    rec[arg] = json!(f[0] as i64 & !m & 0xFF);
                }
                if ab > 0 && f.len() >= need {
                    rec["address"] = json!(be(&f[1..1 + ab]));
                }
                for rp in &resp {
                    let r: &[u8] = lines[*rp].get(i).map(|v| v.as_slice()).unwrap_or(&[]);
                    let idf = c.get("identify").and_then(|v| v.as_str());
                    if idf == Some("jedec") && r.len() >= 4 {
                        let mid = r[1] as i64;
                        if let Some(mk) = makers.get(&mid)
                            && r[2..4] != [0, 0]
                            && r[2..4] != [0xFF, 0xFF]
                        {
                            ident.insert(format!("{mk} {}", crate::pulse::hex(&r[1..4])));
                            rec["jedec_id"] = json!(crate::pulse::hex(&r[1..4]));
                        }
                    }
                    if idf == Some("sfdp") && r.len() >= need + 4 && &r[need..need + 4] == b"SFDP" {
                        ident.insert("SFDP signature".into());
                    }
                    for idr in list(&p, "identify_read") {
                        let at = get_i(idr, "at", 1) as usize;
                        let vals: Vec<i64> = idr
                            .get("values")
                            .and_then(|v| v.as_array())
                            .map(|a| a.iter().filter_map(int).collect())
                            .unwrap_or_default();
                        if f[0] as i64 == get_i(idr, "byte", -1) && r.len() > at && vals.contains(&(r[at] as i64)) {
                            let nm = idr.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            ident.insert(format!("{nm} {:#04x}", r[at]));
                            rec["identity"] = json!(nm);
                        }
                    }
                }
                msgs.push(rec);
            }
            if known + unknown == 0 {
                continue;
            }
            let ratio = known as f64 / (known + unknown) as f64;
            let level = if !ident.is_empty() && ratio >= 0.5 {
                "identified"
            } else if known >= 3
                && ratio >= 0.8
                && consistent as f64 >= 0.8 * known as f64
                && cover <= SPI_TABLE_SELECTIVE
            {
                "consistent"
            } else {
                "none"
            };
            let rank = (
                match level {
                    "identified" => 2,
                    "consistent" => 1,
                    _ => 0,
                },
                known,
            );
            let ev = json!({"known_commands": known, "unknown_commands": unknown, "length_consistent": consistent,
                            "identity": ident.into_iter().collect::<Vec<_>>(), "command_pin": cmd_pin});
            if best.as_ref().is_none_or(|b| rank > b.0) {
                best = Some((
                    rank,
                    Match {
                        device: name(d),
                        level,
                        evidence: ev.as_object().unwrap().clone(),
                        messages: msgs,
                    },
                ));
            }
        }
        if let Some((_, m)) = best
            && m.level != "none"
        {
            out.push(m);
        }
    }
    out
}

/// The matches as the analysis exports them (candidates too).
pub fn exported(ms: &[Match]) -> Vec<Value> {
    ms.iter()
        .map(|m| {
            let mut o = json!({"device": m.device, "level": m.level, "evidence": m.evidence});
            if m.level != "address_only" {
                o["messages"] = json!(m.messages);
            }
            o
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(addr: i64, read: bool, bytes: &[u8]) -> Tx {
        Tx {
            s: json!(0),
            addr,
            read: Some(read),
            ack: true,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn an_sht30_by_its_crc_and_a_qmp6988_by_its_id() {
        let packs = PackSet::builtin(&["**".into()]);
        assert_eq!(packs.len(), 4);
        let (t, h) = ([0x66u8, 0x4C], [0x5Au8, 0x10]);
        let mut r = t.to_vec();
        r.push(crc8(&t, 0x31, 0xFF));
        r.extend(h);
        r.push(crc8(&h, 0x31, 0xFF));
        let txs = vec![
            tx(0x44, false, &[0x24, 0x00]),
            tx(0x44, true, &r),
            tx(0x70, false, &[0xD1]),
            tx(0x70, true, &[0x5C]),
        ];
        let ms = match_i2c(&txs, &packs);
        let names: Vec<(&str, &str)> = ms.iter().map(|m| (m.device.as_str(), m.level)).collect();
        assert!(names.contains(&("SHT30", "identified")), "{names:?}");
        assert!(names.contains(&("QMP6988", "identified")), "{names:?}");
        assert!(glob("i2c/**", "i2c/qst/qmp6988.toml") && !glob("spi/*", "spi/flash/spi_nor.toml"));
        assert_eq!(PackSet::builtin(&["i2c/**".into(), "!i2c/qst/**".into()]).len(), 1);
    }
}
