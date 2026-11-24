//! Numbers written as Python writes them in a reason: `repr(float)`, the `g` format, `round()`.

/// (digits, exponent) of a float's shortest round-trip form: x = 0.d1d2d3... * 10^(exp + 1), d1 not 0.
fn shortest(x: f64) -> (String, i32) {
    let s = format!("{:e}", x.abs());
    let (m, e) = s.split_once('e').unwrap();
    let digits: String = m.chars().filter(|c| c.is_ascii_digit()).collect();
    (digits, e.parse().unwrap())
}

/// Python's `repr(x)` of a float: the shortest digits that read back as it, fixed from 1e-4 up to 1e16, else
/// scientific with a two-digit exponent at least.
pub fn repr_f64(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let (d, e) = shortest(x);
    if (-4..16).contains(&e) {
        let body = if e >= 0 {
            let e = e as usize;
            if d.len() > e + 1 {
                format!("{}.{}", &d[..e + 1], &d[e + 1..])
            } else {
                format!("{}{}.0", d, "0".repeat(e + 1 - d.len()))
            }
        } else {
            format!("0.{}{}", "0".repeat((-e - 1) as usize), d)
        };
        format!("{sign}{body}")
    } else {
        let m = if d.len() > 1 {
            format!("{}.{}", &d[..1], &d[1..])
        } else {
            d.clone()
        };
        format!("{sign}{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
    }
}

/// `str(x)` of a Python float (the same as its repr).
pub fn py_float(x: f64) -> String {
    repr_f64(x)
}

/// Python's `format(x, f".{prec}g")` (`prec` 6 for `:g`).
pub fn g(x: f64, prec: usize) -> String {
    gfmt(x, prec, false)
}

fn gfmt(x: f64, prec: usize, alt: bool) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let p = prec.max(1);
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let s = format!("{:.*e}", p - 1, x);
    let (m, e) = s.split_once('e').unwrap();
    let exp: i32 = e.parse().unwrap();
    let strip = |t: String| -> String {
        if alt || !t.contains('.') {
            t
        } else {
            t.trim_end_matches('0').trim_end_matches('.').to_string()
        }
    };
    if exp >= -4 && exp < p as i32 {
        strip(format!("{:.*}", (p as i32 - 1 - exp) as usize, x))
    } else {
        let m = strip(m.to_string());
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// Python's `round(x)`: to the nearest integer, ties to even.
pub fn round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's `round(x, n)` (to n decimals, by the float's own decimal form near enough for the reports).
pub fn round_n(x: f64, n: i32) -> f64 {
    let s = format!("{:.*}", n.max(0) as usize, x);
    s.parse().unwrap_or(x)
}

/// A number as Python's f-string `{v}` writes an int or a float.
pub fn num(v: f64) -> String {
    repr_f64(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_python() {
        assert_eq!(repr_f64(1e-5), "1e-05");
        assert_eq!(repr_f64(1.5e-7), "1.5e-07");
        assert_eq!(repr_f64(0.0001), "0.0001");
        assert_eq!(repr_f64(1e16), "1e+16");
        assert_eq!(repr_f64(123.0), "123.0");
        assert_eq!(repr_f64(0.1), "0.1");
        assert_eq!(repr_f64(-2.5e-5), "-2.5e-05");
        assert_eq!(repr_f64(1234567.0), "1234567.0");
        assert_eq!(g(0.000123456, 4), "0.0001235");
        assert_eq!(g(1e-5, 4), "1e-05");
        assert_eq!(g(12345.0, 4), "1.234e+04");
        assert_eq!(g(100.0, 4), "100");
        assert_eq!(g(2.5, 6), "2.5");
        assert_eq!(g(1e6, 6), "1e+06");
        assert_eq!(g(0.5, 4), "0.5");
        assert_eq!(round(2.5), 2);
        assert_eq!(round(3.5), 4);
    }
}
