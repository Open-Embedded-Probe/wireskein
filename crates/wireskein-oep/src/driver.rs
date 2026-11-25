//! An OEP probe as one kind of WireSkein's capture devices (wireskein-capture's `Driver`, kind `oep`): found on USB by
//! its vendor bulk interface, opened by an OEP address (`capture_at`'s), held by its lease, captured from as
//! `capture_at` does (the same steps, the same result: §45 moved them behind the interface without a change).

use crate::acquire::{self, Held};
use crate::transport::{VENDOR_CLASS, VENDOR_PROTOCOL, VENDOR_SUBCLASS};
use nusb::MaybeFuture;
use serde_json::{Map, Value, json};
use wireskein_capture::driver::{Caps, Ctl, Device, Driver, Found, Hold, Open, Thresholds};
use wireskein_capture::{Acquired, Mode, Request, Result};

/// How long a lease lasts unless renewed (the capture renews it while it runs).
const LEASE_MS: u32 = 30_000;

pub struct Oep;

impl Driver for Oep {
    fn kind(&self) -> &'static str {
        "oep"
    }
    fn about(&self) -> &'static str {
        "an OEP probe: oep:oep://UNIT_ID[?via=usb|tcp:HOST:PORT|tty:PATH], oep:tcp://HOST:PORT, oep:/dev/..."
    }
    /// The probes on USB (their vendor bulk interface; the serial number is the unit_id). Probes on a network or a
    /// serial port are not listed.
    fn list(&self) -> Result<Vec<Found>> {
        let list = nusb::list_devices()
            .wait()
            .map_err(|e| wireskein_capture::Error::Io(format!("USB devices: {e}")))?;
        let mut out = vec![];
        for d in list {
            let ours = d.interfaces().any(|i| {
                i.class() == VENDOR_CLASS && i.subclass() == VENDOR_SUBCLASS && i.protocol() == VENDOR_PROTOCOL
            });
            let Some(serial) = d.serial_number().filter(|_| ours) else {
                continue;
            };
            let unit = serial.to_ascii_lowercase();
            let mut facts = Map::new();
            facts.insert("unit_id".into(), json!(unit));
            facts.insert(
                "vid_pid".into(),
                json!(format!("{:04x}:{:04x}", d.vendor_id(), d.product_id())),
            );
            if let Some(p) = d.product_string() {
                facts.insert("product".into(), json!(p));
            }
            out.push(Found {
                source: format!("oep:oep://{unit}"),
                label: format!(
                    "OEP probe {unit}{}",
                    d.product_string().map(|p| format!(" ({p})")).unwrap_or_default()
                ),
                facts,
            });
        }
        Ok(out)
    }
    fn open(&self, address: &str, how: &Open) -> Result<Box<dyn Device>> {
        let addr = acquire::address_of(address)?;
        let held = Held::open(&addr, address, &how.owner).map_err(|e| acquire::probe_error(address, e))?;
        Ok(Box::new(OepDevice { held: Some(held) }))
    }
}

pub struct OepDevice {
    held: Option<Held>,
}

impl OepDevice {
    fn held(&mut self) -> Result<&mut Held> {
        self.held
            .as_mut()
            .ok_or_else(|| wireskein_capture::Error::Runtime("the probe was let go".into()))
    }
}

impl Device for OepDevice {
    fn facts(&self) -> Map<String, Value> {
        let mut m = Map::new();
        if let Some(h) = &self.held {
            m.insert("address".into(), json!(h.target));
            if let Some(u) = h.addr.unit() {
                m.insert("unit_id".into(), json!(u));
            }
            m.insert("boot_id".into(), json!(h.boot_id));
        }
        m
    }
    fn hold(&self) -> Hold {
        match &self.held {
            Some(h) => Hold::Lease {
                owner: h.owner.clone(),
                ms: LEASE_MS,
            },
            None => Hold::None,
        }
    }
    /// What can be said without asking the probe's capture functions (their configure answers the rest: a capture
    /// asked beyond the probe is refused by it, with its reason).
    fn caps(&mut self) -> Result<Caps> {
        let facts = self.held()?.facts();
        Ok(Caps {
            lines: vec![],
            rates: vec![],
            stream: Some(Default::default()),
            memory: Some(Default::default()),
            modes: vec![Mode::OneShot, Mode::Stream, Mode::Repeat],
            triggers: acquire::TRIGGERS.iter().map(|s| s.to_string()).collect(),
            thresholds: Thresholds::Fixed,
            reductions: acquire::REDUCTIONS.iter().map(|s| s.to_string()).collect(),
            firmware: vec![],
            extra: [("probe".to_string(), Value::Object(facts))].into_iter().collect(),
        })
    }
    fn capture(&mut self, req: &Request, ctl: &Ctl) -> Result<Acquired> {
        req.check()?;
        if req.threshold_v.is_some() {
            return Err(wireskein_capture::Error::Value(
                "an OEP probe's threshold is its own: no threshold to set".into(),
            ));
        }
        let h = self.held()?;
        let target = h.target.clone();
        Ok(h.capture(req, ctl).map_err(|e| acquire::probe_error(&target, e))?)
    }
    fn close(mut self: Box<Self>) -> Result<()> {
        if let Some(h) = self.held.take() {
            h.end();
        }
        Ok(())
    }
}
