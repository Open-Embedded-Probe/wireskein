//! A JSON value as Python holds one: dicts keep their keys' order, ints and floats apart, written as Python's
//! `json.dumps` writes them (floats by their repr) and shown as Python's `str()` shows them (in a reason).

use crate::pyfmt::repr_f64;
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum J {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<J>),
    Dict(Vec<(String, J)>),
}

pub static NULL: J = J::Null;

impl J {
    pub fn dict() -> J {
        J::Dict(vec![])
    }

    /// The value at a key of a dict (Null when there is none: Python's `x.get(k)`).
    pub fn get(&self, k: &str) -> &J {
        match self {
            J::Dict(v) => v.iter().find(|(a, _)| a == k).map(|(_, b)| b).unwrap_or(&NULL),
            _ => &NULL,
        }
    }

    pub fn has(&self, k: &str) -> bool {
        matches!(self, J::Dict(v) if v.iter().any(|(a, _)| a == k))
    }

    pub fn get_mut(&mut self, k: &str) -> Option<&mut J> {
        match self {
            J::Dict(v) => v.iter_mut().find(|(a, _)| a == k).map(|(_, b)| b),
            _ => None,
        }
    }

    /// Sets a key of a dict (in its place when it is there, else at the end).
    pub fn set(&mut self, k: &str, val: impl Into<J>) {
        let val = val.into();
        if let J::Dict(v) = self {
            if let Some(e) = v.iter_mut().find(|(a, _)| a == k) {
                e.1 = val;
            } else {
                v.push((k.to_string(), val));
            }
        }
    }

    pub fn remove(&mut self, k: &str) -> Option<J> {
        if let J::Dict(v) = self
            && let Some(i) = v.iter().position(|(a, _)| a == k)
        {
            return Some(v.remove(i).1);
        }
        None
    }

    pub fn is_null(&self) -> bool {
        matches!(self, J::Null)
    }

    /// Python's truth of the value.
    pub fn truthy(&self) -> bool {
        match self {
            J::Null => false,
            J::Bool(b) => *b,
            J::Int(i) => *i != 0,
            J::Float(f) => *f != 0.0,
            J::Str(s) => !s.is_empty(),
            J::List(v) => !v.is_empty(),
            J::Dict(v) => !v.is_empty(),
        }
    }

    pub fn f(&self) -> Option<f64> {
        match self {
            J::Int(i) => Some(*i as f64),
            J::Float(f) => Some(*f),
            J::Bool(b) => Some(*b as i64 as f64),
            _ => None,
        }
    }

    pub fn i(&self) -> Option<i64> {
        match self {
            J::Int(i) => Some(*i),
            J::Float(f) => Some(*f as i64),
            J::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }

    pub fn s(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn list(&self) -> &[J] {
        match self {
            J::List(v) => v,
            _ => &[],
        }
    }

    pub fn items(&self) -> &[(String, J)] {
        match self {
            J::Dict(v) => v,
            _ => &[],
        }
    }

    /// `x.get(k, default)` as a float (a value of None given is None: the default only when the key is missing).
    pub fn f_or(&self, k: &str, d: f64) -> f64 {
        if self.has(k) {
            self.get(k).f().unwrap_or(f64::NAN)
        } else {
            d
        }
    }

    /// Python's `x.get(k) or d` for a number.
    pub fn f_truthy_or(&self, k: &str, d: f64) -> f64 {
        let v = self.get(k);
        if v.truthy() { v.f().unwrap_or(d) } else { d }
    }

    /// The value as JSON text, as Python's `json.dumps(.., separators=(",", ":"), ensure_ascii=False)`.
    pub fn dumps(&self) -> String {
        let mut s = String::new();
        self.write_json(&mut s);
        s
    }

    pub fn write_json(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(i) => write!(out, "{i}").unwrap(),
            J::Float(f) => {
                if f.is_nan() {
                    out.push_str("NaN")
                } else if f.is_infinite() {
                    out.push_str(if *f > 0.0 { "Infinity" } else { "-Infinity" })
                } else {
                    out.push_str(&repr_f64(*f))
                }
            }
            J::Str(s) => json_str(s, out),
            J::List(v) => {
                out.push('[');
                for (k, x) in v.iter().enumerate() {
                    if k > 0 {
                        out.push(',');
                    }
                    x.write_json(out);
                }
                out.push(']');
            }
            J::Dict(v) => {
                out.push('{');
                for (k, (a, x)) in v.iter().enumerate() {
                    if k > 0 {
                        out.push(',');
                    }
                    json_str(a, out);
                    out.push(':');
                    x.write_json(out);
                }
                out.push('}');
            }
        }
    }

    /// As Python's `str()` / `repr()` of the value shows it.
    pub fn repr(&self) -> String {
        let mut s = String::new();
        self.write_repr(&mut s);
        s
    }

    fn write_repr(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("None"),
            J::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
            J::Int(i) => write!(out, "{i}").unwrap(),
            J::Float(f) => out.push_str(&crate::pyfmt::py_float(*f)),
            J::Str(s) => out.push_str(&py_str_repr(s)),
            J::List(v) => {
                out.push('[');
                for (k, x) in v.iter().enumerate() {
                    if k > 0 {
                        out.push_str(", ");
                    }
                    x.write_repr(out);
                }
                out.push(']');
            }
            J::Dict(v) => {
                out.push('{');
                for (k, (a, x)) in v.iter().enumerate() {
                    if k > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&py_str_repr(a));
                    out.push_str(": ");
                    x.write_repr(out);
                }
                out.push('}');
            }
        }
    }

    pub fn parse(text: &str) -> Result<J, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }
}

