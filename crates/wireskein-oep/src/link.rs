//! Length-prefixed frames on a byte stream (oep-core §3, transports §1: u16 length, then the message) and what the
//! probe sends: results matched to their request by corr, data pushes and events kept for whoever collects them
//! (oep-client-python's `frames.LengthFrames` and the routing of `link.SerialLink`).

use crate::msg::{self, Error, Result};
use crate::transport::Transport;
use std::collections::VecDeque;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

/// A frame boundary that cannot be right (a length past max_frame).
const MAX_FRAME: usize = 0xFFFF;

pub struct Link {
    pub t: Transport,
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
}

impl Link {
    pub fn new(t: Transport) -> Link {
        let resend = !t.keeps_boundaries;
        Link {
            t,
            buf: Vec::with_capacity(1 << 20),
            at: 0,
            pushes: VecDeque::new(),
            events: VecDeque::new(),
            timeout: Duration::from_secs(3),
            resend,
            stale: 0,
            dropped: 0,
        }
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
            chunk.extend((m.len() as u16).to_le_bytes());
            chunk.extend_from_slice(m);
        }
        self.t.write(&chunk)
    }

    fn take_frame(&mut self) -> Option<Vec<u8>> {
        loop {
            let have = self.buf.len() - self.at;
            if have < 2 {
                return None;
            }
            let n = u16::from_le_bytes([self.buf[self.at], self.buf[self.at + 1]]) as usize;
            if n == 0 {
                self.at += 2; // the reserved keepalive frame
                continue;
            }
            if have < 2 + n {
                return None;
            }
            let f = self.buf[self.at + 2..self.at + 2 + n].to_vec();
            self.at += 2 + n;
            if self.at == self.buf.len() {
                self.buf.clear();
                self.at = 0;
            } else if self.at > (1 << 20) && self.at * 2 > self.buf.len() {
                self.buf.drain(..self.at);
                self.at = 0;
            }
            return Some(f);
        }
    }

    /// The next frame, or None when nothing whole arrived by `deadline`.
    pub fn recv(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>> {
        loop {
            if let Some(f) = self.take_frame() {
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

    /// A frame that is not a result kept (pushes, events) or dropped; true when it was one of them.
    fn route(&mut self, f: Vec<u8>) -> Result<Option<Vec<u8>>> {
        match f.first().copied() {
            Some(msg::ROLE_RESULT) if f.len() >= msg::RESULT_HEADER => Ok(Some(f)),
            Some(msg::ROLE_RESULT) => Err(Error::Protocol("a result shorter than its header".into())),
            Some(msg::ROLE_DATA) if f.len() >= msg::DATA_HEADER => {
                self.pushes.push_back(f);
                Ok(None)
            }
            Some(msg::ROLE_EVENT) if f.len() >= msg::EVENT_HEADER => {
                self.events.push_back(f);
                Ok(None)
            }
            Some(msg::ROLE_DATA) | Some(msg::ROLE_EVENT) => {
                Err(Error::Protocol("a notification shorter than its header".into()))
            }
            _ => {
                self.dropped += 1;
                Ok(None)
            }
        }
    }

    /// The result for `corr`, notifications kept on the way; None at the deadline.
    fn recv_for(&mut self, corr: u16, deadline: Instant) -> Result<Option<Vec<u8>>> {
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

    /// Read what arrives within `timeout` (pushes and events kept); `until_one`: back after the first frame.
    pub fn pump(&mut self, timeout: Duration, until_one: bool) -> Result<usize> {
        let deadline = Instant::now() + timeout;
        let mut n = 0;
        while let Some(f) = self.recv(deadline)? {
            n += 1;
            if self.route(f)?.is_some() {
                self.stale += 1;
            }
            if until_one {
                break;
            }
        }
        Ok(n)
    }

    /// Several requests (each a whole message, its corr in bytes 1-2) and their results in order: up to `inflight`
    /// and `window` bytes outstanding. An answer that does not come: the ones not answered go once more with the same
    /// corr (core §5.1: the probe answers a repeat from what it kept), then the timeout is the caller's.
    pub fn exchange(
        &mut self,
        messages: &[Vec<u8>],
        inflight: usize,
        window: usize,
        wait: Duration,
    ) -> Result<Vec<Vec<u8>>> {
        let mut replies = Vec::with_capacity(messages.len());
        let mut tries = 0;
        loop {
            match self.exchange_once(&messages[replies.len()..], inflight, window, wait, &mut replies) {
                Ok(()) => return Ok(replies),
                Err(Error::Timeout(why)) if self.resend && tries == 0 => {
                    tries += 1;
                    let _ = why;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn exchange_once(
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

    /// One request and its result (sent once more with the same corr when the answer did not come).
    pub fn send(&mut self, message: &[u8], wait: Duration) -> Result<Vec<u8>> {
        Ok(self.exchange(&[message.to_vec()], 1, usize::MAX, wait)?.remove(0))
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
}
