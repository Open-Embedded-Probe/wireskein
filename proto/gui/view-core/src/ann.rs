//! Annotation items indexed by time (a track: one layer of one claim, e.g. I2C transactions, or its bits).
//!
//! Items sorted by start, with the running maximum of their ends: the items in [t0, t1) are found by two binary
//! searches whether they overlap or not. A view lays them out (`layout`): an item wide enough gets its full text,
//! narrower its short text, then a bare box; items under 2 px are merged into busy blocks with their count and worst
//! status; with far more items than pixels each pixel is looked up on its own (two searches a pixel, whatever the
//! count).

use crate::{Tick, Viewport};

/// How an item reads (the order: worse later).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum Status {
    #[default]
    Ok = 0,
    /// read, but something is worth a look (a NACK, a likely claim)
    Note = 1,
    /// doubtful (an ambiguous claim, a frame read with a clock restored)
    Warn = 2,
    /// a check failed
    Error = 3,
    /// activity no item read (uninterpreted)
    Unread = 4,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Item {
    pub s: Tick,
    pub e: Tick,
    pub status: Status,
    /// the caller's id of the item (e.g. its row in the results list)
    pub id: u32,
}

/// One track of items.
#[derive(Default)]
pub struct Track {
    pub name: String,
    items: Vec<Item>,
    /// every item's full and short text, one after another (offsets: 2 an item and one at the end)
    buf: String,
    off: Vec<u32>,
    max_e: Vec<Tick>,
    /// indices of the items whose status is Warn or worse, in order
    bad: Vec<u32>,
}

/// An item or a merge of items laid out in a view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnnShape {
    /// one item: its index in the track, its box and how much of its text fits
    Box { x0: f32, x1: f32, idx: u32, fit: Fit, status: Status },
    /// items too narrow to tell apart: how many, the worst status among them
    Busy { x0: f32, x1: f32, n: u32, status: Status },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    Full,
    Short,
    None,
}

