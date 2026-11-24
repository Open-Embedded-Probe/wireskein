//! WireSkein's core in plain Rust: a capture's lines (their changes) to claims in the analysis format
//! (docs/analysis-format.md), with no Python in it, to be shared by a native app, the web (WASM) and Python
//! (`wireskein-py`).
//!
//! Built up beside the Python engine a part at a time (bench/notes/2026-11-24-rust-plan.ja.md): each part is judged on
//! the bench against the Python one (equal or better, not the same), and only then made the product's default.

pub mod can;
pub mod cec;
pub mod cjtag;
pub mod claim;
pub mod clocked;
pub mod dali;
pub mod dcc;
pub mod devices;
pub mod dht;
pub mod duplex;
pub mod features;
pub mod i2c;
pub mod i2s;
pub mod irnec;
pub mod j1850;
pub mod jtag;
pub mod kernels;
pub mod mdio;
pub mod onewire;
pub mod ps2;
pub mod pulse;
pub mod pulsecode;
pub mod rebuild;
pub mod rgbled;
pub mod riscv_dm;
pub mod rvswd;
pub mod sdmmc;
pub mod sdspi;
pub mod sent;
pub mod spdif;
pub mod spi;
pub mod swd;
pub mod swim;
pub mod swio;
pub mod sync;
pub mod syncunk;
pub mod textup;
pub mod uart;
pub mod upper;
pub mod usb;
pub mod usbpd;
pub mod view;
pub mod wiegand;

pub use claim::Claim;
pub use view::{Channel, View};

/// A reading of some lines that may become a claim: its score, and the best other reading of the same lines by the
/// same reader that is no same reading (another idle level, another rate), if any.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub protocol: String,
    pub roles: std::collections::BTreeMap<String, String>,
    pub score: f64,
    pub alt: Option<f64>,
    pub params: serde_json::Map<String, serde_json::Value>,
    pub evidence: serde_json::Map<String, serde_json::Value>,
    pub layers: Box<serde_json::value::RawValue>,
}

pub const CLAIM_COST: f64 = 0.3;

fn lines(c: &Candidate) -> std::collections::BTreeSet<&str> {
    c.roles.values().map(|s| s.as_str()).collect()
}

/// The set of candidates using each line once with the most value, by branch and bound over the candidates best
/// first (Python's `stack.explanations`); `order`: the candidates by value, best first. Past a bound on the search,
/// the best set found so far (the greedy one is the first found).
fn pack(cands: &[Candidate], order: &[usize], vals: &[f64]) -> Vec<usize> {
    const STEPS: usize = 200_000;
    let sets: Vec<std::collections::BTreeSet<&str>> = order.iter().map(|&i| lines(&cands[i])).collect();
    let mut rest = vec![0.0; order.len() + 1];
    for k in (0..order.len()).rev() {
        rest[k] = rest[k + 1] + vals[k];
    }
    struct Search<'a> {
        sets: Vec<std::collections::BTreeSet<&'a str>>,
        vals: Vec<f64>,
        rest: Vec<f64>,
        best: f64,
        best_set: Vec<usize>,
        cur: Vec<usize>,
        used: std::collections::BTreeMap<&'a str, usize>,
        steps: usize,
    }
    impl Search<'_> {
        fn go(&mut self, k: usize, v: f64) {
            self.steps += 1;
            if v > self.best + 1e-12 {
                self.best = v;
                self.best_set = self.cur.clone();
            }
            if k == self.sets.len() || v + self.rest[k] <= self.best + 1e-12 || self.steps > STEPS {
                return;
            }
            if self.sets[k].iter().all(|l| !self.used.contains_key(l)) {
                self.cur.push(k);
                for l in self.sets[k].clone() {
                    self.used.insert(l, k);
                }
                self.go(k + 1, v + self.vals[k]);
                for l in self.sets[k].clone() {
                    self.used.remove(l);
                }
                self.cur.pop();
            }
            self.go(k + 1, v);
        }
    }
    let mut s = Search {
        sets,
        vals: vals.to_vec(),
        rest,
        best: 0.0,
        best_set: vec![],
        cur: vec![],
        used: Default::default(),
        steps: 0,
    };
    s.go(0, 0.0);
    s.best_set.into_iter().map(|k| order[k]).collect()
}

