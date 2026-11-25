//! The web build's ties to its page (web/index.html; notes §16). Makepad's web build has no wasm-bindgen: it talks
//! to its own JS (makepad-platform's web.js) through the wasm's `env` imports. The page adds a few of ours to the same
//! `env` before it instantiates the wasm: the clock, what it fetched before the program started (a capture named by
//! `?file=`), the URL's arguments, and what the program hands back (`window.__ready`, the bench's text
//! `window.__bench`). Everything here runs on the page's one thread.

mod js {
    // (named as imports of the module `env`: undefined symbols are not allowed at the link)
    #[link(wasm_import_module = "env")]
    unsafe extern "C" {
        /// performance.now()
        pub fn wsk_now() -> f64;
        /// the length in bytes of one of the page's values (0 the URL's search, 1 the fetched capture, 2 its name,
        /// 3 the GPU's name)
        pub fn wsk_len(which: u32) -> u32;
        /// that value copied to ptr (wsk_len bytes)
        pub fn wsk_copy(which: u32, ptr: u32);
        /// a text to the page (0 the console, 1 the bench's text, 2 ready with its info)
        pub fn wsk_put(which: u32, ptr: u32, len: u32);
        /// ms the page has spent in its requestAnimationFrame callbacks
        pub fn wsk_raf_busy() -> f64;
    }
}

pub fn now_ms() -> f64 {
    unsafe { js::wsk_now() }
}

fn get(which: u32) -> Option<Vec<u8>> {
    let n = unsafe { js::wsk_len(which) } as usize;
    if n == 0 {
        return None;
    }
    let mut v = vec![0u8; n];
    unsafe { js::wsk_copy(which, v.as_mut_ptr() as u32) };
    Some(v)
}

fn get_str(which: u32) -> Option<String> {
    get(which).map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn put(which: u32, s: &str) {
    unsafe { js::wsk_put(which, s.as_ptr() as u32, s.len() as u32) };
}

/// The URL's arguments (`?synth=1e8&bench=1`), decoded by the page (key=value lines).
pub fn query() -> Vec<(String, String)> {
    get_str(0)
        .unwrap_or_default()
        .lines()
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, "1"));
            (k.to_string(), v.to_string())
        })
        .collect()
}

/// The capture the page fetched for `?file=`.
pub fn file() -> Option<(String, Vec<u8>)> {
    let b = get(1)?;
    Some((get_str(2).unwrap_or("capture".into()), b))
}

/// The GPU as the page found it (WebGL's renderer; Makepad 1.0 draws with WebGL 1 and its extensions).
pub fn gpu() -> String {
    get_str(3).unwrap_or("?".into())
}

pub fn log(s: &str) {
    put(0, s);
}

/// Everything in (the first picture, the summaries, the analysis): what tools/bench.mjs of proto/gui-web waits for.
pub fn ready(info: &str) {
    put(2, info);
}

/// The bench's text for the page (bench.mjs reads `window.__bench`).
pub fn put_bench(text: &str) {
    put(1, text);
}

/// The wasm memory, MB (it only grows).
pub fn wasm_mb() -> f64 {
    core::arch::wasm32::memory_size(0) as f64 * 65536.0 / 1e6
}

/// ms the page has spent in its requestAnimationFrame callbacks (index.html wraps them): where Makepad's web.js
/// hands the program its frames (the frame loop, the draw, the draw lists to WebGL).
pub fn raf_busy_ms() -> f64 {
    unsafe { js::wsk_raf_busy() }
}
