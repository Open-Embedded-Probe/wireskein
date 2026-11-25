//! A port's line labels (host guide §5.3; oep-client-python's `settings.parse_label`, `labels`, `line_from_labels`,
//! `pin_from_labels`): a channel kept in fn 0's setting `channel.{}.label` as "S.N" (port S's line N by its role),
//! "S.N@P" (and the target pin P it reaches) or "S@P" (port S's line on target pin P). A capture's line may be named
//! by its label instead of the probe's channel name: x035.swdio, x035@pb9.

use crate::host::{self, Host};
use crate::msg::{self, Error, Reader, Result};

/// fn 0's settings ops (core §13): get
pub const OP_GET: u8 = 0x20;
/// describe's setting declaration (form, key kind, keys u16, max_len, flags, name)
pub const TAG_SETTING: u8 = 0x08;
pub const LABEL: &str = "channel.{}.label";

fn port_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=16).contains(&b.len())
        && b[0].is_ascii_alphabetic()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
}

fn line_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// (S, N or None, P or None) of a label of a port's line - "S.N", "S.N@P" or "S@P" - as written; None for any other.
pub fn parse_label(text: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
    let (head, pin) = match text.split_once('@') {
        Some((h, p)) => (h, Some(p)),
        None => (text, None),
    };
    if pin.is_some_and(|p| !line_ok(p)) {
        return None;
    }
    let (port, line) = match head.split_once('.') {
        Some((s, n)) => (s, Some(n)),
        None => (head, None),
    };
    if !port_ok(port) || line.is_some_and(|n| !line_ok(n)) || (line.is_none() && pin.is_none()) {
        return None;
    }
    Some((port, line, pin))
}

fn fold(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// The channel of port `port`'s line `name` among the labels, or None (ASCII case aside).
pub fn line_from_labels(labels: &[(u16, String)], name: &str, port: &str) -> Option<u16> {
    let mut found: Vec<u16> = labels
        .iter()
        .filter(|(_, text)| {
            parse_label(text).is_some_and(|(s, n, _)| n.is_some_and(|n| fold(n) == fold(name)) && fold(s) == fold(port))
        })
        .map(|(ch, _)| *ch)
        .collect();
    found.sort_unstable();
    found.dedup();
    (found.len() == 1).then(|| found[0])
}

/// The channel of port `port`'s line that reaches target pin `pin`, or None (ASCII case aside).
pub fn pin_from_labels(labels: &[(u16, String)], pin: &str, port: &str) -> Option<u16> {
    let mut found: Vec<u16> = labels
        .iter()
        .filter(|(_, text)| {
            parse_label(text).is_some_and(|(s, _, p)| p.is_some_and(|p| fold(p) == fold(pin)) && fold(s) == fold(port))
        })
        .map(|(ch, _)| *ch)
        .collect();
    found.sort_unstable();
    found.dedup();
    (found.len() == 1).then(|| found[0])
}

/// Every channel label now (fn 0's channel.{}.label) as (channel, text); a probe without the settings' get: none.
pub fn labels(hst: &mut Host) -> Result<Vec<(u16, String)>> {
    if !hst.offers(host::CORE_FN, OP_GET)? {
        return Ok(vec![]);
    }
    // the item's number: its place among fn 0's setting declarations (core §13.2)
    let number = hst
        .describe(host::CORE_FN)?
        .into_iter()
        .filter(|(t, v)| *t == TAG_SETTING && v.len() >= 6)
        .position(|(_, v)| String::from_utf8_lossy(&v[6..]) == LABEL)
        .map(|k| k + 1);
    let Some(number) = number else {
        return Ok(vec![]);
    };
    let mut out = vec![];
    for (fn_, n, key, data) in get_all(hst)? {
        if fn_ == host::CORE_FN && n as usize == number {
            out.push((key, msg::shown(&data)));
        }
    }
    Ok(out)
}

/// One value as get gives it: (fn, item number, key, data).
type Value = (u16, u8, u16, Vec<u8>);

/// get (core §13.4), paged: every value as (fn, item number, key, data); read again from the start when the
/// generation moved between pages.
fn get_all(hst: &mut Host) -> Result<Vec<Value>> {
    loop {
        let mut out: Vec<Value> = vec![];
        let mut first_gen = None;
        loop {
            let r = hst.call(host::CORE_FN, OP_GET, &(out.len() as u16).to_le_bytes(), false)?;
            let mut rd = Reader::new(&r.payload);
            let (more, gen_, count) = (rd.u8()?, rd.u32()?, rd.u8()?);
            let mut page = vec![];
            for _ in 0..count {
                let (fn_, number, key, n) = (rd.u16()?, rd.u8()?, rd.u16()?, rd.u16()?);
                let data = rd
                    .bytes(n as usize)
                    .map_err(|_| Error::Protocol("get: a value's data past the answer".into()))?;
                page.push((fn_, number, key, data.to_vec()));
            }
            rd.tail()?;
            match first_gen {
                None => first_gen = Some(gen_),
                Some(g) if g != gen_ => break,
                _ => {}
            }
            let empty = page.is_empty();
            out.extend(page);
            if more == 0 || empty {
                return Ok(out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_parse_as_python_parses_them() {
        assert_eq!(parse_label("x035.swdio"), Some(("x035", Some("swdio"), None)));
        assert_eq!(
            parse_label("x035.swdio@pa13"),
            Some(("x035", Some("swdio"), Some("pa13")))
        );
        assert_eq!(
            parse_label("v407weu6-yd-p4@pb9"),
            Some(("v407weu6-yd-p4", None, Some("pb9")))
        );
        for no in [
            "GPIO30",
            "rgb_led",
            "1x.a",
            "x035.",
            "x035@",
            "x.y.z",
            "abcdefghijklmnopq.a",
            "x035.sw dio",
        ] {
            assert_eq!(parse_label(no), None, "{no}");
        }
        let ls = vec![
            (2u16, "P4.rx@pa9".to_string()),
            (15, "p4.SWIO@pa13".to_string()),
            (50, "p4@pb9".to_string()),
            (51, "rgb_led".to_string()),
            (60, "q.rx".to_string()),
        ];
        assert_eq!(line_from_labels(&ls, "rx", "p4"), Some(2));
        assert_eq!(line_from_labels(&ls, "swio", "P4"), Some(15));
        assert_eq!(pin_from_labels(&ls, "PB9", "p4"), Some(50));
        assert_eq!(pin_from_labels(&ls, "pa9", "p4"), Some(2));
        assert_eq!(line_from_labels(&ls, "tx", "p4"), None);
    }
}
