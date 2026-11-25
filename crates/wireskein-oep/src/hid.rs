//! OEP over a vendor-defined USB HID interface (oep-core §3, transports §1; oep-client-python's `hid_stream`): the
//! way in when vendor bulk is not usable, before a CDC port.
//!
//! Every report carries count (u16), first (u16: where the first frame starting in the report begins, 0xFFFF when
//! none) and count bytes of the length-prefixed frame stream, the rest zero. A reader whose own reading puts the next
//! frame's start elsewhere than first has lost a report: it drops the frame begun and reads on from first (rule 6); a
//! report whose count or first cannot be is dropped and the stream is broken until the next first (rule 7). What the
//! reader hands on is whole frames only, so the link never loses a boundary on HID (a resync waits for no quiet).
//!
//! The interface is OEP's when its report descriptor has a report of usage page 0xFF4F, usage 0x45 with input and
//! output of 3 bytes or more (another vendor HID is not taken). Reached by nusb's interrupt transfers: the interface
//! is taken from the kernel's HID driver (Linux; given back when closed) and claimed, so no other program reads it
//! meanwhile.

use crate::flow::Flow;
use crate::msg::{Error, Result};
use crate::transport::{Sink, Stats, Transport};
use nusb::MaybeFuture;
use nusb::transfer::{Buffer, ControlIn, ControlOut, ControlType, In, Interrupt, Out, Recipient};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const USAGE_PAGE: u32 = 0xFF4F;
pub const USAGE: u32 = 0x45;
/// count(u16), first(u16), after the report ID if any
pub const HEAD: usize = 4;
pub const NO_START: u16 = 0xFFFF;
const HID_CLASS: u8 = 3;
/// IN transfers kept queued (reports are small: these are how many the host can hold back)
const DEPTH: usize = 64;

/// Where the frames of a length-prefixed stream start: the frame begun so far.
#[derive(Debug, Default, Clone)]
pub struct FrameTrack {
    pub partial: Vec<u8>,
}

impl FrameTrack {
    /// The position in `data` where the next frame starts, or NO_START when the frame begun runs past it.
    pub fn next_start(&self, data: &[u8]) -> u16 {
        let p = &self.partial;
        if p.is_empty() {
            return if data.is_empty() { NO_START } else { 0 };
        }
        let need = if p.len() == 1 {
            if data.is_empty() {
                return NO_START;
            }
            2 + (p[0] as usize | (data[0] as usize) << 8) - 1
        } else {
            2 + (p[0] as usize | (p[1] as usize) << 8) - p.len()
        };
        if need < data.len() { need as u16 } else { NO_START }
    }

    /// `data` read on: the frames it completes (each with its length).
    pub fn feed(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.partial.extend_from_slice(data);
        let mut done = vec![];
        while self.partial.len() >= 2 {
            let end = 2 + (self.partial[0] as usize | (self.partial[1] as usize) << 8);
            if self.partial.len() < end {
                break;
            }
            done.push(self.partial.drain(..end).collect());
        }
        done
    }
}

/// The reports read back into whole frames (rules 6 and 7); `losses` counts reports found missing or broken.
#[derive(Debug, Default)]
pub struct Reports {
    track: FrameTrack,
    synced: bool,
    pub losses: u64,
}

impl Reports {
    /// One report (its payload, the report ID taken off): the whole frames it completes, as one byte run.
    pub fn take(&mut self, report: &[u8]) -> Vec<u8> {
        if report.len() < HEAD {
            return vec![];
        }
        let count = u16::from_le_bytes([report[0], report[1]]) as usize;
        let first = u16::from_le_bytes([report[2], report[3]]);
        if count > report.len() - HEAD || (first != NO_START && first as usize >= count) {
            self.track = FrameTrack::default();
            self.synced = false;
            self.losses += 1;
            return vec![];
        }
        if count == 0 {
            return vec![];
        }
        let mut data = &report[HEAD..HEAD + count];
        if self.synced && self.track.next_start(data) != first {
            self.track = FrameTrack::default();
            self.synced = false;
            self.losses += 1;
        }
        if !self.synced {
            if first == NO_START {
                return vec![];
            }
            data = &data[first as usize..];
            self.synced = true;
        }
        self.track.feed(data).concat()
    }
}

