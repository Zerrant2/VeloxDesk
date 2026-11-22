//! Injector thread: owns the platform [`InputInjector`] (created on this thread) and
//! applies events in order. Dropping every [`InputHandle`] releases all keys/buttons.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::input::{DisplayGeometry, InputEvent};

use crate::backends::InjectorFactory;

enum InputCmd {
    Event(InputEvent),
    Displays(Vec<DisplayGeometry>),
    ReleaseAll,
    Shutdown,
}

/// Cloneable sender side.
#[derive(Clone)]
pub struct InputHandle {
    tx: Sender<InputCmd>,
}

impl InputHandle {
    pub fn inject(&self, ev: InputEvent) {
        let _ = self.tx.send(InputCmd::Event(ev));
    }
    pub fn set_displays(&self, d: Vec<DisplayGeometry>) {
        let _ = self.tx.send(InputCmd::Displays(d));
    }
    pub fn release_all(&self) {
        let _ = self.tx.send(InputCmd::ReleaseAll);
    }
}

/// Counters reported when the thread ends.
#[derive(Debug, Clone, Copy, Default)]
pub struct InputStats {
    pub events: u64,
    pub errors: u64,
    /// Sum of (inject time − viewer timestamp) is meaningless without clock sync, so we
    /// record the injection cost itself.
    pub max_inject_us: u64,
}

pub struct InputThread {
    handle: Option<InputHandle>,
    join: Option<JoinHandle<InputStats>>,
}

impl InputThread {
    pub fn start(factory: Arc<InjectorFactory>) -> std::io::Result<Self> {
        let (tx, rx) = channel();
        let join = std::thread::Builder::new().name("vdk-input".into()).spawn(move || run(factory, rx))?;
        Ok(Self { handle: Some(InputHandle { tx }), join: Some(join) })
    }

    pub fn handle(&self) -> InputHandle {
        self.handle.clone().expect("input thread running")
    }

    /// Release everything and stop (blocking; joins the thread).
    pub fn stop(mut self) -> InputStats {
        if let Some(h) = self.handle.take() {
            let _ = h.tx.send(InputCmd::Shutdown);
        }
        self.join.take().and_then(|j| j.join().ok()).unwrap_or_default()
    }
}

fn run(factory: Arc<InjectorFactory>, rx: Receiver<InputCmd>) -> InputStats {
    let mut stats = InputStats::default();
    let mut injector = match factory() {
        Ok(i) => {
            tracing::info!(backend = i.caps().backend, "input injector ready");
            Some(i)
        }
        Err(e) => {
            tracing::warn!("input injection unavailable: {e}");
            None
        }
    };
    let mut warned = false;
    while let Ok(cmd) = rx.recv() {
        if matches!(cmd, InputCmd::Shutdown) {
            break;
        }
        let Some(inj) = injector.as_mut() else { continue };
        match cmd {
            InputCmd::Event(ev) => {
                let t0 = Timestamp::now();
                stats.events += 1;
                if let Err(e) = inj.inject(&ev) {
                    stats.errors += 1;
                    if !warned {
                        tracing::warn!("input injection failed: {e}");
                        warned = true;
                    }
                } else {
                    warned = false;
                }
                stats.max_inject_us = stats.max_inject_us.max(t0.elapsed_us());
            }
            InputCmd::Displays(d) => inj.set_displays(&d),
            InputCmd::Shutdown => {}
            InputCmd::ReleaseAll => {
                if let Err(e) = inj.release_all() {
                    tracing::warn!("release_all failed: {e}");
                }
            }
        }
    }
    if let Some(inj) = injector.as_mut() {
        let _ = inj.release_all();
    }
    stats
}
