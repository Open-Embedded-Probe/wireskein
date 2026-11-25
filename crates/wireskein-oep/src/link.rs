//! Frames on a byte stream and what the probe sends: results matched to their request by corr, data pushes and
//! events kept for whoever collects them (oep-client-python's `frames.LengthFrames`, `cobs` and the routing of
//! `link.SerialLink`).
//!
//! Two framings: length-prefixed (oep-core §3, transports §1: u16 length, then the message - USB vendor bulk, TCP) and
//! COBS with a 0x00 delimiter and a CRC (transports §4: a serial port). A length-prefixed stream has no CRC: a length
//! past max_frame, or a frame shorter than its header, means the boundaries are lost (`Error::Framing`), and the host
//! resyncs (transports §5, `Host::resync`). A COBS frame that does not decode is a broken frame (`Error::Broken`): the
//! next delimiter is the next frame, nothing to find again.

use crate::cobs;
use crate::msg::{self, Error, Result};
use crate::transport::Transport;
use std::collections::VecDeque;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

/// The longest a length-prefixed frame can say until a confirm answer gives the probe's max_frame.
pub const MAX_FRAME: usize = 0xFFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    Length,
    Cobs,
}

/// How a link opens its transport again (transports §5: a TCP connection whose framing broke).
pub type Reopener = Box<dyn FnMut() -> Result<Transport> + Send>;

pub struct Link {
    pub t: Transport,
    pub framing: Framing,
    buf: Vec<u8>,
    at: usize,
    /// data pushes (role 0x04), oldest first
    pub pushes: VecDeque<Vec<u8>>,
    /// events (role 0x03), oldest first
    pub events: VecDeque<Vec<u8>>,
    /// how long one answer is waited for at least
    pub timeout: Duration,
    /// a request whose answer did not come goes once more with the same corr (core §5.1); off on TCP
    pub resend: bool,
    /// results for no request waited for, read past
    pub stale: u64,
    /// frames of a role this host does not handle
    pub dropped: u64,
    /// a length-prefixed frame longer than this is a lost boundary (the probe's max_frame once confirmed)
    pub max_frame: usize,
    /// when this host last wrote (a resync's confirm waits past probe_frame_gap_ms after it, transports §5)
    pub last_write: Option<Instant>,
    /// how to open the transport again (TCP), if it can be
    pub reopener: Option<Reopener>,
    /// resyncs and transports opened again (statistics)
    pub resyncs: u64,
    pub reopens: u64,
    /// COBS: broken frames and the bytes that were not a frame
    pub corrupt: u64,
    pub noise: u64,
    /// a serial port: the line speed in force (the transfer time of an answer counts it, core §4.4)
    pub baud: Option<u32>,
    /// the longest frame last written (core §4.4's transfer time)
    pub tx_len: usize,
    /// a serial port: how its frames go at a raised port_speed rate (oep-client-python's SerialLink counts)
    pub judge: crate::speed::Judge,
}

impl Link {
    pub fn new(t: Transport) -> Link {
        Link::with_framing(t, Framing::Length)
    }

    pub fn with_framing(t: Transport, framing: Framing) -> Link {
        let resend = !t.keeps_boundaries;
        Link {
            t,
            framing,
            buf: Vec::with_capacity(1 << 20),
            at: 0,
            pushes: VecDeque::new(),
            events: VecDeque::new(),
            timeout: Duration::from_secs(3),
            resend,
            stale: 0,
            dropped: 0,
            max_frame: MAX_FRAME,
            last_write: None,
            reopener: None,
            resyncs: 0,
            reopens: 0,
            corrupt: 0,
            noise: 0,
            baud: None,
            tx_len: 0,
            judge: crate::speed::Judge::default(),
        }
    }

    /// A stream that never loses a frame boundary (TCP, transports §2).
    pub fn keeps_boundaries(&self) -> bool {
        self.framing == Framing::Length && self.t.keeps_boundaries
    }

