//! A WireSkein file's markers (wireskein-format §5.2; Python's `_fileformat.markers`, `set_markers`, `put`): read,
//! and written in place of the earlier ones, the file's other entries carried over as they are.

use crate::j::J;
use crate::wsk::{CAPTURE, FORMAT, IDENT};
use std::io::{Read, Write};
use std::path::Path;

pub const MARKERS: &str = "markers/markers.json";
pub const MARKERS_FORMAT: &str = "wireskein-markers/1";

fn open(path: &Path) -> Result<zip::ZipArchive<std::fs::File>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    zip::ZipArchive::new(f).map_err(|e| format!("{}: not a zip: {e}", path.display()))
}

fn read<R: Read + std::io::Seek>(z: &mut zip::ZipArchive<R>, name: &str) -> Result<Option<Vec<u8>>, String> {
    let Ok(mut f) = z.by_name(name) else { return Ok(None) };
    let mut out = vec![];
    f.read_to_end(&mut out).map_err(|e| format!("{name}: {e}"))?;
    Ok(Some(out))
}

/// The header of a WireSkein file of this version (an error for anything else).
fn header<R: Read + std::io::Seek>(z: &mut zip::ZipArchive<R>, path: &Path) -> Result<J, String> {
    let id = read(z, IDENT)?.ok_or_else(|| format!("{}: not a WireSkein file (no {IDENT})", path.display()))?;
    let id = J::parse(&String::from_utf8_lossy(&id))?;
    if id.get("format").s() != Some(FORMAT) {
        return Err(format!(
            "{}: format {}; this version of wireskein reads {FORMAT:?} only",
            path.display(),
            id.get("format").repr()
        ));
    }
    let head = read(z, CAPTURE)?.ok_or_else(|| format!("{}: holds no capture", path.display()))?;
    J::parse(&String::from_utf8_lossy(&head))
}

/// capture.json of a WireSkein file of this version, its keys in the file's order (Python's
/// `_fileformat.read_header`, without its checks of `complete` and `layout`).
pub fn read_header(path: &Path) -> Result<J, String> {
    header(&mut open(path)?, path)
}

/// The entries whose names start with `prefix`, in the zip's order (Python's `attachments` with "attach/").
pub fn entries(path: &Path, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut z = open(path)?;
    let names: Vec<String> = z
        .file_names()
        .filter(|n| n.starts_with(prefix))
        .map(String::from)
        .collect();
    let mut order: Vec<(usize, String)> = names
        .into_iter()
        .filter_map(|n| z.index_for_name(&n).map(|i| (i, n)))
        .collect();
    order.sort();
    let mut out = vec![];
    for (_, n) in order {
        if let Some(d) = read(&mut z, &n)? {
            out.push((n, d));
        }
    }
    Ok(out)
}

/// The entries a capture carries into another file (Python's `_fileformat.extras` of a WireSkein file: every entry
/// but the capture itself; of a .sr, its attachments, notes and the entries its extra lists), in the zip's order;
/// none for a VCD.
pub fn extras(path: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    match crate::sniff_path(path) {
        Some(crate::Kind::WireSkein) => {
            let head = header(&mut open(path)?, path)?;
            let own: Vec<String> = head
                .get("parts")
                .list()
                .iter()
                .flat_map(|p| p.get("channels").list().iter().filter_map(|c| c.get("file").s()))
                .map(String::from)
                .collect();
            Ok(entries(path, "")?
                .into_iter()
                .filter(|(n, _)| n != IDENT && n != CAPTURE && !own.contains(n) && !n.ends_with('/'))
                .collect())
        }
        Some(crate::Kind::Sr) => {
            let mut z = open(path)?;
            let listed: Vec<String> = match read(&mut z, crate::sr::EXTRA)? {
                Some(d) => {
                    let x = J::parse(&String::from_utf8_lossy(&d)).unwrap_or_default();
                    if x.get("format").s() == Some("wireskein-sr-extra/1") {
                        x.get("extras")
                            .list()
                            .iter()
                            .filter_map(|v| v.s().map(String::from))
                            .collect()
                    } else {
                        vec![]
                    }
                }
                None => vec![],
            };
            Ok(entries(path, "")?
                .into_iter()
                .filter(|(n, _)| n.starts_with("attach/") || n.starts_with("notes/") || listed.contains(n))
                .collect())
        }
        _ => Ok(vec![]),
    }
}

/// The markers of a WireSkein file ([{"t", "end"?, "label", "note"?, ...}], their keys in the file's order).
pub fn markers(path: &Path) -> Result<Vec<J>, String> {
    let mut z = open(path)?;
    let Some(data) = read(&mut z, MARKERS)? else {
        return Ok(vec![]);
    };
    let doc = J::parse(&String::from_utf8_lossy(&data))?;
    if doc.get("format").s() != Some(MARKERS_FORMAT) {
        return Err(format!(
            "{}: markers format {}, this version reads {MARKERS_FORMAT:?}",
            path.display(),
            doc.get("format").repr()
        ));
    }
    let out = doc.get("markers").list().to_vec();
    let n = header(&mut z, path)?.get("parts").list().len() as i64;
    let bad: Vec<String> = out
        .iter()
        .filter(|m| !(0..n).contains(&m.get("part").i().unwrap_or(0)))
        .take(3)
        .map(|m| m.repr())
        .collect();
    if !bad.is_empty() {
        return Err(format!(
            "{}: markers of a part the file does not have (of {n}): [{}]",
            path.display(),
            bad.join(", ")
        ));
    }
    Ok(out)
}

