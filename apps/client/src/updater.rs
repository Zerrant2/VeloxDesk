//! Self-update engine backed by GitHub Releases (`Zerrant2/VeloxDesk`, R7).
//!
//! Pure-Rust HTTPS via `ureq` v3 (`rustls` + `gzip`), strict GitHub host allowlist on both
//! initial URLs and every redirect hop, optional SHA-256 digest & Windows PE (`MZ`) verification,
//! and atomic self-replacement with automatic rollback on rename failure.

use std::cmp::Ordering;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[allow(dead_code)]
pub const GITHUB_REPO: &str = "Zerrant2/VeloxDesk";
#[allow(dead_code)]
pub const RELEASES_LATEST_URL: &str = "https://api.github.com/repos/Zerrant2/VeloxDesk/releases/latest";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const API_GLOBAL_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_RECV_BODY_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_GLOBAL_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

const ALLOWED_GITHUB_HOSTS: &[&str] = &[
    "github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// Raw release tag (e.g. `"v0.3.0"`).
    pub tag_name: String,
    /// Normalized semver without leading `'v'` (e.g. `"0.3.0"`).
    pub version: String,
    /// Release title (falls back to `tag_name`).
    pub name: String,
    /// Release notes markdown.
    pub body: String,
    /// Web URL of the release page.
    pub html_url: String,
    /// Selected binary asset filename (e.g. `"veloxdesk.exe"`).
    pub asset_name: String,
    /// Validated HTTPS asset download URL on an allowed GitHub host.
    pub download_url: String,
    /// Reported asset size in bytes.
    pub asset_size: u64,
    /// Optional lowercase hex SHA-256 parsed from GitHub asset `digest: "sha256:..."`.
    pub sha256: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckOutcome {
    UpToDate { current: String, latest: String },
    Available(ReleaseInfo),
}

// ───────────────────────────── SemVer ─────────────────────────────

/// Parse a SemVer string (`[v|V]MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]`).
#[allow(dead_code)]
pub fn parse_semver(s: &str) -> Option<(u64, u64, u64, Option<String>)> {
    let trimmed = s.trim();
    let without_v = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    // Build metadata (+...) does not affect version precedence.
    let without_build = without_v.split('+').next()?;
    let (core, pre) = match without_build.split_once('-') {
        Some((c, p)) if !p.is_empty() => (c, Some(p.to_string())),
        Some(_) => return None, // trailing '-' with empty prerelease
        None => (without_build, None),
    };
    let mut parts = core.split('.');
    let major = parse_uint(parts.next()?)?;
    let minor = parse_uint(parts.next()?)?;
    let patch = parse_uint(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch, pre))
}

