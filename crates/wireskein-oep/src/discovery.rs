//! Finding a probe that serves OEP over TCP on the local network (oep-client-python's `discovery`, its minimal
//! engine): DNS-SD service `_oep._tcp` over mDNS (transports §3). An OEP address without a route whose probe is not
//! on USB is looked for here (host guide §5.3), as is `?via=net`.
//!
//! The query: PTR `_oep._tcp.local` (then SRV / TXT / A for what the answers left out), sent to 224.0.0.251:5353 from
//! an ephemeral port with the unicast-response bit, out of every IPv4 interface; a second socket on 5353 also listens
//! for multicast answers when the port can be shared. The instance's TXT carries `unit_id=`; one is used only when a
//! TCP connection to it answers confirm and describe's unit_id is that one (host guide §4.1: `_oep` is not a
//! registered name). IPv4 only; mDNS stays on the local link.

use crate::msg::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

pub const SERVICE: &str = "_oep._tcp.local.";
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PORT: u16 = 5353;
const T_A: u16 = 1;
const T_PTR: u16 = 12;
const T_TXT: u16 = 16;
const T_SRV: u16 = 33;
const QU: u16 = 0x8000;
/// how long a browse listens (discovery.find_unit's)
pub const BROWSE: Duration = Duration::from_secs(3);

/// One announced probe: its instance, TXT unit_id, SRV host and port, and that host's IPv4 addresses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub instance: String,
    pub unit_id: Option<String>,
    pub host: String,
    pub port: u16,
    pub addresses: Vec<String>,
}

impl Found {
    /// (address, port) to connect to: the first address, else the host name.
    pub fn target(&self) -> Option<(String, u16)> {
        let at = self
            .addresses
            .first()
            .cloned()
            .unwrap_or_else(|| self.host.trim_end_matches('.').to_string());
        (self.port != 0 && !at.is_empty()).then_some((at, self.port))
    }
}

fn encode_name(name: &str) -> Vec<u8> {
    let mut out = vec![];
    for label in name.trim_end_matches('.').split('.') {
        out.push(label.len().min(63) as u8);
        out.extend_from_slice(&label.as_bytes()[..label.len().min(63)]);
    }
    out.push(0);
    out
}

/// A query of these (name, type) questions, class IN with the unicast-response bit.
pub fn query(questions: &[(String, u16)], id: u16) -> Vec<u8> {
    let mut out = vec![];
    for x in [id, 0, questions.len() as u16, 0, 0, 0] {
        out.extend(x.to_be_bytes());
    }
    for (n, t) in questions {
        out.extend(encode_name(n));
        out.extend(t.to_be_bytes());
        out.extend((1 | QU).to_be_bytes());
    }
    out
}

fn read_name(data: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels = vec![];
    let (mut end, mut jumps) = (None, 0);
    loop {
        let n = *data.get(at)? as usize;
        if n & 0xC0 == 0xC0 {
            if jumps > 32 {
                return None;
            }
            let lo = *data.get(at + 1)? as usize;
            end.get_or_insert(at + 2);
            at = (n & 0x3F) << 8 | lo;
            jumps += 1;
            continue;
        }
        if n & 0xC0 != 0 {
            return None;
        }
        if n == 0 {
            return Some((labels.join(".") + ".", end.unwrap_or(at + 1)));
        }
        labels.push(String::from_utf8_lossy(data.get(at + 1..at + 1 + n)?).to_string());
        at += 1 + n;
    }
}

/// A record's data: PTR name, SRV (target, port), TXT {key: value}, A address.
#[derive(Debug, Clone, PartialEq)]
pub enum Rdata {
    Ptr(String),
    Srv(String, u16),
    Txt(BTreeMap<String, String>),
    A(String),
}

