//! Embedded background host runner for the unified GUI ("This Desk" + "Remote Desk").
//!
//! Spawns a lightweight tokio runtime thread running `veloxdesk_service::RunningHost`,
//! forwarding host lifecycle events and interactive `ApprovalHandler` requests to the
//! winit UI loop via `EventLoopProxy<UserEvent>`.

use std::net::SocketAddr;

use veloxdesk_protocol::control::Permissions;
use veloxdesk_ui::{AcceptDecision, IncomingSessionInfo, PasswordMode, SavedTrustedPeer};

/// Initial host authentication settings supplied when starting the embedded host.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub struct EmbeddedHostAuthOpts {
    pub password_mode: PasswordMode,
    pub permanent_password_hash: Option<String>,
    pub trusted_peers: Vec<SavedTrustedPeer>,
    pub require_accept: bool,
}

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
    OtpCleared,
    TrustedPeerAdded(SavedTrustedPeer),
    ApprovalRequested {
        info: IncomingSessionInfo,
        default_permissions: Permissions,
        reply: tokio::sync::oneshot::Sender<AcceptDecision>,
    },
    Stopped,
    Error(String),
}

/// Parse a 64-character hex-encoded Ed25519 public key into `[u8; 32]`.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub fn parse_hex_pubkey_32(hex: &str) -> Option<[u8; 32]> {
    let s = hex.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Encode a 32-byte Ed25519 public key as lowercase hex.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub fn encode_hex_pubkey_32(pk: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in pk {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Validate an Argon2id PHC string (`$argon2id$...`) loaded from persisted settings.
#[cfg(feature = "net")]
pub fn is_valid_phc_hash(phc: &str) -> bool {
    veloxdesk_crypto::auth::PasswordHash::from_phc_string(phc).is_ok()
}

#[cfg(not(feature = "net"))]
pub fn is_valid_phc_hash(phc: &str) -> bool {
    phc.starts_with("$argon2id$") && phc.len() > 20
}

/// Hash a permanent password into an Argon2id PHC string (intended to run on a worker thread).
#[cfg(feature = "net")]
pub fn hash_password_phc(password: &str) -> anyhow::Result<String> {
    let ph = veloxdesk_crypto::auth::PasswordHash::hash(password)
        .map_err(|e| anyhow::anyhow!("password hashing failed: {e}"))?;
    Ok(ph.to_phc_string())
}

#[cfg(not(feature = "net"))]
pub fn hash_password_phc(_password: &str) -> anyhow::Result<String> {
    anyhow::bail!("permanent password hashing requires the `net` feature")
}

#[cfg(feature = "net")]
pub use net::EmbeddedHostHandle;

#[cfg(not(feature = "net"))]
pub struct EmbeddedHostHandle;

#[cfg(not(feature = "net"))]
impl EmbeddedHostHandle {
    pub fn start(
        _rendezvous: Option<SocketAddr>,
        _auth: EmbeddedHostAuthOpts,
        _proxy: veloxdesk_render::winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
        lang: std::sync::Arc<std::sync::atomic::AtomicU8>,
    ) -> anyhow::Result<Self> {
        let l = veloxdesk_ui::Language::from_u8(lang.load(std::sync::atomic::Ordering::Relaxed));
        anyhow::bail!("{}", veloxdesk_ui::i18n::tr(l).err_embedded_host_requires_net)
    }

    pub fn rotate_otp(&self) {}

    pub fn set_require_accept(&self, _require: bool) {}

    pub fn set_permanent_password_phc(&self, _phc: Option<String>) {}

    pub fn set_otp_enabled(&self, _enabled: bool) {}

    pub fn remove_trusted_peer(&self, _public_key_hex: String) {}

    pub fn stop(&self) {}
}

#[cfg(feature = "net")]
mod net {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
    use std::thread::JoinHandle;

    use tokio::sync::{mpsc, oneshot};
    use veloxdesk_crypto::auth::PasswordHash;
    use veloxdesk_protocol::control::Permissions;
    use veloxdesk_render::winit::event_loop::EventLoopProxy;
    use veloxdesk_service::{
        ApprovalDecision, ApprovalHandler, ApprovalRequest, HostAuthConfig, HostBackends, HostConfig, HostStatusEvent,
        RunningHost, TrustedPeer,
    };
    use veloxdesk_ui::{AcceptDecision, IncomingSessionInfo, Language, PasswordMode, SavedTrustedPeer, i18n::tr};

    use super::{EmbeddedHostAuthOpts, EmbeddedHostEvent, encode_hex_pubkey_32, parse_hex_pubkey_32};
    use crate::app::UserEvent;

    fn current_lang(flag: &AtomicU8) -> Language {
        Language::from_u8(flag.load(Ordering::Relaxed))
    }

    fn saved_to_trusted_peer(saved: &SavedTrustedPeer) -> Option<TrustedPeer> {
        let pk = parse_hex_pubkey_32(&saved.public_key_hex)?;
        let perms = Permissions {
            view: true,
            input: saved.allow_input,
            clipboard: saved.allow_clipboard,
            file_transfer: saved.allow_file_transfer,
            audio: saved.allow_audio,
            terminal: saved.allow_terminal,
            tunnel: saved.allow_tunnel,
            privacy_mode: saved.allow_privacy_mode,
            restart: false,
            allow_chat: true,
            allow_power: false,
        };
        Some(TrustedPeer::new(pk, saved.name.clone()).with_permissions(perms))
    }

    fn trusted_peer_to_saved(peer: &TrustedPeer) -> SavedTrustedPeer {
        SavedTrustedPeer {
            public_key_hex: encode_hex_pubkey_32(&peer.public_key),
            device_id: peer.device_id(),
            name: peer.name.clone(),
            fingerprint: peer.fingerprint(),
            allow_input: peer.permissions.input,
            allow_clipboard: peer.permissions.clipboard,
            allow_file_transfer: peer.permissions.file_transfer,
            allow_audio: peer.permissions.audio,
            allow_terminal: peer.permissions.terminal,
            allow_tunnel: peer.permissions.tunnel,
            allow_privacy_mode: peer.permissions.privacy_mode,
        }
    }

    enum HostCmd {
        RotateOtp,
        SetRequireAccept(bool),
        SetPermanentPasswordHash(Option<PasswordHash>),
        SetOtpEnabled(bool),
        RemoveTrustedPeer([u8; 32]),
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
            auth: EmbeddedHostAuthOpts,
            proxy: EventLoopProxy<UserEvent>,
            lang: Arc<AtomicU8>,
        ) -> anyhow::Result<Self> {
            let id = crate::viewer::load_identity(None)?;
            let require_flag = Arc::new(AtomicBool::new(auth.require_accept));
            let require_cb = require_flag.clone();
            let proxy_cb = proxy.clone();
            let lang_cb = lang.clone();

            let approval_handler: ApprovalHandler = Arc::new(move |req: ApprovalRequest| {
                let require_now = require_cb.load(Ordering::Relaxed);
                let proxy = proxy_cb.clone();
                let lang = lang_cb.clone();
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
                        return ApprovalDecision::reject(tr(current_lang(&lang)).host_reject_ui_unavailable);
                    }
                    match rx.await {
                        Ok(dec) if dec.accepted => {
                            if dec.trust_peer {
                                ApprovalDecision::accept_and_trust(dec.permissions)
                            } else {
                                ApprovalDecision::accept(dec.permissions)
                            }
                        }
                        Ok(_) => ApprovalDecision::reject(tr(current_lang(&lang)).host_reject_by_user),
                        Err(_) => ApprovalDecision::reject(tr(current_lang(&lang)).host_reject_prompt_closed),
                    }
                })
            });

            let parsed_perm_hash = auth.permanent_password_hash.as_deref().and_then(|phc| {
                match PasswordHash::from_phc_string(phc) {
                    Ok(h) => Some(h),
                    Err(e) => {
                        tracing::warn!("ignoring corrupt permanent_password_hash in settings: {e}");
                        None
                    }
                }
            });

            let mut trusted_peers = Vec::with_capacity(auth.trusted_peers.len());
            for saved in &auth.trusted_peers {
                match saved_to_trusted_peer(saved) {
                    Some(p) => trusted_peers.push(p),
                    None => tracing::warn!(
                        key = %saved.public_key_hex,
                        "ignoring malformed trusted_peer public_key_hex in settings"
                    ),
                }
            }

            // BLOCKER #1: in `OneTime` mode the saved permanent password MUST NOT be accepted,
            // even though its PHC hash remains persisted on disk if the user switches back later.
            let effective_perm_hash = if auth.password_mode == PasswordMode::OneTime {
                None
            } else {
                parsed_perm_hash
            };

            // Safety invariant: NEVER leave host with neither OTP nor permanent password.
            let auto_otp = auth.password_mode != PasswordMode::Permanent || effective_perm_hash.is_none();

            let cfg = HostConfig {
                rendezvous,
                auth: HostAuthConfig {
                    auto_otp,
                    permanent_password_hash: effective_perm_hash,
                    trusted_peers,
                    require_accept: auth.require_accept,
                    allow_no_auth: false,
                    approval_handler: Some(approval_handler),
                    ..HostAuthConfig::open()
                },
                ..HostConfig::default()
            };

            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            let thread = std::thread::Builder::new()
                .name("veloxdesk-host".into())
                .spawn(move || run_host_thread(cfg, id, cmd_rx, proxy, lang))?;

            Ok(Self { cmd_tx, require_accept: require_flag, thread: Some(thread) })
        }

        pub fn rotate_otp(&self) {
            let _ = self.cmd_tx.send(HostCmd::RotateOtp);
        }

        pub fn set_require_accept(&self, require: bool) {
            self.require_accept.store(require, Ordering::Relaxed);
            let _ = self.cmd_tx.send(HostCmd::SetRequireAccept(require));
        }

        pub fn set_permanent_password_phc(&self, phc: Option<String>) {
            let parsed = match phc {
                Some(s) => match PasswordHash::from_phc_string(&s) {
                    Ok(h) => Some(h),
                    Err(e) => {
                        tracing::warn!("ignoring invalid PHC hash update: {e}");
                        return;
                    }
                },
                None => None,
            };
            let _ = self.cmd_tx.send(HostCmd::SetPermanentPasswordHash(parsed));
        }

        pub fn set_otp_enabled(&self, enabled: bool) {
            let _ = self.cmd_tx.send(HostCmd::SetOtpEnabled(enabled));
        }

        pub fn remove_trusted_peer(&self, public_key_hex: String) {
            if let Some(pk) = parse_hex_pubkey_32(&public_key_hex) {
                let _ = self.cmd_tx.send(HostCmd::RemoveTrustedPeer(pk));
            } else {
                tracing::warn!(key = %public_key_hex, "cannot revoke malformed trusted peer key");
            }
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
        lang: Arc<AtomicU8>,
    ) {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("veloxdesk-host-rt")
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = proxy.send_event(UserEvent::Host(EmbeddedHostEvent::Error(
                    tr(current_lang(&lang)).fmt_err_host_runtime(&e.to_string()),
                )));
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
                    HostStatusEvent::TrustedPeerAdded { peer } => {
                        EmbeddedHostEvent::TrustedPeerAdded(trusted_peer_to_saved(&peer))
                    }
                    HostStatusEvent::ChatReceived { .. } | HostStatusEvent::ChatAcked { .. } => return,
                    HostStatusEvent::Error(e) => EmbeddedHostEvent::Error(e),
                };
                let _ = ev_proxy.send_event(UserEvent::Host(mapped));
            });

            let running = match RunningHost::start(cfg, id, HostBackends::system(), Some(on_event)).await {
                Ok(h) => h,
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::Host(EmbeddedHostEvent::Error(
                        tr(current_lang(&lang)).fmt_err_host_start(&e.to_string()),
                    )));
                    return;
                }
            };

            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    HostCmd::RotateOtp => {
                        // `HostAuthState::rotate_otp` already invokes the `on_event` hook with `OtpRotated`.
                        let _ = running.auth_state().rotate_otp();
                    }
                    HostCmd::SetRequireAccept(req) => {
                        running.auth_state().set_require_accept(req);
                    }
                    HostCmd::SetPermanentPasswordHash(hash) => {
                        running.auth_state().set_permanent_password_hash(hash);
                    }
                    HostCmd::SetOtpEnabled(enabled) => {
                        let auth_state = running.auth_state();
                        if !enabled && auth_state.permanent_password_hash().is_none() {
                            // Defense-in-depth (MAJOR #3): never disable OTP if no permanent password is active.
                            let had_otp = auth_state.current_otp().is_some();
                            if let Some(otp) = auth_state.set_otp_enabled(true)
                                && had_otp
                            {
                                let _ = proxy.send_event(UserEvent::Host(EmbeddedHostEvent::OtpRotated { otp }));
                            }
                        } else if enabled {
                            // MINOR #5: when transitioning None -> Some, `set_otp_enabled(true)` already fires
                            // `on_otp_rotated` -> `EmbeddedHostEvent::OtpRotated`; only emit here if it was
                            // already `Some` so UI stays synced without duplicate events.
                            let had_otp = auth_state.current_otp().is_some();
                            if let Some(otp) = auth_state.set_otp_enabled(true)
                                && had_otp
                            {
                                let _ = proxy.send_event(UserEvent::Host(EmbeddedHostEvent::OtpRotated { otp }));
                            }
                        } else {
                            let _ = auth_state.set_otp_enabled(false);
                            let _ = proxy.send_event(UserEvent::Host(EmbeddedHostEvent::OtpCleared));
                        }
                    }
                    HostCmd::RemoveTrustedPeer(pk) => {
                        let _ = running.auth_state().remove_trusted_peer(&pk);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_pubkey_roundtrip_and_validation() {
        let pk = [0xABu8; 32];
        let hex = encode_hex_pubkey_32(&pk);
        assert_eq!(hex.len(), 64);
        assert_eq!(parse_hex_pubkey_32(&hex), Some(pk));
        assert_eq!(parse_hex_pubkey_32(&hex.to_uppercase()), Some(pk));
        assert_eq!(parse_hex_pubkey_32("short"), None);
        assert_eq!(parse_hex_pubkey_32(&"zz".repeat(32)), None);
    }
}