fn parse_uint(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn cmp_prerelease(a: &str, b: &str) -> Ordering {
    let mut ia = a.split('.');
    let mut ib = b.split('.');
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(pa), Some(pb)) => {
                let ord = match (parse_uint(pa), parse_uint(pb)) {
                    (Some(na), Some(nb)) => na.cmp(&nb),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => pa.cmp(pb),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// Returns `true` iff `candidate` is strictly newer than `current` per SemVer 2.0.0.
#[allow(dead_code)]
pub fn is_newer_version(current: &str, candidate: &str) -> bool {
    let (Some(cur), Some(cand)) = (parse_semver(current), parse_semver(candidate)) else {
        return false;
    };
    let core_ord = (cand.0, cand.1, cand.2).cmp(&(cur.0, cur.1, cur.2));
    if core_ord != Ordering::Equal {
        return core_ord == Ordering::Greater;
    }
    match (&cur.3, &cand.3) {
        (None, None) => false,
        (Some(_), None) => true,  // prerelease < stable
        (None, Some(_)) => false, // stable > prerelease
        (Some(cp), Some(np)) => cmp_prerelease(np, cp) == Ordering::Greater,
    }
}

// ───────────────────────────── URL Security ─────────────────────────────

/// Strictly validate that `url` is `https://` on an exact allowed GitHub release host
/// and contains no whitespace or control characters anywhere in the URL.
#[allow(dead_code)]
pub fn is_allowed_download_url(url: &str) -> bool {
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let Some((host, port)) = parse_authority(rest) else {
        return false;
    };
    if let Some(p) = port
        && p != 443
    {
        return false;
    }
    let host_lower = host.to_ascii_lowercase();
    ALLOWED_GITHUB_HOSTS.contains(&host_lower.as_str())
}

fn is_allowed_url(url: &str, allow_localhost_http: bool) -> bool {
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    if is_allowed_download_url(url) {
        return true;
    }
    if !allow_localhost_http {
        return false;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let Some((host, _port)) = parse_authority(rest) else {
        return false;
    };
    matches!(host.to_ascii_lowercase().as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

/// Extract `(host, Option<port>)` from the authority portion after `scheme://`.
/// Rejects userinfo (`@`), empty hosts, whitespace/control chars, and invalid ports.
fn parse_authority(after_scheme: &str) -> Option<(&str, Option<u16>)> {
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if authority.is_empty()
        || authority.contains('@')
        || authority.bytes().any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return None;
    }
    if authority.starts_with('[') {
        // IPv6 literal: `[::1]` or `[::1]:8080`
        let end = authority.find(']')?;
        let host = &authority[..=end];
        let tail = &authority[end + 1..];
        if tail.is_empty() {
            Some((host, None))
        } else if let Some(p) = tail.strip_prefix(':') {
            let port: u16 = p.parse().ok()?;
            Some((host, Some(port)))
        } else {
            None
        }
    } else if let Some((host, p)) = authority.rsplit_once(':') {
        if host.is_empty() || host.contains(':') {
            return None;
        }
        let port: u16 = p.parse().ok()?;
        Some((host, Some(port)))
    } else {
        Some((authority, None))
    }
}

fn resolve_redirect_url(base_url: &str, location: &str) -> Result<String> {
    let location = location.trim();
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.to_string());
    }
    if location.starts_with("//") {
        return Ok(format!("https:{location}"));
    }
    if location.starts_with('/') {
        let scheme_end = base_url
            .find("://")
            .ok_or_else(|| anyhow!("invalid base URL for redirect"))?;
        let after_scheme = &base_url[scheme_end + 3..];
        let auth_end = after_scheme
            .find(['/', '?', '#'])
            .unwrap_or(after_scheme.len());
        let origin = &base_url[..scheme_end + 3 + auth_end];
        return Ok(format!("{origin}{location}"));
    }
    bail!("unsupported relative redirect location: {location}")
}

// ───────────────────────────── GitHub API & HTTP ─────────────────────────────

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

fn user_agent() -> String {
    format!("VeloxDesk/{}", env!("CARGO_PKG_VERSION"))
}

fn build_api_agent() -> ureq::Agent {
    let cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(API_GLOBAL_TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(cfg)
}

fn build_download_agent() -> ureq::Agent {
    let cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_body(Some(DOWNLOAD_RECV_BODY_TIMEOUT))
        .timeout_global(Some(DOWNLOAD_GLOBAL_TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(cfg)
}

enum FetchOutcome {
    Ok(ureq::http::Response<ureq::Body>),
    NotFound,
}

fn get_with_validated_redirects(
    agent: &ureq::Agent,
    initial_url: &str,
    accept: &str,
    allow_localhost_http: bool,
) -> Result<FetchOutcome> {
    let mut url = initial_url.to_string();
    let ua = user_agent();

    for hop in 0..=MAX_REDIRECTS {
        if !is_allowed_url(&url, allow_localhost_http) {
            bail!("refusing disallowed update URL: {url}");
        }
        let resp = agent
            .get(&url)
            .header("User-Agent", &ua)
            .header("Accept", accept)
            .call()
            .with_context(|| format!("HTTP GET {url} failed"))?;

        let status = resp.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if hop == MAX_REDIRECTS {
                bail!("too many redirects (>{MAX_REDIRECTS}) while fetching {initial_url}");
            }
            let loc = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect response {status} missing Location header"))?;
            url = resolve_redirect_url(&url, loc)?;
            continue;
        }
        if status == 404 {
            return Ok(FetchOutcome::NotFound);
        }
        if status != 200 {
            bail!("unexpected HTTP status {status} from {url}");
        }
        return Ok(FetchOutcome::Ok(resp));
    }
    bail!("redirect loop exceeded for {initial_url}")
}

fn select_platform_asset(assets: &[GhAsset]) -> Option<&GhAsset> {
    #[cfg(windows)]
    {
        if let Some(a) = assets.iter().find(|a| a.name.eq_ignore_ascii_case("veloxdesk.exe")) {
            return Some(a);
        }
        assets.iter().find(|a| {
            let lower = a.name.to_ascii_lowercase();
            lower.starts_with("veloxdesk") && lower.ends_with(".exe") && !lower.contains("server")
        })
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(a) = assets.iter().find(|a| a.name == "veloxdesk") {
            return Some(a);
        }
        assets.iter().find(|a| {
            let lower = a.name.to_ascii_lowercase();
            lower.starts_with("veloxdesk")
                && lower.contains("linux")
                && !lower.contains("server")
                && !lower.ends_with(".sha256")
                && !lower.ends_with(".sig")
        })
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(a) = assets.iter().find(|a| a.name == "veloxdesk") {
            return Some(a);
        }
        assets.iter().find(|a| {
            let lower = a.name.to_ascii_lowercase();
            lower.starts_with("veloxdesk")
                && (lower.contains("macos") || lower.contains("darwin"))
                && !lower.contains("server")
                && !lower.ends_with(".sha256")
                && !lower.ends_with(".sig")
        })
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        assets.first()
    }
}

fn parse_sha256_digest(digest: Option<&str>) -> Option<String> {
    let raw = digest?.trim();
    let hex = raw
        .strip_prefix("sha256:")
        .or_else(|| raw.strip_prefix("SHA256:"))?;
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

fn normalize_version(v: &str) -> String {
    v.trim()
        .strip_prefix('v')
        .or_else(|| v.trim().strip_prefix('V'))
        .unwrap_or(v.trim())
        .to_string()
}

/// Query GitHub Releases `/latest` for `Zerrant2/VeloxDesk` and compare against `current_version`.
#[allow(dead_code)]
pub fn check_for_update(current_version: &str) -> Result<UpdateCheckOutcome> {
    check_for_update_from_url(RELEASES_LATEST_URL, current_version, false)
}

/// Query a GitHub Releases JSON endpoint and compare against `current_version`.
///
/// `allow_localhost_http` must only be `true` in unit/integration tests against a local mock server.
#[allow(dead_code)]
pub fn check_for_update_from_url(
    api_url: &str,
    current_version: &str,
    allow_localhost_http: bool,
) -> Result<UpdateCheckOutcome> {
    let current_norm = normalize_version(current_version);
    let agent = build_api_agent();
    let mut resp = match get_with_validated_redirects(
        &agent,
        api_url,
        "application/vnd.github+json",
        allow_localhost_http,
    )? {
        FetchOutcome::Ok(r) => r,
        FetchOutcome::NotFound => {
            // Repository has no published releases yet — treat cleanly as up to date.
            return Ok(UpdateCheckOutcome::UpToDate {
                current: current_norm.clone(),
                latest: current_norm,
            });
        }
    };
    let body = resp.body_mut().read_to_string().context("read release API response")?;
    let gh: GhRelease = serde_json::from_str(&body).context("parse GitHub release JSON")?;

    let latest_norm = normalize_version(&gh.tag_name);

    if gh.draft || gh.prerelease || !is_newer_version(current_version, &gh.tag_name) {
        return Ok(UpdateCheckOutcome::UpToDate {
            current: current_norm,
            latest: latest_norm,
        });
    }

    let asset = select_platform_asset(&gh.assets)
        .ok_or_else(|| anyhow!("release {} has no compatible binary asset for this platform", gh.tag_name))?;

    if !is_allowed_url(&asset.browser_download_url, allow_localhost_http) {
        bail!("release asset download URL is not on an allowed GitHub host: {}", asset.browser_download_url);
    }

    let html_url = if is_allowed_url(&gh.html_url, allow_localhost_http) {
        gh.html_url
    } else {
        format!("https://github.com/{GITHUB_REPO}/releases/tag/{}", gh.tag_name)
    };

    let name = gh
        .name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| gh.tag_name.clone());

    Ok(UpdateCheckOutcome::Available(ReleaseInfo {
        tag_name: gh.tag_name,
        version: latest_norm,
        name,
        body: gh.body.unwrap_or_default(),
        html_url,
        asset_name: asset.name.clone(),
        download_url: asset.browser_download_url.clone(),
        asset_size: asset.size,
        sha256: parse_sha256_digest(asset.digest.as_deref()),
    }))
}

// ───────────────────────────── Download, Verify & Atomic Replace ─────────────────────────────

fn sibling_with_suffix(exe: &Path, suffix: &str) -> Result<PathBuf> {
    let name = exe
        .file_name()
        .ok_or_else(|| anyhow!("executable path has no filename: {}", exe.display()))?;
    let mut os = name.to_os_string();
    os.push(suffix);
    Ok(exe.with_file_name(os))
}

struct RemoveOnDrop {
    path: Option<PathBuf>,
}

impl RemoveOnDrop {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }
    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            let _ = fs::remove_file(p);
        }
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    s
}

/// Best-effort cleanup of stale `.old` / `.new` files left from a prior self-update.
///
/// On Windows, the exiting parent process may still hold an image lock on `<exe>.old`
/// for a few milliseconds after spawning the updated process; if removal returns
/// `PermissionDenied`, a small background thread retries up to 10 × 250 ms.
#[allow(dead_code)]
pub fn cleanup_old_executable() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    cleanup_stale_files_for(&exe);
}

fn cleanup_stale_files_for(exe: &Path) {
    if let Ok(new) = sibling_with_suffix(exe, ".new") {
        let _ = fs::remove_file(new);
    }
    if let Ok(old) = sibling_with_suffix(exe, ".old") {
        match fs::remove_file(&old) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                let _ = std::thread::Builder::new()
                    .name("vdk-cleanup-old".into())
                    .spawn(move || {
                        for _ in 0..10 {
                            std::thread::sleep(Duration::from_millis(250));
                            match fs::remove_file(&old) {
                                Ok(()) => break,
                                Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
                                Err(_) => {}
                            }
                        }
                    });
            }
            Err(_) => {}
        }
    }
}

