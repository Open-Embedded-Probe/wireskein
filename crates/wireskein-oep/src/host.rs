//! The host side of a session (oep-core §4-§10; oep-client-python's `host.Host` and `core`): corr and session ids,
//! confirm, list and describe, open / take / keepalive / end, the probe's clock, the plan, subscriptions.

use crate::link::Link;
use crate::msg::{self, Error, Reader, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const CORE_FN: u16 = 0;
pub const OP_CONFIRM: u8 = 0x01;
pub const OP_LIST: u8 = 0x02;
pub const OP_DESCRIBE: u8 = 0x03;
pub const OP_CLOCK: u8 = 0x04;
pub const OP_OPEN: u8 = 0x10;
pub const OP_END: u8 = 0x11;
pub const OP_KEEPALIVE: u8 = 0x12;
pub const OP_LOCK_STATE: u8 = 0x13;
pub const OP_SUBSCRIBE: u8 = 0x01;
pub const OP_UNSUBSCRIBE: u8 = 0x02;
pub const PLAN_NAME: &str = "oep.probe.plan";
pub const OP_PLAN_APPLY: u8 = 0x10;
pub const OP_PLAN_RELEASE: u8 = 0x11;
pub const TAG_ROLE_ASSIGNMENT: u8 = 0x10;
/// fn 0's describe tags (core §7.5)
pub const TAG_FIRMWARE: u8 = 0x40;
pub const TAG_MODEL: u8 = 0x41;
pub const TAG_UNIT_ID: u8 = 0x42;
pub const TAG_CHANNEL_NAME: u8 = 0x44;
pub const TAG_CHIP: u8 = 0x4C;
pub const TAG_MAX_OP_MS: u8 = 0x4D;
const QUICK_OP_MAX_MS: u64 = 1000;
const WAIT_ADD_MS: u64 = 1000;

/// confirm's answer (core §7.1).
#[derive(Debug, Clone)]
pub struct Limits {
    pub revision: u8,
    pub max_frame: usize,
    pub window: usize,
    pub max_inflight: usize,
    pub boot_id: u32,
}

/// A reading of the probe's clock (core §7.6): the host's monotonic ns just before and after, the probe's uptime.
#[derive(Debug, Clone, Copy)]
pub struct ClockReading {
    pub before_ns: i128,
    pub after_ns: i128,
    pub uptime_ns: u64,
    pub boot_id: u32,
}

impl ClockReading {
    pub fn host_ns(&self) -> i128 {
        (self.before_ns + self.after_ns).div_euclid(2)
    }
    pub fn uncertainty_ns(&self) -> i128 {
        (self.after_ns - self.before_ns + 1).div_euclid(2)
    }
}

/// Nanoseconds on the system's monotonic clock (CLOCK_MONOTONIC: Python's time.monotonic_ns, so the host times in
/// meta.probe.clock compare with a recorder's).
#[cfg(target_os = "linux")]
pub fn monotonic_ns() -> i128 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as i128 * 1_000_000_000 + t.tv_nsec as i128
}

/// Nanoseconds on this process's monotonic clock.
#[cfg(not(target_os = "linux"))]
pub fn monotonic_ns() -> i128 {
    use std::sync::OnceLock;
    static T0: OnceLock<Instant> = OnceLock::new();
    let t0 = *T0.get_or_init(Instant::now);
    Instant::now().duration_since(t0).as_nanos() as i128
}

/// A random u32, never 0 (a session id, core §6.1), from the standard library's randomly keyed hasher.
fn random_u32() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    loop {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(monotonic_ns() as u128);
        let v = h.finish() as u32;
        if v != 0 {
            return v;
        }
    }
}

pub struct Host {
    pub link: Link,
    corr: u16,
    pub session: Option<u32>,
    pub limits: Option<Limits>,
    pub lease_ms: Option<u32>,
    describes: HashMap<u16, Vec<(u8, Vec<u8>)>>,
    fns: HashMap<String, (u16, u8)>,
    /// the longest a request may take on the probe beyond the usual (a blocking capture op)
    pub expect_ms: u64,
}

impl Host {
    pub fn new(link: Link) -> Host {
        Host {
            link,
            corr: 0,
            session: None,
            limits: None,
            lease_ms: None,
            describes: HashMap::new(),
            fns: HashMap::new(),
            expect_ms: 0,
        }
    }

    fn next_corr(&mut self) -> u16 {
        self.corr = self.corr % 0xFFFF + 1;
        self.corr
    }

