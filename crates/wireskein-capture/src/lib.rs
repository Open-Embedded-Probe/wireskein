//! What every capture device of WireSkein shares (bench/notes/2026-11-24-rust-plan.ja.md §45), in plain Rust:
//!
//! - `driver`: a kind of device's interface - find (`Driver::list`), open and hold (`Driver::open`, `Hold`: an OEP
//!   lease, a lock), ask what it can do (`Device::caps`: lines, rates by how many lines, streaming or into its own
//!   memory and their limits, triggers, thresholds, firmware to send), capture (`Device::capture`: one-shot, repeat,
//!   stream), stop (`Ctl`), close; the firmware a device needs found where its user keeps it (`find_firmware`)
//! - `request`: what to capture, the same for every device (Python's wireskein.sources.Request)
//! - `flow`, `stream`: the received bytes within one budget, the IN side never waiting for the host (§42)
//! - `spool`: the bytes put together in order of their positions, a skipped position a loss (on_loss stop /
//!   continue), each line's bits to disk
//! - `packed`: samples of 1, 2, 4 bytes (or any width) into each line's bits and its changes
//! - `parts`, `save`: the capture put together, and written out (wireskein-io)
//! - `source`: `<kind>:<address>` and the registry of kinds
//! - `demo`: the demo device (`demo:`), a counter as packed samples, streaming or kept in memory

pub mod demo;
pub mod driver;
pub mod error;
pub mod flow;
pub mod packed;
pub mod parts;
pub mod ratio;
pub mod request;
pub mod save;
pub mod source;
pub mod spool;
pub mod stream;

pub use driver::{Caps, Ctl, Device, Driver, Found, Hold, Open, Progress};
pub use error::{Error, Result};
pub use parts::{Acquired, AnalogChan, Chan, Data, Loss, Part};
pub use request::{Mode, Request};
pub use save::{fmt_g, to_capture, to_doc};
pub use source::Registry;
