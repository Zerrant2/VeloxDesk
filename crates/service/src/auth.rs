//! Host-side E2EE authentication and per-session authorization (ARCHITECTURE §7, M2).
//!
//! Supports:
//! * Permanent password (`AuthMethod::PermanentPassword`): Argon2id + HMAC-SHA256 challenge-response.
//! * Rotating one-time password (`AuthMethod::OneTimePassword`): single-use, rotated immediately on login.
//! * Trusted Ed25519 public-key whitelist (`AuthMethod::PublicKey`): passwordless signature over `nonce || host_pk`.
//! * Interactive accept callback ([`ApprovalHandler`]) with granular [`Permissions`] and "remember trusted peer".
//! * M1 open mode ([`HostAuthConfig::open`]): skips `AuthChallenge` when no auth policy is configured,
//!   while still verifying TLS peer certificate ↔ `Hello` identity binding when TLS is present.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::timeout;
use veloxdesk_core_traits::{CoreError, Result, TransportChannel};
use veloxdesk_crypto::auth::{
    Argon2Params, PasswordHash, derive_password_key, generate_nonce, generate_otp, verify_password_proof,
    verify_pubkey_challenge,
};
use veloxdesk_crypto::{device_id_from_public_key, fingerprint};
use veloxdesk_protocol::control::{
    AuthChallenge, AuthMethod, AuthResponse, AuthResult, Close, Hello, Permissions, SessionMode, envelope::Msg,
};
use veloxdesk_transport::{ControlReceiver, ControlSender};

use crate::config::HostConfig;

/// Default length of automatically generated one-time passwords (unambiguous alphanumeric).
pub const DEFAULT_OTP_LEN: usize = 8;
/// How long to wait for a viewer to answer `AuthChallenge` (allows time for password prompt).
const AUTH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait for a host user to respond to an interactive approval prompt.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
/// Close code sent when authentication or authorization fails.
pub const CLOSE_AUTH_FAILED: u32 = 4;

/// A viewer Ed25519 identity trusted to connect without a password or accept prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct TrustedPeer {
    pub public_key: [u8; 32],
    pub name: String,
    pub permissions: Permissions,
}

impl TrustedPeer {
    /// Create a trusted peer entry with full permissions.
    pub fn new(public_key: [u8; 32], name: impl Into<String>) -> Self {
        Self { public_key, name: name.into(), permissions: Permissions::all() }
    }

    pub fn with_permissions(mut self, permissions: Permissions) -> Self {
        self.permissions = permissions;
        self
    }

    pub fn device_id(&self) -> String {
        device_id_from_public_key(&self.public_key)
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_key)
    }
}

/// Information presented to the host user when interactive approval is required.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub peer_device_id: String,
    pub peer_public_key: Option<[u8; 32]>,
    pub peer_fingerprint: String,
    pub peer_name: String,
    pub peer_os: String,
    pub requested_mode: SessionMode,
    pub default_permissions: Permissions,
}

/// Host user's response to an [`ApprovalRequest`].
#[derive(Debug, Clone)]
pub struct ApprovalDecision {
    pub accepted: bool,
    pub reason: String,
    pub permissions: Permissions,
    pub remember_peer: bool,
}

impl ApprovalDecision {
    pub fn accept(permissions: Permissions) -> Self {
        Self { accepted: true, reason: String::new(), permissions, remember_peer: false }
    }

    pub fn accept_and_trust(permissions: Permissions) -> Self {
        Self { accepted: true, reason: String::new(), permissions, remember_peer: true }
    }

    pub fn reject(reason: impl Into<String>) -> Self {
        Self {
            accepted: false,
            reason: reason.into(),
            permissions: Permissions::default(),
            remember_peer: false,
        }
    }
}

/// Async callback invoked by the host when `HostAuthConfig::require_accept` is enabled.
pub type ApprovalHandler = Arc<
    dyn Fn(ApprovalRequest) -> Pin<Box<dyn Future<Output = ApprovalDecision> + Send>> + Send + Sync,
>;

