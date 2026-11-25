//! WireSkein's built-in kinds of capture device (bench/notes/2026-11-24-rust-plan.ja.md §45): each kind is a driver of
//! wireskein-capture's interface, chosen by a source's `<kind>:` (`oep:oep://30eda0ea068b`, `demo:counter`). The
//! kinds are WireSkein's own: a new one is a driver added here (docs/devices.md).
//!
//! ```no_run
//! use wireskein_capture::{Ctl, Open, Request};
//! let req = Request::new(vec![("A".into(), "D0".into())], 1_000_000, 10_000);
//! let got = wireskein_sources::builtin().capture("demo:counter", &req, &Open::new("me"), &Ctl::new())?;
//! # Ok::<(), wireskein_capture::Error>(())
//! ```

pub use wireskein_capture::{Acquired, Ctl, Error, Open, Registry, Request, Result};

/// Every built-in kind: `demo` (no hardware), `oep` (an OEP probe).
pub fn builtin() -> Registry {
    Registry::new()
        .with(Box::new(wireskein_capture::demo::Demo))
        .with(Box::new(wireskein_oep::driver::Oep))
}

/// One capture from `source` (`<kind>:<address>`), held as `owner`.
pub fn capture(source: &str, req: &Request, owner: &str, ctl: &Ctl) -> Result<Acquired> {
    builtin().capture(source, req, &Open::new(owner), ctl)
}