/// Length-prefixed frames as output reports of `output_size` (the report ID not counted): count, first, the bytes,
/// zeros to the end.
#[derive(Debug, Default)]
pub struct Writer {
    track: FrameTrack,
}

impl Writer {
    pub fn reports(&mut self, data: &[u8], output_size: usize) -> Vec<Vec<u8>> {
        let room = output_size - HEAD;
        let mut out = vec![];
        for part in data.chunks(room) {
            let first = self.track.next_start(part);
            self.track.feed(part);
            let mut r = (part.len() as u16).to_le_bytes().to_vec();
            r.extend(first.to_le_bytes());
            r.extend_from_slice(part);
            r.resize(output_size, 0);
            out.push(r);
        }
        out
    }
}

/// A report of a HID report descriptor: usage page, the top collection's usage, report ID (0: none), input and
/// output bytes (the ID not counted).
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub usage_page: u32,
    pub usage: u32,
    pub report_id: u8,
    pub input: usize,
    pub output: usize,
}

/// The reports of a HID report descriptor (short items only; enough for a vendor interface).
pub fn parse_report_descriptor(desc: &[u8]) -> Vec<Report> {
    let mut reports: Vec<Report> = vec![];
    let (mut usage_page, mut report_id, mut size, mut count) = (0u32, 0u32, 0u32, 0u32);
    let (mut usage, mut top, mut depth) = (0u32, 0u32, 0u32);
    let mut stack = vec![];
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            i += 3 + desc.get(i + 1).copied().unwrap_or(0) as usize;
            continue;
        }
        let n = [0usize, 1, 2, 4][(prefix & 3) as usize];
        let mut value = 0u32;
        for (k, b) in desc.iter().skip(i + 1).take(n).enumerate() {
            value |= (*b as u32) << (8 * k);
        }
        let (tag, kind) = (prefix & 0xF0, prefix & 0x0C);
        i += 1 + n;
        match (kind, tag) {
            (0x04, 0x00) => usage_page = value,
            (0x04, 0x70) => size = value,
            (0x04, 0x80) => report_id = value,
            (0x04, 0x90) => count = value,
            (0x04, 0xA0) => stack.push((usage_page, report_id, size, count)),
            (0x04, 0xB0) => {
                if let Some(s) = stack.pop() {
                    (usage_page, report_id, size, count) = s;
                }
            }
            (0x08, 0x00) => usage = value,
            (0x00, 0xA0) => {
                if depth == 0 {
                    top = usage;
                }
                depth += 1;
            }
            (0x00, 0xC0) => depth = depth.saturating_sub(1),
            (0x00, 0x80) | (0x00, 0x90) => {
                let bytes = ((size * count) as usize).div_ceil(8);
                let at = match reports
                    .iter()
                    .position(|r| r.usage_page == usage_page && r.report_id as u32 == report_id)
                {
                    Some(k) => k,
                    None => {
                        reports.push(Report {
                            usage_page,
                            usage: top,
                            report_id: report_id as u8,
                            input: 0,
                            output: 0,
                        });
                        reports.len() - 1
                    }
                };
                if tag == 0x80 {
                    reports[at].input += bytes;
                } else {
                    reports[at].output += bytes;
                }
            }
            _ => {}
        }
    }
    reports
}

/// OEP's report carrying both directions, if the descriptor has one.
pub fn vendor_report(desc: &[u8]) -> Option<Report> {
    parse_report_descriptor(desc)
        .into_iter()
        .find(|r| (r.usage_page, r.usage) == (USAGE_PAGE, USAGE) && r.input >= 3 && r.output >= 3)
}

/// How the HID reports go out: interrupt OUT, else SET_REPORT (output) on the control pipe.
struct HidSink {
    iface: nusb::Interface,
    out: Option<Mutex<nusb::Endpoint<Interrupt, Out>>>,
    number: u8,
    report_id: u8,
    output: usize,
    writer: Writer,
}