    fn max_op_ms(&self) -> Option<u64> {
        let d = self.describes.get(&CORE_FN)?;
        let v = msg::first(d, TAG_MAX_OP_MS)?;
        (v.len() == 4).then(|| u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as u64)
    }

    /// core §4.4: the wait for an answer is at least the request's processing bound + host_wait_add_ms.
    fn wait_for(&self, fn_: u16, op: u8) -> Duration {
        let quick =
            fn_ == CORE_FN && [OP_CONFIRM, OP_LIST, OP_DESCRIBE, OP_CLOCK, OP_KEEPALIVE, OP_LOCK_STATE].contains(&op);
        let bound = match self.max_op_ms() {
            None => QUICK_OP_MAX_MS,
            Some(m) if quick => m.min(QUICK_OP_MAX_MS),
            Some(m) => m,
        };
        Duration::from_millis(bound.max(self.expect_ms) + WAIT_ADD_MS)
    }

    fn session_for(&self, locked: bool) -> u32 {
        if locked { self.session.unwrap_or(0) } else { 0 }
    }

    /// One request: rejections are errors; any other answer comes back.
    pub fn request(&mut self, fn_: u16, op: u8, payload: &[u8], locked: bool) -> Result<msg::Reply> {
        let sid = self.session_for(locked);
        self.request_as(fn_, op, payload, sid)
    }

    fn request_as(&mut self, fn_: u16, op: u8, payload: &[u8], sid: u32) -> Result<msg::Reply> {
        let corr = self.next_corr();
        let m = msg::request(corr, fn_, op, sid, payload);
        let wait = self.wait_for(fn_, op);
        let raw = self.link.send(&m, wait)?;
        let r = msg::Reply::parse(&raw)?;
        if r.corr != corr {
            return Err(Error::Protocol(format!(
                "result for correlation {}, expected {corr}",
                r.corr
            )));
        }
        if r.resolution == msg::REJECTED {
            if r.detail == msg::NO_SESSION {
                self.session = None;
            }
            return Err(Error::Rejected(r.detail, r.payload));
        }
        Ok(r)
    }

    /// request() that must work (completed success).
    pub fn call(&mut self, fn_: u16, op: u8, payload: &[u8], locked: bool) -> Result<msg::Reply> {
        let r = self.request(fn_, op, payload, locked)?;
        if !r.succeeded() {
            return Err(Error::Failed(r.describe()));
        }
        Ok(r)
    }

    /// Several requests in flight (the probe's in-flight and window limits), every one of which must work.
    pub fn pipeline_calls(&mut self, reqs: &[(u16, u8, Vec<u8>)], locked: bool) -> Result<Vec<msg::Reply>> {
        let sid = self.session_for(locked);
        let mut msgs = vec![];
        let mut corrs = vec![];
        let mut wait = Duration::ZERO;
        for (fn_, op, p) in reqs {
            let c = self.next_corr();
            corrs.push(c);
            msgs.push(msg::request(c, *fn_, *op, sid, p));
            wait = wait.max(self.wait_for(*fn_, *op));
        }
        let (inflight, window) = self
            .limits
            .as_ref()
            .map(|l| (l.max_inflight, l.window))
            .unwrap_or((1, 0xFFFF));
        let raws = self.link.exchange(&msgs, inflight, window, wait)?;
        let mut out = vec![];
        for (raw, c) in raws.iter().zip(corrs) {
            let r = msg::Reply::parse(raw)?;
            if r.corr != c {
                return Err(Error::Protocol(format!(
                    "result for correlation {}, expected {c}",
                    r.corr
                )));
            }
            if r.resolution == msg::REJECTED {
                return Err(Error::Rejected(r.detail, r.payload));
            }
            if !r.succeeded() {
                return Err(Error::Failed(r.describe()));
            }
            out.push(r);
        }
        Ok(out)
    }

    /// confirm (core §7.1): revision 1, the probe's limits.
    pub fn confirm(&mut self) -> Result<Limits> {
        let r = self.call(CORE_FN, OP_CONFIRM, b"OEP?\x01\x01", false)?;
        let mut rd = Reader::new(&r.payload);
        if rd.bytes(4)? != b"OEP!" {
            return Err(Error::Protocol("confirm answered another magic".into()));
        }
        let revision = rd.u8()?;
        if revision != 1 {
            return Err(Error::Protocol(format!(
                "confirm answered revision {revision}, outside the 1..1 asked"
            )));
        }
        let (max_frame, window, inflight, boot_id) = (rd.u16()?, rd.u32()?, rd.u8()?, rd.u32()?);
        if max_frame < 64 || (window as usize) < max_frame as usize || inflight < 1 {
            return Err(Error::Runtime(format!(
                "confirm answered max_frame {max_frame}, window {window}, max_inflight {inflight}: outside core §7.1"
            )));
        }
        let l = Limits {
            revision,
            max_frame: max_frame as usize,
            window: window as usize,
            max_inflight: inflight as usize,
            boot_id,
        };
        self.limits = Some(l.clone());
        Ok(l)
    }

