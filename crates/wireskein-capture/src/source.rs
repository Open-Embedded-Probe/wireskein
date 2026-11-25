//! A source by its kind: `<kind>:<address>` (`oep:oep://30eda0ea068b`, `demo:counter?lines=8`), the kind choosing the
//! driver, the address what that driver opens.

use crate::driver::{self, Ctl, Device, Driver, Found, Open};
use crate::error::{Error, Result};
use crate::parts::Acquired;
use crate::request::Request;

/// `<kind>:<address>` -> (kind, address).
pub fn split(source: &str) -> Result<(&str, &str)> {
    match source.split_once(':') {
        Some((k, a)) if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') => {
            Ok((k, a))
        }
        _ => Err(Error::Value(format!(
            "{source}: a source is KIND:ADDRESS (oep:oep://UNIT_ID, demo:counter)"
        ))),
    }
}

/// The devices found, and each kind that could not look with why.
pub type Listed = (Vec<Found>, Vec<(String, Error)>);

/// The kinds of device a program knows, each by its driver.
#[derive(Default)]
pub struct Registry {
    drivers: Vec<Box<dyn Driver>>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }
    pub fn with(mut self, d: Box<dyn Driver>) -> Registry {
        self.drivers.push(d);
        self
    }
    pub fn kinds(&self) -> Vec<&'static str> {
        let mut k: Vec<&'static str> = self.drivers.iter().map(|d| d.kind()).collect();
        k.sort();
        k
    }
    pub fn drivers(&self) -> &[Box<dyn Driver>] {
        &self.drivers
    }
    /// The driver of `kind`; Error::Value naming the kinds known when there is none.
    pub fn driver(&self, kind: &str) -> Result<&dyn Driver> {
        self.drivers
            .iter()
            .find(|d| d.kind() == kind)
            .map(|d| d.as_ref())
            .ok_or_else(|| Error::Value(format!("unknown source '{kind}' (known: {})", self.kinds().join(", "))))
    }
    /// The devices that can be seen, of every kind (or of `kind`); a kind that could not look says why in
    /// the second list.
    pub fn list(&self, kind: Option<&str>) -> Result<Listed> {
        let (mut found, mut failed) = (vec![], vec![]);
        let ds: Vec<&dyn Driver> = match kind {
            Some(k) => vec![self.driver(k)?],
            None => self.drivers.iter().map(|d| d.as_ref()).collect(),
        };
        for d in ds {
            match d.list() {
                Ok(f) => found.extend(f),
                Err(e) => failed.push((d.kind().to_string(), e)),
            }
        }
        Ok((found, failed))
    }
    pub fn open(&self, source: &str, how: &Open) -> Result<Box<dyn Device>> {
        let (k, a) = split(source)?;
        self.driver(k)?.open(a, how)
    }
    /// One capture from `source`: opened, captured, closed.
    pub fn capture(&self, source: &str, req: &Request, how: &Open, ctl: &Ctl) -> Result<Acquired> {
        let (k, a) = split(source)?;
        driver::capture_with(self.driver(k)?, a, req, how, ctl)
    }
}
