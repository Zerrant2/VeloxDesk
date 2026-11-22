//! Embedded background host runner for the unified GUI ("This Desk" + "Remote Desk").
//!
//! Spawns a lightweight tokio runtime thread running `veloxdesk_service::RunningHost`,
//! forwarding host lifecycle events and interactive `ApprovalHandler` requests to the
//! winit UI loop via `EventLoopProxy<UserEvent>`.

use std::net::SocketAddr;

use veloxdesk_protocol::control::Permissions;
use veloxdesk_ui::{AcceptDecision, IncomingSessionInfo};

/// Events posted from the embedded background host thread to the winit event loop.
#[derive(Debug)]
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub enum EmbeddedHostEvent {
    Listening {
        bind: SocketAddr,
        device_id: String,
        otp: Option<String>,
        rendezvous: Option<SocketAddr>,
    },
    SessionStarted {
        peer_device_id: String,
    },
    SessionEnded {
        peer_device_id: String,
        reason: String,
    },
    OtpRotated {
        otp: String,
    },
    ApprovalRequested {
        info: IncomingSessionInfo,
        default_permissions: Permissions,
        reply: tokio::sync::oneshot::Sender<AcceptDecision>,
    },
    Stopped,
    Error(String),
}

#[cfg(feature = "net")]
pub use net::EmbeddedHostHandle;

#[cfg(not(feature = "net"))]
pub struct EmbeddedHostHandle;

#[cfg(not(feature = "net"))]
impl EmbeddedHostHandle {
    pub fn start(
        _rendezvous: Option<SocketAddr>,
        _require_accept: bool,
        _proxy: veloxdesk_render::winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
    ) -> anyhow::Result<Self> {
        anyhow::bail!("embedded host requires the `net` feature")
    }

    pub fn rotate_otp(&self) {}

    pub fn set_require_accept(&self, _require: bool) {}

    pub fn stop(&self) {}
}

#[cfg(feature = "net")]
mod net {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use tokio::sync::{mpsc, oneshot};
    use veloxdesk_render::winit::event_loop::EventLoopProxy;
    use veloxdesk_service::{
        ApprovalDecision, ApprovalHandler, ApprovalRequest, HostAuthConfig, HostBackends, HostConfig, HostStatusEvent,
        RunningHost,
    };
    use veloxdesk_ui::{AcceptDecision, IncomingSessionInfo};

    use super::EmbeddedHostEvent;
    use crate::app::UserEvent;

    enum HostCmd {
        RotateOtp,
        SetRequireAccept(bool),
        Stop,
    }