/// The claims among the candidates: the set using each line once with the most value (the lines a reading explains
/// times its score past the claim's cost; `pack`); each claim's margin over the best rival (another reading on some of its
/// lines, not the same protocol on fewer of them), its verdict from the two.
pub fn choose(mut cands: Vec<Candidate>) -> Vec<Claim> {
    cands.retain(|c| c.score > CLAIM_COST);
    let value = |c: &Candidate| c.roles.len() as f64 * (c.score - CLAIM_COST);
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| value(&cands[b]).partial_cmp(&value(&cands[a])).unwrap().then(a.cmp(&b)));
    let vals: Vec<f64> = order.iter().map(|&i| value(&cands[i])).collect();
    let chosen = pack(&cands, &order, &vals);
    let mut out = vec![];
    for &i in &chosen {
        let c = &cands[i];
        let ls = lines(c);
        // the margin: the best set without this reading (nor a less complete one of its protocol on its lines), the
        // value lost over the lines whose reading changes; its own alternative (another idle level, another rate)
        // a rival too, as one line read otherwise
        let keep: Vec<usize> = order
            .iter()
            .copied()
            .filter(|&j| j != i && !(cands[j].protocol == c.protocol && lines(&cands[j]).is_subset(&ls)))
            .collect();
        let kv: Vec<f64> = keep.iter().map(|&j| value(&cands[j])).collect();
        let alt = pack(&cands, &keep, &kv);
        let total = |set: &[usize]| set.iter().map(|&j| value(&cands[j])).sum::<f64>();
        let owner = |set: &[usize]| -> std::collections::BTreeMap<String, usize> {
            set.iter()
                .flat_map(|&j| cands[j].roles.values().map(move |l| (l.clone(), j)))
                .collect()
        };
        let (ob, oa) = (owner(&chosen), owner(&alt));
        let changed = ob
            .keys()
            .chain(oa.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|l| ob.get(*l) != oa.get(*l))
            .count();
        let mut margin = (total(&chosen) - total(&alt)) / changed.max(1) as f64;
        let mut rival = alt.iter().any(|&j| !lines(&cands[j]).is_disjoint(&ls));
        if let Some(a) = c.alt {
            margin = margin.min(c.score - a.max(CLAIM_COST));
            rival = true;
        }
        // (unknown-sync: a kind of its own, never a known protocol's verdict)
        let verdict = if c.protocol == "sync_unknown" {
            "unknown-sync"
        } else {
            verdict(c.score, margin, rival)
        };
        // a bus of several targets (an SPI with a select per device) comes out as its targets' claims, each its own
        // reading under the bus's verdict
        if c.params.contains_key("targets")
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(c.layers.get())
            && let Some(ts) = v["targets"].as_array()
        {
            for t in ts {
                out.push(Claim {
                    protocol: c.protocol.clone(),
                    roles: serde_json::from_value(t["roles"].clone()).unwrap_or_default(),
                    verdict: verdict.into(),
                    score: (t["score"].as_f64().unwrap_or(c.score) * 1e4).round() / 1e4,
                    margin: (margin * 1e4).round() / 1e4,
                    params: t["params"].as_object().cloned().unwrap_or_default(),
                    evidence: t["evidence"].as_object().cloned().unwrap_or_default(),
                    layers: claim::raw(serde_json::to_string(&t["layers"]).unwrap()),
                    total: t["score"].as_f64().unwrap_or(c.score),
                    rival,
                    bus: c.params.get("bus").and_then(|b| b.as_str()).map(String::from),
                });
            }
            continue;
        }
        out.push(Claim {
            protocol: c.protocol.clone(),
            roles: c.roles.clone(),
            verdict: verdict.into(),
            score: (c.score * 1e4).round() / 1e4,
            margin: (margin * 1e4).round() / 1e4,
            params: c.params.clone(),
            evidence: c.evidence.clone(),
            layers: c.layers.clone(),
            total: c.score,
            rival,
            bus: None,
        });
    }
    out
}

/// The verdict at a score and margin, by the engine's rule (over a rival reading, lower bars).
pub fn verdict(score: f64, margin: f64, rival: bool) -> &'static str {
    let confirmed = (score >= 0.85 && margin >= 0.15)
        || (rival && ((score >= 0.80 && margin >= 0.25) || (score >= 0.60 && margin >= 0.50)));
    if confirmed {
        "confirmed"
    } else if score >= 0.6 && margin >= 0.05 {
        "likely"
    } else {
        "ambiguous"
    }
}

pub const GENERIC: [&str; 3] = ["uart", "i2c", "spi"]; // readers of any lines' bits, of no framing of their own
pub const SPECIFIC: f64 = 0.6; // a reading of fixed framing this sure stands for a generic one of the same lines

/// Every reading of a part's lines, before the claims are chosen. A generic reading (UART, I2C, SPI) of lines a
/// reading of fixed framing explains is left out where that one is sure: an SPI reading of MDC and MDIO (whose
/// 64-bit frames are whole bytes as well) is MDIO's frames told without their preamble and start, an SPI reading of
/// JTAG's, SD's or I2S's lines (on their clock) their scans, commands or slots told as bytes, an I2C or UART reading
/// of a USB pair its packets' changes.
pub fn candidates(view: &View) -> Vec<Candidate> {
    candidates_with(view, &Default::default())
}