/// `json.dumps(v, indent=1, ensure_ascii=False)`.
pub fn dumps_indent(v: &J) -> String {
    fn go(v: &J, depth: usize, out: &mut String) {
        let pad = |d: usize| format!("\n{}", " ".repeat(d));
        match v {
            J::List(a) if !a.is_empty() => {
                out.push('[');
                for (k, x) in a.iter().enumerate() {
                    if k > 0 {
                        out.push(',');
                    }
                    out.push_str(&pad(depth + 1));
                    go(x, depth + 1, out);
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            J::Dict(a) if !a.is_empty() => {
                out.push('{');
                for (k, (key, x)) in a.iter().enumerate() {
                    if k > 0 {
                        out.push(',');
                    }
                    out.push_str(&pad(depth + 1));
                    out.push_str(&J::from(key.as_str()).dumps());
                    out.push_str(": ");
                    go(x, depth + 1, out);
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
            x => x.write_json(out),
        }
    }
    let mut s = String::new();
    go(v, 0, &mut s);
    s
}

/// Store one entry (not the capture itself) in place of an older one, the others carried over as they are.
pub fn put(path: &Path, entry: &str, data: &[u8]) -> Result<(), String> {
    if entry.is_empty() || entry.starts_with('/') || entry.split('/').any(|x| x == "..") || entry.ends_with('/') {
        return Err(format!("bad entry name {entry:?}"));
    }
    let mut z = open(path)?;
    let head = header(&mut z, path)?;
    let own: Vec<&str> = head
        .get("parts")
        .list()
        .iter()
        .flat_map(|p| p.get("channels").list().iter().filter_map(|c| c.get("file").s()))
        .collect();
    if entry == IDENT || entry == CAPTURE || own.contains(&entry) {
        return Err(format!("{entry} is part of the capture itself"));
    }
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().map(|s| s.to_string_lossy()).unwrap_or_default()
    ));
    {
        let f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let mut w = zip::ZipWriter::new(f);
        for i in 0..z.len() {
            let f = z.by_index_raw(i).map_err(|e| e.to_string())?;
            if f.name() == entry {
                continue;
            }
            w.raw_copy_file(f).map_err(|e| e.to_string())?;
        }
        let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        w.start_file(entry, o).map_err(|e| e.to_string())?;
        w.write_all(data).map_err(|e| e.to_string())?;
        w.finish().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Replace the markers (Python's `set_markers`): each needs a number "t" and a text "label"; "end" a number; "part"
/// a part of the file (in a file of several parts always written, 0 when not given); keys of no value left out.
pub fn set_markers(path: &Path, items: &[J]) -> Result<(), String> {
    let n = {
        let mut z = open(path)?;
        header(&mut z, path)?.get("parts").list().len() as i64
    };
    let num = |v: &J| matches!(v, J::Int(_) | J::Float(_));
    let mut clean = vec![];
    for m in items {
        if !num(m.get("t")) || m.get("label").s().is_none() {
            return Err(format!("a marker needs a number t and a text label: {}", m.repr()));
        }
        if !m.get("end").is_null() && !num(m.get("end")) {
            return Err(format!("marker end must be a number: {}", m.repr()));
        }
        let part = m.get("part");
        if !part.is_null() && !matches!(part, J::Int(k) if (0..n).contains(k)) {
            return Err(format!(
                "marker part must be a part of the file (0 .. {}): {}",
                n - 1,
                m.repr()
            ));
        }
        let mut d: Vec<(String, J)> = m.items().iter().filter(|(_, v)| !v.is_null()).cloned().collect();
        if n > 1 {
            let k = part.i().unwrap_or(0);
            match d.iter_mut().find(|(a, _)| a == "part") {
                Some(x) => x.1 = J::Int(k),
                None => d.push(("part".into(), J::Int(k))),
            }
        }
        clean.push(J::Dict(d));
    }
    let doc = J::Dict(vec![
        ("format".into(), J::from(MARKERS_FORMAT)),
        ("markers".into(), J::List(clean)),
    ]);
    put(path, MARKERS, dumps_indent(&doc).as_bytes())
}

/// Put markers of one's own ("by": a tool's name) in place of that one's earlier ones; others' markers stay.
pub fn add_markers(path: &Path, items: &[J], by: &str) -> Result<(), String> {
    if by.is_empty() || by == "viewer" {
        return Err(format!("by names the tool that places them (not {by:?})"));
    }
    let mut all: Vec<J> = markers(path)?
        .into_iter()
        .filter(|m| m.get("by").s() != Some(by))
        .collect();
    for m in items {
        let mut d = m.items().to_vec();
        d.retain(|(k, _)| k != "by");
        d.push(("by".into(), J::from(by)));
        all.push(J::Dict(d));
    }
    set_markers(path, &all)
}
