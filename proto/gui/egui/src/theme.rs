//! Colors (dark / light) and fonts (a Japanese font from the system as a fallback).

use eframe::egui::{self, Color32};
use view_core::ann::Status;

#[derive(Clone, Copy)]
pub struct Pal {
    pub bg: Color32,
    pub lane_alt: Color32,
    pub grid: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub line: Color32,
    pub analog: Color32,
    pub accent: Color32,
    pub marker: [Color32; 2],
    pub ok: Color32,
    pub note: Color32,
    pub warn: Color32,
    pub error: Color32,
    pub unread: Color32,
    pub sel: Color32,
}

pub fn pal(dark: bool) -> Pal {
    if dark {
        Pal {
            bg: Color32::from_rgb(0x16, 0x18, 0x1d),
            lane_alt: Color32::from_rgb(0x1b, 0x1e, 0x24),
            grid: Color32::from_rgb(0x2a, 0x2e, 0x36),
            text: Color32::from_rgb(0xdd, 0xe1, 0xe8),
            dim: Color32::from_rgb(0x8a, 0x93, 0xa3),
            line: Color32::from_rgb(0x4c, 0xd1, 0x9b),
            analog: Color32::from_rgb(0x6c, 0xb4, 0xff),
            accent: Color32::from_rgb(0xff, 0xd1, 0x4a),
            marker: [Color32::from_rgb(0xff, 0x6b, 0xd6), Color32::from_rgb(0x5f, 0xe3, 0xff)],
            ok: Color32::from_rgb(0x2f, 0x5d, 0x8c),
            note: Color32::from_rgb(0x8a, 0x6d, 0x1f),
            warn: Color32::from_rgb(0xa0, 0x56, 0x1a),
            error: Color32::from_rgb(0xb0, 0x30, 0x3a),
            unread: Color32::from_rgb(0x5a, 0x5a, 0x66),
            sel: Color32::from_rgb(0xff, 0xff, 0xff),
        }
    } else {
        Pal {
            bg: Color32::from_rgb(0xfb, 0xfb, 0xfc),
            lane_alt: Color32::from_rgb(0xf1, 0xf3, 0xf6),
            grid: Color32::from_rgb(0xdc, 0xe0, 0xe6),
            text: Color32::from_rgb(0x1d, 0x22, 0x2b),
            dim: Color32::from_rgb(0x6a, 0x72, 0x80),
            line: Color32::from_rgb(0x0b, 0x8a, 0x5c),
            analog: Color32::from_rgb(0x1d, 0x6f, 0xd6),
            accent: Color32::from_rgb(0xd9, 0x8c, 0x00),
            marker: [Color32::from_rgb(0xd0, 0x2f, 0xa8), Color32::from_rgb(0x00, 0x8e, 0xb0)],
            ok: Color32::from_rgb(0xc9, 0xdc, 0xf2),
            note: Color32::from_rgb(0xf3, 0xe1, 0xa6),
            warn: Color32::from_rgb(0xf6, 0xc6, 0x99),
            error: Color32::from_rgb(0xf2, 0xae, 0xb3),
            unread: Color32::from_rgb(0xc8, 0xc8, 0xd0),
            sel: Color32::from_rgb(0x10, 0x10, 0x10),
        }
    }
}

impl Pal {
    pub fn status(&self, s: Status) -> Color32 {
        match s {
            Status::Ok => self.ok,
            Status::Note => self.note,
            Status::Warn => self.warn,
            Status::Error => self.error,
            Status::Unread => self.unread,
        }
    }
    pub fn verdict(&self, v: &str) -> Color32 {
        match v {
            "confirmed" => Color32::from_rgb(0x3f, 0xb9, 0x50),
            "likely" => Color32::from_rgb(0xe3, 0xb3, 0x41),
            _ => Color32::from_rgb(0xf0, 0x7a, 0x3a),
        }
    }
}

/// A Japanese font from the system after egui's own (native only; the web build would have to carry one).
pub fn install_fonts(ctx: &egui::Context) -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let cands = [
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0u32),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/truetype/noto/NotoSansJP-Regular.ttf", 0),
            ("C:\\Windows\\Fonts\\YuGothM.ttc", 0),
            ("C:\\Windows\\Fonts\\meiryo.ttc", 0),
            ("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0),
        ];
        for (p, idx) in cands {
            if let Ok(bytes) = std::fs::read(p) {
                let mut fonts = egui::FontDefinitions::default();
                let mut fd = egui::FontData::from_owned(bytes);
                fd.index = idx;
                fonts.font_data.insert("jp".into(), std::sync::Arc::new(fd));
                for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts.families.entry(fam).or_default().push("jp".into());
                }
                ctx.set_fonts(fonts);
                return Some(p.to_string());
            }
        }
    }
    let _ = ctx;
    None
}