/// Every reading, UART lines in `bauds` read at the baud given.
pub fn candidates_with(view: &View, bauds: &std::collections::BTreeMap<String, f64>) -> Vec<Candidate> {
    let mut cands = uart::candidates_with(view, bauds);
    cands.extend(i2c::candidates(view));
    cands.extend(spi::candidates(view));
    let groups = clocked::groups(view);
    let mut specific = mdio::candidates(view);
    specific.extend(jtag::candidates_of(&groups));
    specific.extend(cjtag::candidates(view));
    specific.extend(sdmmc::candidates_of(&groups));
    specific.extend(i2s::candidates_of(&groups, view));
    specific.extend(usb::candidates(view));
    let sure: Vec<(Option<&str>, std::collections::BTreeSet<&str>)> = specific
        .iter()
        .filter(|c| c.score >= SPECIFIC)
        .map(|c| {
            let clock = ["mdc", "tck", "tckc", "clk", "sck"]
                .iter()
                .find_map(|r| c.roles.get(*r).map(|s| s.as_str()));
            (clock, lines(c))
        })
        .collect();
    cands.retain(|c| {
        !GENERIC.contains(&c.protocol.as_str())
            || !sure.iter().any(|(clock, ls)| {
                lines(c).is_subset(ls) && (c.protocol != "spi" || c.roles.get("clk").map(|s| s.as_str()) == *clock)
            })
    });
    cands.extend(specific);
    // the readers of a line's own timing, each reading checked (CRC, parity, fixed framing, presence pulses, inverse
    // bytes; not an LED string's, whose bits carry no check): a line one of them reads
    // surely carries that protocol, and a generic reading using it (a UART of its pulses, an I2C or SPI taking it as
    // data beside some clock) is left out, as SPI on MDIO's lines above
    let mut own = cec::candidates(view);
    own.extend(dali::candidates(view));
    own.extend(dcc::candidates(view));
    own.extend(j1850::candidates(view));
    own.extend(sent::candidates(view));
    own.extend(spdif::candidates(view));
    own.extend(swim::candidates(view));
    own.extend(usbpd::candidates(view));
    own.extend(wiegand::candidates(view));
    own.extend(onewire::candidates(view));
    own.extend(irnec::candidates(view));
    own.extend(dht::candidates(view));
    own.extend(rvswd::candidates(view));
    own.extend(swio::candidates(view));
    let taken: std::collections::BTreeSet<String> = own
        .iter()
        .filter(|c| c.score >= SPECIFIC)
        .flat_map(|c| c.roles.values().cloned())
        .collect();
    cands.retain(|c| !(GENERIC.contains(&c.protocol.as_str()) && c.roles.values().any(|l| taken.contains(l))));
    cands.extend(own);
    cands.extend(rgbled::candidates(view));
    cands.extend(ps2::candidates(view));
    cands.extend(swd::candidates(view));
    cands.extend(can::candidates(view));
    cands.extend(unknown(&cands, syncunk::candidates_of(&groups)));
    cands
}

pub const UNKNOWN_KNOWN_MIN: f64 = 0.5; // a known reading this sure on an unknown reading's clock and lines ...

/// The unknown clocked readings that explain what no known reading explains better (Python's `roots_pack`): none
/// sharing a line with a known reading that scores as much (a free clock "sampling" a known bus's lines must not
/// win by their count), none whose clock and lines hold a sure known reading (its frames that bus's, with their
/// structure).
fn unknown(known: &[Candidate], unk: Vec<Candidate>) -> Vec<Candidate> {
    let clock_of = |c: &Candidate| -> Option<String> {
        ["clk", "scl", "mdc", "tck", "tckc", "sck", "swclk"]
            .iter()
            .find_map(|r| c.roles.get(*r).cloned())
            .or_else(|| c.roles.values().next().cloned())
    };
    unk.into_iter()
        .filter(|u| {
            let ls = lines(u);
            !known.iter().any(|k| {
                let kl = lines(k);
                (k.score >= u.score && !kl.is_disjoint(&ls))
                    || (k.score >= UNKNOWN_KNOWN_MIN && clock_of(k) == u.roles.get("clk").cloned() && kl.is_subset(&ls))
            })
        })
        .collect()
}

/// What the analysis is told (Python's `staged._hinted`): the protocols to read (none: all), each pin's protocol,
/// role and baud, the pins left out.
#[derive(Debug, Default, Clone)]
pub struct Hints {
    pub protocols: std::collections::BTreeSet<String>,
    pub pins: std::collections::BTreeMap<String, PinHint>,
    pub exclude: std::collections::BTreeSet<String>,
}