impl Sink for HidSink {
    fn send(&mut self, data: &[u8]) -> Result<()> {
        for r in self.writer.reports(data, self.output) {
            let mut wire = vec![];
            if self.report_id != 0 {
                wire.push(self.report_id);
            }
            wire.extend(r);
            match &self.out {
                Some(ep) => {
                    let mut ep = ep.lock().map_err(|_| Error::Io("the HID OUT endpoint".into()))?;
                    let c = ep.transfer_blocking(Buffer::from(wire), Duration::from_secs(1));
                    c.status.map_err(|e| Error::Io(format!("HID write: {e}")))?;
                }
                None => {
                    self.iface
                        .control_out(
                            ControlOut {
                                control_type: ControlType::Class,
                                recipient: Recipient::Interface,
                                request: 0x09,
                                value: 0x0200 | self.report_id as u16,
                                index: self.number as u16,
                                data: &wire,
                            },
                            Duration::from_secs(1),
                        )
                        .wait()
                        .map_err(|e| Error::Io(format!("HID write (SET_REPORT): {e}")))?;
                }
            }
        }
        Ok(())
    }
}

/// The report descriptor of interface `number` (GET_DESCRIPTOR 0x22 on the interface).
fn report_descriptor(iface: &nusb::Interface, number: u8) -> Result<Vec<u8>> {
    iface
        .control_in(
            ControlIn {
                control_type: ControlType::Standard,
                recipient: Recipient::Interface,
                request: 0x06,
                value: 0x2200,
                index: number as u16,
                length: 4096,
            },
            Duration::from_secs(1),
        )
        .wait()
        .map_err(|e| Error::Io(format!("HID report descriptor: {e}")))
}

/// The probe's OEP HID interface opened (taken from the kernel's driver and claimed), its reports read on a thread
/// of their own as whole frames.
pub fn open_hid(info: &nusb::DeviceInfo) -> Result<Transport> {
    let numbers: Vec<u8> = info
        .interfaces()
        .filter(|i| i.class() == HID_CLASS)
        .map(|i| i.interface_number())
        .collect();
    if numbers.is_empty() {
        return Err(Error::NotFound("no HID interface on the device".into()));
    }
    let dev = info.open().wait().map_err(|e| Error::Io(format!("USB open: {e}")))?;
    let mut why = vec![];
    for number in numbers {
        let iface = match dev.detach_and_claim_interface(number).wait() {
            Ok(i) => i,
            Err(e) => {
                why.push(format!("interface {number}: {e}"));
                continue;
            }
        };
        let desc = match report_descriptor(&iface, number) {
            Ok(d) => d,
            Err(e) => {
                why.push(format!("interface {number}: {e}"));
                continue;
            }
        };
        let Some(rep) = vendor_report(&desc) else {
            why.push(format!(
                "interface {number}: not OEP's report (usage page 0xFF4F, usage 0x45)"
            ));
            continue;
        };
        let Some(d) = iface.descriptor() else {
            continue;
        };
        let (mut ain, mut aout) = (None, None);
        for e in d.endpoints() {
            if e.transfer_type() != nusb::descriptors::TransferType::Interrupt {
                continue;
            }
            if e.address() & 0x80 != 0 {
                ain.get_or_insert(e.address());
            } else {
                aout.get_or_insert(e.address());
            }
        }
        let Some(ain) = ain else {
            why.push(format!("interface {number}: no interrupt IN endpoint"));
            continue;
        };
        let mut rd = iface
            .endpoint::<Interrupt, In>(ain)
            .map_err(|e| Error::Io(format!("HID IN endpoint: {e}")))?;
        let out = match aout {
            Some(a) => Some(Mutex::new(
                iface
                    .endpoint::<Interrupt, Out>(a)
                    .map_err(|e| Error::Io(format!("HID OUT endpoint: {e}")))?,
            )),
            None => None,
        };
        let in_len = rep.input + usize::from(rep.report_id != 0);
        let packet = rd.max_packet_size().max(1);
        let req = in_len.div_ceil(packet) * packet;
        for _ in 0..DEPTH {
            let b = rd.allocate(req);
            rd.submit(b);
        }
        let (tx, rx) = channel::<Vec<u8>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let flow = Arc::new(Flow::default());
        let losses = Arc::new(AtomicU64::new(0));
        let (st, stats2, flow2, losses2, id) =
            (stop.clone(), stats.clone(), flow.clone(), losses.clone(), rep.report_id);
        let thread = std::thread::Builder::new()
            .name("oep-hid-in".into())
            .spawn(move || hid_in(rd, tx, st, stats2, flow2, losses2, id, req))?;
        let sink = HidSink {
            iface,
            out,
            number,
            report_id: rep.report_id,
            output: rep.output,
            writer: Writer::default(),
        };
        let mut t = Transport::with_parts(rx, flow, Box::new(sink), stop, Some(thread), "rust-usb-hid", stats);
        t.whole_frames = true;
        t.report_losses = Some(losses);
        return Ok(t);
    }
    Err(Error::NotFound(format!("no OEP HID interface ({})", why.join("; "))))
}

