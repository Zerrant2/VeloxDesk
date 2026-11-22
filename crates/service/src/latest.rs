//! "Latest frame wins" hand-off between pipeline threads (ARCHITECTURE §1: queues of
//! depth ≤ 1). A producer overwrites the pending item; the consumer also receives
//! out-of-band commands through the same condvar so it wakes immediately for either.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

struct State<T, C> {
    item: Option<T>,
    cmds: VecDeque<C>,
    closed: bool,
    dropped: u64,
}

/// Shared slot: one pending `T` (overwritten) plus an unbounded command queue.
pub struct LatestSlot<T, C> {
    inner: Arc<(Mutex<State<T, C>>, Condvar)>,
}

impl<T, C> Clone for LatestSlot<T, C> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

/// What [`LatestSlot::wait`] returned.
#[derive(Debug)]
pub enum Wake<T, C> {
    Item(T),
    Cmd(C),
    Timeout,
    Closed,
}

impl<T, C> Default for LatestSlot<T, C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, C> LatestSlot<T, C> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new((
                Mutex::new(State { item: None, cmds: VecDeque::new(), closed: false, dropped: 0 }),
                Condvar::new(),
            )),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<T, C>> {
        self.inner.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Publish `item`, replacing (dropping) any item not yet consumed.
    /// Returns `false` once the slot is closed.
    pub fn put(&self, item: T) -> bool {
        let mut s = self.lock();
        if s.closed {
            return false;
        }
        if s.item.replace(item).is_some() {
            s.dropped += 1;
        }
        drop(s);
        self.inner.1.notify_one();
        true
    }

    /// Queue a command (never dropped). Returns `false` once closed.
    pub fn command(&self, cmd: C) -> bool {
        let mut s = self.lock();
        if s.closed {
            return false;
        }
        s.cmds.push_back(cmd);
        drop(s);
        self.inner.1.notify_one();
        true
    }

    /// Close: wakes the consumer with `Closed`; further puts/commands are rejected.
    pub fn close(&self) {
        self.lock().closed = true;
        self.inner.1.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Items overwritten before being consumed.
    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }

    /// Wait up to `timeout` for a command (served first) or an item.
    pub fn wait(&self, timeout: Duration) -> Wake<T, C> {
        let deadline = Instant::now() + timeout;
        let mut s = self.lock();
        loop {
            if s.closed {
                return Wake::Closed;
            }
            if let Some(c) = s.cmds.pop_front() {
                return Wake::Cmd(c);
            }
            if let Some(i) = s.item.take() {
                return Wake::Item(i);
            }
            let now = Instant::now();
            if now >= deadline {
                return Wake::Timeout;
            }
            s = self.inner.1.wait_timeout(s, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Non-blocking: next pending command.
    pub fn try_command(&self) -> Option<C> {
        self.lock().cmds.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_wins_and_commands_first() {
        let s: LatestSlot<u32, &'static str> = LatestSlot::new();
        assert!(s.put(1));
        assert!(s.put(2));
        assert!(s.command("kf"));
        assert_eq!(s.dropped(), 1);
        assert!(matches!(s.wait(Duration::ZERO), Wake::Cmd("kf")));
        assert!(matches!(s.wait(Duration::ZERO), Wake::Item(2)));
        assert!(matches!(s.wait(Duration::from_millis(5)), Wake::Timeout));
        s.close();
        assert!(!s.put(3));
        assert!(matches!(s.wait(Duration::from_secs(5)), Wake::Closed));
    }

    #[test]
    fn wakes_across_threads() {
        let s: LatestSlot<u32, ()> = LatestSlot::new();
        let s2 = s.clone();
        let t = std::thread::spawn(move || s2.wait(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(20));
        s.put(7);
        assert!(matches!(t.join().unwrap(), Wake::Item(7)));
    }
}