    /// Every describe TLV of `fn` (paged by first; cached for the session).
    pub fn describe(&mut self, fn_: u16) -> Result<Vec<(u8, Vec<u8>)>> {
        if let Some(d) = self.describes.get(&fn_) {
            return Ok(d.clone());
        }
        let mut data = vec![];
        let mut first = 0u16;
        loop {
            let mut p = fn_.to_le_bytes().to_vec();
            p.extend(first.to_le_bytes());
            let r = self.call(CORE_FN, OP_DESCRIBE, &p, false)?;
            let more = *r
                .payload
                .first()
                .ok_or_else(|| Error::Protocol("describe: empty answer".into()))?;
            let chunk = &r.payload[1..];
            first += msg::split_tlvs(chunk)?.len() as u16;
            data.extend_from_slice(chunk);
            if more == 0 || chunk.is_empty() {
                break;
            }
        }
        let out = msg::split_tlvs(&data)?;
        self.describes.insert(fn_, out.clone());
        Ok(out)
    }

    /// The fn of the interface `name` in `revision` (list, paged by first).
    pub fn find(&mut self, name: &str, revision: u8) -> Result<u16> {
        if let Some(&(fn_, rev)) = self.fns.get(name) {
            return if rev == revision {
                Ok(fn_)
            } else {
                Err(Error::Runtime(format!(
                    "{name} (fn {fn_}) is revision {rev} on this probe; this client speaks revision {revision} only"
                )))
            };
        }
        let mut seen = 0u16;
        loop {
            let r = self.call(CORE_FN, OP_LIST, &seen.to_le_bytes(), false)?;
            let mut rd = Reader::new(&r.payload);
            let (total, count) = (rd.u16()?, rd.u8()?);
            for _ in 0..count {
                let (fn_, _instance, rev, n) = (rd.u16()?, rd.u16()?, rd.u8()?, rd.u8()?);
                let nm = msg::shown(rd.bytes(n as usize)?);
                if fn_ != CORE_FN {
                    self.fns.entry(nm).or_insert((fn_, rev));
                }
            }
            seen += count as u16;
            if count == 0 || seen >= total {
                break;
            }
        }
        match self.fns.get(name) {
            Some(_) => self.find(name, revision),
            None => Err(Error::Runtime(format!("probe does not offer {name}"))),
        }
    }

    /// open (core §6.4): a new session under a random id; `owner` shown to other hosts.
    pub fn open(&mut self, lease_ms: u32, force: bool, owner: Option<&str>) -> Result<(u32, u32)> {
        if self.max_op_ms().is_none() {
            self.describe(CORE_FN)?;
        }
        let sid = random_u32();
        let mut p = lease_ms.to_le_bytes().to_vec();
        p.push(force as u8);
        if let Some(o) = owner {
            p.extend(msg::tlv(0x01, owner_text(o).as_bytes()));
        }
        let r = self.request_as(CORE_FN, OP_OPEN, &p, sid)?;
        if !r.succeeded() {
            return Err(Error::Failed(r.describe()));
        }
        let mut rd = Reader::new(&r.payload);
        let (lease, boot_id) = (rd.u32()?, rd.u32()?);
        self.session = Some(sid);
        self.lease_ms = Some(lease);
        Ok((lease, boot_id))
    }

    /// Take the lock as host guide §6 says: wait out another holder's lease (up to `wait`), and name it if it keeps
    /// it going. -> (lease, boot_id)
    pub fn take(&mut self, lease_ms: u32, owner: Option<&str>, wait: Duration) -> Result<(u32, u32)> {
        let deadline = Instant::now() + wait;
        loop {
            match self.open(lease_ms, false, owner) {
                Err(Error::Rejected(msg::LOCKED, p)) => {
                    let ms = if p.len() >= 4 {
                        u32::from_le_bytes([p[0], p[1], p[2], p[3]])
                    } else {
                        0
                    };
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() || Duration::from_millis(ms as u64) > left {
                        let who = msg::owner_of(p.get(4..).unwrap_or(&[])).unwrap_or("another session".into());
                        return Err(Error::Runtime(format!(
                            "the probe is in use by {who} (lease {ms} ms left, kept going)"
                        )));
                    }
                    std::thread::sleep(left.min(Duration::from_millis(ms as u64 + 50)));
                }
                other => return other,
            }
        }
    }

