//! OEP v1 messages (oep-spec oep-core §2-§5, §10; oep-client-python's `message`): a request's header, a result's, the
//! TLV tails, the readers of fixed parts, and the errors a request ends in.

use std::fmt;

pub const ROLE_REQUEST: u8 = 0x01;
pub const ROLE_RESULT: u8 = 0x02;
pub const ROLE_EVENT: u8 = 0x03;
pub const ROLE_DATA: u8 = 0x04;
pub const REQUEST_HEADER: usize = 10;
pub const RESULT_HEADER: usize = 5;
pub const DATA_HEADER: usize = 5;
pub const EVENT_HEADER: usize = 6;

pub const REJECTED: u8 = 0x00;
pub const COMPLETED: u8 = 0x01;
pub const SUCCESS: u8 = 0x00;

pub const UNKNOWN_OPERATION: u8 = 0x02;
pub const UNAVAILABLE: u8 = 0x04;
pub const NO_SESSION: u8 = 0x07;
pub const LOCKED: u8 = 0x08;
pub const UNSUPPORTED: u8 = 0x0B;

pub const TAG_CRITICAL: u8 = 0x80;
pub const TAG_NAMED_EXTENSION: u8 = 0x7F;

/// What a request, the link or the capture ended in. The kinds are those a caller tells apart (Python's exception
/// types): `Value` a request this host refuses before sending (ValueError), `Timeout` (TimeoutError), `NotFound`
/// (FileNotFoundError: no such device), the rest a failure of the probe or the link (RuntimeError).
#[derive(Debug, Clone)]
pub enum Error {
    /// the probe refused: (reason, the rejected answer's payload)
    Rejected(u8, Vec<u8>),
    /// the probe ran it and it did not work
    Failed(String),
    Protocol(String),
    Timeout(String),
    NotFound(String),
    Value(String),
    Runtime(String),
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;

const REJECT_NAMES: [(u8, &str); 11] = [
    (0x01, "unknown function"),
    (0x02, "unknown operation"),
    (0x03, "malformed payload"),
    (0x04, "unavailable"),
    (0x06, "window exceeded"),
    (0x07, "no session"),
    (0x08, "locked"),
    (0x09, "session required"),
    (0x0A, "no resource"),
    (0x0B, "unsupported"),
    (0x0C, "result lost"),
];

impl Error {
    /// The tag an `unsupported` refusal names (core §4.3: the TLV as received; None for a fixed-part value).
    pub fn unsupported_tag(&self) -> Option<u8> {
        match self {
            Error::Rejected(UNSUPPORTED, p) => p.first().copied().filter(|&t| t != 0),
            _ => None,
        }
    }
    pub fn is_rejected(&self, reason: u8) -> bool {
        matches!(self, Error::Rejected(r, _) if *r == reason)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Rejected(LOCKED, p) if p.len() >= 4 => {
                let ms = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                let who = owner_of(&p[4..]).map(|o| format!(" by {o}")).unwrap_or_default();
                write!(f, "locked{who} ({ms} ms of its lease left)")
            }
            Error::Rejected(r, _) => {
                let name = REJECT_NAMES.iter().find(|(k, _)| k == r).map(|(_, n)| n.to_string());
                write!(f, "rejected: {}", name.unwrap_or_else(|| format!("reason 0x{r:02x}")))
            }
            Error::Failed(s)
            | Error::Protocol(s)
            | Error::Timeout(s)
            | Error::NotFound(s)
            | Error::Value(s)
            | Error::Runtime(s)
            | Error::Io(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e.to_string())
    }
}

/// A request (core §4.1): role corr(u16) fn(u16) op(u8) session_id(u32) payload.
pub fn request(corr: u16, fn_: u16, op: u8, session: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER + payload.len());
    out.push(ROLE_REQUEST);
    out.extend(corr.to_le_bytes());
    out.extend(fn_.to_le_bytes());
    out.push(op);
    out.extend(session.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// A result (core §4.2): role corr(u16) resolution(u8) detail(u8) payload.
#[derive(Debug, Clone)]
pub struct Reply {
    pub corr: u16,
    pub resolution: u8,
    pub detail: u8,
    pub payload: Vec<u8>,
}

impl Reply {
    pub fn parse(frame: &[u8]) -> Result<Reply> {
        if frame.len() < RESULT_HEADER || frame[0] != ROLE_RESULT {
            return Err(Error::Protocol("not a result".into()));
        }
        Ok(Reply {
            corr: u16::from_le_bytes([frame[1], frame[2]]),
            resolution: frame[3],
            detail: frame[4],
            payload: frame[RESULT_HEADER..].to_vec(),
        })
    }
    pub fn succeeded(&self) -> bool {
        self.resolution == COMPLETED && self.detail == SUCCESS
    }
    pub fn describe(&self) -> String {
        match (self.resolution, self.detail) {
            (REJECTED, _) => Error::Rejected(self.detail, vec![]).to_string(),
            (COMPLETED, 0) => "completed".into(),
            (COMPLETED, 1) => "failed".into(),
            (COMPLETED, 2) => "partial".into(),
            (COMPLETED, d) => format!("unknown outcome 0x{d:02x}"),
            (r, _) => format!("unknown resolution 0x{r:02x}"),
        }
    }
}

/// One TLV: tag(u8) len(u16) value (core §2.2).
pub fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + value.len());
    out.push(tag);
    out.extend((value.len() as u16).to_le_bytes());
    out.extend_from_slice(value);
    out
}

