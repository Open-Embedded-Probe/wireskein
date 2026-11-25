//! The session id a host that may stop and run again keeps per probe (host guide §5, transports §3;
//! oep-client-python's `kept_session`, the same file so either client ends what the other left).
//!
//! A probe keeps a session when its transport closes: a capture killed half way leaves its lock until the lease runs
//! out. So the host keeps the id of the session it has open in a file per probe, keyed by its unit_id, and the next
//! run, before its first open, opens that id and ends it at once (core §6.2: the holder's open again keeps the
//! session; the end that follows releases what it held). Both carry corrs ahead of every one the run before sent: the
//! file keeps, with the id, a corr reserved ahead of what the run has used, renewed `RESERVE` corrs at a time.
//!
//! The file is `<directory>/<unit_id>.session`: the id and the reserved corr as hex (`ssssssss cccc`), empty when no
//! session is open. The directory is $OEP_SESSION_DIR, else $XDG_RUNTIME_DIR/oep-client, else the user's cache
//! directory (%LOCALAPPDATA%\oep-client\sessions on Windows, $XDG_CACHE_HOME or ~/.cache /oep-client/sessions
//! elsewhere). While a host keeps the file it holds an exclusive lock on it (flock, as Python's fcntl.flock), dropped
//! when the file is let go or the process ends however it ends; a file another live process holds is left alone.
//! On a system without flock here (Windows) nothing is kept.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const ENV: &str = "OEP_SESSION_DIR";
/// corrs reserved at a time: far below half the space (core §5.1)
pub const RESERVE: u16 = 1024;

/// The corr `n` after `corr` in the cycle that never uses 0 (core §4.1).
pub fn ahead(corr: u16, n: u16) -> u16 {
    ((corr as u32 - 1 + n as u32) % 0xFFFF + 1) as u16
}

pub fn default_dir() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(d) = env(ENV) {
        return PathBuf::from(d);
    }
    if let Some(d) = env("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("oep-client");
    }
    if cfg!(windows)
        && let Some(d) = env("LOCALAPPDATA")
    {
        return PathBuf::from(d).join("oep-client").join("sessions");
    }
    let cache = env("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"));
    cache.join("oep-client").join("sessions")
}

/// core §7.5's unit_id grammar ([a-z0-9-], 1-32): safe as a file name.
fn safe(unit: &str) -> bool {
    (1..=32).contains(&unit.len())
        && unit
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(unix)]
fn lock(f: &File) -> bool {
    rustix::fs::flock(f, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok()
}

#[cfg(not(unix))]
fn lock(_: &File) -> bool {
    false
}

/// What `before_open` found: the session the run before left (and the corr it reserved), to be ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Previous {
    pub sid: u32,
    pub corr: u16,
}

/// One host's kept session id (`Host::kept`).
#[derive(Debug, Default)]
pub struct KeptSession {
    pub directory: Option<PathBuf>,
    pub path: Option<PathBuf>,
    file: Option<File>,
    /// before_open ran (kept or not)
    pub done: bool,
    /// the id found from the run before (None: none) and the corr it reserved
    pub previous: Option<u32>,
    pub previous_corr: Option<u16>,
    pub ended_previous: bool,
    pub sid: Option<u32>,
    /// every corr this run sends stays behind this one
    pub reserved: Option<u16>,
    /// why nothing is kept, if so
    pub why_not: String,
}

impl KeptSession {
    pub fn new(directory: Option<PathBuf>) -> KeptSession {
        KeptSession {
            directory,
            ..Default::default()
        }
    }

