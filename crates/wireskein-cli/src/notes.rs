//! `note` and `attach`: an entry appended to a WireSkein file's log, a free-form file stored in it (Python's
//! `wireskein note` / `wireskein attach`, `_fileformat.note` / `attach`).

use crate::{Args, Takes, fail, usage_error};
use std::path::Path;
use std::process::ExitCode;
use wireskein_io::attach::{self, FileError};
use wireskein_io::j::J;

/// `wireskein note FILE TEXT [--json] [--kind K]`: the note's number printed.
pub fn note(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &["--kind"],
        flags: &["--json"],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("note", &e),
    };
    let [file, text] = a.pos.as_slice() else {
        return usage_error("note", "give FILE and TEXT");
    };
    let content = if a.has("--json") {
        match J::parse(text) {
            Ok(v) => v,
            Err(e) => return fail("note", &format!("JSONDecodeError: {e}")),
        }
    } else {
        J::from(text.as_str())
    };
    let fields: Vec<(String, J)> = a
        .get("--kind")
        .filter(|k| !k.is_empty())
        .map(|k| ("kind".to_string(), J::from(k)))
        .into_iter()
        .collect();
    match attach::note(Path::new(file), &content, &fields) {
        Ok(n) => {
            println!("{n}");
            ExitCode::SUCCESS
        }
        Err(e) => fail("note", &py_error(&e)),
    }
}

/// `wireskein attach FILE NAME [SRC | --text TEXT] [--replace]`.
pub fn attach(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &["--text"],
        flags: &["--replace"],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("attach", &e),
    };
    let (file, name, src) = match a.pos.as_slice() {
        [f, n] => (f, n, None),
        [f, n, s] => (f, n, Some(s)),
        _ => return usage_error("attach", "give FILE, NAME and SRC or --text"),
    };
    let data = match (src, a.get("--text")) {
        (Some(s), None) => match std::fs::read(s) {
            Ok(d) => d,
            Err(e) => return fail("attach", &attach::os_error(&e, Path::new(s))),
        },
        (None, Some(t)) => t.as_bytes().to_vec(),
        _ => {
            // (Python's sys.exit with no command named)
            eprintln!("give SRC (a file) or --text");
            return ExitCode::FAILURE;
        }
    };
    match attach::attach(Path::new(file), name, &data, a.has("--replace")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail("attach", &py_error(&e)),
    }
}

/// An error as Python's command line shows it: the exception's name before the message, but for ValueError,
/// FileNotFoundError and BadZipFile (`cli.main`).
fn py_error(e: &FileError) -> String {
    match e {
        FileError::Exists(m) => format!("FileExistsError: {m}"),
        FileError::Os(m) if m.starts_with("[Errno 13]") => format!("PermissionError: {m}"),
        FileError::Os(m) if m.starts_with("[Errno 21]") => format!("IsADirectoryError: {m}"),
        FileError::Os(m) => format!("OSError: {m}"),
        FileError::NotFound(m) | FileError::BadZip(m) | FileError::Invalid(m) => m.clone(),
    }
}