    /// Running embedded host handle owned by `App` in GUI mode. Stops cleanly on [`Drop`].
    pub struct EmbeddedHostHandle {
        cmd_tx: mpsc::UnboundedSender<HostCmd>,
        require_accept: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl EmbeddedHostHandle {
        pub fn start(
            rendezvous: Option<SocketAddr>,
            require_accept: bool,
            proxy: EventLoopProxy<UserEvent>,
        ) -> anyhow::Result<Self> {
            let id = crate::viewer::load_identity(None)?;
            let require_flag = Arc::new(AtomicBool::new(require_accept));
            let require_cb = require_flag.clone();
            let proxy_cb = proxy.clone();

            let approval_handler: ApprovalHandler = Arc::new(move |req: ApprovalRequest| {
                let require_now = require_cb.load(Ordering::Relaxed);
                let proxy = proxy_cb.clone();
                Box::pin(async move {
                    if !require_now {
                        return ApprovalDecision::accept(req.default_permissions);
                    }
                    let (tx, rx) = oneshot::channel::<AcceptDecision>();
                    let info = IncomingSessionInfo {
                        peer_device_id: req.peer_device_id,
                        peer_name: req.peer_name,
                        peer_fingerprint: req.peer_fingerprint,
                        peer_public_key: req.peer_public_key.unwrap_or([0u8; 32]),
                    };
                    if proxy
                        .send_event(UserEvent::Host(EmbeddedHostEvent::ApprovalRequested {
                            info,
                            default_permissions: req.default_permissions,
                            reply: tx,
                        }))
                        .is_err()
                    {
                        return ApprovalDecision::reject("host UI unavailable");
                    }
                    match rx.await {
                        Ok(dec) if dec.accepted => {
                            if dec.trust_peer {
                                ApprovalDecision::accept_and_trust(dec.permissions)
                            } else {
                                ApprovalDecision::accept(dec.permissions)
                            }
                        }
                        Ok(_) => ApprovalDecision::reject("rejected by host user"),
                        Err(_) => ApprovalDecision::reject("approval prompt closed"),
                    }
                })
            });

            let cfg = HostConfig {
                rendezvous,
                auth: HostAuthConfig {
                    auto_otp: true,
                    require_accept,
                    allow_no_auth: false,
                    approval_handler: Some(approval_handler),
                    ..HostAuthConfig::open()
                },
                ..HostConfig::default()
            };

            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            let thread = std::thread::Builder::new()
                .name("veloxdesk-host".into())
                .spawn(move || run_host_thread(cfg, id, cmd_rx, proxy))?;

            Ok(Self { cmd_tx, require_accept: require_flag, thread: Some(thread) })
        }

        pub fn rotate_otp(&self) {
            let _ = self.cmd_tx.send(HostCmd::RotateOtp);
        }

        pub fn set_require_accept(&self, require: bool) {
            self.require_accept.store(require, Ordering::Relaxed);
            let _ = self.cmd_tx.send(HostCmd::SetRequireAccept(require));
        }

        pub fn stop(&self) {
            let _ = self.cmd_tx.send(HostCmd::Stop);
        }
    }

    impl Drop for EmbeddedHostHandle {
        fn drop(&mut self) {
            let _ = self.cmd_tx.send(HostCmd::Stop);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn run_host_thread(
        cfg: HostConfig,
        id: veloxdesk_crypto::DeviceIdentity,
        mut cmd_rx: mpsc::UnboundedReceiver<HostCmd>,
        proxy: EventLoopProxy<UserEvent>,
    ) {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("veloxdesk-host-rt")
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = proxy.send_event(UserEvent::Host(EmbeddedHostEvent::Error(format!(
                    "cannot start host runtime: {e}"
                ))));
                return;
            }
        };

        rt.block_on(async move {
            let ev_proxy = proxy.clone();
            let on_event: Arc<dyn Fn(HostStatusEvent) + Send + Sync> = Arc::new(move |ev| {
                let mapped = match ev {
                    HostStatusEvent::Listening { bind, device_id, otp, rendezvous } => {
                        EmbeddedHostEvent::Listening { bind, device_id, otp, rendezvous }
                    }
                    HostStatusEvent::SessionStarted { peer_device_id } => {
                        EmbeddedHostEvent::SessionStarted { peer_device_id }
                    }
                    HostStatusEvent::SessionEnded { peer_device_id, reason } => {
                        EmbeddedHostEvent::SessionEnded { peer_device_id, reason }
                    }
                    HostStatusEvent::OtpRotated { otp } => EmbeddedHostEvent::OtpRotated { otp },
                    HostStatusEvent::Error(e) => EmbeddedHostEvent::Error(e),
                };
                let _ = ev_proxy.send_event(UserEvent::Host(mapped));
            });

            let running = match RunningHost::start(cfg, id, HostBackends::system(), Some(on_event)).await {
                Ok(h) => h,
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::Host(EmbeddedHostEvent::Error(format!(
                        "failed to start local host: {e}"
                    ))));
                    return;
                }
            };

            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    HostCmd::RotateOtp => {
                        let otp = running.auth_state().rotate_otp();
                        let _ = proxy.send_event(UserEvent::Host(EmbeddedHostEvent::OtpRotated { otp }));
                    }
                    HostCmd::SetRequireAccept(req) => {
                        running.auth_state().set_require_accept(req);
                    }
                    HostCmd::Stop => {
                        running.stop();
                        let _ = running.join().await;
                        break;
                    }
                }
            }

            let _ = proxy.send_event(UserEvent::Host(EmbeddedHostEvent::Stopped));
        });
    }
}