/// TLVs in order; a truncated one or one numbered 0 breaks the answer (core §2.2).
pub fn split_tlvs(data: &[u8]) -> Result<Vec<(u8, Vec<u8>)>> {
    let mut out = vec![];
    let mut at = 0;
    while at < data.len() {
        if at + 3 > data.len() {
            return Err(Error::Protocol("TLV: truncated header".into()));
        }
        let tag = data[at];
        let n = u16::from_le_bytes([data[at + 1], data[at + 2]]) as usize;
        at += 3;
        if at + n > data.len() {
            return Err(Error::Protocol(format!("TLV 0x{tag:02x}: truncated value")));
        }
        if tag & 0x7F == 0 {
            return Err(Error::Protocol("a TLV numbered 0 (core §2.2)".into()));
        }
        out.push((tag, data[at..at + n].to_vec()));
        at += n;
    }
    Ok(out)
}

/// The first TLV of `tag` (core §2.3: a tag sent twice, the first is used).
pub fn first(tlvs: &[(u8, Vec<u8>)], tag: u8) -> Option<&[u8]> {
    tlvs.iter().find(|(t, _)| *t == tag).map(|(_, v)| v.as_slice())
}

/// The named extensions in TLVs (core §2.2: tag 0x7F, name_len name data), the first of a name kept.
pub fn named(tlvs: &[(u8, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = vec![];
    for (tag, v) in tlvs {
        if tag & 0x7F != TAG_NAMED_EXTENSION || v.is_empty() || 1 + v[0] as usize > v.len() {
            continue;
        }
        let Ok(name) = std::str::from_utf8(&v[1..1 + v[0] as usize]) else {
            continue;
        };
        if name.is_ascii() && !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), v[1 + v[0] as usize..].to_vec()));
        }
    }
    out
}

/// Text from an answer made safe to show (core §2.1): invalid UTF-8 and control characters replaced.
pub fn shown(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .chars()
        .map(|c| {
            if (c as u32) < 0x20 || c as u32 == 0x7F {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// The owner TLV (0x01) after lock_state's fixed part or a locked refusal's remaining ms.
pub fn owner_of(tail: &[u8]) -> Option<String> {
    let tlvs = split_tlvs(tail).ok()?;
    let v = first(&tlvs, 0x01)?;
    (1..=32).contains(&v.len()).then(|| shown(v))
}

/// Reads a payload's fixed part front to back (too short: a broken answer).
pub struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, at: 0 }
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.at + n > self.data.len() {
            return Err(Error::Protocol(format!(
                "payload of {} bytes: needs {} for its fixed part",
                self.data.len(),
                self.at + n
            )));
        }
        let out = &self.data[self.at..self.at + n];
        self.at += n;
        Ok(out)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    pub fn u32(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> Result<u64> {
        let b = self.bytes(8)?;
        Ok(u64::from_le_bytes(b.try_into().expect("8 bytes")))
    }
    /// A byte string with its u16 length in front.
    pub fn counted16(&mut self) -> Result<&'a [u8]> {
        let n = self.u16()? as usize;
        self.bytes(n)
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let out = &self.data[self.at..];
        self.at = self.data.len();
        out
    }
    pub fn tail(&mut self) -> Result<Vec<(u8, Vec<u8>)>> {
        split_tlvs(self.rest())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tlvs_round_trip_and_named_extensions() {
        let mut data = tlv(0x01, b"\x05");
        data.extend(tlv(0x7F, b"\x03abcXY"));
        let t = split_tlvs(&data).unwrap();
        assert_eq!(first(&t, 1), Some(&b"\x05"[..]));
        assert_eq!(named(&t), vec![("abc".to_string(), b"XY".to_vec())]);
        assert!(split_tlvs(&[0x01, 0x05, 0x00]).is_err());
    }

    #[test]
    fn a_locked_refusal_says_who_and_how_long() {
        let mut p = 1500u32.to_le_bytes().to_vec();
        p.extend(tlv(0x01, b"bench"));
        assert_eq!(
            Error::Rejected(LOCKED, p).to_string(),
            "locked by bench (1500 ms of its lease left)"
        );
    }
}