    /// Once before the first open: the file for `unit` found and locked. -> the session the run before left there
    /// (with a reserved corr), when it is not `current`, for the host to end.
    pub fn take_file(&mut self, unit: Option<&str>, current: Option<u32>) -> Option<Previous> {
        if self.done {
            return None;
        }
        self.done = true;
        let unit = match unit {
            Some(u) if !u.starts_with("x-") && safe(u) => u.to_string(),
            u => {
                self.why_not = format!("unit_id {u:?} names no unit");
                return None;
            }
        };
        let dir = self.directory.clone().unwrap_or_else(default_dir);
        let path = dir.join(format!("{unit}.session"));
        let opened = (|| -> std::io::Result<File> {
            create_private_dir(&dir)?;
            open_private(&path)
        })();
        let mut f = match opened {
            Ok(f) => f,
            Err(e) => {
                self.why_not = format!("cannot keep it: {e}");
                return None;
            }
        };
        if !lock(&f) {
            self.why_not = format!("{} is kept by another running host", path.display());
            return None;
        }
        let mut text = String::new();
        let _ = f.seek(SeekFrom::Start(0));
        let _ = (&f).take(64).read_to_string(&mut text);
        let words: Vec<&str> = text.split_whitespace().collect();
        let sid = words.first().and_then(|w| u32::from_str_radix(w, 16).ok());
        let corr = words.get(1).and_then(|w| u16::from_str_radix(w, 16).ok());
        let bad = (!words.is_empty() && sid.is_none()) || (words.len() > 1 && corr.is_none());
        (self.previous, self.previous_corr) = if bad {
            (None, None)
        } else {
            (sid.filter(|&s| s != 0), corr)
        };
        self.path = Some(path);
        self.file = Some(f);
        match (self.previous, self.previous_corr) {
            (Some(s), Some(c)) if Some(s) != current && c != 0 => Some(Previous { sid: s, corr: c }),
            _ => None,
        }
    }

    fn write(&mut self, text: &str) {
        if let Some(f) = self.file.as_mut() {
            let _ = f.seek(SeekFrom::Start(0));
            let _ = f.set_len(0);
            let _ = f.write_all(text.as_bytes());
        }
    }

    /// Session `sid` opened with `corr`: keep its id and reserve the corrs after it.
    pub fn opened(&mut self, sid: u32, corr: u16) {
        self.sid = Some(sid);
        self.reserve(corr);
    }

    /// A request goes out with `corr`: once it nears the reservation, reserve the next block.
    pub fn sent(&mut self, corr: u16) {
        if let (Some(_), Some(r)) = (self.sid, self.reserved) {
            let d = (r as i32 - corr as i32).rem_euclid(0x10000);
            if !(0 < d && d < 0x8000) {
                self.reserve(corr);
            }
        }
    }

    fn reserve(&mut self, corr: u16) {
        let r = ahead(corr, RESERVE);
        self.reserved = Some(r);
        let sid = self.sid.unwrap_or(0);
        self.write(&format!("{sid:08x} {r:04x}\n"));
    }

    pub fn ended(&mut self) {
        self.sid = None;
        self.reserved = None;
        self.write("");
    }

    /// Let the file go (the id stays in it for the next run when a session is still open).
    pub fn release(&mut self) {
        self.file = None;
        self.done = false;
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn open_private(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrs_ahead_skip_zero() {
        assert_eq!(ahead(1, 1), 2);
        assert_eq!(ahead(0xFFFF, 1), 1);
        assert_eq!(ahead(0xFFF0, RESERVE), 1009);
    }

    #[test]
    fn the_file_as_python_writes_it() {
        let dir = std::env::temp_dir().join(format!("oep-kept-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut k = KeptSession::new(Some(dir.clone()));
        assert_eq!(k.take_file(Some("abc-1"), None), None);
        k.opened(0x1234_abcd, 7);
        let text = std::fs::read_to_string(dir.join("abc-1.session")).unwrap();
        assert_eq!(text, "1234abcd 0407\n");
        // another host of this process cannot take the file while it is kept (flock is per open file)
        let mut other = KeptSession::new(Some(dir.clone()));
        if cfg!(unix) {
            assert_eq!(other.take_file(Some("abc-1"), None), None);
            assert!(other.why_not.contains("another running host"));
        }
        k.release();
        let mut next = KeptSession::new(Some(dir.clone()));
        assert_eq!(
            next.take_file(Some("abc-1"), None),
            Some(Previous {
                sid: 0x1234_abcd,
                corr: 0x0407
            })
        );
        next.ended();
        assert_eq!(std::fs::read_to_string(dir.join("abc-1.session")).unwrap(), "");
        let mut x = KeptSession::new(Some(dir.clone()));
        assert_eq!(x.take_file(Some("x-virtual"), None), None);
        assert!(x.path.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