/// Authentication and authorization configuration for a host.
#[derive(Clone)]
pub struct HostAuthConfig {
    /// Current one-time password (single-use; rotated immediately upon successful OTP login).
    pub one_time_password: Option<String>,
    /// Automatically generate and rotate an 8-char OTP when none is explicitly provided.
    pub auto_otp: bool,
    /// Argon2id hash of the permanent password (never stores plaintext).
    pub permanent_password_hash: Option<PasswordHash>,
    /// Whitelisted Ed25519 peers allowed passwordless `AuthMethod::PublicKey` login.
    pub trusted_peers: Vec<TrustedPeer>,
    /// Require interactive approval via [`Self::approval_handler`] (bypassed by trusted peers).
    pub require_accept: bool,
    /// Allow unauthenticated access (M1 open mode when no other auth mechanism is configured).
    pub allow_no_auth: bool,
    /// Interactive approval callback (used when `require_accept` is true).
    pub approval_handler: Option<ApprovalHandler>,
    /// Host Ed25519 public key included in `AuthChallenge` (automatically populated by `run_host`).
    pub host_public_key: Option<[u8; 32]>,
    /// Shared mutable auth state (OTP rotation and runtime trusted peers across sessions).
    pub shared_state: Option<Arc<HostAuthState>>,
}

impl fmt::Debug for HostAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostAuthConfig")
            .field("has_one_time_password", &self.one_time_password.is_some())
            .field("auto_otp", &self.auto_otp)
            .field("has_permanent_password", &self.permanent_password_hash.is_some())
            .field("trusted_peers", &self.trusted_peers.len())
            .field("require_accept", &self.require_accept)
            .field("allow_no_auth", &self.allow_no_auth)
            .field("has_approval_handler", &self.approval_handler.is_some())
            .field("has_host_public_key", &self.host_public_key.is_some())
            .finish()
    }
}

impl PartialEq for HostAuthConfig {
    fn eq(&self, other: &Self) -> bool {
        self.one_time_password == other.one_time_password
            && self.auto_otp == other.auto_otp
            && self.permanent_password_hash == other.permanent_password_hash
            && self.trusted_peers == other.trusted_peers
            && self.require_accept == other.require_accept
            && self.allow_no_auth == other.allow_no_auth
            && self.approval_handler.is_some() == other.approval_handler.is_some()
            && self.host_public_key == other.host_public_key
    }
}

impl Default for HostAuthConfig {
    fn default() -> Self {
        Self::open()
    }
}

impl HostAuthConfig {
    /// M1-compatible open mode: no `AuthChallenge`, auto-accepts every viewer.
    pub fn open() -> Self {
        Self {
            one_time_password: None,
            auto_otp: false,
            permanent_password_hash: None,
            trusted_peers: Vec::new(),
            require_accept: false,
            allow_no_auth: true,
            approval_handler: None,
            host_public_key: None,
            shared_state: None,
        }
    }

    pub fn with_permanent_password(hash: PasswordHash) -> Self {
        Self { permanent_password_hash: Some(hash), allow_no_auth: false, ..Self::open() }
    }

    pub fn with_otp(otp: impl Into<String>) -> Self {
        Self { one_time_password: Some(otp.into()), allow_no_auth: false, ..Self::open() }
    }

    pub fn with_auto_otp() -> Self {
        Self { auto_otp: true, allow_no_auth: false, ..Self::open() }
    }

    pub fn with_trusted_peers(trusted_peers: Vec<TrustedPeer>) -> Self {
        Self { trusted_peers, allow_no_auth: false, ..Self::open() }
    }

    pub fn require_accept_with(handler: ApprovalHandler) -> Self {
        Self { require_accept: true, allow_no_auth: false, approval_handler: Some(handler), ..Self::open() }
    }

    /// True iff no authentication or interactive approval policy is enabled (M1 fast path).
    pub fn is_open(&self) -> bool {
        self.allow_no_auth
            && self.permanent_password_hash.is_none()
            && self.one_time_password.is_none()
            && !self.auto_otp
            && !self.require_accept
            && self.trusted_peers.is_empty()
            && self.shared_state.as_ref().is_none_or(|s| s.is_open())
    }
}

type OtpRotatedCallback = Arc<dyn Fn(String) + Send + Sync>;
pub(crate) type TrustedPeerCallback = Arc<dyn Fn(TrustedPeer) + Send + Sync>;

struct AuthInner {
    current_otp: Option<String>,
    trusted_peers: Vec<TrustedPeer>,
    permanent_password_hash: Option<PasswordHash>,
    require_accept_override: Option<bool>,
    on_otp_rotated: Option<OtpRotatedCallback>,
    on_trusted_peer_added: Option<TrustedPeerCallback>,
}

