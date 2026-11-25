//! A capture put together as it comes (oep-client-python's `acquire._Spool`, `_from_blocks`, `from_interleaved`):
//! wireskein-capture's spool, shared by every device (§45). OEP's capture format 0 is its packed samples
//! (`Format::Packed`), format 1 its block layout (`Format::Blocks` of `blocks::Blocks`).

pub use wireskein_capture::parts::{AnalogChan, Chan, Data, Loss, Part};
pub use wireskein_capture::spool::{Feed, Format, Spool, SpoolWorker, round_div, segment_channels};
