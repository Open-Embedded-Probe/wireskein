//! What to capture (Python's wireskein.sources.Request): the lines by name and the device's id for each, the rate,
//! how many samples or how long, a trigger, the mode (one-shot, repeat, stream), what to do at a loss; the same for
//! every device (a device refuses, with its reason, what it cannot do).

use crate::error::{Error, Result};
use std::path::PathBuf;

pub const REDUCTIONS: [&str; 4] = ["sample", "any_high", "any_low", "change_latch"];
pub const TRIGGERS: [&str; 5] = ["high", "low", "rise", "fall", "both"];
pub const ANALOG_TRIGGERS: [&str; 4] = ["high", "low", "rise", "fall"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    OneShot,
    Stream,
    Repeat,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Mode> {
        Ok(match s {
            "oneshot" => Mode::OneShot,
            "stream" => Mode::Stream,
            "repeat" => Mode::Repeat,
            _ => return Err(Error::Value(format!("mode {s:?}: oneshot, stream or repeat"))),
        })
    }
    pub fn name(&self) -> &'static str {
        match self {
            Mode::OneShot => "oneshot",
            Mode::Stream => "stream",
            Mode::Repeat => "repeat",
        }
    }
}

/// What to capture.
#[derive(Debug, Clone)]
pub struct Request {
    /// (name to record, the device's channel id)
    pub channels: Vec<(String, String)>,
    pub rate_hz: u64,
    pub samples: u64,
    /// (logic channel, one of TRIGGERS)
    pub trigger: Option<(String, String)>,
    pub pretrigger: Option<u64>,
    /// trigger_opts arm 0: the trigger looked for at once
    pub arm_now: bool,
    pub timeout_s: f64,
    /// name -> (reduction, D)
    pub resolution: Vec<(String, (String, u64))>,
    pub mode: Mode,
    pub duration_s: Option<f64>,
    pub segments: Option<u64>,
    /// stream: stop at the first loss (else go on to the end)
    pub on_loss_stop: bool,
    /// stream: where its channels are put together on disk while it runs (None: the system's temporary directory)
    pub spool_dir: Option<PathBuf>,
    pub glitch_filter_ns: Option<u32>,
    pub input_hysteresis: Option<bool>,
    /// logic lines' threshold in volts, for a device that sets one (None: the device's own)
    pub threshold_v: Option<f64>,
    /// analog: (name to record, the probe's channel name, frontend)
    pub analog: Vec<(String, String, Option<u8>)>,
    /// analog samples a second (None: `rate_hz`) and how many (None: as long as the logic capture)
    pub analog_rate_hz: Option<u64>,
    pub analog_samples: Option<u64>,
    /// a trigger on an analog channel: the volts it crosses or stays beyond
    pub trigger_volts: Option<f64>,
}

impl Request {
    pub fn new(channels: Vec<(String, String)>, rate_hz: u64, samples: u64) -> Request {
        Request {
            channels,
            rate_hz,
            samples,
            trigger: None,
            pretrigger: None,
            arm_now: false,
            timeout_s: 10.0,
            resolution: vec![],
            mode: Mode::OneShot,
            duration_s: None,
            segments: None,
            on_loss_stop: true,
            spool_dir: None,
            glitch_filter_ns: None,
            input_hysteresis: None,
            threshold_v: None,
            analog: vec![],
            analog_rate_hz: None,
            analog_samples: None,
            trigger_volts: None,
        }
    }

    pub fn analog_names(&self) -> Vec<&str> {
        self.analog.iter().map(|(n, _, _)| n.as_str()).collect()
    }

    /// acquire.Request's checks.
    pub fn check(&self) -> Result<()> {
        let names: Vec<&str> = self.channels.iter().map(|(n, _)| n.as_str()).collect();
        let anames = self.analog_names();
        let mut all: Vec<&str> = names.iter().chain(anames.iter()).copied().collect();
        let given = all.clone();
        all.sort();
        all.dedup();
        if given.is_empty() || all.len() != given.len() {
            return Err(Error::Value(format!(
                "channel names must be given and differ: {given:?}"
            )));
        }
        if let Some((name, kind)) = &self.trigger {
            if anames.contains(&name.as_str()) {
                if !ANALOG_TRIGGERS.contains(&kind.as_str()) || self.trigger_volts.is_none() {
                    return Err(Error::Value(format!(
                        "trigger {name}:{kind}: an analog channel takes {} and the volts it crosses or stays beyond \
                         ({name}:fall:2.9)",
                        ANALOG_TRIGGERS.join("/")
                    )));
                }
            } else if !names.contains(&name.as_str())
                || !TRIGGERS.contains(&kind.as_str())
                || self.trigger_volts.is_some()
            {
                return Err(Error::Value(format!(
                    "trigger {name}:{kind}: the channel must be one captured, the kind one of {} (volts only for an \
                     analog channel)",
                    TRIGGERS.join(", ")
                )));
            }
        }
        if self.mode == Mode::Repeat
            && !self.analog.is_empty()
            && self.segments.is_some_and(|s| s >= 1)
            && self.samples > 0
        {
            return Err(Error::Value("repeat: logic channels only".into()));
        }
        match self.mode {
            Mode::Repeat if !self.segments.is_some_and(|s| s >= 1) || self.samples == 0 => {
                return Err(Error::Value(
                    "repeat needs samples and segments (how many acquisitions, 1 or more)".into(),
                ));
            }
            Mode::OneShot | Mode::Stream if self.segments.is_some() => {
                return Err(Error::Value("segments is for repeat only".into()));
            }
            Mode::Stream if !self.duration_s.is_some_and(|d| d > 0.0) => {
                return Err(Error::Value("a stream needs duration_s (how long)".into()));
            }
            Mode::Stream if self.trigger.is_some() => {
                return Err(Error::Value("a stream is started at once: no trigger".into()));
            }
            _ => {}
        }
        if self.glitch_filter_ns == Some(0) {
            return Err(Error::Value("glitch_filter_ns 0: 1 or more (None: no filter)".into()));
        }
        for (name, (mode, d)) in &self.resolution {
            if !names.contains(&name.as_str()) || !REDUCTIONS.contains(&mode.as_str()) || *d < 2 {
                return Err(Error::Value(format!(
                    "{name}=({mode:?}, {d}): a resolution is for a logic channel captured, (one of {}, 2 or more \
                     samples)",
                    REDUCTIONS.join(", ")
                )));
            }
        }
        Ok(())
    }
}
