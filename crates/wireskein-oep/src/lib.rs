//! The part of an OEP client a capture needs (bench/notes/2026-11-24-rust-plan.ja.md §37), in plain Rust: find the
//! probe (an oep:// address's unit_id as its USB serial, or a TCP address), open it (USB vendor bulk with many IN
//! transfers queued, or TCP), take its lease, run a logic capture on oep.fixture.logic (one-shot, repeat, stream;
//! capture format 0 and format 1's reductions) and put it together: the parts with their losses, times and the probe's
//! facts, as oep-client-python's `acquire` gives them, and as WireSkein's capture (`to_capture`, wireskein-io).
//!
//! Analog lines go to oep.fixture.analog, alone or with logic in a capture group (started together, or on the group's
//! trigger source); the session a run before left open is ended first (`kept`); a length-prefixed link that loses its
//! frame boundaries resyncs (transports §5).
//!
//! Not here (oep-client-python's, which wireskein's Python source keeps using for them): HID and CDC routes, DNS-SD,
//! brokers, a line named by its label (S.N), formats 2-4.
//!
//! ```no_run
//! let req = wireskein_oep::Request::new(vec![("A".into(), "GPIO30".into())], 20_000_000, 100_000);
//! let got = wireskein_oep::capture_at("oep://30eda0ea068b", &req, "wireskein capture")?;
//! # Ok::<(), wireskein_oep::Error>(())
//! ```

pub mod acquire;
pub mod assemble;
pub mod blocks;
pub mod capture;
pub mod cobs;
pub mod flow;
pub mod host;
pub mod kept;
pub mod link;
pub mod msg;
pub mod ratio;
pub mod transport;

pub use acquire::{Acquired, Address, Mode, Request, capture_at, parse_address};
pub use assemble::{AnalogChan, Chan, Data, Loss, Part};
pub use msg::{Error, Result};

pub mod driver;

/// The capture as WireSkein's (`to_capture`), as a document for Python (`to_doc`), a stream's pushes put together
/// (`assemble_pushes`): wireskein-capture's, every device's.
pub use wireskein_capture::save::{assemble_pushes, fmt_g, to_capture, to_doc};