/// Shared mutable authentication state across sessions on a host (rotating OTP and runtime
/// trusted-peer additions from "Remember this device").
pub struct HostAuthState {
    inner: Mutex<AuthInner>,
}

impl HostAuthState {
    pub fn from_config(cfg: &HostAuthConfig) -> Arc<Self> {
        let current_otp = cfg
            .one_time_password
            .clone()
            .or_else(|| cfg.auto_otp.then(|| generate_otp(DEFAULT_OTP_LEN)));
        Arc::new(Self {
            inner: Mutex::new(AuthInner {
                current_otp,
                trusted_peers: cfg.trusted_peers.clone(),
                permanent_password_hash: cfg.permanent_password_hash.clone(),
                require_accept_override: None,
                on_otp_rotated: None,
                on_trusted_peer_added: None,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AuthInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True when no dynamic OTP, permanent password, trusted peer, or accept requirement is set.
    pub fn is_open(&self) -> bool {
        let g = self.lock();
        g.current_otp.is_none()
            && g.trusted_peers.is_empty()
            && g.permanent_password_hash.is_none()
            && g.require_accept_override != Some(true)
    }

    /// Dynamically override whether interactive approval is required for new connections.
    pub fn set_require_accept(&self, require: bool) {
        self.lock().require_accept_override = Some(require);
    }

    /// Effective `require_accept` flag, honoring any runtime override set via [`Self::set_require_accept`].
    pub fn effective_require_accept(&self, default: bool) -> bool {
        self.lock().require_accept_override.unwrap_or(default)
    }

    /// Install an optional callback invoked whenever the OTP rotates (manually or after login).
    pub(crate) fn set_on_otp_rotated(&self, cb: Option<OtpRotatedCallback>) {
        self.lock().on_otp_rotated = cb;
    }

    /// Install an optional callback invoked whenever a trusted peer is added or updated.
    pub(crate) fn set_on_trusted_peer_added(&self, cb: Option<TrustedPeerCallback>) {
        self.lock().on_trusted_peer_added = cb;
    }

    /// Set or clear the permanent Argon2id password hash live (takes effect on the next connection).
    pub fn set_permanent_password_hash(&self, hash: Option<PasswordHash>) {
        self.lock().permanent_password_hash = hash;
    }

    /// Enable or disable one-time password authentication live.
    ///
    /// - When `enabled == true`: if no OTP is currently active, generates a fresh OTP, stores it,
    ///   fires the OTP-rotation callback, and returns `Some(otp)`. If an OTP is already active,
    ///   returns `Some(existing)` without rotating.
    /// - When `enabled == false`: clears the active OTP (`None`) and returns `None`.
    pub fn set_otp_enabled(&self, enabled: bool) -> Option<String> {
        let (result, notify) = {
            let mut g = self.lock();
            if enabled {
                if let Some(existing) = &g.current_otp {
                    (Some(existing.clone()), None)
                } else {
                    let next = fresh_otp(None);
                    g.current_otp = Some(next.clone());
                    (Some(next.clone()), g.on_otp_rotated.clone().map(|cb| (cb, next)))
                }
            } else {
                g.current_otp = None;
                (None, None)
            }
        };
        if let Some((cb, otp)) = notify {
            cb(otp);
        }
        result
    }

    /// Currently active one-time password, if OTP login is enabled.
    pub fn current_otp(&self) -> Option<String> {
        self.lock().current_otp.clone()
    }

    /// Generate and install a fresh OTP guaranteed to differ from the previous one.
    pub fn rotate_otp(&self) -> String {
        let (next, cb) = {
            let mut g = self.lock();
            let next = fresh_otp(g.current_otp.as_deref());
            g.current_otp = Some(next.clone());
            (next, g.on_otp_rotated.clone())
        };
        if let Some(cb) = cb {
            cb(next.clone());
        }
        next
    }

    /// Atomically consume `matched` if it is still the active OTP, immediately rotating to a
    /// fresh OTP to prevent replay. Returns `Some(new_otp)` on success, or `None` if `matched`
    /// was already consumed/rotated by another session.
    pub fn consume_and_rotate_otp(&self, matched: &str) -> Option<String> {
        let (next, cb) = {
            let mut g = self.lock();
            if g.current_otp.as_deref() != Some(matched) {
                return None;
            }
            let next = fresh_otp(Some(matched));
            g.current_otp = Some(next.clone());
            (next, g.on_otp_rotated.clone())
        };
        if let Some(cb) = cb {
            cb(next.clone());
        }
        Some(next)
    }

    pub fn permanent_password_hash(&self) -> Option<PasswordHash> {
        self.lock().permanent_password_hash.clone()
    }

    pub fn find_trusted(&self, pk: &[u8; 32]) -> Option<TrustedPeer> {
        self.lock().trusted_peers.iter().find(|p| &p.public_key == pk).cloned()
    }

    pub fn add_trusted_peer(&self, peer: TrustedPeer) {
        let cb = {
            let mut g = self.lock();
            if let Some(existing) = g.trusted_peers.iter_mut().find(|p| p.public_key == peer.public_key) {
                *existing = peer.clone();
            } else {
                g.trusted_peers.push(peer.clone());
            }
            g.on_trusted_peer_added.clone()
        };
        if let Some(cb) = cb {
            cb(peer);
        }
    }

    /// Remove a trusted peer by its Ed25519 public key. Returns `true` if a matching peer was removed.
    pub fn remove_trusted_peer(&self, pk: &[u8; 32]) -> bool {
        let mut g = self.lock();
        let before = g.trusted_peers.len();
        g.trusted_peers.retain(|p| &p.public_key != pk);
        g.trusted_peers.len() != before
    }

    pub fn trusted_peers(&self) -> Vec<TrustedPeer> {
        self.lock().trusted_peers.clone()
    }
}

fn fresh_otp(previous: Option<&str>) -> String {
    loop {
        let cand = generate_otp(DEFAULT_OTP_LEN);
        if previous != Some(cand.as_str()) {
            return cand;
        }
    }
}

/// Verified identity and granted permissions returned after the auth handshake succeeds.
#[derive(Debug, Clone)]
pub struct AuthOutcome {
    pub peer_device_id: String,
    pub peer_public_key: Option<[u8; 32]>,
    pub permissions: Permissions,
}

async fn reject_auth(
    writer: &mut ControlSender,
    ch: &dyn TransportChannel,
    reason: impl Into<String>,
) -> CoreError {
    let reason = reason.into();
    let _ = writer
        .send(&Msg::AuthResult(AuthResult { ok: false, reason: reason.clone(), permissions: None, session_id: 0 }).into())
        .await;
    let _ = writer.send(&Msg::Close(Close { code: CLOSE_AUTH_FAILED, reason: reason.clone() }).into()).await;
    let _ = writer.finish().await;
    // Allow QUIC to flush AuthResult/Close before closing the connection.
    tokio::time::sleep(Duration::from_millis(30)).await;
    ch.close(CLOSE_AUTH_FAILED, &reason);
    CoreError::AccessDenied(reason.into())
}

fn default_session_permissions(cfg: &HostConfig) -> Permissions {
    Permissions {
        view: true,
        input: cfg.allow_input,
        clipboard: true,
        file_transfer: true,
        audio: cfg.audio_enabled,
        tunnel: true,
        terminal: true,
        restart: false,
        privacy_mode: true,
    }
}

/// Verify peer identity and execute the M1/M2 authentication handshake after receiving `Hello`.
pub(crate) async fn authenticate(
    sid: u64,
    hello: &Hello,
    ch: &Arc<dyn TransportChannel>,
    cfg: &HostConfig,
    writer: &mut ControlSender,
    reader: &mut ControlReceiver,
) -> Result<AuthOutcome> {
    let ch: &dyn TransportChannel = ch.as_ref();
    // 1. Bind Hello identity claims to the mutual-TLS Ed25519 peer certificate whenever present.
    let tls_pk = ch.peer_public_key();
    if let Some(pk) = tls_pk {
        if !hello.public_key.is_empty() && hello.public_key.as_slice() != pk {
            return Err(reject_auth(writer, ch, "Hello public_key does not match TLS certificate").await);
        }
        let expected_id = device_id_from_public_key(&pk);
        if !hello.device_id.is_empty() && hello.device_id != expected_id {
            return Err(reject_auth(writer, ch, "Hello device_id does not match TLS certificate").await);
        }
    }

    let peer_pk: Option<[u8; 32]> = tls_pk.or_else(|| hello.public_key.as_slice().try_into().ok());
    if tls_pk.is_none()
        && !cfg.auth.is_open()
        && let Some(pk) = peer_pk
    {
        let expected_id = device_id_from_public_key(&pk);
        if !hello.device_id.is_empty() && hello.device_id != expected_id {
            return Err(reject_auth(writer, ch, "Hello device_id does not match Hello public_key").await);
        }
    }

    let peer_device_id =
        peer_pk.as_ref().map(device_id_from_public_key).unwrap_or_else(|| hello.device_id.clone());

    // 2. M1 open mode: skip AuthChallenge and auto-accept with configured permissions.
    if cfg.auth.is_open() {
        let perms = default_session_permissions(cfg);
        writer
            .send(
                &Msg::AuthResult(AuthResult {
                    ok: true,
                    reason: String::new(),
                    permissions: Some(perms.clone()),
                    session_id: sid,
                })
                .into(),
            )
            .await?;
        return Ok(AuthOutcome { peer_device_id, peer_public_key: peer_pk, permissions: perms });
    }

    // 3. Build and send AuthChallenge.
    let state = cfg.auth.shared_state.clone().unwrap_or_else(|| HostAuthState::from_config(&cfg.auth));
    let trusted_entry = peer_pk.as_ref().and_then(|pk| state.find_trusted(pk));
    let perm_hash = state.permanent_password_hash();
    let active_otp = state.current_otp();

    let mut methods = Vec::new();
    if trusted_entry.is_some() {
        methods.push(AuthMethod::PublicKey as i32);
    }
    if perm_hash.is_some() {
        methods.push(AuthMethod::PermanentPassword as i32);
    }
    if active_otp.is_some() {
        methods.push(AuthMethod::OneTimePassword as i32);
    }
    let require_accept = state.effective_require_accept(cfg.auth.require_accept);
    let allow_method_none = cfg.auth.allow_no_auth
        || (require_accept && cfg.auth.approval_handler.is_some() && perm_hash.is_none() && active_otp.is_none());
    if allow_method_none {
        methods.push(AuthMethod::None as i32);
    }
    if methods.is_empty() {
        return Err(reject_auth(writer, ch, "no authentication method available for this peer").await);
    }

    let nonce = generate_nonce();
    let (salt, argon2_params) = if let Some(ph) = &perm_hash {
        (ph.salt.clone(), ph.params)
    } else if active_otp.is_some() {
        (generate_nonce()[..16].to_vec(), Argon2Params::default())
    } else {
        (Vec::new(), Argon2Params { m_cost_kib: 0, t_cost: 0, p_cost: 0 })
    };
    let host_pk = cfg.auth.host_public_key.unwrap_or([0u8; 32]);
    let (host_pk_bytes, host_device_id) = match cfg.auth.host_public_key {
        Some(pk) => (pk.to_vec(), device_id_from_public_key(&pk)),
        None => (Vec::new(), String::new()),
    };

    writer
        .send(
            &Msg::AuthChallenge(AuthChallenge {
                nonce: nonce.to_vec(),
                salt: salt.clone(),
                argon2_m_cost_kib: argon2_params.m_cost_kib,
                argon2_t_cost: argon2_params.t_cost,
                argon2_p_cost: argon2_params.p_cost,
                methods: methods.clone(),
                host_public_key: host_pk_bytes,
                host_device_id,
            })
            .into(),
        )
        .await?;

    // 4. Receive AuthResponse.
    let resp: AuthResponse = match timeout(AUTH_RESPONSE_TIMEOUT, reader.recv()).await {
        Ok(Ok(env)) => match env.msg {
            Some(Msg::AuthResponse(r)) => r,
            Some(Msg::Close(c)) => {
                ch.close(c.code, &c.reason);
                return Err(CoreError::Closed);
            }
            other => {
                return Err(reject_auth(writer, ch, format!("expected AuthResponse, got {other:?}")).await);
            }
        },
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(reject_auth(writer, ch, "timed out waiting for AuthResponse").await);
        }
    };

    if !methods.contains(&resp.method) {
        return Err(reject_auth(writer, ch, "authentication method not allowed").await);
    }

    // 5. Verify cryptographic proof.
    let mut authed_as_trusted: Option<TrustedPeer> = None;
    match AuthMethod::try_from(resp.method) {
        Ok(AuthMethod::PublicKey) => {
            let (Some(pk), Some(tp)) = (peer_pk, trusted_entry) else {
                return Err(reject_auth(writer, ch, "peer is not in trusted whitelist").await);
            };
            if !verify_pubkey_challenge(&pk, &nonce, &host_pk, &resp.proof) {
                return Err(reject_auth(writer, ch, "invalid public key proof").await);
            }
            authed_as_trusted = Some(tp);
        }
        Ok(AuthMethod::PermanentPassword) => {
            let mut ok = perm_hash
                .as_ref()
                .is_some_and(|ph| verify_password_proof(&ph.derived_key, &nonce, &resp.proof));
            // Also accept the active OTP if both password methods were offered (single password prompt UX).
            if !ok
                && let Some(otp) = active_otp.as_ref()
                && verify_otp_proof(otp, &salt, argon2_params, &nonce, &resp.proof).await?
            {
                if let Some(next_otp) = state.consume_and_rotate_otp(otp) {
                    tracing::info!(session = sid, otp = %next_otp, "one-time password consumed and rotated");
                    ok = true;
                } else {
                    return Err(reject_auth(writer, ch, "one-time password already used").await);
                }
            }
            if !ok {
                return Err(reject_auth(writer, ch, "invalid password").await);
            }
        }
        Ok(AuthMethod::OneTimePassword) => {
            let Some(otp) = active_otp else {
                return Err(reject_auth(writer, ch, "one-time password is not active").await);
            };
            if !verify_otp_proof(&otp, &salt, argon2_params, &nonce, &resp.proof).await? {
                return Err(reject_auth(writer, ch, "invalid one-time password").await);
            }
            match state.consume_and_rotate_otp(&otp) {
                Some(next_otp) => {
                    tracing::info!(session = sid, otp = %next_otp, "one-time password consumed and rotated");
                }
                None => {
                    return Err(reject_auth(writer, ch, "one-time password already used").await);
                }
            }
        }
        Ok(AuthMethod::None) => {
            // Permitted because allow_method_none was checked when building `methods`.
        }
        Err(_) => {
            return Err(reject_auth(writer, ch, "unknown authentication method").await);
        }
    }

    // 6. Determine granted permissions (and run interactive approval if required).
    let perms = if let Some(tp) = authed_as_trusted {
        let mut p = tp.permissions;
        p.input &= cfg.allow_input;
        if !p.view {
            return Err(reject_auth(writer, ch, "trusted peer does not have view permission").await);
        }
        p
    } else {
        let default_permissions = default_session_permissions(cfg);
        if require_accept {
            let Some(handler) = &cfg.auth.approval_handler else {
                return Err(
                    reject_auth(writer, ch, "host requires approval but no approval handler is configured").await,
                );
            };
            let req = ApprovalRequest {
                peer_device_id: peer_device_id.clone(),
                peer_public_key: peer_pk,
                peer_fingerprint: peer_pk.as_ref().map(fingerprint).unwrap_or_default(),
                peer_name: hello.device_name.clone(),
                peer_os: hello.os.clone(),
                requested_mode: SessionMode::try_from(hello.mode).unwrap_or(SessionMode::Desktop),
                default_permissions,
            };
            let decision = match timeout(APPROVAL_TIMEOUT, (handler)(req)).await {
                Ok(d) => d,
                Err(_) => return Err(reject_auth(writer, ch, "host approval timed out").await),
            };
            if !decision.accepted {
                let reason = if decision.reason.is_empty() {
                    "connection rejected by host user".to_string()
                } else {
                    decision.reason
                };
                return Err(reject_auth(writer, ch, reason).await);
            }
            let mut p = decision.permissions;
            p.input &= cfg.allow_input;
            if !p.view {
                return Err(reject_auth(writer, ch, "view permission denied by host").await);
            }
            if decision.remember_peer
                && let Some(pk) = peer_pk
            {
                state.add_trusted_peer(TrustedPeer {
                    public_key: pk,
                    name: hello.device_name.clone(),
                    permissions: p.clone(),
                });
            }
            p
        } else {
            default_permissions
        }
    };

    writer
        .send(
            &Msg::AuthResult(AuthResult {
                ok: true,
                reason: String::new(),
                permissions: Some(perms.clone()),
                session_id: sid,
            })
            .into(),
        )
        .await?;

    Ok(AuthOutcome { peer_device_id, peer_public_key: peer_pk, permissions: perms })
}

async fn verify_otp_proof(
    otp: &str,
    salt: &[u8],
    params: Argon2Params,
    nonce: &[u8; 32],
    proof: &[u8],
) -> Result<bool> {
    let otp = otp.to_string();
    let salt = salt.to_vec();
    let key = tokio::task::spawn_blocking(move || derive_password_key(&otp, &salt, params))
        .await
        .map_err(|e| CoreError::platform(format!("argon2 task failed: {e}")))?
        .map_err(CoreError::from)?;
    Ok(verify_password_proof(&key, nonce, proof))
}

/// Parse a 64-char hex Ed25519 public key (used by TOML config files).
pub fn parse_hex_pubkey(s: &str) -> anyhow::Result<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        anyhow::bail!("public key hex must be 64 characters, got {}", s.len());
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|_| anyhow::anyhow!("invalid hex in public key"))?;
    }
    Ok(out)
}

/// Lower-case hex encoding of a 32-byte Ed25519 public key.
pub fn encode_hex_pubkey(pk: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for b in pk {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otp_consumes_and_rotates_once() {
        let cfg = HostAuthConfig::with_otp("INIT1234");
        let st = HostAuthState::from_config(&cfg);
        assert_eq!(st.current_otp().as_deref(), Some("INIT1234"));
        let next = st.consume_and_rotate_otp("INIT1234").expect("first consume succeeds");
        assert_ne!(next, "INIT1234");
        assert_eq!(next.len(), DEFAULT_OTP_LEN);
        assert_eq!(st.current_otp().as_deref(), Some(next.as_str()));
        // Replay fails.
        assert!(st.consume_and_rotate_otp("INIT1234").is_none());
    }

    #[test]
    fn hex_pubkey_roundtrip() {
        let pk = [0xABu8; 32];
        let hex = encode_hex_pubkey(&pk);
        assert_eq!(hex.len(), 64);
        assert_eq!(parse_hex_pubkey(&hex).unwrap(), pk);
        assert!(parse_hex_pubkey("deadbeef").is_err());
    }

    #[test]
    fn live_mutations_otp_permanent_password_and_trusted_peers() {
        let cfg = HostAuthConfig::with_otp("INIT1234");
        let st = HostAuthState::from_config(&cfg);

        let rotated: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let rotated_c = rotated.clone();
        st.set_on_otp_rotated(Some(Arc::new(move |otp| rotated_c.lock().unwrap().push(otp))));

        let added_peers: Arc<Mutex<Vec<TrustedPeer>>> = Arc::new(Mutex::new(Vec::new()));
        let added_c = added_peers.clone();
        st.set_on_trusted_peer_added(Some(Arc::new(move |p| added_c.lock().unwrap().push(p))));

        // set_otp_enabled(true) when already Some returns existing without rotating or firing callback.
        assert_eq!(st.set_otp_enabled(true).as_deref(), Some("INIT1234"));
        assert!(rotated.lock().unwrap().is_empty());

        // set_otp_enabled(false) clears OTP.
        assert_eq!(st.set_otp_enabled(false), None);
        assert_eq!(st.current_otp(), None);

        // set_otp_enabled(true) when None generates a fresh OTP and fires callback.
        let fresh = st.set_otp_enabled(true).expect("fresh OTP generated");
        assert_eq!(fresh.len(), DEFAULT_OTP_LEN);
        assert_eq!(st.current_otp().as_deref(), Some(fresh.as_str()));
        assert_eq!(rotated.lock().unwrap().as_slice(), &[fresh]);

        // set_permanent_password_hash(Some/None) updates live.
        let ph = PasswordHash::hash_with(
            "perm-secret",
            Argon2Params { m_cost_kib: 1024, t_cost: 1, p_cost: 1 },
        )
        .unwrap();
        st.set_permanent_password_hash(Some(ph.clone()));
        assert_eq!(st.permanent_password_hash(), Some(ph));
        st.set_permanent_password_hash(None);
        assert_eq!(st.permanent_password_hash(), None);

        // add_trusted_peer fires callback; remove_trusted_peer removes and returns bool.
        let pk = [0x55u8; 32];
        let peer = TrustedPeer::new(pk, "work-laptop");
        st.add_trusted_peer(peer.clone());
        assert_eq!(added_peers.lock().unwrap().as_slice(), &[peer]);
        assert!(st.find_trusted(&pk).is_some());
        assert!(st.remove_trusted_peer(&pk));
        assert!(st.find_trusted(&pk).is_none());
        assert!(!st.remove_trusted_peer(&pk));
    }
}
