//! The host side of a session (oep-core §4-§10; oep-client-python's `host.Host` and `core`): corr and session ids,
//! confirm, list and describe, open / take / keepalive / end, the probe's clock, the plan, subscriptions.

use crate::kept::KeptSession;
use crate::link::{Framing, Link};
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
/// transports §5: read and discard until the input is this quiet before a resync's confirm
const RESYNC_QUIET: Duration = Duration::from_millis(50);
/// ... at least this long since the host's last write (longer than probe_frame_gap_ms, 200 ms)
const RESYNC_WAIT: Duration = Duration::from_millis(250);
/// a resync still not quiet after this closes the transport and opens it again (TCP)
const NOISY: Duration = Duration::from_secs(1);
/// vendor bulk: pushes that keep coming are a session's and stop when its lease ends, lease_max_ms at the most
const PUSHES_END: Duration = Duration::from_millis(60_000 + 200);
/// a held serial port: resends at once after a broken frame, each, while frames keep arriving (host guide §8)
const BROKEN_RESENDS: u32 = 3;
/// confirm's request body: magic, the revisions this client handles (1..1)
const CONFIRM_BODY: &[u8] = b"OEP?\x01\x01";
/// core's lease_min_ms: the lease of a kept session's open again, ended at once
const LEASE_MIN_MS: u32 = 1000;
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
    /// the session id kept per probe for the next run (`kept`; None: not kept)
    pub kept: Option<KeptSession>,
    /// core §5.1: a request and its resend got no answer: nothing else goes out before a confirm (why, or empty)
    pub failed_transport: String,
    /// requests sent once more with the same corr
    pub retries: u64,
    /// the boot_id of the last confirm or open answer, and whether one changed (a reboot)
    pub boot_id: Option<u32>,
    pub rebooted: bool,
    /// fn -> (min_bytes, max_delay_ms) of its subscribe, and whether they stayed on a transport since closed
    subscriptions: Vec<(u16, u16, u32)>,
    moved: bool,
    /// the probing rule's confirm (transports §3): no resync, a failure is the caller's (who closes the link)
    pub probing: bool,
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
            kept: None,
            failed_transport: String::new(),
            retries: 0,
            boot_id: None,
            rebooted: false,
            subscriptions: vec![],
            moved: false,
            probing: false,
        }
    }

    /// A boot_id seen (confirm, open, the link's own confirm): a change is a reboot (core §6.5).
    fn boot_id_seen(&mut self, b: u32) {
        if self.boot_id.is_some_and(|x| x != b) {
            self.rebooted = true;
            self.fns.clear();
            self.describes.clear();
        }
        self.boot_id = Some(b);
    }

    fn next_corr(&mut self) -> u16 {
        self.corr = self.corr % 0xFFFF + 1;
        if let Some(k) = self.kept.as_mut() {
            k.sent(self.corr); // the next run must go on past it (kept)
        }
        self.corr
    }

    /// The last corr sent (a link's own confirm takes the next).
    pub fn corr(&self) -> u16 {
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
        Duration::from_millis(bound.max(self.expect_ms) + WAIT_ADD_MS) + self.link.transfer_time()
    }

    /// A serial port just opened: confirms at the boot speed, each waiting 0.5 s, for port_speed_idle_ms +
    /// host_wait_add_ms; the last one waits its whole floor (oep-client-python's `wait_boot_speed`).
    pub fn wait_boot_speed(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(3000 + WAIT_ADD_MS);
        loop {
            let last = Instant::now() + Duration::from_millis(500) >= deadline;
            let wait = if last {
                self.wait_for(CORE_FN, OP_CONFIRM)
            } else {
                Duration::from_millis(500)
            };
            let corr = self.next_corr();
            let m = msg::request(corr, CORE_FN, OP_CONFIRM, 0, CONFIRM_BODY);
            self.link.write(&[m])?;
            let end = Instant::now() + wait;
            loop {
                match self.link.recv_for(corr, end) {
                    Ok(Some(r)) => {
                        self.confirmed(&r);
                        return Ok(());
                    }
                    Ok(None) => break,
                    Err(Error::Broken(_)) => continue, // leftovers of a frame at another rate
                    Err(e) => return Err(e),
                }
            }
            if last {
                let b = self.link.baud.unwrap_or(0);
                return Err(Error::Timeout(format!("no answer to confirm at {b} for 4.0 s")));
            }
        }
    }

    fn session_for(&self, locked: bool) -> u32 {
        if locked { self.session.unwrap_or(0) } else { 0 }
    }

    /// One request: rejections are errors; any other answer comes back.
    pub fn request(&mut self, fn_: u16, op: u8, payload: &[u8], locked: bool) -> Result<msg::Reply> {
        let sid = self.session_for(locked);
        self.request_as(fn_, op, payload, sid)
    }

    /// Before a request: a failed transport recovered (core §5.1), subscriptions left on a closed transport made
    /// again (core §10.3).
    fn before_request(&mut self) -> Result<()> {
        if !self.failed_transport.is_empty() {
            self.recover_transport(3)?;
        }
        if self.moved {
            self.moved = false;
            if self.session.is_some() {
                for (f, min, delay) in self.subscriptions.clone() {
                    self.subscribe(f, min, delay)?;
                }
            }
        }
        Ok(())
    }

    /// `message` sent and its answer read, sent again as core §5.1 allows (oep-client-python's `_send_loop`): after
    /// a lost or broken answer the link is set right (`recover`), then the request goes once more with the same corr
    /// (a broken frame on a serial port up to BROKEN_RESENDS times); the resend unanswered too fails the transport.
    fn send_loop(&mut self, message: &[u8], corr: u16, wait: Duration) -> Result<Vec<u8>> {
        let (mut waited, mut broken) = (0u32, 0u32);
        loop {
            self.link.write(&[message.to_vec()])?;
            let deadline = Instant::now() + wait.max(self.link.timeout);
            let e = match self.link.recv_for(corr, deadline) {
                Ok(Some(r)) => return Ok(r),
                Ok(None) => Error::Timeout("no result from the probe".into()),
                Err(e @ (Error::Framing(_) | Error::Broken(_))) => e,
                Err(e) => return Err(e),
            };
            let corrupt = matches!(e, Error::Broken(_));
            if corrupt {
                self.link.corrupt += 1;
            }
            self.recover(&e)?;
            if corrupt && self.link.framing == Framing::Cobs && broken < BROKEN_RESENDS {
                broken += 1;
            } else if waited > 0 || broken >= BROKEN_RESENDS {
                return Err(self.fail_transport(&e));
            } else {
                waited += 1;
            }
            if !self.link.resend {
                self.given_up(&e);
                return Err(e);
            }
            self.retries += 1;
        }
    }

    /// After a lost, broken or missing answer: the link left in step for the next request. A serial port needs
    /// nothing (a late answer is read past by its corr), nor does TCP unless its framing broke (transports §5).
    fn recover(&mut self, e: &Error) -> Result<()> {
        if self.probing || (self.link.keeps_boundaries() && !matches!(e, Error::Framing(_))) {
            return Ok(());
        }
        if self.link.framing == Framing::Length {
            self.resync(3)?;
        }
        Ok(())
    }

    /// core §5.1: the resend went unanswered too - the transport failed (TransportFailed, a TimeoutError). A
    /// length-framed link whose framing broke has just resynced; a serial port and TCP recover before the next request.
    fn fail_transport(&mut self, e: &Error) -> Error {
        let why = format!("a request and its resend got no answer ({e})");
        if self.probing {
            return e.clone();
        }
        if self.link.framing != Framing::Length || (self.link.keeps_boundaries() && !matches!(e, Error::Framing(_))) {
            self.failed_transport = why.clone();
        }
        Error::Timeout(why)
    }

    /// A request not sent again (resend off: TCP): on TCP whose boundaries held, and on a serial port, the transport
    /// is confirmed before anything else goes out (core §5.1).
    fn given_up(&mut self, e: &Error) {
        if self.probing {
            return;
        }
        if (self.link.keeps_boundaries() && !matches!(e, Error::Framing(_))) || self.link.framing == Framing::Cobs {
            self.failed_transport = format!("a request got no answer and was not sent again ({e})");
        }
    }

    /// transports §5: read and discard until the input is quiet for 50 ms, then prove the link with a confirm (a
    /// read, safe to send), then go on (oep-client-python's `SerialLink.resync`). Pushes keep the input busy until
    /// their session's lease ends, so on vendor bulk it waits up to PUSHES_END; TCP closes the connection and opens it
    /// again first (and again once when it stays noisy). Nothing that changes state goes out before the confirm.
    pub fn resync(&mut self, tries: u32) -> Result<()> {
        self.link.resyncs += 1;
        if self.link.framing != Framing::Length {
            return Ok(()); // COBS frames carry their own boundaries: nothing to find again
        }
        let keeps = self.link.keeps_boundaries();
        if keeps {
            self.link.reopen()?;
            self.moved = true;
        }
        let limit = if keeps { NOISY } else { NOISY.max(PUSHES_END) };
        let mut reopened = false;
        for _ in 0..tries {
            while !self.link.discard_until_quiet(RESYNC_QUIET, limit) {
                if reopened || self.link.reopener.is_none() {
                    return Err(Error::Connection(
                        "resync: the input never went quiet - close the transport and open it again (transports §5)"
                            .into(),
                    ));
                }
                self.link.reopen()?;
                self.moved = true;
                reopened = true;
            }
            if self.confirm_own()? {
                return Ok(());
            }
        }
        Err(Error::Connection(format!(
            "resync: no confirm came back in {tries} tries"
        )))
    }

    /// The link's own confirm, after waiting past probe_frame_gap_ms since the last write: true when its answer
    /// came (its boot_id and max_frame taken), false when it did not or the frames broke again.
    fn confirm_own(&mut self) -> Result<bool> {
        if let Some(t) = self.link.last_write {
            let left = (t + RESYNC_WAIT).saturating_duration_since(Instant::now());
            if !left.is_zero() {
                std::thread::sleep(left);
            }
        }
        let corr = self.next_corr();
        let m = msg::request(corr, CORE_FN, OP_CONFIRM, 0, CONFIRM_BODY);
        self.link.write(&[m])?;
        let deadline = Instant::now() + self.wait_for(CORE_FN, OP_CONFIRM).max(self.link.timeout);
        match self.link.recv_for(corr, deadline) {
            Ok(Some(r)) => {
                self.confirmed(&r);
                Ok(true)
            }
            Ok(None) | Err(Error::Framing(_)) | Err(Error::Broken(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// A confirm answer read by the link for itself: its max_frame and boot_id.
    fn confirmed(&mut self, raw: &[u8]) {
        let Ok(r) = msg::Reply::parse(raw) else { return };
        let p = &r.payload;
        if r.succeeded() && p.len() >= 16 && &p[..4] == b"OEP!" && p[4] >= 1 {
            let mf = u16::from_le_bytes([p[5], p[6]]) as usize;
            self.link.max_frame = mf.max(64);
            self.boot_id_seen(u32::from_le_bytes([p[12], p[13], p[14], p[15]]));
        }
    }

    /// core §5.1: before anything else after a failed transport - the length framing resynced; a serial port read
    /// quiet and confirmed until the answer with its own corr comes. ConnectionError when none is answered.
    pub fn recover_transport(&mut self, tries: u32) -> Result<()> {
        let why = std::mem::take(&mut self.failed_transport);
        if self.link.framing == Framing::Length {
            if let Err(e) = self.resync(tries) {
                self.failed_transport = why;
                return Err(e);
            }
            return Ok(());
        }
        for _ in 0..tries {
            self.link.discard_until_quiet(RESYNC_QUIET, NOISY);
            if self.confirm_own()? {
                return Ok(());
            }
        }
        let e = Error::Connection(format!(
            "the transport failed ({why}) and no confirm came back in {tries} tries: close and open it again"
        ));
        self.failed_transport = why;
        Err(e)
    }

    /// Read what arrives within `timeout` (pushes and events kept on the link); a lost boundary resyncs (transports
    /// §5) and comes back with what was read.
    pub fn pump(&mut self, timeout: Duration, until_one: bool) -> Result<usize> {
        match self.link.pump(timeout, until_one) {
            Err(Error::Framing(_)) => {
                self.resync(3)?;
                Ok(0)
            }
            r => r,
        }
    }

    fn request_as(&mut self, fn_: u16, op: u8, payload: &[u8], sid: u32) -> Result<msg::Reply> {
        self.before_request()?;
        let corr = self.next_corr();
        let m = msg::request(corr, fn_, op, sid, payload);
        let wait = self.wait_for(fn_, op);
        let raw = self.send_loop(&m, corr, wait)?;
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
        self.before_request()?;
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
        let raws = self.exchange(&msgs, inflight, window, wait)?;
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

    /// Pipelined: up to `inflight` requests and `window` bytes outstanding, results in order (oep-client-python's
    /// `SerialLink.exchange`). After a broken or missing answer the link is set right and the requests not answered
    /// yet go again with the same corr (core §5.1); a failure past that is the caller's.
    fn exchange(&mut self, msgs: &[Vec<u8>], inflight: usize, window: usize, wait: Duration) -> Result<Vec<Vec<u8>>> {
        let mut replies: Vec<Vec<u8>> = Vec::with_capacity(msgs.len());
        let (mut waited, mut broken) = (0u32, 0u32);
        loop {
            let mut got = vec![];
            let res = self
                .link
                .exchange_once(&msgs[replies.len()..], inflight, window, wait, &mut got);
            replies.extend(got);
            let e = match res {
                Ok(()) => return Ok(replies),
                Err(e @ (Error::Timeout(_) | Error::Framing(_) | Error::Broken(_))) => e,
                Err(e) => return Err(e),
            };
            let corrupt = matches!(e, Error::Broken(_));
            if corrupt {
                self.link.corrupt += 1;
            }
            self.recover(&e)?;
            if !self.link.resend {
                self.given_up(&e);
                return Err(e);
            }
            if corrupt && self.link.framing == Framing::Cobs && broken < BROKEN_RESENDS {
                broken += 1;
            } else if waited > 0 || broken >= BROKEN_RESENDS {
                return Err(self.fail_transport(&e));
            } else {
                waited += 1;
            }
            self.retries += 1;
        }
    }

    /// confirm (core §7.1): revision 1, the probe's limits.
    pub fn confirm(&mut self) -> Result<Limits> {
        let r = self.call(CORE_FN, OP_CONFIRM, CONFIRM_BODY, false)?;
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
        self.link.max_frame = l.max_frame; // a length-framed reader bounds frames by it (core §4.4)
        self.boot_id_seen(boot_id);
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
        if self.session.is_none() && self.kept.as_ref().is_some_and(|k| !k.done) {
            self.end_kept(owner)?; // once: the previous run's session ended (host guide §5)
        }
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
        self.boot_id_seen(boot_id);
        self.subscriptions.clear();
        self.moved = false;
        self.session = Some(sid);
        self.lease_ms = Some(lease);
        let corr = self.corr;
        if let Some(k) = self.kept.as_mut() {
            k.opened(sid, corr);
        }
        Ok((lease, boot_id))
    }

    /// kept's before_open: find the file by the probe's unit_id, lock it, end the session it names.
    fn end_kept(&mut self, owner: Option<&str>) -> Result<()> {
        let unit = self
            .describe(CORE_FN)?
            .into_iter()
            .find(|(t, _)| *t == TAG_UNIT_ID)
            .map(|(_, v)| String::from_utf8_lossy(&v).to_string());
        let current = self.session;
        let Some(prev) = self.kept.as_mut().and_then(|k| k.take_file(unit.as_deref(), current)) else {
            return Ok(());
        };
        let ended = self.end_previous(prev.sid, owner, Some(prev.corr))?;
        if let Some(k) = self.kept.as_mut() {
            k.ended_previous = ended;
        }
        Ok(())
    }

    /// End session `sid` that a previous run of this host left open (host guide §5): open it - taken as a resend of
    /// its open while it holds the lock (core §6.2) - and end it at once, releasing what it held. -> true when it was
    /// ended; false when no session holds the lock (nothing to end) or another one does. `corr`: one at or ahead of
    /// every corr the previous run sent (kept's reservation): the open and the end go on after it (core §4.1, §5.1).
    pub fn end_previous(&mut self, sid: u32, owner: Option<&str>, corr: Option<u16>) -> Result<bool> {
        if let Some(c) = corr {
            self.corr = c;
        }
        let (locked, _, _) = self.lock_state()?;
        if !locked {
            return Ok(false);
        }
        let mut p = LEASE_MIN_MS.to_le_bytes().to_vec();
        p.push(0);
        if let Some(o) = owner {
            p.extend(msg::tlv(0x01, owner_text(o).as_bytes()));
        }
        match self.request_as(CORE_FN, OP_OPEN, &p, sid) {
            Ok(_) => {}
            Err(Error::Rejected(..)) => return Ok(false),
            Err(e) => return Err(e),
        }
        match self.request_as(CORE_FN, OP_END, &[], sid) {
            Ok(_) => Ok(true),
            Err(Error::Rejected(..)) => Ok(false),
            Err(e) => Err(e),
        }
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
            if let Some(k) = self.kept.as_mut() {
                k.ended();
            }
            return Ok(());
        }
        let r = self.request(CORE_FN, OP_END, &[], true);
        self.session = None;
        r?;
        if let Some(k) = self.kept.as_mut() {
            k.ended();
        }
        Ok(())
    }

    /// lock_state: (locked, remaining ms, owner).
    pub fn lock_state(&mut self) -> Result<(bool, u32, Option<String>)> {
        let r = self.call(CORE_FN, OP_LOCK_STATE, &[], false)?;
        let mut rd = Reader::new(&r.payload);
        let (locked, remaining) = (rd.u8()?, rd.u32()?);
        Ok((locked != 0, remaining, msg::owner_of(rd.rest())))
    }

    pub fn clock(&mut self) -> Result<ClockReading> {
        self.before_request()?;
        let corr = self.next_corr();
        let m = msg::request(corr, CORE_FN, OP_CLOCK, 0, &[]);
        let wait = self.wait_for(CORE_FN, OP_CLOCK);
        let before = monotonic_ns();
        let raw = self.send_loop(&m, corr, wait)?;
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
        self.call(fn_, OP_SUBSCRIBE, &p, true)?;
        self.subscriptions.retain(|s| s.0 != fn_);
        self.subscriptions.push((fn_, min_bytes, max_delay_ms));
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Transport;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<(u8, u16)>>>;

    /// A probe of confirm and lock_state on length frames: each request logged (op, corr); `garble` says, for the
    /// n-th request (from 0), what comes before its answer (None: no answer at all).
    fn probe(
        log: Log,
        garble: impl Fn(usize) -> Option<Vec<u8>> + Send + 'static,
    ) -> impl FnMut(&[u8]) -> Vec<Vec<u8>> {
        let mut n = 0usize;
        move |data: &[u8]| {
            let mut out = vec![];
            let mut at = 0;
            while at + 2 <= data.len() {
                let len = u16::from_le_bytes([data[at], data[at + 1]]) as usize;
                let m = &data[at + 2..at + 2 + len];
                at += 2 + len;
                let (corr, op) = (u16::from_le_bytes([m[1], m[2]]), m[5]);
                log.lock().unwrap().push((op, corr));
                let k = n;
                n += 1;
                let Some(before) = garble(k) else { continue };
                let payload: Vec<u8> = match op {
                    OP_CONFIRM => {
                        let mut p = b"OEP!\x01".to_vec();
                        p.extend(512u16.to_le_bytes());
                        p.extend(4096u32.to_le_bytes());
                        p.push(1);
                        p.extend(7u32.to_le_bytes());
                        p
                    }
                    _ => vec![0, 0, 0, 0, 0],
                };
                let mut r = vec![msg::ROLE_RESULT];
                r.extend(corr.to_le_bytes());
                r.extend([msg::COMPLETED, msg::SUCCESS]);
                r.extend(payload);
                let mut chunk = before;
                chunk.extend((r.len() as u16).to_le_bytes());
                chunk.extend(r);
                out.push(chunk);
            }
            out
        }
    }

    #[test]
    fn a_lost_boundary_on_vendor_bulk_resyncs_and_sends_again() {
        let log: Log = Arc::default();
        // request 1 (the first lock_state): a length past max_frame comes before its answer
        let t = Transport::scripted(
            probe(log.clone(), |k| {
                Some(if k == 1 { vec![0xFF, 0x7F, 9, 9] } else { vec![] })
            }),
            false,
        );
        let mut h = Host::new(Link::new(t));
        h.confirm().unwrap();
        let (locked, _, _) = h.lock_state().unwrap();
        assert!(!locked);
        assert_eq!(h.link.resyncs, 1);
        assert_eq!(h.retries, 1);
        let ops: Vec<(u8, u16)> = log.lock().unwrap().clone();
        // confirm, lock_state, the resync's confirm (a new corr), lock_state again with its corr
        assert_eq!(
            ops,
            vec![(OP_CONFIRM, 1), (OP_LOCK_STATE, 2), (OP_CONFIRM, 3), (OP_LOCK_STATE, 2)]
        );
        assert_eq!(h.link.max_frame, 512);
    }

    #[test]
    fn a_lost_boundary_on_tcp_opens_the_connection_again_and_is_the_callers() {
        let log: Log = Arc::default();
        let t = Transport::scripted(
            probe(log.clone(), |k| Some(if k == 1 { vec![0xFF, 0x7F] } else { vec![] })),
            true,
        );
        let mut h = Host::new(Link::new(t));
        let l2 = log.clone();
        h.link.reopener = Some(Box::new(move || {
            Ok(Transport::scripted(probe(l2.clone(), |_| Some(vec![])), true))
        }));
        h.confirm().unwrap();
        // resend off on TCP: the request is not sent again, the error is the caller's - after the connection was
        // opened again and confirmed (transports §5)
        assert!(matches!(h.lock_state(), Err(Error::Framing(_))));
        assert_eq!((h.link.reopens, h.retries), (1, 0));
        assert!(h.failed_transport.is_empty());
        assert!(h.lock_state().is_ok());
        let ops: Vec<u8> = log.lock().unwrap().iter().map(|x| x.0).collect();
        assert_eq!(ops, vec![OP_CONFIRM, OP_LOCK_STATE, OP_CONFIRM, OP_LOCK_STATE]);
    }

    #[test]
    fn no_answer_on_tcp_confirms_on_a_new_connection_before_the_next_request() {
        let log: Log = Arc::default();
        let t = Transport::scripted(probe(log.clone(), |k| if k == 1 { None } else { Some(vec![]) }), true);
        let mut h = Host::new(Link::new(t));
        let l2 = log.clone();
        h.link.reopener = Some(Box::new(move || {
            Ok(Transport::scripted(probe(l2.clone(), |_| Some(vec![])), true))
        }));
        h.link.timeout = Duration::from_millis(10);
        h.confirm().unwrap();
        assert!(matches!(h.lock_state(), Err(Error::Timeout(_))));
        assert!(!h.failed_transport.is_empty());
        assert!(h.lock_state().is_ok());
        assert_eq!(h.link.reopens, 1);
        let ops: Vec<u8> = log.lock().unwrap().iter().map(|x| x.0).collect();
        assert_eq!(ops, vec![OP_CONFIRM, OP_LOCK_STATE, OP_CONFIRM, OP_LOCK_STATE]);
    }
}
