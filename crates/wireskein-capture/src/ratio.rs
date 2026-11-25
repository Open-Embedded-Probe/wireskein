//! Exact fractions (Python's `fractions.Fraction` as acquire uses it for times and rates): an analog channel's
//! first tick and rate, where a group stream's analog run falls in a logic part.

use std::cmp::Ordering;
use std::ops::{Add, Div, Mul, Sub};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ratio {
    pub num: i128,
    pub den: i128,
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Ratio {
    pub fn new(num: i128, den: i128) -> Ratio {
        assert!(den != 0, "a fraction over 0");
        let g = gcd(num, den).max(1);
        let s = if den < 0 { -1 } else { 1 };
        Ratio {
            num: s * num / g,
            den: s * den / g,
        }
    }
    pub fn int(n: i128) -> Ratio {
        Ratio { num: n, den: 1 }
    }
    pub fn floor(&self) -> i128 {
        self.num.div_euclid(self.den)
    }
    pub fn ceil(&self) -> i128 {
        -(-self.num).div_euclid(self.den)
    }
    /// Python's round(): to the nearest, a half to the even one.
    pub fn round(&self) -> i128 {
        let f = self.floor();
        let twice = 2 * (self.num - f * self.den);
        match twice.cmp(&self.den) {
            Ordering::Less => f,
            Ordering::Greater => f + 1,
            Ordering::Equal => f + (f & 1),
        }
    }
    pub fn pair(&self) -> (i64, i64) {
        (self.num as i64, self.den as i64)
    }
    pub fn to_f64(&self) -> f64 {
        self.num as f64 / self.den as f64
    }
    pub fn is_positive(&self) -> bool {
        self.num > 0
    }
}

impl From<(u64, u64)> for Ratio {
    fn from((n, d): (u64, u64)) -> Ratio {
        Ratio::new(n as i128, d as i128)
    }
}

impl Add for Ratio {
    type Output = Ratio;
    fn add(self, o: Ratio) -> Ratio {
        Ratio::new(self.num * o.den + o.num * self.den, self.den * o.den)
    }
}

impl Sub for Ratio {
    type Output = Ratio;
    fn sub(self, o: Ratio) -> Ratio {
        Ratio::new(self.num * o.den - o.num * self.den, self.den * o.den)
    }
}

impl Mul for Ratio {
    type Output = Ratio;
    fn mul(self, o: Ratio) -> Ratio {
        let (g1, g2) = (gcd(self.num, o.den).max(1), gcd(o.num, self.den).max(1));
        Ratio::new((self.num / g1) * (o.num / g2), (self.den / g2) * (o.den / g1))
    }
}

impl Div for Ratio {
    type Output = Ratio;
    fn div(self, o: Ratio) -> Ratio {
        Ratio::new(self.num * o.den, self.den * o.num)
    }
}

impl PartialOrd for Ratio {
    fn partial_cmp(&self, o: &Ratio) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Ratio {
    fn cmp(&self, o: &Ratio) -> Ordering {
        (self.num * o.den).cmp(&(o.num * self.den))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_python_fractions() {
        let a = Ratio::new(7, 2);
        assert_eq!((a.floor(), a.ceil(), a.round()), (3, 4, 4));
        assert_eq!(Ratio::new(5, 2).round(), 2);
        assert_eq!(Ratio::new(-7, 2).floor(), -4);
        assert_eq!(Ratio::new(-7, 2).ceil(), -3);
        assert_eq!(Ratio::new(1, 3) + Ratio::new(1, 6), Ratio::new(1, 2));
        assert_eq!(Ratio::new(2, 4).pair(), (1, 2));
        assert!(Ratio::new(1, 3) < Ratio::new(1, 2));
    }
}
