//! Injector thread: owns the platform [`InputInjector`] (created on this thread) and
//! applies events in order. Dropping every [`InputHandle`] releases all keys/buttons.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::input::{DisplayGeometry, InputEvent, InputFeedback};

use crate::backends::InjectorFactory;

const FEEDBACK_POLL_INTERVAL: Duration = Duration::from_millis(8);

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
        Self::start_with_feedback(factory, None)
    }

    pub fn start_with_feedback(
        factory: Arc<InjectorFactory>,
        feedback: Option<UnboundedSender<InputFeedback>>,
    ) -> std::io::Result<Self> {
        let (tx, rx) = channel();
        let join = std::thread::Builder::new()
            .name("vdk-input".into())
            .spawn(move || run(factory, rx, feedback))?;
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

fn drain_feedback(inj: &mut dyn veloxdesk_core_traits::InputInjector, tx: Option<&UnboundedSender<InputFeedback>>) {
    let Some(tx) = tx else { return };
    for fb in inj.poll_feedback() {
        let _ = tx.send(fb);
    }
}

fn run(
    factory: Arc<InjectorFactory>,
    rx: Receiver<InputCmd>,
    feedback: Option<UnboundedSender<InputFeedback>>,
) -> InputStats {
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
    loop {
        match rx.recv_timeout(FEEDBACK_POLL_INTERVAL) {
            Ok(InputCmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                if let Some(inj) = injector.as_deref_mut() {
                    drain_feedback(inj, feedback.as_ref());
                }
            }
            Ok(cmd) => {
                let Some(inj) = injector.as_deref_mut() else { continue };
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
                    InputCmd::Shutdown => break,
                    InputCmd::ReleaseAll => {
                        if let Err(e) = inj.release_all() {
                            tracing::warn!("release_all failed: {e}");
                        }
                    }
                }
                drain_feedback(inj, feedback.as_ref());
            }
        }
    }
    if let Some(inj) = injector.as_mut() {
        let _ = inj.release_all();
    }
    stats
}
