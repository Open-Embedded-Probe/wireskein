//! What a capture ended in. The kinds are those a caller tells apart (Python's exception types): `Value` a request
//! the host refuses before sending (ValueError), `NotFound` no such device or file (FileNotFoundError), `Timeout`
//! (TimeoutError), `Io` (OSError), `Connection` a link that could not be set right again (ConnectionError), the
//! rest a failure of the device (RuntimeError).

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Value(String),
    NotFound(String),
    Timeout(String),
    Io(String),
    Connection(String),
    /// the data broke the device's own rules (a broken stream)
    Protocol(String),
    Runtime(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Value(s)
            | Error::NotFound(s)
            | Error::Timeout(s)
            | Error::Io(s)
            | Error::Connection(s)
            | Error::Protocol(s)
            | Error::Runtime(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e.to_string())
    }
}
