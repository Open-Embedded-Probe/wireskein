//! A WireSkein file's attachments and notes (wireskein-format §5; Python's `_fileformat.attach` and `note`): a
//! free-form file stored as `attach/NAME`, an entry appended to the capture's log as `notes/NNNN.json`. Both add an
//! entry at the zip's end and leave the others as they are; only an attachment replaced rewrites the file (the
//! others carried over as they are, as `markers::put`).

use crate::j::{J, py_str_repr};
use crate::wsk::{FORMAT, IDENT};
use std::io::{Read, Write};
use std::path::Path;

pub const ATTACH: &str = "attach/";
pub const NOTES: &str = "notes/";

/// Why an attachment or a note was not stored, told apart as Python's exceptions are.
#[derive(Debug, Clone, PartialEq)]
pub enum FileError {
    /// the attachment is there and `replace` was not given (FileExistsError)
    Exists(String),
    /// the file is not there (FileNotFoundError)
    NotFound(String),
    /// another error of the system (OSError)
    Os(String),
    /// not a zip (zipfile.BadZipFile)
    BadZip(String),
    /// not a WireSkein file of this version, a bad name (ValueError)
    Invalid(String),
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::Exists(m)
            | FileError::NotFound(m)
            | FileError::Os(m)
            | FileError::BadZip(m)
            | FileError::Invalid(m) => f.write_str(m),
        }
    }
}

impl FileError {
    /// An error of the system on `path`, as Python shows it.
    pub fn of_io(e: &std::io::Error, path: &Path) -> FileError {
        let m = os_error(e, path);
        if e.kind() == std::io::ErrorKind::NotFound {
            FileError::NotFound(m)
        } else {
            FileError::Os(m)
        }
    }
}

/// An OS error as Python shows it (`[Errno 2] No such file or directory: 'x'`), where it has a number.
pub fn os_error(e: &std::io::Error, path: &Path) -> String {
    let p = py_str_repr(&path.to_string_lossy());
    match e.raw_os_error() {
        Some(n) => {
            let text = e.to_string();
            let text = text.strip_suffix(&format!(" (os error {n})")).unwrap_or(&text);
            format!("[Errno {n}] {text}: {p}")
        }
        None => format!("{e}: {p}"),
    }
}

/// The zip of a WireSkein file of this version (Python's `_check`: its ident there and of this format).
fn open_checked(path: &Path) -> Result<zip::ZipArchive<std::fs::File>, FileError> {
    let f = std::fs::File::open(path).map_err(|e| FileError::of_io(&e, path))?;
    let mut z = zip::ZipArchive::new(f).map_err(|_| FileError::BadZip("File is not a zip file".into()))?;
    let ident = {
        let Ok(mut f) = z.by_name(IDENT) else {
            return Err(FileError::Invalid(format!(
                "{}: not a WireSkein file (no {IDENT})",
                path.display()
            )));
        };
        let mut s = String::new();
        f.read_to_string(&mut s)
            .map_err(|e| FileError::BadZip(format!("{IDENT}: {e}")))?;
        s
    };
    let fmt = J::parse(&ident).map_err(FileError::Invalid)?;
    let fmt = fmt.get("format");
    if fmt.s() != Some(FORMAT) {
        return Err(FileError::Invalid(format!(
            "{}: format {}; this version of wireskein reads {} only",
            path.display(),
            fmt.repr(),
            py_str_repr(FORMAT)
        )));
    }
    Ok(z)
}

/// One entry added at the zip's end (deflated), the others left as they are.
fn append(path: &Path, entry: &str, data: &[u8]) -> Result<(), FileError> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| FileError::of_io(&e, path))?;
    let os = |e: zip::result::ZipError| FileError::Os(format!("{}: {e}", path.display()));
    let mut w = zip::ZipWriter::new_append(f).map_err(os)?;
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    w.start_file(entry, o).map_err(os)?;
    w.write_all(data).map_err(|e| FileError::of_io(&e, path))?;
    w.finish().map_err(os)?;
    Ok(())
}

/// An attachment's entry (`attach/NAME`; Python's `_attach_name`): no empty name, none from the root, no "..".
pub fn attach_name(name: &str) -> Result<String, String> {
    if name.is_empty() || name.starts_with('/') || name.split('/').any(|x| x == "..") {
        return Err(format!("bad attachment name {}", py_str_repr(name)));
    }
    Ok(format!("{ATTACH}{name}"))
}

/// Store a free-form file in a WireSkein file as `attach/NAME` (Python's `_fileformat.attach`, the data as it is
/// stored: Python's str as UTF-8, dict / list as JSON with indent 1, bytes as they are). The channel data is not
/// rewritten unless an attachment of that name is replaced (`replace`).
pub fn attach(path: &Path, name: &str, data: &[u8], replace: bool) -> Result<(), FileError> {
    let entry = attach_name(name).map_err(FileError::Invalid)?;
    let exists = open_checked(path)?.index_for_name(&entry).is_some();
    if exists && !replace {
        return Err(FileError::Exists(format!(
            "{}: {entry} exists (replace=True to overwrite)",
            path.display()
        )));
    }
    if exists {
        crate::markers::put(path, &entry, data).map_err(FileError::Os)
    } else {
        append(path, &entry, data)
    }
}