/// Stream-download `release` to `<dir>/<target_exe_name>.new`, verifying size limits,
/// non-empty content, Windows PE (`MZ`) magic, and SHA-256 digest (if present).
#[allow(dead_code)]
pub fn download_and_stage(
    release: &ReleaseInfo,
    target_exe: &Path,
    allow_localhost_http: bool,
    mut on_progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    if !is_allowed_url(&release.download_url, allow_localhost_http) {
        bail!("disallowed download URL: {}", release.download_url);
    }
    if release.asset_size > MAX_DOWNLOAD_BYTES {
        bail!(
            "release asset size {} exceeds maximum allowed {} bytes",
            release.asset_size,
            MAX_DOWNLOAD_BYTES
        );
    }

    let staged_path = sibling_with_suffix(target_exe, ".new")?;
    let _ = fs::remove_file(&staged_path);

    let agent = build_download_agent();
    let mut resp = match get_with_validated_redirects(
        &agent,
        &release.download_url,
        "application/octet-stream",
        allow_localhost_http,
    )? {
        FetchOutcome::Ok(r) => r,
        FetchOutcome::NotFound => bail!("HTTP 404 Not Found downloading {}", release.download_url),
    };

    let total_hint = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .or((release.asset_size > 0).then_some(release.asset_size));

    if let Some(total) = total_hint
        && total > MAX_DOWNLOAD_BYTES
    {
        bail!("download Content-Length {total} exceeds maximum allowed {MAX_DOWNLOAD_BYTES} bytes");
    }

    let mut guard = RemoveOnDrop::new(staged_path.clone());
    let mut file = File::create(&staged_path)
        .with_context(|| format!("create staged update file {}", staged_path.display()))?;

    let mut reader = resp.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded = 0u64;
    let mut magic = [0u8; 2];
    let mut magic_len = 0usize;

    on_progress(0, total_hint);

    loop {
        let n = reader.read(&mut buf).context("read update download stream")?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        downloaded = downloaded
            .checked_add(n as u64)
            .ok_or_else(|| anyhow!("download size overflow"))?;
        if downloaded > MAX_DOWNLOAD_BYTES {
            bail!("download exceeded maximum allowed {MAX_DOWNLOAD_BYTES} bytes");
        }
        if magic_len < 2 {
            let take = (2 - magic_len).min(chunk.len());
            magic[magic_len..magic_len + take].copy_from_slice(&chunk[..take]);
            magic_len += take;
        }
        hasher.update(chunk);
        file.write_all(chunk).context("write staged update chunk")?;
        on_progress(downloaded, total_hint);
    }

    file.flush().context("flush staged update file")?;
    file.sync_all().context("sync staged update file")?;
    drop(file);

    if downloaded == 0 {
        bail!("downloaded update binary is empty");
    }

    #[cfg(windows)]
    if magic_len < 2 || &magic != b"MZ" {
        bail!("downloaded Windows update binary does not start with valid PE 'MZ' header");
    }

    if let Some(expected_sha) = &release.sha256 {
        let actual_sha = hex_lower(&hasher.finalize());
        if actual_sha != expected_sha.to_ascii_lowercase() {
            bail!("update SHA-256 mismatch: expected {expected_sha}, got {actual_sha}");
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&staged_path, perms).context("set executable permission on staged binary")?;
    }

    guard.disarm();
    Ok(staged_path)
}