    /// Frames written in one go (a frame is never split by a pause here, transports §2).
    pub fn write(&mut self, messages: &[Vec<u8>]) -> Result<()> {
        let mut chunk = Vec::new();
        for m in messages {
            if m.is_empty() || m.len() > MAX_FRAME {
                return Err(Error::Value(format!(
                    "message length {} outside 1..{MAX_FRAME}",
                    m.len()
                )));
            }
            match self.framing {
                Framing::Length => {
                    chunk.extend((m.len() as u16).to_le_bytes());
                    chunk.extend_from_slice(m);
                }
                Framing::Cobs => chunk.extend(cobs::frame(m)),
            }
        }
        self.last_write = Some(Instant::now());
        if self.framing == Framing::Cobs {
            self.judge.moved(chunk.len() as u64, self.baud);
        }
        self.tx_len = messages
            .iter()
            .map(|m| match self.framing {
                Framing::Length => m.len() + 2,
                Framing::Cobs => cobs::frame_max(m.len()),
            })
            .max()
            .unwrap_or(0);
        self.t.write(&chunk)
    }

    fn compact(&mut self) {
        if self.at == self.buf.len() {
            self.buf.clear();
            self.at = 0;
        } else if self.at > (1 << 20) && self.at * 2 > self.buf.len() {
            self.buf.drain(..self.at);
            self.at = 0;
        }
    }

    fn take_frame(&mut self) -> Result<Option<Vec<u8>>> {
        match self.framing {
            Framing::Length => self.take_length(),
            Framing::Cobs => self.take_cobs(),
        }
    }