/// The time now as Python's `datetime.now().astimezone().isoformat(timespec="seconds")`: the local time and its
/// offset from UTC ("2026-11-25T14:03:07+09:00").
pub fn now_local() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

/// Append one entry to the capture's log (Python's `_fileformat.note`; never rewrites the others):
/// {"time": ISO 8601, "content": ..., **fields}, written as Python's `json.dumps` writes it. Its number.
pub fn note(path: &Path, content: &J, fields: &[(String, J)]) -> Result<usize, FileError> {
    note_at(path, content, fields, &now_local())
}

/// `note` at a given time (its text as the entry keeps it).
pub fn note_at(path: &Path, content: &J, fields: &[(String, J)], time: &str) -> Result<usize, FileError> {
    let n = open_checked(path)?
        .file_names()
        .filter(|x| x.starts_with(NOTES))
        .count()
        + 1;
    let mut entry = J::Dict(vec![
        ("time".into(), J::from(time)),
        ("content".into(), content.clone()),
    ]);
    for (k, v) in fields {
        entry.set(k, v.clone());
    }
    append(path, &format!("{NOTES}{n:04}.json"), entry.dumps_py().as_bytes())?;
    Ok(n)
}

/// The notes of a WireSkein file, in their numbers' order (Python's `_fileformat.notes`).
pub fn notes(path: &Path) -> Result<Vec<J>, String> {
    let mut z = open_checked(path).map_err(|e| e.to_string())?;
    let mut names: Vec<String> = z
        .file_names()
        .filter(|x| x.starts_with(NOTES))
        .map(String::from)
        .collect();
    names.sort();
    let mut out = vec![];
    for n in names {
        let mut s = String::new();
        z.by_name(&n)
            .map_err(|e| e.to_string())?
            .read_to_string(&mut s)
            .map_err(|e| format!("{n}: {e}"))?;
        out.push(J::parse(&s)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wireskein-io-attach-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        d
    }

    fn file(path: &Path) {
        let f = std::fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default();
        w.start_file(IDENT, o).unwrap();
        w.write_all(br#"{"format": "wireskein/1"}"#).unwrap();
        w.start_file("capture.json", o).unwrap();
        w.write_all(b"{}").unwrap();
        w.finish().unwrap();
    }

    fn read(path: &Path, entry: &str) -> Option<Vec<u8>> {
        let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut f = z.by_name(entry).ok()?;
        let mut out = vec![];
        f.read_to_end(&mut out).unwrap();
        Some(out)
    }

    #[test]
    fn attachments_added_refused_replaced() {
        let p = tmp("a.wireskein");
        file(&p);
        attach(&p, "x/readme.txt", b"one", false).unwrap();
        assert_eq!(read(&p, "attach/x/readme.txt").unwrap(), b"one");
        assert!(matches!(
            attach(&p, "x/readme.txt", b"two", false),
            Err(FileError::Exists(_))
        ));
        attach(&p, "x/readme.txt", b"two", true).unwrap();
        assert_eq!(read(&p, "attach/x/readme.txt").unwrap(), b"two");
        assert_eq!(read(&p, IDENT).unwrap(), br#"{"format": "wireskein/1"}"#);
        for bad in ["", "/abs", "a/../b"] {
            assert!(attach_name(bad).is_err());
        }
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn notes_numbered_in_order() {
        let p = tmp("n.wireskein");
        file(&p);
        let kind = vec![("kind".to_string(), J::from("setup"))];
        assert_eq!(
            note_at(&p, &J::from("first"), &[], "2026-11-25T14:03:07+09:00").unwrap(),
            1
        );
        assert_eq!(
            note_at(&p, &J::parse(r#"{"a": [1, 2.5]}"#).unwrap(), &kind, "t").unwrap(),
            2
        );
        assert_eq!(
            String::from_utf8(read(&p, "notes/0001.json").unwrap()).unwrap(),
            r#"{"time": "2026-11-25T14:03:07+09:00", "content": "first"}"#
        );
        assert_eq!(
            String::from_utf8(read(&p, "notes/0002.json").unwrap()).unwrap(),
            r#"{"time": "t", "content": {"a": [1, 2.5]}, "kind": "setup"}"#
        );
        assert_eq!(notes(&p).unwrap().len(), 2);
        let t = now_local();
        assert_eq!(t.len(), 25, "{t}");
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn not_a_wireskein_file() {
        let p = tmp("x.zip");
        std::fs::write(&p, b"not a zip").unwrap();
        assert_eq!(
            note(&p, &J::Null, &[]).unwrap_err(),
            FileError::BadZip("File is not a zip file".into())
        );
        std::fs::remove_file(&p).unwrap();
        assert!(matches!(note(&p, &J::Null, &[]), Err(FileError::NotFound(m)) if m.starts_with("[Errno 2] ")));
    }
}