#[derive(Debug, Default, Clone)]
pub struct PinHint {
    pub protocol: Option<String>,
    pub role: Option<String>,
    pub baud: Option<f64>,
}

impl Hints {
    pub fn from_json(v: &serde_json::Value) -> Hints {
        let mut h = Hints::default();
        let strs = |x: &serde_json::Value| -> Vec<String> {
            x.as_array()
                .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        h.protocols = strs(&v["protocols"]).into_iter().collect();
        h.exclude = strs(&v["exclude_pins"]).into_iter().collect();
        if let Some(m) = v["pins"].as_object() {
            for (pin, i) in m {
                let p = PinHint {
                    protocol: i["protocol"].as_str().map(String::from),
                    role: i["role"].as_str().map(String::from),
                    baud: i["baud"].as_f64(),
                };
                if !h.protocols.is_empty()
                    && let Some(proto) = &p.protocol
                {
                    h.protocols.insert(proto.clone());
                }
                h.pins.insert(pin.clone(), p);
            }
        }
        h
    }

    /// Whether a reading keeps to the hints: a protocol asked for, and each of its pins in the protocol and role
    /// named for it (an SPI's data roles: data0, data1 ... answer mosi and miso).
    pub fn allows(&self, c: &Candidate) -> bool {
        if !self.protocols.is_empty() && !self.protocols.contains(&c.protocol) {
            return false;
        }
        c.roles.iter().all(|(role, pin)| match self.pins.get(pin) {
            None => true,
            Some(h) => {
                h.protocol.as_ref().is_none_or(|p| *p == c.protocol)
                    && h.role.as_ref().is_none_or(|r| {
                        r == role
                            || (c.protocol == "spi"
                                && role.starts_with("data")
                                && matches!(r.as_str(), "mosi" | "miso"))
                    })
            }
        })
    }
}

/// The claims on a part's lines (built up protocol by protocol), under the hints.
pub fn analyze(view: &View, hints: &serde_json::Value) -> Vec<Claim> {
    analyze_full(view, hints).0
}

/// The claims on a part's lines after the upper stage, and the relations it found (a UART pair's SCPI).
pub fn analyze_full(view: &View, hints: &serde_json::Value) -> (Vec<Claim>, Vec<serde_json::Value>) {
    upper::stage_with(analyze_engine(view, hints), view.rate)
}

/// The engine's claims under the hints, before the upper stage (for a caller with an upper stage of its own:
/// Python's, with its device packs and definitions directories).
pub fn analyze_engine(view: &View, hints: &serde_json::Value) -> Vec<Claim> {
    choose(readings(view, hints))
}

/// Every reading of a part's lines the hints allow, before the claims are chosen (a decode spec's bus is read
/// whether or not it is claimed).
pub fn readings(view: &View, hints: &serde_json::Value) -> Vec<Candidate> {
    let h = Hints::from_json(hints);
    if h.protocols.is_empty() && h.pins.is_empty() && h.exclude.is_empty() {
        return candidates(view);
    }
    let kept = View {
        rate: view.rate,
        n_samples: view.n_samples,
        channels: view
            .channels
            .iter()
            .filter(|c| !h.exclude.contains(c.name))
            .copied()
            .collect(),
    };
    let bauds: std::collections::BTreeMap<String, f64> = h
        .pins
        .iter()
        .filter_map(|(p, i)| i.baud.map(|b| (p.clone(), b)))
        .collect();
    let mut cands = candidates_with(&kept, &bauds);
    cands.retain(|c| h.allows(c));
    cands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_keep_readings_to_them() {
        let h = Hints::from_json(
            &serde_json::json!({"protocols": ["uart"], "pins": {"A": {"protocol": "uart", "role": "data", "baud": 9600}}, "exclude_pins": ["B"]}),
        );
        assert!(h.protocols.contains("uart") && h.exclude.contains("B") && h.pins["A"].baud == Some(9600.0));
        let c = |p: &str, role: &str| Candidate {
            protocol: p.into(),
            roles: [(role.to_string(), "A".to_string())].into(),
            score: 0.9,
            alt: None,
            params: Default::default(),
            evidence: Default::default(),
            layers: claim::raw("{}".into()),
        };
        assert!(h.allows(&c("uart", "data")) && !h.allows(&c("i2c", "sda")) && !h.allows(&c("uart", "tx")));
    }

    #[test]
    fn an_empty_part_has_no_claims() {
        let v = View::new(1e6, 1000, vec![Channel::new("A", 1, &[])]).unwrap();
        assert!(analyze(&v, &serde_json::Value::Null).is_empty());
    }
}
