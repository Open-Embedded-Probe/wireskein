//! The checks of buses and coded lines (`verify.check_uart` ... `check_sd_spi`, `check_protocol`).

use crate::cap::Cap;
use crate::checks::{Out, V, out};
use crate::j::J;

pub fn run(kind: &str, _cap: &Cap, _x: &J) -> Out {
    out(V::Unchecked, J::dict(), format!("{kind}: not in the Rust verify yet"))
}