/// Every record of a response (answers, authority, additional) of the four types; a broken record ends the packet.
pub fn records(p: &[u8]) -> Vec<(String, Rdata)> {
    let mut out = vec![];
    if p.len() < 12 {
        return out;
    }
    let w = |at: usize| u16::from_be_bytes([p[at], p[at + 1]]);
    if w(2) & 0x8000 == 0 {
        return out;
    }
    let (qd, n) = (w(4), w(6) as usize + w(8) as usize + w(10) as usize);
    let mut at = 12;
    for _ in 0..qd {
        let Some((_, a)) = read_name(p, at) else { return out };
        at = a + 4;
    }
    for _ in 0..n {
        let Some((name, a)) = read_name(p, at) else { return out };
        at = a;
        if at + 10 > p.len() {
            return out;
        }
        let (rtype, rdlen) = (w(at), w(at + 8) as usize);
        at += 10;
        let (r0, r1) = (at, at + rdlen);
        at = r1;
        if r1 > p.len() {
            return out;
        }
        let d = match rtype {
            T_PTR => match read_name(p, r0) {
                Some((n, _)) => Rdata::Ptr(n),
                None => return out,
            },
            T_SRV if rdlen >= 7 => match read_name(p, r0 + 6) {
                Some((n, _)) => Rdata::Srv(n, w(r0 + 4)),
                None => return out,
            },
            T_TXT => {
                let mut txt = BTreeMap::new();
                let mut i = r0;
                while i < r1 {
                    let k = p[i] as usize;
                    let entry = String::from_utf8_lossy(&p[(i + 1).min(r1)..(i + 1 + k).min(r1)]).to_string();
                    i += 1 + k;
                    if !entry.is_empty() {
                        let (k, v) = entry.split_once('=').unwrap_or((&entry, ""));
                        txt.entry(k.to_ascii_lowercase()).or_insert_with(|| v.to_string());
                    }
                }
                Rdata::Txt(txt)
            }
            T_A if rdlen == 4 => Rdata::A(format!("{}.{}.{}.{}", p[r0], p[r0 + 1], p[r0 + 2], p[r0 + 3])),
            _ => continue,
        };
        out.push((name, d));
    }
    out
}

/// What the answers have said so far, and the questions still open.
#[derive(Debug, Default)]
pub struct Browser {
    pub instances: BTreeSet<String>,
    srv: BTreeMap<String, (String, u16)>,
    txt: BTreeMap<String, BTreeMap<String, String>>,
    a: BTreeMap<String, Vec<String>>,
}

impl Browser {
    pub fn feed(&mut self, packet: &[u8]) {
        for (name, d) in records(packet) {
            let key = name.to_ascii_lowercase();
            match d {
                Rdata::Ptr(n) if key == SERVICE => {
                    self.instances.insert(n);
                }
                Rdata::Srv(h, p) => {
                    self.srv.insert(key, (h, p));
                }
                Rdata::Txt(t) => {
                    self.txt.insert(key, t);
                }
                Rdata::A(ip) => {
                    let v = self.a.entry(key).or_default();
                    if !v.contains(&ip) {
                        v.push(ip);
                    }
                }
                _ => {}
            }
        }
    }

    pub fn questions(&self) -> Vec<(String, u16)> {
        let mut out = vec![(SERVICE.to_string(), T_PTR)];
        for inst in &self.instances {
            let k = inst.to_ascii_lowercase();
            if !self.srv.contains_key(&k) {
                out.push((inst.clone(), T_SRV));
            }
            if !self.txt.contains_key(&k) {
                out.push((inst.clone(), T_TXT));
            }
            if let Some((h, _)) = self.srv.get(&k)
                && !h.is_empty()
                && !self.a.contains_key(&h.to_ascii_lowercase())
            {
                out.push((h.clone(), T_A));
            }
        }
        out
    }

