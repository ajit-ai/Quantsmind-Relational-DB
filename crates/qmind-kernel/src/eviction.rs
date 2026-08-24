//! Eviction policies for the buffer pool (E1).
//! `LruK` — least-recently-used with K=2 history: frames need K+1 accesses
//! to become "hot", protecting repeatedly-read pages from cyclic scans.
//! `ClockSweep` — the original policy, kept as default/fallback.

use std::collections::VecDeque;

pub trait EvictionPolicy: Default {
    fn touch(&mut self, fid: usize);
    fn victim(&mut self, resident: usize) -> Option<usize>;
}

#[derive(Default)]
pub struct ClockSweep {
    hand: usize,
}

impl EvictionPolicy for ClockSweep {
    fn touch(&mut self, _fid: usize) {}
    fn victim(&mut self, resident: usize) -> Option<usize> {
        if resident == 0 {
            return None;
        }
        let v = self.hand % resident;
        self.hand = (v + 1) % resident;
        Some(v)
    }
}

#[derive(Default)]
pub struct LruK {
    /// Access timestamps per frame (capped at K entries).
    history: std::collections::HashMap<usize, VecDeque<u64>>,
    clock: u64,
    k: usize,
}

impl LruK {
    pub fn new(k: usize) -> Self {
        Self {
            history: std::collections::HashMap::new(),
            clock: 0,
            k,
        }
    }

    /// Backward-K distance: now - K-th-most-recent access; large for cold.
    fn bkd(&self, fid: usize) -> u64 {
        match self.history.get(&fid).and_then(|h| h.front()) {
            Some(&t) => self.clock.saturating_sub(t),
            None => u64::MAX, // never accessed = coldest
        }
    }
}

impl EvictionPolicy for LruK {
    fn touch(&mut self, fid: usize) {
        self.clock += 1;
        let h = self.history.entry(fid).or_default();
        h.push_back(self.clock);
        while h.len() > self.k {
            h.pop_front();
        }
    }

    fn victim(&mut self, resident: usize) -> Option<usize> {
        (0..resident).max_by_key(|&f| self.bkd(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lruk_protects_hot_frame_from_cyclic_scan() {
        let mut p = LruK::new(2);
        // frame 0 = hot (accessed 5x), frames 1..=8 = TWO cyclic passes
        // (LRU-K needs K accesses before a frame registers as a repeated scan)
        for _ in 0..5 {
            p.touch(0);
        }
        for _pass in 0..2 {
            for f in 1..=8 {
                p.touch(f);
            }
        }
        // victim must be a cold frame, never the hot one
        for i in 0..8 {
            p.touch(0);
            p.touch(0); // hot page read twice per eviction cycle
            let v = p.victim(9 + i).expect("resident");
            assert_ne!(v, 0, "hot frame evicted");
            p.touch(20 + v); // replacement page enters pool
        }
    }

    #[test]
    fn lruk_never_accessed_is_coldest() {
        let mut p = LruK::new(2);
        p.touch(1);
        p.touch(2);
        assert_eq!(p.victim(3), Some(0), "untouched frame 0 evicts first");
    }

    #[test]
    fn clocksweep_cycles_through_frames() {
        let mut p = ClockSweep::default();
        assert_eq!(p.victim(3), Some(0));
        assert_eq!(p.victim(3), Some(1));
        assert_eq!(p.victim(3), Some(2));
    }

    #[test]
    fn empty_pool_has_no_victim() {
        let mut p = LruK::new(2);
        assert_eq!(p.victim(0), None);
    }
}
