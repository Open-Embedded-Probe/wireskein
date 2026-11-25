//! The web build's ties to its page (web/index.html; notes §15): what the page fetched before the program started (the
//! font, a capture named by `?file=`), the URL's arguments, and what the program hands back (`window.__ready`, the
//! bench's text `window.__bench`, the screenshot `window.__shotUrl`). Everything here runs on the page's one thread.

use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};

fn global(name: &str) -> JsValue {
    Reflect::get(&js_sys::global(), &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}

fn set_global(name: &str, v: &JsValue) {
    let _ = Reflect::set(&js_sys::global(), &JsValue::from_str(name), v);
}

fn bytes(name: &str) -> Option<Vec<u8>> {
    let v = global(name);
    v.dyn_ref::<Uint8Array>().map(|a| a.to_vec())
}

/// The URL's arguments (`?synth=1e8&bench=1`), decoded.
pub fn query() -> Vec<(String, String)> {
    let search = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .unwrap_or_default();
    search
        .trim_start_matches('?')
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, "1"));
            let dec = |s: &str| {
                js_sys::decode_uri_component(&s.replace('+', " "))
                    .map(String::from)
                    .unwrap_or_else(|_| s.to_string())
            };
            (dec(k), dec(v))
        })
        .collect()
}

/// The font the page fetched (`window.__wskFont`, its family `window.__wskFontFamily`): the web has no system fonts
/// for cosmic-text to find, and iced carries none of its own here.
/// Also the family to use where the program asks for a monospace font (`window.__wskFontMono`: Noto Sans CJK's
/// collection carries "Noto Sans Mono CJK JP"); without it Font::MONOSPACE finds nothing and its text is left out.
pub fn font() -> Option<(Vec<u8>, String, String)> {
    let b = bytes("__wskFont")?;
    let fam = global("__wskFontFamily").as_string().unwrap_or_default();
    let mono = global("__wskFontMono").as_string().unwrap_or_else(|| fam.clone());
    Some((b, fam, mono))
}

/// The capture the page fetched for `?file=` (`window.__wskFile`, its name `window.__wskFileName`).
pub fn file() -> Option<(String, Vec<u8>)> {
    let b = bytes("__wskFile")?;
    let name = global("__wskFileName").as_string().unwrap_or("capture".into());
    Some((name, b))
}

/// The adapter as the page found it before the program started (`window.__wskGpu`: WebGPU's adapter info, else
/// WebGL2's renderer); wgpu asks the same way (WebGPU when an adapter comes, else WebGL2).
pub fn gpu() -> String {
    global("__wskGpu").as_string().unwrap_or("?".into())
}

pub fn log(s: &str) {
    web_sys::console::log_1(&JsValue::from_str(s));
}

/// ms since the page's navigation started (performance.now()).
pub fn page_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(f64::NAN)
}

/// Everything in (the first picture, the summaries, the analysis): what tools/bench.mjs of proto/gui-web waits for.
pub fn ready(info: &str) {
    set_global("__wskInfo", &JsValue::from_str(info));
    set_global("__ready", &JsValue::TRUE);
}

/// The bench's text for the page (bench.mjs reads `window.__bench`).
pub fn put_bench(text: &str) {
    log(text);
    set_global("__bench", &JsValue::from_str(text));
}

/// The wasm memory, MB (it only grows).
pub fn wasm_mb() -> f64 {
    let m = wasm_bindgen::memory();
    m.dyn_ref::<js_sys::WebAssembly::Memory>()
        .map(|m| m.buffer().unchecked_into::<js_sys::ArrayBuffer>().byte_length() as f64 / 1e6)
        .unwrap_or(f64::NAN)
}

/// ms the page has spent in its requestAnimationFrame callbacks (index.html wraps them; NaN without it): where winit
/// hands iced its redraws (update, view, layout, draw and the GPU's commands of a frame).
pub fn raf_busy_ms() -> f64 {
    global("__rafBusy").as_f64().unwrap_or(f64::NAN)
}
