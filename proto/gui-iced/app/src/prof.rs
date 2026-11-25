//! The time a frame spends where (--bench only): egui, eframe and egui-wgpu mark their steps with the `profiling`
//! crate; built with its tracing back end, every step is a tracing span, and this small subscriber adds up the time
//! inside each span name (inclusive, the main thread only). Nothing is installed without --bench.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tracing_core::field::{Field, Visit};
use tracing_core::span::{Attributes, Id, Record};
use tracing_core::{Event, Interest, Metadata, Subscriber};

#[derive(Default)]
struct Names {
    /// the callsite (its metadata's address) → the index of its name
    by_site: HashMap<usize, usize>,
    names: Vec<String>,
}

#[derive(Clone, Default)]
pub struct Totals {
    /// seconds and calls, by the index of the name
    pub secs: Vec<(f64, u64)>,
}

static NAMES: OnceLock<Mutex<Names>> = OnceLock::new();
static TOTALS: OnceLock<Mutex<Totals>> = OnceLock::new();
static MAIN: OnceLock<std::thread::ThreadId> = OnceLock::new();

thread_local! {
    static STACK: RefCell<Vec<(usize, Instant)>> = const { RefCell::new(Vec::new()) };
}

struct Msg(Option<String>);
impl Visit for Msg {
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        if f.name() == "message" {
            self.0 = Some(format!("{v:?}"));
        }
    }
}

/// `a::b::c::Type::method` → `Type::method`
fn short(s: &str) -> String {
    let parts: Vec<&str> = s.split("::").filter(|p| !p.starts_with('{')).collect();
    let k = parts.len().saturating_sub(2);
    parts[k..].join("::")
}

struct Prof;

impl Subscriber for Prof {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::always()
    }
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, a: &Attributes<'_>) -> Id {
        let site = a.metadata() as *const _ as usize;
        let mut n = NAMES.get().unwrap().lock().unwrap();
        let idx = match n.by_site.get(&site) {
            Some(&i) => i,
            None => {
                let name = if a.metadata().name() == "function_scope" {
                    let mut m = Msg(None);
                    a.record(&mut m);
                    short(&m.0.unwrap_or_default())
                } else {
                    a.metadata().name().to_string()
                };
                let i = match n.names.iter().position(|x| *x == name) {
                    Some(i) => i,
                    None => {
                        n.names.push(name);
                        n.names.len() - 1
                    }
                };
                n.by_site.insert(site, i);
                i
            }
        };
        Id::from_u64(idx as u64 + 1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, id: &Id) {
        if MAIN.get() != Some(&std::thread::current().id()) {
            return;
        }
        STACK.with(|s| s.borrow_mut().push((id.into_u64() as usize - 1, Instant::now())));
    }
    fn exit(&self, _: &Id) {
        if MAIN.get() != Some(&std::thread::current().id()) {
            return;
        }
        // a name inside the same name (egui-wgpu's `Queue::submit` around wgpu's) is counted once, the outer one
        let popped = STACK.with(|s| {
            let mut s = s.borrow_mut();
            let top = s.pop();
            top.filter(|(i, _)| !s.iter().any(|(j, _)| j == i))
        });
        if let Some((i, t)) = popped {
            let dt = t.elapsed().as_secs_f64();
            let mut tot = TOTALS.get().unwrap().lock().unwrap();
            if tot.secs.len() <= i {
                tot.secs.resize(i + 1, (0.0, 0));
            }
            tot.secs[i].0 += dt;
            tot.secs[i].1 += 1;
        }
    }
    fn clone_span(&self, id: &Id) -> Id {
        id.clone()
    }
    fn try_close(&self, _: Id) -> bool {
        false
    }
}

/// Install the subscriber (once, from the main thread).
pub fn install() {
    let _ = MAIN.set(std::thread::current().id());
    let _ = NAMES.set(Mutex::new(Names::default()));
    let _ = TOTALS.set(Mutex::new(Totals::default()));
    let _ = tracing_core::dispatcher::set_global_default(tracing_core::Dispatch::new(Prof));
}

/// The totals so far.
pub fn snapshot() -> Totals {
    TOTALS.get().map(|t| t.lock().unwrap().clone()).unwrap_or_default()
}

/// Seconds in the span `name` between two snapshots, and its calls.
pub fn between(a: &Totals, b: &Totals, name: &str) -> (f64, u64) {
    let Some(n) = NAMES.get() else { return (0.0, 0) };
    let n = n.lock().unwrap();
    let Some(i) = n.names.iter().position(|x| x == name) else {
        return (0.0, 0);
    };
    let get = |t: &Totals| t.secs.get(i).copied().unwrap_or((0.0, 0));
    let (x, y) = (get(a), get(b));
    (y.0 - x.0, y.1 - x.1)
}

/// Every name and its seconds between two snapshots, the largest first.
pub fn all_between(a: &Totals, b: &Totals) -> Vec<(String, f64, u64)> {
    let Some(n) = NAMES.get() else { return vec![] };
    let n = n.lock().unwrap();
    let mut out: Vec<(String, f64, u64)> = (0..b.secs.len())
        .map(|i| {
            let x = a.secs.get(i).copied().unwrap_or((0.0, 0));
            (
                n.names.get(i).cloned().unwrap_or_default(),
                b.secs[i].0 - x.0,
                b.secs[i].1 - x.1,
            )
        })
        .filter(|r| r.2 > 0)
        .collect();
    out.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap());
    out
}