/// Atomically replace `current_exe` with `staged_exe`, preserving a `.old` backup during the
/// swap and rolling back if the second rename fails.
#[allow(dead_code)]
pub fn replace_executable_at(current_exe: &Path, staged_exe: &Path) -> Result<()> {
    let old_exe = sibling_with_suffix(current_exe, ".old")?;
    match fs::remove_file(&old_exe) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("remove stale backup {}", old_exe.display())),
    }

    fs::rename(current_exe, &old_exe).with_context(|| {
        format!(
            "rename running executable {} -> {}",
            current_exe.display(),
            old_exe.display()
        )
    })?;

    if let Err(err) = fs::rename(staged_exe, current_exe) {
        let rollback = fs::rename(&old_exe, current_exe);
        match rollback {
            Ok(()) => bail!(
                "failed to move staged update {} -> {} (rolled back original): {err}",
                staged_exe.display(),
                current_exe.display()
            ),
            Err(rb_err) => bail!(
                "failed to move staged update {} -> {}: {err}; rollback from {} also failed: {rb_err}",
                staged_exe.display(),
                current_exe.display(),
                old_exe.display()
            ),
        }
    }

    Ok(())
}

/// Download, verify, and atomically install `release` over the running executable without
/// spawning the new process yet. Returns the installed executable path so the caller can
/// cleanly release sockets, IPC pipes, and tray icons before calling [`spawn_relaunched_executable`].
#[allow(dead_code)]
pub fn install_update(
    release: &ReleaseInfo,
    on_progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    let current_exe = std::env::current_exe().context("resolve current executable path")?;
    let staged_exe = download_and_stage(release, &current_exe, false, on_progress)?;
    replace_executable_at(&current_exe, &staged_exe)?;
    Ok(current_exe)
}

