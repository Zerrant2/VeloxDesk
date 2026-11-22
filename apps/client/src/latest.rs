//! "Latest frame wins" single-slot mailbox between a producer thread and the render thread.

use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

pub struct Latest<T> {
    slot: Mutex<Option<T>>,
    dropped: AtomicU64,
}

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Self { slot: Mutex::new(None), dropped: AtomicU64::new(0) }
    }
}

impl<T> Latest<T> {
    /// Store `v`, replacing (and counting) any value not yet taken. Returns `true` when the
    /// slot was empty, i.e. the consumer needs a wake-up.
    pub fn put(&self, v: T) -> bool {
        let old = self.slot.lock().replace(v);
        if old.is_some() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            false
        } else {
            true
        }
    }
    pub fn take(&self) -> Option<T> {
        self.slot.lock().take()
    }
    /// Frames overwritten before the consumer got to them.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_wins() {
        let l = Latest::default();
        assert!(l.put(1));
        assert!(!l.put(2));
        assert_eq!(l.take(), Some(2));
        assert_eq!(l.take(), None);
        assert_eq!(l.dropped(), 1);
    }
}
