//! In-memory `Arc`-backed clipboard backend for deterministic unit & integration tests.

use std::sync::{Arc, Mutex};

use veloxdesk_core_traits::Result;

use crate::{ClipboardBackend, ClipboardItem};

#[derive(Default)]
struct State {
    current: Option<ClipboardItem>,
    reads: u64,
    writes: u64,
}

/// Thread-safe in-memory [`ClipboardBackend`] suitable for deterministic tests.
#[derive(Clone, Default)]
pub struct MemoryClipboard {
    state: Arc<Mutex<State>>,
}

impl MemoryClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_item(item: ClipboardItem) -> Self {
        let m = Self::new();
        m.set(Some(item));
        m
    }

    /// Replace the stored clipboard content directly (simulates a local user copy).
    pub fn set(&self, item: Option<ClipboardItem>) {
        self.state.lock().expect("poisoned").current = item;
    }

    /// Inspect the stored clipboard content without incrementing backend read count.
    pub fn get(&self) -> Option<ClipboardItem> {
        self.state.lock().expect("poisoned").current.clone()
    }

    /// Number of times `ClipboardBackend::write` was called by the sync loop.
    pub fn write_count(&self) -> u64 {
        self.state.lock().expect("poisoned").writes
    }

    /// Number of times `ClipboardBackend::read` was called by the sync loop.
    pub fn read_count(&self) -> u64 {
        self.state.lock().expect("poisoned").reads
    }
}

impl ClipboardBackend for MemoryClipboard {
    fn read(&mut self) -> Result<Option<ClipboardItem>> {
        let mut s = self.state.lock().expect("poisoned");
        s.reads += 1;
        Ok(s.current.clone())
    }

    fn write(&mut self, item: &ClipboardItem) -> Result<()> {
        let mut s = self.state.lock().expect("poisoned");
        s.writes += 1;
        s.current = Some(item.clone());
        Ok(())
    }
}