#[allow(clippy::too_many_arguments)]
fn hid_in(
    mut ep: nusb::Endpoint<Interrupt, In>,
    tx: Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    flow: Arc<Flow>,
    losses: Arc<AtomicU64>,
    report_id: u8,
    req: usize,
) {
    let mut reports = Reports::default();
    while !stop.load(Ordering::SeqCst) && ep.pending() > 0 {
        let Some(c) = ep.wait_next_complete(Duration::from_millis(100)) else {
            continue;
        };
        if let Ok(()) = c.status {
            let n = c.actual_len.min(c.buffer.len());
            let mut r = &c.buffer[..n];
            let ours = if report_id != 0 {
                let ok = r.first() == Some(&report_id);
                r = r.get(1..).unwrap_or(&[]);
                ok
            } else {
                true
            };
            if ours && !r.is_empty() {
                stats.bytes.fetch_add(n as u64, Ordering::Relaxed);
                stats.transfers.fetch_add(1, Ordering::Relaxed);
                let frames = reports.take(r);
                losses.store(reports.losses, Ordering::Relaxed);
                if !frames.is_empty() {
                    let k = frames.len() as u64;
                    if !crate::flow::deliver(&tx, frames, k, &flow, &stats.held, &stop) {
                        return;
                    }
                }
            }
        } else if matches!(c.status, Err(nusb::transfer::TransferError::Cancelled)) {
            continue;
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut b = c.buffer;
        b.clear();
        b.set_requested_len(req);
        ep.submit(b);
    }
    ep.cancel_all();
    while ep.pending() > 0 {
        if ep.wait_next_complete(Duration::from_secs(1)).is_none() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lf(m: &[u8]) -> Vec<u8> {
        let mut v = (m.len() as u16).to_le_bytes().to_vec();
        v.extend_from_slice(m);
        v
    }

    #[test]
    fn frames_go_out_in_reports_and_come_back_whole() {
        let frames: Vec<Vec<u8>> = (1..40u8).map(|k| lf(&vec![k; k as usize * 3])).collect();
        let stream: Vec<u8> = frames.concat();
        let reps = Writer::default().reports(&stream, 64);
        let mut r = Reports::default();
        let back: Vec<u8> = reps.iter().flat_map(|x| r.take(x)).collect();
        assert_eq!(back, stream);
        assert_eq!(r.losses, 0);
        // a report lost: the frame begun is dropped, reading goes on from the next report's first
        let mut r = Reports::default();
        let mut back = vec![];
        for (k, x) in reps.iter().enumerate() {
            if k != 5 {
                back.extend(r.take(x));
            }
        }
        assert_eq!(r.losses, 1);
        assert!(back.len() < stream.len());
        let mut t = FrameTrack::default();
        let whole = t.feed(&back);
        assert!(t.partial.is_empty() && whole.iter().all(|f| frames.contains(f)));
    }

    #[test]
    fn the_vendor_report_is_found() {
        // usage page 0xFF4F, usage 0x45, collection, report size 8, count 63 in, 63 out
        let d = [
            0x06, 0x4F, 0xFF, 0x09, 0x45, 0xA1, 0x01, 0x75, 0x08, 0x95, 0x3F, 0x09, 0x01, 0x81, 0x02, 0x09, 0x01, 0x91,
            0x02, 0xC0,
        ];
        let r = vendor_report(&d).unwrap();
        assert_eq!((r.report_id, r.input, r.output), (0, 63, 63));
        assert!(vendor_report(&d[3..]).is_none());
    }
}