/// Launch `exe` detached with the current process's arguments (`args_os().skip(1)`).
#[allow(dead_code)]
pub fn spawn_relaunched_executable(exe: &Path) -> Result<()> {
    let mut cmd = Command::new(exe);
    cmd.args(std::env::args_os().skip(1));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
        .with_context(|| format!("relaunch updated executable {}", exe.display()))?;
    Ok(())
}

/// Download, verify, atomically install `release` over the running executable, and launch
/// the updated executable detached so the caller can exit cleanly.
#[allow(dead_code)]
pub fn apply_update_and_relaunch(
    release: &ReleaseInfo,
    on_progress: impl FnMut(u64, Option<u64>),
) -> Result<()> {
    let current_exe = install_update(release, on_progress)?;
    spawn_relaunched_executable(&current_exe)
}

// ───────────────────────────── Unit Tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parse_semver_and_is_newer_version() {
        assert_eq!(parse_semver("0.2.0"), Some((0, 2, 0, None)));
        assert_eq!(parse_semver("v0.10.3"), Some((0, 10, 3, None)));
        assert_eq!(parse_semver("V1.2.3-rc.1+abc123"), Some((1, 2, 3, Some("rc.1".into()))));
        assert_eq!(parse_semver("1.2"), None);
        assert_eq!(parse_semver("1.2.3-"), None);
        assert_eq!(parse_semver("abc"), None);

        // Multi-digit numeric comparison
        assert!(is_newer_version("0.2.0", "0.10.0"));
        assert!(is_newer_version("v0.2.0", "v0.2.1"));
        assert!(is_newer_version("0.9.9", "1.0.0"));

        // Equal & downgrades rejected
        assert!(!is_newer_version("0.2.0", "0.2.0"));
        assert!(!is_newer_version("v0.2.0", "0.2.0"));
        assert!(!is_newer_version("0.10.0", "0.2.0"));
        assert!(!is_newer_version("1.0.0", "0.9.9"));

        // Prerelease vs stable
        assert!(is_newer_version("0.2.0-rc1", "0.2.0"));
        assert!(!is_newer_version("0.2.0", "0.2.0-rc1"));
        assert!(is_newer_version("0.2.0-alpha.2", "0.2.0-alpha.10"));
        assert!(!is_newer_version("0.2.0-alpha.10", "0.2.0-alpha.2"));
    }

    #[test]
    fn allowed_download_url_enforces_strict_https_github_hosts() {
        // Valid hosts
        assert!(is_allowed_download_url(
            "https://github.com/Zerrant2/VeloxDesk/releases/download/v0.3.0/veloxdesk.exe"
        ));
        assert!(is_allowed_download_url(
            "https://api.github.com/repos/Zerrant2/VeloxDesk/releases/latest"
        ));
        assert!(is_allowed_download_url(
            "https://objects.githubusercontent.com/github-production-release-asset/123"
        ));
        assert!(is_allowed_download_url(
            "https://release-assets.githubusercontent.com/some/path"
        ));
        assert!(is_allowed_download_url(
            "https://github-releases.githubusercontent.com/some/path"
        ));

        // Valid presigned S3/GitHub URL with `&` query params is allowed
        assert!(is_allowed_download_url(
            "https://objects.githubusercontent.com/asset?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Signature=abc123"
        ));

        // Rejected schemes, spoofing, userinfo, non-443 ports, whitespace, and control chars
        assert!(!is_allowed_download_url("http://github.com/Zerrant2/VeloxDesk/releases/download/v0.3.0/veloxdesk.exe"));
        assert!(!is_allowed_download_url("file:///C:/Windows/System32/cmd.exe"));
        assert!(!is_allowed_download_url("https://evil-github.com/veloxdesk.exe"));
        assert!(!is_allowed_download_url("https://evilgithub.com/veloxdesk.exe"));
        assert!(!is_allowed_download_url("https://github.com.evil.org/veloxdesk.exe"));
        assert!(!is_allowed_download_url("https://github.com@evil.org/veloxdesk.exe"));
        assert!(!is_allowed_download_url("https://github.com:8443/veloxdesk.exe"));
        assert!(!is_allowed_download_url("https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.2.0 & calc"));
        assert!(!is_allowed_download_url("https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.2.0\r\nX-Inject: 1"));
        assert!(!is_allowed_download_url("https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.2.0\x00"));
        assert!(!is_allowed_url("http://127.0.0.1:8080/a b", true));
        assert!(!is_allowed_url("http://127.0.0.1:8080/a\x1bb", true));
    }

    #[test]
    fn resolve_redirect_handles_protocol_relative_and_query_bases() {
        assert_eq!(
            resolve_redirect_url("https://github.com/a/b", "  //objects.githubusercontent.com/x?a=1&b=2  ").unwrap(),
            "https://objects.githubusercontent.com/x?a=1&b=2"
        );
        assert_eq!(
            resolve_redirect_url("https://github.com?foo=bar", "/releases/download/v0.3.0/veloxdesk.exe").unwrap(),
            "https://github.com/releases/download/v0.3.0/veloxdesk.exe"
        );
        assert_eq!(
            resolve_redirect_url("https://github.com#frag", "/a").unwrap(),
            "https://github.com/a"
        );
    }

    #[test]
    fn http_404_latest_release_treated_as_up_to_date() {
        let (base, handle) = spawn_mock_http_server(vec![MockRoute {
            path: "/latest".into(),
            status: 404,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: br#"{"message":"Not Found"}"#.to_vec(),
        }]);
        let outcome = check_for_update_from_url(&format!("{base}/latest"), "0.2.0", true).unwrap();
        assert_eq!(
            outcome,
            UpdateCheckOutcome::UpToDate { current: "0.2.0".into(), latest: "0.2.0".into() }
        );
        handle.join().unwrap();
    }

    fn temp_test_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("vdx-updater-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct MockRoute {
        path: String,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    fn spawn_mock_http_server(routes: Vec<MockRoute>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let expected_reqs = routes.len();

        let handle = thread::spawn(move || {
            for _ in 0..expected_reqs {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut req_line = String::new();
                if reader.read_line(&mut req_line).is_err() {
                    continue;
                }
                // Drain headers
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                let path = req_line.split_whitespace().nth(1).unwrap_or("/");
                if let Some(route) = routes.iter().find(|r| r.path == path) {
                    let reason = match route.status {
                        200 => "OK",
                        302 => "Found",
                        _ => "Error",
                    };
                    let mut resp = format!(
                        "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                        route.status,
                        route.body.len()
                    );
                    for (k, v) in &route.headers {
                        resp.push_str(&format!("{k}: {v}\r\n"));
                    }
                    resp.push_str("\r\n");
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(&route.body);
                    let _ = stream.flush();
                } else {
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
            }
        });

        (base, handle)
    }

    #[test]
    fn mock_http_check_download_stage_replace_and_rollback() {
        // Fake new binary payload starting with valid PE 'MZ' header.
        let mut fake_bin = b"MZ\x90\x00VELOXDESK_NEW_BINARY_PAYLOAD".to_vec();
        fake_bin.extend_from_slice(&[0x5Au8; 4096]);
        let expected_sha = hex_lower(&Sha256::digest(&fake_bin));

        let asset_name = if cfg!(windows) { "veloxdesk.exe" } else { "veloxdesk" };

        // 1. Test UpToDate check
        let up_to_date_json = r#"{"tag_name":"v0.2.0","name":"VeloxDesk v0.2.0","body":"Current","html_url":"https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.2.0","draft":false,"prerelease":false,"assets":[]}"#;
        let (base1, h1) = spawn_mock_http_server(vec![MockRoute {
            path: "/latest".into(),
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: up_to_date_json.as_bytes().to_vec(),
        }]);
        let outcome1 = check_for_update_from_url(&format!("{base1}/latest"), "0.2.0", true).unwrap();
        assert_eq!(
            outcome1,
            UpdateCheckOutcome::UpToDate { current: "0.2.0".into(), latest: "0.2.0".into() }
        );
        h1.join().unwrap();

        // 2. Test Available + 302 redirect + SHA-256 verification + atomic replace
        let (base2, h2) = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let base = format!("http://{addr}");
            let available_json = format!(
                r#"{{
                    "tag_name": "v0.3.0",
                    "name": "VeloxDesk v0.3.0",
                    "body": "Bug fixes and improvements",
                    "html_url": "https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.3.0",
                    "draft": false,
                    "prerelease": false,
                    "assets": [{{
                        "name": "{asset_name}",
                        "browser_download_url": "{base}/redirect",
                        "size": {},
                        "digest": "sha256:{expected_sha}"
                    }}]
                }}"#,
                fake_bin.len()
            );
            let bin_clone = fake_bin.clone();
            let redirect_target = format!("{base}/asset");
            let handle = thread::spawn(move || {
                // Expect 4 requests: /latest, /redirect -> /asset (good), /redirect -> /asset (bad sha test = 2 more -> 5 total)
                for _ in 0..5 {
                    let Ok((mut stream, _)) = listener.accept() else {
                        break;
                    };
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut req_line = String::new();
                    if reader.read_line(&mut req_line).is_err() {
                        continue;
                    }
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
                            break;
                        }
                    }
                    let path = req_line.split_whitespace().nth(1).unwrap_or("/");
                    match path {
                        "/latest" => {
                            let hdr = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                available_json.len()
                            );
                            let _ = stream.write_all(hdr.as_bytes());
                            let _ = stream.write_all(available_json.as_bytes());
                        }
                        "/redirect" => {
                            let hdr = format!(
                                "HTTP/1.1 302 Found\r\nLocation: {redirect_target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                            let _ = stream.write_all(hdr.as_bytes());
                        }
                        "/asset" => {
                            let hdr = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                bin_clone.len()
                            );
                            let _ = stream.write_all(hdr.as_bytes());
                            let _ = stream.write_all(&bin_clone);
                        }
                        _ => {
                            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                    }
                    let _ = stream.flush();
                }
            });
            (base, handle)
        };

        let outcome2 = check_for_update_from_url(&format!("{base2}/latest"), "0.2.0", true).unwrap();
        let release = match outcome2 {
            UpdateCheckOutcome::Available(r) => r,
            other => panic!("expected Available, got {other:?}"),
        };
        assert_eq!(release.version, "0.3.0");
        assert_eq!(release.asset_name, asset_name);
        assert_eq!(release.sha256.as_deref(), Some(expected_sha.as_str()));

        let dir = temp_test_dir("replace");
        let cur_exe = dir.join(asset_name);
        fs::write(&cur_exe, b"MZ_OLD_VERSION").unwrap();

        let mut progress_calls = 0usize;
        let mut last_done = 0u64;
        let staged = download_and_stage(&release, &cur_exe, true, |done, total| {
            progress_calls += 1;
            last_done = done;
            assert_eq!(total, Some(fake_bin.len() as u64));
        })
        .unwrap();
        assert!(progress_calls >= 2);
        assert_eq!(last_done, fake_bin.len() as u64);
        assert!(staged.exists());

        // Atomic swap
        replace_executable_at(&cur_exe, &staged).unwrap();
        assert_eq!(fs::read(&cur_exe).unwrap(), fake_bin);
        let old_backup = sibling_with_suffix(&cur_exe, ".old").unwrap();
        assert_eq!(fs::read(&old_backup).unwrap(), b"MZ_OLD_VERSION");
        assert!(!staged.exists());

        // Bad SHA-256 fails and cleans up .new
        let mut bad_release = release.clone();
        bad_release.sha256 = Some("0".repeat(64));
        assert!(download_and_stage(&bad_release, &cur_exe, true, |_, _| {}).is_err());
        assert!(!sibling_with_suffix(&cur_exe, ".new").unwrap().exists());

        // Rollback test: when staged file does not exist, original executable is restored!
        let missing_staged = dir.join("does-not-exist.new");
        assert!(replace_executable_at(&cur_exe, &missing_staged).is_err());
        assert_eq!(fs::read(&cur_exe).unwrap(), fake_bin);
        assert!(!old_backup.exists());

        // Cleanup removes stale .old and .new siblings
        let stale_old = sibling_with_suffix(&cur_exe, ".old").unwrap();
        let stale_new = sibling_with_suffix(&cur_exe, ".new").unwrap();
        fs::write(&stale_old, b"old").unwrap();
        fs::write(&stale_new, b"new").unwrap();
        cleanup_stale_files_for(&cur_exe);
        assert!(!stale_old.exists());
        assert!(!stale_new.exists());

        h2.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
