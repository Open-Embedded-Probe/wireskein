//! The web build's meter.rs (the same calls as proto/gui/egui/src/meter.rs): the page has no thread CPU clock nor a
//! process RSS. "CPU" here is the wall time the page spent in its requestAnimationFrame callbacks (index.html counts
//! it; the page has one thread, so it is busy time, not waiting), "rss" the wasm memory.

pub fn rss_mb() -> Option<f64> {
    Some(crate::web::wasm_mb())
}

pub fn thread_cpu_ms() -> f64 {
    crate::web::raf_busy_ms()
}

pub fn init() {}

pub fn method() -> String {
    "the web: rss = the wasm memory; main CPU = the wall time in the page's requestAnimationFrame callbacks \
     (index.html), where Makepad's web.js hands the program its frames and sends the draw lists to WebGL"
        .into()
}

pub fn exe_time() -> String {
    "? (the web)".into()
}

pub struct Check {
    at: f64,
}

pub fn check() -> Check {
    Check { at: thread_cpu_ms() }
}

impl Check {
    pub fn report(&self) -> String {
        format!(
            "main thread busy in requestAnimationFrame over the run: {:.0} ms",
            thread_cpu_ms() - self.at
        )
    }
}