    fn take_length(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            let have = self.buf.len() - self.at;
            if have < 2 {
                return Ok(None);
            }
            let n = u16::from_le_bytes([self.buf[self.at], self.buf[self.at + 1]]) as usize;
            if n == 0 {
                self.at += 2; // the reserved keepalive frame
                continue;
            }
            if n > self.max_frame {
                return Err(Error::Framing(format!("frame length {n} exceeds {}", self.max_frame)));
            }
            if have < 2 + n {
                return Ok(None);
            }
            let f = self.buf[self.at + 2..self.at + 2 + n].to_vec();
            self.at += 2 + n;
            self.compact();
            return Ok(Some(f));
        }
    }

    fn take_cobs(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            let Some(end) = self.buf[self.at..].iter().position(|&b| b == 0) else {
                return Ok(None);
            };
            let raw = self.buf[self.at..self.at + end].to_vec();
            self.at += end + 1; // the 0x00 also starts the next candidate
            self.compact();
            if raw.is_empty() {
                continue;
            }
            return match cobs::unframe(&raw) {
                Ok(f) => {
                    self.judge.moved(raw.len() as u64 + 2, self.baud);
                    self.judge.count(crate::speed::Kind::Good, self.baud);
                    Ok(Some(f))
                }
                Err(why) => {
                    self.noise += raw.len() as u64;
                    self.judge.count(crate::speed::Kind::Broken, self.baud);
                    Err(Error::Broken(why))
                }
            };
        }
    }

    /// The next frame, or None when nothing whole arrived by `deadline`.
    pub fn recv(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>> {
        loop {
            if let Some(f) = self.take_frame()? {
                return Ok(Some(f));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            match self.t.rx.recv_timeout(deadline - now) {
                Ok(chunk) => {
                    self.buf.extend_from_slice(&chunk);
                    // whatever else is already there, without waiting
                    while let Ok(more) = self.t.rx.try_recv() {
                        self.buf.extend_from_slice(&more);
                        if self.buf.len() - self.at > (8 << 20) {
                            break;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::Io("the link to the probe closed".into()));
                }
            }
        }
    }

    /// A frame that decoded but cannot be read: on a length-framed link the boundaries are in doubt (a resync
    /// follows); on a serial port it is a broken frame like a bad CRC.
    fn broken(&mut self, f: &[u8], why: &str) -> Error {
        match self.framing {
            Framing::Length => Error::Framing(why.into()),
            Framing::Cobs => {
                self.noise += f.len() as u64;
                Error::Broken(why.into())
            }
        }
    }

    /// A frame that is not a result kept (pushes, events) or dropped; a result comes back.
    fn route(&mut self, f: Vec<u8>) -> Result<Option<Vec<u8>>> {
        match f.first().copied() {
            Some(msg::ROLE_RESULT) if f.len() >= msg::RESULT_HEADER => Ok(Some(f)),
            Some(msg::ROLE_RESULT) => Err(self.broken(&f, "a result shorter than its header")),
            Some(msg::ROLE_DATA) if f.len() >= msg::DATA_HEADER => {
                self.pushes.push_back(f);
                Ok(None)
            }
            Some(msg::ROLE_EVENT) if f.len() >= msg::EVENT_HEADER => {
                self.events.push_back(f);
                Ok(None)
            }
            Some(msg::ROLE_DATA) => Err(self.broken(&f, "a data frame shorter than its header")),
            Some(msg::ROLE_EVENT) => Err(self.broken(&f, "an event shorter than its header")),
            _ => {
                self.dropped += 1;
                Ok(None)
            }
        }
    }

    /// The result for `corr`, notifications kept on the way; None at the deadline. A result for a corr not waited for
    /// is read past (core §10.1) - it starts no resync.
    pub fn recv_for(&mut self, corr: u16, deadline: Instant) -> Result<Option<Vec<u8>>> {
        loop {
            let Some(f) = self.recv(deadline)? else {
                return Ok(None);
            };
            if let Some(r) = self.route(f)? {
                if u16::from_le_bytes([r[1], r[2]]) == corr {
                    return Ok(Some(r));
                }
                self.stale += 1;
            }
        }
    }

    /// Read what arrives within `timeout` (pushes and events kept); `until_one`: back after the first frame. A lost
    /// boundary comes back as `Error::Framing` (the host resyncs); a broken COBS frame is counted and read past.
    pub fn pump(&mut self, timeout: Duration, until_one: bool) -> Result<usize> {
        let deadline = Instant::now() + timeout;
        let mut n = 0;
        loop {
            let f = match self.recv(deadline) {
                Ok(Some(f)) => f,
                Ok(None) => return Ok(n),
                Err(Error::Broken(_)) => {
                    self.corrupt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            };
            match self.route(f) {
                Ok(Some(_)) => self.stale += 1,
                Ok(None) => {}
                Err(Error::Broken(_)) => {
                    self.corrupt += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
            n += 1;
            if until_one || Instant::now() >= deadline {
                return Ok(n);
            }
        }
    }

    /// Several requests (each a whole message, its corr in bytes 1-2) and their results in order, sent once: up to
    /// `inflight` and `window` bytes outstanding; each answer waited for `wait` (at least the link's timeout). The
    /// results that came are in `replies` whatever happens.
    pub fn exchange_once(
        &mut self,
        messages: &[Vec<u8>],
        inflight: usize,
        window: usize,
        wait: Duration,
        replies: &mut Vec<Vec<u8>>,
    ) -> Result<()> {
        let mut out: VecDeque<(u16, usize)> = VecDeque::new();
        let mut batch: Vec<Vec<u8>> = vec![];
        let corr = |m: &[u8]| u16::from_le_bytes([m[1], m[2]]);
        for m in messages {
            let size = m.len() + 2;
            while !out.is_empty()
                && (out.len() >= inflight.max(1) || out.iter().map(|x| x.1).sum::<usize>() + size > window)
            {
                if !batch.is_empty() {
                    self.write(&batch)?;
                    batch.clear();
                }
                let (c, _) = out.pop_front().expect("outstanding");
                replies.push(self.answer(c, wait)?);
            }
            batch.push(m.clone());
            out.push_back((corr(m), size));
        }
        if !batch.is_empty() {
            self.write(&batch)?;
        }
        while let Some((c, _)) = out.pop_front() {
            replies.push(self.answer(c, wait)?);
        }
        Ok(())
    }

    fn answer(&mut self, corr: u16, wait: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + wait.max(self.timeout);
        self.recv_for(corr, deadline)?
            .ok_or_else(|| Error::Timeout("no result from the probe".into()))
    }

    /// transports §5 / §3: read and discard until the input is quiet for `quiet`, at most `limit`.
    pub fn discard_until_quiet(&mut self, quiet: Duration, limit: Duration) -> bool {
        self.buf.clear();
        self.at = 0;
        let end = Instant::now() + limit;
        while Instant::now() < end {
            match self.t.rx.recv_timeout(quiet) {
                Ok(_) => continue,
                Err(_) => return true,
            }
        }
        false
    }

    /// core §4.4's transfer time of one answer on a port with a line speed: (L + max_frame x (1 +
    /// notify_pending_max_frames)) x 10 / baud; 0 elsewhere.
    pub fn transfer_time(&self) -> Duration {
        match (self.framing, self.baud) {
            (Framing::Cobs, Some(b)) if b > 0 => {
                let mf = if self.max_frame == MAX_FRAME {
                    64
                } else {
                    self.max_frame
                };
                Duration::from_secs_f64((self.tx_len + mf * 3) as f64 * 10.0 / b as f64)
            }
            _ => Duration::ZERO,
        }
    }

    /// What was read and not yet framed dropped (a serial port at another rate).
    pub fn discard_input(&mut self) {
        self.buf.clear();
        self.at = 0;
        while self.t.rx.try_recv().is_ok() {}
    }

    /// transports §5: the transport closed and opened again (a TCP connection whose framing broke: quiet input and a
    /// confirm do not reset the probe's parser there). The session stays on the probe.
    pub fn reopen(&mut self) -> Result<()> {
        let Some(open) = self.reopener.as_mut() else {
            return Err(Error::Connection(
                "the framing broke: close the connection and open it again (transports §5)".into(),
            ));
        };
        let t = open()?;
        self.t = t; // the old one dropped: closed
        self.buf.clear();
        self.at = 0;
        self.last_write = None;
        self.reopens += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    fn link_of(chunks: Vec<Vec<u8>>, framing: Framing) -> Link {
        let (tx, rx) = sync_channel(64);
        for c in chunks {
            tx.send(c).unwrap();
        }
        std::mem::forget(tx); // open: nothing more comes
        Link::with_framing(Transport::from_rx(rx), framing)
    }

    fn lf(m: &[u8]) -> Vec<u8> {
        let mut v = (m.len() as u16).to_le_bytes().to_vec();
        v.extend_from_slice(m);
        v
    }

    #[test]
    fn a_length_past_max_frame_is_a_lost_boundary() {
        let mut lk = link_of(vec![lf(&[2, 1, 0, 1, 0]), vec![0xFF, 0x7F, 1, 2, 3]], Framing::Length);
        lk.max_frame = 512;
        let d = Instant::now() + Duration::from_millis(50);
        assert_eq!(lk.recv(d).unwrap().unwrap(), vec![2, 1, 0, 1, 0]);
        assert!(matches!(lk.recv(d), Err(Error::Framing(_))));
    }

    #[test]
    fn a_short_notification_is_a_lost_boundary_on_length_and_broken_on_cobs() {
        let mut lk = link_of(vec![lf(&[4, 1, 0])], Framing::Length);
        assert!(matches!(
            lk.pump(Duration::from_millis(50), false),
            Err(Error::Framing(_))
        ));
        let mut lk = link_of(
            vec![cobs::frame(&[4, 1, 0]), cobs::frame(&[2, 7, 0, 1, 0])],
            Framing::Cobs,
        );
        assert_eq!(lk.pump(Duration::from_millis(50), false).unwrap(), 1);
        assert_eq!(lk.corrupt, 1);
    }
}
