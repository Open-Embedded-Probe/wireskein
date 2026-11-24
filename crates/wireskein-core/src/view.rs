//! A part's lines as their changes: what every engine reads (Python's `wireskein.capture.Part.view()`).

/// One logic line: its level before the part, and the ticks where it changes (increasing).
#[derive(Debug, Clone, Copy)]
pub struct Channel<'a> {
    pub name: &'a str,
    pub initial: u8,
    pub edges: &'a [i64],
}

impl<'a> Channel<'a> {
    pub fn new(name: &'a str, initial: u8, edges: &'a [i64]) -> Self {
        Channel { name, initial, edges }
    }

    /// The level at a tick (after the changes at or before it).
    pub fn level_at(&self, tick: i64) -> u8 {
        let n = self.edges.partition_point(|&e| e <= tick);
        self.initial ^ (n as u8 & 1)
    }
}

/// A part: its rate (ticks a second), its length in ticks, and its lines.
#[derive(Debug, Clone)]
pub struct View<'a> {
    pub rate: f64,
    pub n_samples: i64,
    pub channels: Vec<Channel<'a>>,
}

impl<'a> View<'a> {
    /// A view, its lines checked: a level 0 or 1, the changes increasing and inside the part.
    pub fn new(rate: f64, n_samples: i64, channels: Vec<Channel<'a>>) -> Result<Self, String> {
        if rate.is_nan() || rate <= 0.0 {
            return Err(format!("rate {rate}: not a positive number"));
        }
        for c in &channels {
            if c.initial > 1 {
                return Err(format!("{}: initial level {}", c.name, c.initial));
            }
            if c.edges.windows(2).any(|w| w[1] <= w[0]) {
                return Err(format!("{}: its changes do not increase", c.name));
            }
            if c.edges.first().is_some_and(|&e| e < 0) || c.edges.last().is_some_and(|&e| e >= n_samples) {
                return Err(format!("{}: a change outside the part (0..{n_samples})", c.name));
            }
        }
        Ok(View {
            rate,
            n_samples,
            channels,
        })
    }

    pub fn channel(&self, name: &str) -> Option<&Channel<'a>> {
        self.channels.iter().find(|c| c.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_checks() {
        let e = [10, 20];
        let c = Channel::new("A", 1, &e);
        assert_eq!(
            (c.level_at(0), c.level_at(10), c.level_at(19), c.level_at(20)),
            (1, 0, 0, 1)
        );
        assert!(View::new(1e6, 100, vec![c]).is_ok());
        assert!(View::new(1e6, 15, vec![c]).is_err());
        assert!(View::new(1e6, 100, vec![Channel::new("B", 0, &[5, 5])]).is_err());
        assert!(View::new(0.0, 100, vec![]).is_err());
    }
}
