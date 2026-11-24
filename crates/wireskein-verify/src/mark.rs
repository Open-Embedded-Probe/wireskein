//! What the checks found written into each capture file's markers (Python's `verify._mark`; `wireskein verify
//! --mark`): every notice and every change `becomes` found, at its tick, by "verify"; the markers of others kept,
//! verify's earlier ones replaced.

use crate::J;
use std::collections::BTreeMap;
use std::path::Path;

/// The markers of a report, by capture file.
pub fn markers_of(rep: &J) -> BTreeMap<String, Vec<J>> {
    let mut per: BTreeMap<String, Vec<J>> = BTreeMap::new();
    let mk = |t: &J, part: &J, label: String| {
        J::Dict(vec![
            ("t".into(), t.clone()),
            ("part".into(), part.clone()),
            ("label".into(), J::from(label)),
            ("by".into(), J::from("verify")),
        ])
    };
    for r in rep.get("results").list() {
        let Some(cap) = r.get("capture").s().filter(|s| !s.is_empty()) else {
            continue;
        };
        let f = cap
            .split('#')
            .next()
            .unwrap_or("")
            .split('@')
            .next()
            .unwrap_or("")
            .to_string();
        let path = r.get("path").s().unwrap_or("");
        for m in r.get("notices").list() {
            let label = format!(
                "{}: {path}",
                m.get("kind").s().map_or_else(|| m.get("kind").repr(), String::from)
            );
            per.entry(f.clone())
                .or_default()
                .push(mk(m.get("tick"), m.get("part"), label));
        }
        let measured = r.get("measured");
        let got = if measured.has("outer") {
            measured.get("outer")
        } else {
            measured
        };
        if r.get("check").s() == Some("becomes") && !got.get("change_tick").is_null() {
            let part = if got.has("change_part") {
                got.get("change_part").clone()
            } else {
                J::Int(0)
            };
            per.entry(f)
                .or_default()
                .push(mk(got.get("change_tick"), &part, format!("change: {path}")));
        }
    }
    per
}

/// Each capture file of the run given the report's markers (verify's earlier ones replaced).
pub fn mark(run_dir: &Path, rep: &J) -> Result<(), String> {
    let per = markers_of(rep);
    let run = std::fs::read_to_string(run_dir.join("run.json")).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    let doc = J::parse(&run)?;
    let mut files: Vec<&str> = doc
        .get("captures")
        .list()
        .iter()
        .filter_map(|c| c.get("file").s())
        .collect();
    files.sort_unstable();
    files.dedup();
    for f in files {
        let p = run_dir.join(f);
        let mut all: Vec<J> = wireskein_io::markers::markers(&p)?
            .into_iter()
            .filter(|m| m.get("by").s() != Some("verify"))
            .collect();
        all.extend(per.get(f).cloned().unwrap_or_default());
        wireskein_io::markers::set_markers(&p, &all)?;
    }
    Ok(())
}
