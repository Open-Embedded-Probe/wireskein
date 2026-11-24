//! The checks that read a bus with the analysis (`verify.check_i2c`, `check_spi`, `check_protocol`).

use crate::cap::Cap;
use crate::checks::{Out, V, out};
use crate::j::J;

pub fn run(kind: &str, _cap: &Cap, _x: &J) -> Out {
    out(V::Unchecked, J::dict(), format!("{kind}: not in the Rust verify yet"))
}