impl J {
    /// `json.dumps(v)` with Python's default separators (", ", ": ").
    pub fn dumps_py(&self) -> String {
        match self {
            J::List(v) => format!("[{}]", v.iter().map(J::dumps_py).collect::<Vec<_>>().join(", ")),
            J::Dict(v) => format!(
                "{{{}}}",
                v.iter()
                    .map(|(k, x)| format!("{}: {}", J::from(k).dumps(), x.dumps_py()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => self.dumps(),
        }
    }
}

/// Python's `==`: dicts whatever their keys' order, 1 == 1.0 == True.
pub fn py_eq(a: &J, b: &J) -> bool {
    match (a, b) {
        (J::Dict(x), J::Dict(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && py_eq(v, v2)))
        }
        (J::List(x), J::List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (J::Null, J::Null) => true,
        (J::Str(x), J::Str(y)) => x == y,
        (x, y) => match (x.f(), y.f()) {
            (Some(p), Some(q)) => p == q,
            _ => false,
        },
    }
}

/// Python's `str()`: a str as it is, anything else as its repr.
pub fn py_str(v: &J) -> String {
    match v {
        J::Str(s) => s.clone(),
        _ => v.repr(),
    }
}

/// Python's repr of a str.
pub fn py_str_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c)
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => write!(out, "\\x{:02x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
}

impl<'de> Deserialize<'de> for J {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<J, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = J;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E>(self) -> Result<J, E> {
                Ok(J::Null)
            }
            fn visit_none<E>(self) -> Result<J, E> {
                Ok(J::Null)
            }
            fn visit_bool<E>(self, b: bool) -> Result<J, E> {
                Ok(J::Bool(b))
            }
            fn visit_i64<E>(self, i: i64) -> Result<J, E> {
                Ok(J::Int(i))
            }
            fn visit_u64<E>(self, i: u64) -> Result<J, E> {
                Ok(i64::try_from(i).map(J::Int).unwrap_or(J::Float(i as f64)))
            }
            fn visit_f64<E>(self, f: f64) -> Result<J, E> {
                Ok(J::Float(f))
            }
            fn visit_str<E>(self, s: &str) -> Result<J, E> {
                Ok(J::Str(s.to_string()))
            }
            fn visit_string<E>(self, s: String) -> Result<J, E> {
                Ok(J::Str(s))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<J, A::Error> {
                let mut v = vec![];
                while let Some(x) = a.next_element()? {
                    v.push(x);
                }
                Ok(J::List(v))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<J, A::Error> {
                let mut v: Vec<(String, J)> = vec![];
                while let Some((k, x)) = a.next_entry::<String, J>()? {
                    if let Some(e) = v.iter_mut().find(|(a, _)| *a == k) {
                        e.1 = x;
                    } else {
                        v.push((k, x));
                    }
                }
                Ok(J::Dict(v))
            }
        }
        d.deserialize_any(V)
    }
}

impl From<bool> for J {
    fn from(b: bool) -> J {
        J::Bool(b)
    }
}
impl From<i64> for J {
    fn from(i: i64) -> J {
        J::Int(i)
    }
}
impl From<i32> for J {
    fn from(i: i32) -> J {
        J::Int(i as i64)
    }
}
impl From<u8> for J {
    fn from(i: u8) -> J {
        J::Int(i as i64)
    }
}
impl From<usize> for J {
    fn from(i: usize) -> J {
        J::Int(i as i64)
    }
}
impl From<f64> for J {
    fn from(f: f64) -> J {
        J::Float(f)
    }
}
impl From<&str> for J {
    fn from(s: &str) -> J {
        J::Str(s.to_string())
    }
}
impl From<String> for J {
    fn from(s: String) -> J {
        J::Str(s)
    }
}
impl From<&String> for J {
    fn from(s: &String) -> J {
        J::Str(s.clone())
    }
}
impl<T: Into<J>> From<Option<T>> for J {
    fn from(o: Option<T>) -> J {
        o.map_or(J::Null, Into::into)
    }
}
impl<T: Into<J>> From<Vec<T>> for J {
    fn from(v: Vec<T>) -> J {
        J::List(v.into_iter().map(Into::into).collect())
    }
}
impl From<&J> for J {
    fn from(j: &J) -> J {
        j.clone()
    }
}

/// A dict of the pairs given, in their order: `jd!{"a" => 1, "b" => x}`.
#[macro_export]
macro_rules! jd {
    () => { $crate::j::J::Dict(vec![]) };
    ($($k:expr => $v:expr),+ $(,)?) => {
        $crate::j::J::Dict(vec![$(($k.to_string(), $crate::j::J::from($v))),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_and_writes_as_python() {
        let j = J::parse(r#"{"b": 1, "a": [1.5, null, true, "x'y"], "c": 1e-05}"#).unwrap();
        assert_eq!(j.dumps(), r#"{"b":1,"a":[1.5,null,true,"x'y"],"c":1e-05}"#);
        assert_eq!(j.repr(), r#"{'b': 1, 'a': [1.5, None, True, "x'y"], 'c': 1e-05}"#);
        assert_eq!(j.get("b"), &J::Int(1));
        assert!(j.get("z").is_null());
    }
}
