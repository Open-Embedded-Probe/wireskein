//! The part of an OEP client a capture needs (bench/notes/2026-11-24-rust-plan.ja.md §37, §40, §42, §47), in plain
//! Rust: find the probe (an oep:// address's unit_id as its USB serial, else by DNS-SD; a TCP address; a serial port),
//! open it (USB vendor bulk with many IN transfers queued, HID, its CDC port, TCP, a serial port), take its lease, run
//! a logic capture on oep.fixture.logic (one-shot, repeat, stream; capture format 0 and format 1's reductions) and put
//! it together: the parts with their losses, times and the probe's facts, as oep-client-python's `acquire` gives
//! them, and as WireSkein's capture (`to_capture`, wireskein-io).
//!
//! Analog lines go to oep.fixture.analog, alone or with logic in a capture group (started together, or on the group's
//! trigger source); a line may be named by its label (S.N, S@P: `labels`); the session a run before left open is
//! ended first (`kept`); a length-prefixed link that loses its frame boundaries resyncs (transports §5); a UART
//! bridge's link is raised by port_speed and judged in use as oep-client-python does, with its record (`speed`).
//!
//! The short forms of an address (tcp://HOST without a port, a bare `usb`, a serial port by any path) open as
//! oep-client-python's open_host opens them. Formats 2-4 (analog blocks, timed states, the test counter) are read
//! (`formats`), though no WireSkein request asks for them yet.
//!
//! Not here (oep-client-python's, which wireskein's Python source keeps using for them): brokers; HID and CDC off
//! Linux, serial ports off POSIX.
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
pub mod discovery;
pub mod flow;
pub mod formats;
pub mod hid;
pub mod host;
pub mod kept;
pub mod labels;
pub mod link;
pub mod msg;
pub mod ratio;
pub mod speed;
pub mod transport;

pub use acquire::{Acquired, Address, Mode, Request, capture_at, parse_address};
pub use assemble::{AnalogChan, Chan, Data, Loss, Part};
pub use msg::{Error, Result};

pub mod driver;

/// The capture as WireSkein's (`to_capture`), as a document for Python (`to_doc`), a stream's pushes put together
/// (`assemble_pushes`): wireskein-capture's, every device's.
pub use wireskein_capture::save::{assemble_pushes, fmt_g, to_capture, to_doc};