    pub fn found(&self) -> Vec<Found> {
        self.instances
            .iter()
            .map(|inst| {
                let k = inst.to_ascii_lowercase();
                let (host, port) = self.srv.get(&k).cloned().unwrap_or_default();
                let unit = self
                    .txt
                    .get(&k)
                    .and_then(|t| t.get("unit_id"))
                    .filter(|u| !u.is_empty())
                    .cloned();
                let label = inst
                    .strip_suffix(&format!(".{SERVICE}"))
                    .unwrap_or(inst)
                    .replace("\\032", " ");
                Found {
                    instance: label,
                    unit_id: unit,
                    addresses: self.a.get(&host.to_ascii_lowercase()).cloned().unwrap_or_default(),
                    host,
                    port,
                }
            })
            .collect()
    }
}

/// This machine's IPv4 interface addresses (Linux: the kernel's local host routes), 127.0.0.1 last.
pub fn interface_addresses() -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = vec![];
    if let Ok(t) = std::fs::read_to_string("/proc/net/fib_trie") {
        let mut last: Option<Ipv4Addr> = None;
        for line in t.lines() {
            let l = line.trim();
            if let Some(ip) = l.strip_prefix("|-- ") {
                last = ip.parse().ok();
            } else if l.contains("/32 host LOCAL")
                && let Some(ip) = last
                && !out.contains(&ip)
                && !ip.is_loopback()
                && !ip.is_link_local()
            {
                out.push(ip);
            }
        }
    }
    out.push(Ipv4Addr::LOCALHOST);
    out
}

#[cfg(unix)]
fn group_socket(ifs: &[Ipv4Addr]) -> Option<UdpSocket> {
    use rustix::net::{AddressFamily, SocketType, bind, socket, sockopt};
    let s = socket(AddressFamily::INET, SocketType::DGRAM, None).ok()?;
    sockopt::set_socket_reuseaddr(&s, true).ok()?;
    let _ = sockopt::set_socket_reuseport(&s, true);
    bind(&s, &SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT)).ok()?;
    let joined = ifs
        .iter()
        .filter(|ip| sockopt::set_ip_add_membership(&s, &GROUP, ip).is_ok())
        .count();
    (joined > 0).then(|| UdpSocket::from(s))
}

#[cfg(not(unix))]
fn group_socket(_ifs: &[Ipv4Addr]) -> Option<UdpSocket> {
    None
}

#[cfg(unix)]
fn send_query(q: &UdpSocket, packet: &[u8], ifs: &[Ipv4Addr]) -> usize {
    use rustix::net::sockopt;
    let mut sent = 0;
    for ip in ifs {
        if sockopt::set_ip_multicast_if(q, ip).is_ok() && q.send_to(packet, SocketAddrV4::new(GROUP, PORT)).is_ok() {
            sent += 1;
        }
    }
    if sent == 0 && q.send_to(packet, SocketAddrV4::new(GROUP, PORT)).is_ok() {
        sent = 1;
    }
    sent
}

#[cfg(not(unix))]
fn send_query(q: &UdpSocket, packet: &[u8], _ifs: &[Ipv4Addr]) -> usize {
    q.send_to(packet, SocketAddrV4::new(GROUP, PORT))
        .map(|_| 1)
        .unwrap_or(0)
}

