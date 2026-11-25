//! Colors: prototypes 1–3's palette (the same 16, the same order), dark and light, as Makepad's Vec4 (0..1).
//! The waveform's instances carry their color (resolved here on the CPU); the panels drawn by this program read
//! the same palette. Makepad's own widgets take its theme (theme_desktop_dark / _light, app.rs).

use makepad_widgets::{Vec4, vec4};
use view_core::ann::Status;

pub const BG: usize = 0;
pub const LANE_ALT: usize = 1;
pub const GRID: usize = 2;
pub const TEXT: usize = 3;
pub const DIM: usize = 4;
pub const LINE: usize = 5;
pub const ANALOG: usize = 6;
pub const ACCENT: usize = 7;
pub const MARKER1: usize = 8;
pub const OK: usize = 10;
pub const ERROR: usize = 13;
pub const UNREAD: usize = 14;
pub const SEL: usize = 15;

const DARK: [u32; 16] = [
    0x16181d, 0x1b1e24, 0x2a2e36, 0xdde1e8, 0x8a93a3, 0x4cd19b, 0x6cb4ff, 0xffd14a, 0xff6bd6, 0x5fe3ff, 0x2f5d8c,
    0x8a6d1f, 0xa0561a, 0xb0303a, 0x5a5a66, 0xffffff,
];
const LIGHT: [u32; 16] = [
    0xfbfbfc, 0xf1f3f6, 0xdce0e6, 0x1d222b, 0x6a7280, 0x0b8a5c, 0x1d6fd6, 0xd98c00, 0xd02fa8, 0x008eb0, 0xc9dcf2,
    0xf3e1a6, 0xf6c699, 0xf2aeb3, 0xc8c8d0, 0x101010,
];

pub fn rgb(x: u32) -> Vec4 {
    vec4(
        ((x >> 16) & 255) as f32 / 255.0,
        ((x >> 8) & 255) as f32 / 255.0,
        (x & 255) as f32 / 255.0,
        1.0,
    )
}

#[derive(Clone, Copy, Debug)]
pub struct Pal {
    pub dark: bool,
    pub c: [Vec4; 16],
    pub panel: Vec4,
    pub border: Vec4,
}

pub fn pal(dark: bool) -> Pal {
    let src = if dark { &DARK } else { &LIGHT };
    let mut c = [vec4(0.0, 0.0, 0.0, 1.0); 16];
    for (i, &x) in src.iter().enumerate() {
        c[i] = rgb(x);
    }
    Pal {
        dark,
        c,
        panel: if dark { rgb(0x1e2128) } else { rgb(0xffffff) },
        border: if dark { rgb(0x2e323b) } else { rgb(0xd8dce3) },
    }
}

impl Pal {
    pub fn text(&self) -> Vec4 {
        self.c[TEXT]
    }
    pub fn dim(&self) -> Vec4 {
        self.c[DIM]
    }
    pub fn accent(&self) -> Vec4 {
        self.c[ACCENT]
    }
    /// A status as text (the list's marks): stronger than the annotation's fill.
    pub fn status_text(&self, s: Status) -> Vec4 {
        match s {
            Status::Ok => self.dim(),
            Status::Note => rgb(if self.dark { 0xe3b341 } else { 0x9a6700 }),
            Status::Warn => rgb(if self.dark { 0xf07a3a } else { 0xbc4c00 }),
            Status::Error => rgb(if self.dark { 0xff6b6b } else { 0xcf222e }),
            Status::Unread => self.dim(),
        }
    }
    pub fn verdict(&self, v: &str) -> Vec4 {
        match v {
            "confirmed" => rgb(0x3fb950),
            "likely" => rgb(0xe3b341),
            _ => rgb(0xf07a3a),
        }
    }
}

pub fn with_alpha(c: Vec4, a: f32) -> Vec4 {
    vec4(c.x, c.y, c.z, c.w * a)
}