    pub fn keepalive(&mut self) -> Result<()> {
        self.call(CORE_FN, OP_KEEPALIVE, &[], true).map(|_| ())
    }

    /// end (core §6.4): the probe releases the lock and everything the session made.
    pub fn end(&mut self) -> Result<()> {
        if self.session.is_none() {
            return Ok(());
        }
        let r = self.request(CORE_FN, OP_END, &[], true);
        self.session = None;
        r.map(|_| ())
    }

    /// lock_state: (locked, remaining ms, owner).
    pub fn lock_state(&mut self) -> Result<(bool, u32, Option<String>)> {
        let r = self.call(CORE_FN, OP_LOCK_STATE, &[], false)?;
        let mut rd = Reader::new(&r.payload);
        let (locked, remaining) = (rd.u8()?, rd.u32()?);
        Ok((locked != 0, remaining, msg::owner_of(rd.rest())))
    }

    pub fn clock(&mut self) -> Result<ClockReading> {
        let corr = self.next_corr();
        let m = msg::request(corr, CORE_FN, OP_CLOCK, 0, &[]);
        let wait = self.wait_for(CORE_FN, OP_CLOCK);
        let before = monotonic_ns();
        let raw = self.link.send(&m, wait)?;
        let after = monotonic_ns();
        let r = msg::Reply::parse(&raw)?;
        if r.corr != corr || !r.succeeded() {
            return Err(Error::Failed(r.describe()));
        }
        let mut rd = Reader::new(&r.payload);
        let (boot_id, uptime_ns) = (rd.u32()?, rd.u64()?);
        Ok(ClockReading {
            before_ns: before,
            after_ns: after,
            uptime_ns,
            boot_id,
        })
    }

    /// `n` readings, the one with the shortest round trip (of the last boot) kept.
    pub fn clock_best(&mut self, n: usize) -> Result<ClockReading> {
        let mut best: Option<ClockReading> = None;
        for _ in 0..n {
            let r = self.clock()?;
            let better = match best {
                None => true,
                Some(b) => r.boot_id != b.boot_id || r.after_ns - r.before_ns < b.after_ns - b.before_ns,
            };
            if better {
                best = Some(r);
            }
        }
        best.ok_or_else(|| Error::Value("clock_best: n >= 1".into()))
    }

    /// Events and data pushes from `fn` (core §10.3): sent when min_bytes are ready or max_delay_ms after the first.
    pub fn subscribe(&mut self, fn_: u16, min_bytes: u16, max_delay_ms: u32) -> Result<()> {
        let mut p = min_bytes.to_le_bytes().to_vec();
        p.extend(max_delay_ms.to_le_bytes());
        self.call(fn_, OP_SUBSCRIBE, &p, true).map(|_| ())
    }

    /// The plan (oep-if-plan §2.1): (fn, role, channel) for every role.
    pub fn plan_apply(&mut self, assignments: &[(u16, u8, u16)]) -> Result<()> {
        let plan = self.find(PLAN_NAME, 1)?;
        let mut p = vec![];
        for (f, role, ch) in assignments {
            let mut v = f.to_le_bytes().to_vec();
            v.push(*role);
            v.extend(ch.to_le_bytes());
            p.extend(msg::tlv(TAG_ROLE_ASSIGNMENT, &v));
        }
        self.call(plan, OP_PLAN_APPLY, &p, true).map(|_| ())
    }

    pub fn plan_release(&mut self, fns: &[u16]) -> Result<()> {
        let plan = self.find(PLAN_NAME, 1)?;
        let mut p = vec![fns.len() as u8];
        for f in fns {
            p.extend(f.to_le_bytes());
        }
        self.call(plan, OP_PLAN_RELEASE, &p, true).map(|_| ())
    }
}

/// open's owner as it may go (core §2.1, §6.4): control characters as '?', cut to 32 bytes on a character boundary.
pub fn owner_text(owner: &str) -> String {
    let mut s: String = owner
        .chars()
        .map(|c| if (c as u32) < 0x20 || c as u32 == 0x7F { '?' } else { c })
        .collect();
    while s.len() > 32 {
        s.pop();
    }
    s
}