/// Every probe announcing `_oep._tcp` within `timeout`.
pub fn browse(timeout: Duration) -> Result<Vec<Found>> {
    let ifs = interface_addresses();
    let q =
        UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| Error::Io(format!("mDNS: {e}")))?;
    let _ = q.set_multicast_ttl_v4(255);
    let g = group_socket(&ifs);
    let mut socks = vec![q];
    socks.extend(g);
    for s in &socks {
        s.set_nonblocking(true).map_err(|e| Error::Io(format!("mDNS: {e}")))?;
    }
    let mut b = Browser::default();
    let deadline = Instant::now() + timeout;
    let (mut next, mut gap) = (Instant::now(), Duration::from_millis(250));
    let mut buf = vec![0u8; 9000];
    let mut id = (crate::host::monotonic_ns() as u64 & 0xFFFF) as u16;
    while Instant::now() < deadline {
        if Instant::now() >= next {
            id = id.wrapping_add(1);
            send_query(&socks[0], &query(&b.questions(), id), &ifs);
            next = Instant::now() + gap;
            gap = (gap * 2).min(Duration::from_secs(1));
        }
        let mut got = false;
        for s in &socks {
            while let Ok((n, _)) = s.recv_from(&mut buf) {
                b.feed(&buf[..n]);
                got = true;
            }
        }
        if !got {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(b.found())
}

/// Whether an instance is an OEP probe (host guide §4.1): a TCP connection, confirm, and describe's unit_id is its
/// TXT unit_id; then closed (no session). None when it is, else what it said instead.
pub fn check(f: &Found) -> Option<String> {
    let Some(unit) = f.unit_id.as_deref() else {
        return Some("no unit_id in its TXT record".into());
    };
    let Some((at, port)) = f.target() else {
        return Some("no SRV port or address".into());
    };
    let addr = crate::acquire::Address::Tcp {
        host: at,
        port,
        unit: Some(unit.to_string()),
    };
    match crate::acquire::open(&addr) {
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    }
}

/// The probe whose TXT unit_id is `unit` (ASCII case aside), verified (`check`): (address, port). NotFound when none
/// answers within the browse or none is verified.
pub fn find_unit(unit: &str) -> Result<(String, u16)> {
    let hits: Vec<Found> = browse(BROWSE)?
        .into_iter()
        .filter(|f| f.unit_id.as_deref().is_some_and(|u| u.eq_ignore_ascii_case(unit)) && f.target().is_some())
        .collect();
    if hits.is_empty() {
        return Err(Error::NotFound(format!(
            "no probe with unit_id {unit} announces _oep._tcp.local on this network (DNS-SD over mDNS stays on the \
             local link, and a probe need not advertise, transports §3: name it as tcp://HOST:PORT)"
        )));
    }
    let mut why = vec![];
    for f in &hits {
        match check(f) {
            None => return Ok(f.target().expect("a target")),
            Some(w) => {
                let (a, p) = f.target().expect("a target");
                why.push(format!("tcp://{a}:{p} ({}): {w}", f.instance));
            }
        }
    }
    Err(Error::NotFound(format!(
        "no announced instance with unit_id {unit} is verified as that OEP probe (host guide §4.1): {}",
        why.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rr(name: &str, t: u16, data: &[u8]) -> Vec<u8> {
        let mut v = encode_name(name);
        v.extend(t.to_be_bytes());
        v.extend(1u16.to_be_bytes());
        v.extend(120u32.to_be_bytes());
        v.extend((data.len() as u16).to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn an_answer_is_read_into_a_probe() {
        let inst = "OEP fafe00000003._oep._tcp.local.";
        let mut srv = vec![0, 0, 0, 0];
        srv.extend(7450u16.to_be_bytes());
        srv.extend(encode_name("oep-fafe.local."));
        let txt = [b"\x14unit_id=fafe00000003".as_slice(), b"\x03a=b"].concat();
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 4, 0, 0, 0, 0];
        p.extend(rr(SERVICE, T_PTR, &encode_name(inst)));
        p.extend(rr(inst, T_SRV, &srv));
        p.extend(rr(inst, T_TXT, &txt));
        p.extend(rr("oep-fafe.local.", T_A, &[127, 0, 0, 1]));
        let mut b = Browser::default();
        b.feed(&p);
        let f = b.found();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].instance, "OEP fafe00000003");
        assert_eq!(f[0].unit_id.as_deref(), Some("fafe00000003"));
        assert_eq!(f[0].target(), Some(("127.0.0.1".into(), 7450)));
        assert_eq!(b.questions(), vec![(SERVICE.to_string(), T_PTR)]);
        let q = query(&b.questions(), 7);
        assert_eq!(&q[..6], &[0, 7, 0, 0, 0, 1]);
    }
}