impl Track {
    /// A track of items with their full and short texts (sorted here by start).
    pub fn new(name: &str, mut items: Vec<(Item, String, String)>) -> Self {
        items.sort_by_key(|x| (x.0.s, x.0.e));
        let mut t = Track { name: name.into(), ..Default::default() };
        let mut m = Tick::MIN;
        for (i, (it, full, short)) in items.into_iter().enumerate() {
            let it = Item { e: it.e.max(it.s + 1), ..it };
            m = m.max(it.e);
            t.max_e.push(m);
            if it.status >= Status::Warn {
                t.bad.push(i as u32);
            }
            t.items.push(it);
            t.off.push(t.buf.len() as u32);
            t.buf.push_str(&full);
            t.off.push(t.buf.len() as u32);
            t.buf.push_str(&short);
        }
        t.off.push(t.buf.len() as u32);
        t.buf.shrink_to_fit();
        t
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn item(&self, i: usize) -> &Item {
        &self.items[i]
    }
    pub fn items(&self) -> &[Item] {
        &self.items
    }
    pub fn text(&self, i: usize) -> &str {
        &self.buf[self.off[2 * i] as usize..self.off[2 * i + 1] as usize]
    }
    pub fn short(&self, i: usize) -> &str {
        &self.buf[self.off[2 * i + 1] as usize..self.off[2 * i + 2] as usize]
    }
    /// The items that overlap [t0, t1) are among this range (all of them, and with non-overlapping items only them).
    pub fn range(&self, t0: Tick, t1: Tick) -> std::ops::Range<usize> {
        let a = self.max_e.partition_point(|&e| e <= t0);
        let b = self.items.partition_point(|it| it.s < t1);
        a..b.max(a)
    }
    /// The item under tick t (the last to start among those covering it).
    pub fn hit(&self, t: Tick) -> Option<usize> {
        let r = self.range(t, t + 1);
        r.rev().find(|&i| self.items[i].s <= t && t < self.items[i].e)
    }
    /// The last item starting at or before t (the list's row for a time).
    pub fn index_at(&self, t: Tick) -> Option<usize> {
        self.items.partition_point(|it| it.s <= t).checked_sub(1)
    }
    /// The next item after `i` (or after tick t when `i` is None) whose status is at least Warn.
    pub fn next_bad(&self, after: Option<usize>, t: Tick) -> Option<usize> {
        let k = match after {
            Some(i) => self.bad.partition_point(|&b| (b as usize) <= i),
            None => self.bad.partition_point(|&b| self.items[b as usize].s <= t),
        };
        self.bad.get(k).map(|&b| b as usize)
    }
    pub fn prev_bad(&self, before: Option<usize>, t: Tick) -> Option<usize> {
        let k = match before {
            Some(i) => self.bad.partition_point(|&b| (b as usize) < i),
            None => self.bad.partition_point(|&b| self.items[b as usize].s < t),
        };
        k.checked_sub(1).map(|k| self.bad[k] as usize)
    }

    /// The track laid out for a view; `char_px` the width of a character, to tell which text fits.
    pub fn layout(&self, vp: &Viewport, char_px: f32, out: &mut Vec<AnnShape>) {
        out.clear();
        let t0 = vp.t0.floor() as Tick;
        let t1 = vp.t1().ceil() as Tick;
        let r = self.range(t0, t1);
        let width = vp.width.max(1.0);
        if r.len() as f32 > 2.0 * width {
            self.layout_dense(vp, out);
            return;
        }
        let mut busy: Option<(f32, f32, u32, Status)> = None;
        for i in r {
            let it = &self.items[i];
            if it.e as f64 <= vp.t0 {
                continue;
            }
            let x0 = vp.x_of(it.s as f64).max(-1.0);
            let x1 = vp.x_of(it.e as f64).min(width + 1.0);
            if x1 - x0 < 2.0 {
                match &mut busy {
                    Some((_, bx1, n, st)) if x0 <= *bx1 + 1.0 => {
                        *bx1 = bx1.max(x1.max(x0 + 1.0));
                        *n += 1;
                        *st = (*st).max(it.status);
                    }
                    _ => {
                        if let Some((a, b, n, st)) = busy.take() {
                            out.push(AnnShape::Busy { x0: a, x1: b, n, status: st });
                        }
                        busy = Some((x0, x1.max(x0 + 1.0), 1, it.status));
                    }
                }
                continue;
            }
            if let Some((a, b, n, st)) = busy.take() {
                out.push(AnnShape::Busy { x0: a, x1: b, n, status: st });
            }
            let w = x1 - x0 - 6.0;
            let fit = if self.text(i).chars().count() as f32 * char_px <= w {
                Fit::Full
            } else if self.short(i).chars().count() as f32 * char_px <= w {
                Fit::Short
            } else {
                Fit::None
            };
            out.push(AnnShape::Box { x0, x1, idx: i as u32, fit, status: it.status });
        }
        if let Some((a, b, n, st)) = busy.take() {
            out.push(AnnShape::Busy { x0: a, x1: b, n, status: st });
        }
    }

    /// Far more items than pixels: each pixel's items counted by two searches, the worst status from the bad index.
    fn layout_dense(&self, vp: &Viewport, out: &mut Vec<AnnShape>) {
        let w = vp.width.ceil() as usize;
        let mut cur: Option<(f32, f32, u32, Status)> = None;
        let mut i0 = self.items.partition_point(|it| (it.s as f64) < vp.t0);
        for x in 0..w {
            let tb = vp.t_of(x as f32 + 1.0);
            let i1 = self.items.partition_point(|it| (it.s as f64) < tb);
            let n = (i1 - i0) as u32;
            if n == 0 {
                if let Some((a, b, k, st)) = cur.take() {
                    out.push(AnnShape::Busy { x0: a, x1: b, n: k, status: st });
                }
                i0 = i1;
                continue;
            }
            // the worst: any bad item among [i0, i1)
            let k = self.bad.partition_point(|&b| (b as usize) < i0);
            let st = match self.bad.get(k) {
                Some(&b) if (b as usize) < i1 => {
                    // the worst of the bad ones in the pixel (few)
                    let mut s = Status::Warn;
                    for &bb in self.bad[k..].iter().take_while(|&&bb| (bb as usize) < i1).take(16) {
                        s = s.max(self.items[bb as usize].status);
                    }
                    s
                }
                _ => Status::Ok,
            };
            match &mut cur {
                Some((_, b, k, s)) if *s == st => {
                    *b = x as f32 + 1.0;
                    *k += n;
                }
                _ => {
                    if let Some((a, b, k, s)) = cur.take() {
                        out.push(AnnShape::Busy { x0: a, x1: b, n: k, status: s });
                    }
                    cur = Some((x as f32, x as f32 + 1.0, n, st));
                }
            }
            i0 = i1;
        }
        if let Some((a, b, k, s)) = cur.take() {
            out.push(AnnShape::Busy { x0: a, x1: b, n: k, status: s });
        }
    }

    pub fn mem_bytes(&self) -> usize {
        self.items.capacity() * std::mem::size_of::<Item>()
            + self.max_e.capacity() * 8
            + self.bad.capacity() * 4
            + self.buf.capacity()
            + self.off.capacity() * 4
    }
}
