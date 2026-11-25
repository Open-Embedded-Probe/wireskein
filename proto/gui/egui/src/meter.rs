//! The process's memory and this thread's CPU time, taken the same way in every prototype (bench/notes/2026-11-25-gui-study.ja.md
//! §14). This file is prototype 1's; proto/gui-iced and proto/gui-makepad include it by its path.
//!
//! - Linux: the resident set (/proc/self/statm) and `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`.
//! - Windows: the working set (`K32GetProcessMemoryInfo`, kernel32) and the thread's cycles (`QueryThreadCycleTime`), turned
//!   into time by a rate measured at the start (`init`: the thread kept busy a few ms at a time, its cycles over the wall
//!   clock). `GetThreadTimes` would be simpler but moves in the scheduler's ticks (15.6 ms), too coarse for a part of a
//!   frame; it is kept as a check over a whole run (`check`).
//! - elsewhere (the web): none.
//!
//! No crate for it: the few kernel32 functions are declared here.
#![allow(dead_code)]

use std::sync::OnceLock;

/// The resident set (Linux) / working set (Windows) of the process, MB.
pub fn rss_mb() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: sysconf has no preconditions
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as f64;
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1).and_then(|x| x.parse::<f64>().ok()))
            .map(|p| p * page / 1e6)
    }
    #[cfg(windows)]
    {
        win::memory().map(|m| m.working_set as f64 / 1e6)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

/// The CPU time of the calling thread, ms (NaN where it is not known).
pub fn thread_cpu_ms() -> f64 {
    #[cfg(target_os = "linux")]
    {
        let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: a valid timespec to fill
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
        t.tv_sec as f64 * 1e3 + t.tv_nsec as f64 / 1e6
    }
    #[cfg(windows)]
    {
        match (win::thread_cycles(), rate()) {
            (Some(c), Some(hz)) => c as f64 / hz * 1e3,
            _ => f64::NAN,
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        f64::NAN
    }
}

/// The calling thread's CPU time by the coarse clock (Windows: `GetThreadTimes`, user + kernel; Linux: the same as
/// `thread_cpu_ms`), ms. For checking `thread_cpu_ms` over a whole run.
pub fn thread_cpu_coarse_ms() -> f64 {
    #[cfg(windows)]
    {
        win::thread_times().map(|t| t as f64 / 1e4).unwrap_or(f64::NAN)
    }
    #[cfg(not(windows))]
    {
        thread_cpu_ms()
    }
}

static RATE: OnceLock<Option<f64>> = OnceLock::new();

/// Windows: the rate of `QueryThreadCycleTime`'s count, Hz (measured once).
fn rate() -> Option<f64> {
    *RATE.get_or_init(measure_rate)
}

/// Measures what has to be measured before the frames (Windows: the cycle rate, about 60 ms of one thread kept busy).
/// Call it at the start, before the capture is opened.
pub fn init() {
    #[cfg(windows)]
    let _ = rate();
}

#[cfg(windows)]
fn measure_rate() -> Option<f64> {
    use std::time::{Duration, Instant};
    // the thread busy for 6 ms, 10 times: the highest cycles / wall time is the count's rate (a time it was put off the
    // CPU only lowers it)
    let mut best: Option<f64> = None;
    for _ in 0..10 {
        let c0 = win::thread_cycles()?;
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(6) {
            std::hint::spin_loop();
        }
        let c1 = win::thread_cycles()?;
        let hz = (c1 - c0) as f64 / t0.elapsed().as_secs_f64();
        best = Some(best.map_or(hz, |b: f64| b.max(hz)));
    }
    best.filter(|&hz| hz > 1e6)
}

#[cfg(not(windows))]
fn measure_rate() -> Option<f64> {
    None
}

/// How the numbers are taken, for a bench's header.
pub fn method() -> String {
    #[cfg(target_os = "linux")]
    {
        "rss /proc/self/statm; thread CPU clock_gettime(CLOCK_THREAD_CPUTIME_ID)".into()
    }
    #[cfg(windows)]
    {
        format!(
            "rss = working set (K32GetProcessMemoryInfo); thread CPU QueryThreadCycleTime at {} (measured at the start)",
            rate()
                .map(|hz| format!("{:.3} GHz", hz / 1e9))
                .unwrap_or("? (not measured: NaN)".into())
        )
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        "none (NaN)".into()
    }
}

/// The whole run's main-thread CPU by both clocks, for a bench's footer (Windows: the cycles and `GetThreadTimes` should
/// agree within a few %; Linux: one clock).
pub struct Check {
    fine: f64,
    coarse: f64,
}

pub fn check() -> Check {
    Check {
        fine: thread_cpu_ms(),
        coarse: thread_cpu_coarse_ms(),
    }
}

impl Check {
    pub fn report(&self) -> String {
        let fine = thread_cpu_ms() - self.fine;
        let coarse = thread_cpu_coarse_ms() - self.coarse;
        if cfg!(windows) {
            format!(
                "main thread CPU over the run: {:.0} ms by the cycles, {:.0} ms by GetThreadTimes (15.6 ms steps; {:+.1} %)",
                fine,
                coarse,
                (fine / coarse - 1.0) * 100.0
            )
        } else {
            format!("main thread CPU over the run: {fine:.0} ms")
        }
    }
}

/// When the running program was built (its file's time, UTC), for a bench's header: an old binary shows itself.
pub fn exe_time() -> String {
    let t = std::env::current_exe()
        .and_then(|p| p.metadata())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
    let Some(t) = t else { return "?".into() };
    let s = t.as_secs() as i64;
    let (days, rem) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    // days since 1970-01-01 to a civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem % 3600 / 60)
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    /// PROCESS_MEMORY_COUNTERS
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn GetCurrentThread() -> *mut c_void;
        fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut Counters, cb: u32) -> i32;
        fn QueryThreadCycleTime(thread: *mut c_void, cycles: *mut u64) -> i32;
        fn GetThreadTimes(
            thread: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }

    pub struct Memory {
        pub working_set: usize,
        pub peak_working_set: usize,
        /// committed private bytes
        pub pagefile: usize,
    }

    pub fn memory() -> Option<Memory> {
        let mut c = Counters {
            cb: std::mem::size_of::<Counters>() as u32,
            ..Default::default()
        };
        // SAFETY: the pseudo handle of this process; a counters struct of the size given
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
        (ok != 0).then_some(Memory {
            working_set: c.working_set_size,
            peak_working_set: c.peak_working_set_size,
            pagefile: c.pagefile_usage,
        })
    }

    pub fn thread_cycles() -> Option<u64> {
        let mut n = 0u64;
        // SAFETY: the pseudo handle of this thread; a u64 to fill
        let ok = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut n) };
        (ok != 0).then_some(n)
    }

    /// user + kernel time of this thread, 100 ns
    pub fn thread_times() -> Option<u64> {
        let (mut a, mut b, mut k, mut u) = Default::default();
        // SAFETY: the pseudo handle of this thread; four FILETIMEs to fill
        let ok = unsafe { GetThreadTimes(GetCurrentThread(), &mut a, &mut b, &mut k, &mut u) };
        let v = |f: &FileTime| (u64::from(f.high) << 32) | u64::from(f.low);
        (ok != 0).then(|| v(&k) + v(&u))
    }
}
