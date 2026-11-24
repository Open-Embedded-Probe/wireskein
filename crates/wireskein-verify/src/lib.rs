//! WireSkein's verify in plain Rust (Python's `wireskein.verify`, docs/run-format.ja.md): a run directory recorded
//! with `wireskein.runlog` (run.json: the log with its heading markers, the captures, the expectations by section)
//! judged on its captures. The sections come from the log's headings (on the host clock, or the probe's when both of
//! a section's headings carry probe_ns: then the stretch of a capture it covers); each expectation is checked on
//! every capture of its section that holds its pins, as Python's verify does, and the report is its document
//! (`wireskein verify --json`).

pub mod analog;
pub mod bus;
pub mod cap;
pub mod checks;
pub mod coded;
pub mod decoded;
pub mod mark;
pub mod report;
pub mod timing;
pub mod tree;

pub use j::J;
pub use mark::mark;
pub use report::{failed, junit, lines, summary_line, verify};
pub use wireskein_io::{j, jd, pyfmt};
