//! The clock: std's Instant on the desktop. The web (wasm32-unknown-unknown) has none (`Instant::now()` panics there)
//! and Makepad's web build has no wasm-bindgen for web-time to stand on: the page's `performance.now()` through an
//! import of our own (web.rs, web/index.html).

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;

#[cfg(target_arch = "wasm32")]
pub use web::Instant;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::time::Duration;

    /// ms since the page's navigation started.
    #[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
    pub struct Instant(f64);

    impl Instant {
        pub fn now() -> Instant {
            Instant(crate::web::now_ms())
        }
        pub fn elapsed(&self) -> Duration {
            Instant::now().duration_since(*self)
        }
        pub fn duration_since(&self, earlier: Instant) -> Duration {
            Duration::from_secs_f64(((self.0 - earlier.0) / 1e3).max(0.0))
        }
    }

    impl std::ops::Sub for Instant {
        type Output = Duration;
        fn sub(self, rhs: Instant) -> Duration {
            self.duration_since(rhs)
        }
    }
}
