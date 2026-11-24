//! Built-in Windows Virtual Microphone Driver (`VB-CABLE`) 1-click background installer (v0.6.2).
//!
//! Downloads the official Microsoft WHQL-signed VB-Audio Virtual Cable driver archive over HTTPS
//! (`ureq` v3 + `rustls`), enforces a strict host allowlist on every redirect hop, verifies ZIP
//! and Windows PE (`MZ`) headers, extracts silently, and runs elevated installation via `ShellExecuteExW`.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use veloxdesk_render::winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

pub const VBCABLE_INDEX_URL: &str = "https://vb-audio.com/Cable/index.htm";

pub const FALLBACK_ZIP_URLS: &[&str] = &[
    "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip",
    "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip",
    "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack43.zip",
];

const ALLOWED_VBCABLE_HOSTS: &[&str] = &[
    "vb-audio.com",
    "www.vb-audio.com",
    "download.vb-audio.com",
];

pub const MIN_ZIP_BYTES: u64 = 64 * 1024;
pub const MAX_ZIP_BYTES: u64 = 32 * 1024 * 1024;
const INDEX_TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_RECV_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_GLOBAL_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_REDIRECTS: usize = 5;

static INSTALL_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Lifecycle events sent from the VB-CABLE installer worker thread to the UI loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtualMicEvent {
    Downloading,
    Extracting,
    WaitingForUac,
    Completed { device_name: Option<String> },
    Cancelled,
    Failed(String),
}

/// Strictly validate that `url` is `https://` on an exact allowed VB-Audio host (`:443` only)
/// with no userinfo (`@`), whitespace, or control characters anywhere.
pub fn is_allowed_vbcable_url(url: &str) -> bool {
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
    ALLOWED_VBCABLE_HOSTS.contains(&host_lower.as_str())
}

fn parse_authority(after_scheme: &str) -> Option<(&str, Option<u16>)> {
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty()
        || authority.contains('@')
        || authority.starts_with('[')
        || authority.bytes().any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return None;
    }
    if let Some((host, p)) = authority.rsplit_once(':') {
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
    if location.is_empty() || location.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("invalid redirect Location header: {location}");
    }
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.to_string());
    }
    if location.starts_with("//") {
        return Ok(format!("https:{location}"));
    }
    let scheme_end = base_url
        .find("://")
        .ok_or_else(|| anyhow!("invalid base URL for redirect"))?;
    let after_scheme = &base_url[scheme_end + 3..];
    let auth_end = after_scheme.find(['/', '?', '#']).unwrap_or(after_scheme.len());
    let origin = &base_url[..scheme_end + 3 + auth_end];
    if location.starts_with('/') {
        return Ok(format!("{origin}{location}"));
    }
    let path_part = &after_scheme[auth_end..];
    let clean_path = path_part.split(['?', '#']).next().unwrap_or("/");
    let dir = match clean_path.rfind('/') {
        Some(idx) => &clean_path[..=idx],
        None => "/",
    };
    Ok(format!("{origin}{dir}{location}"))
}

/// Check ZIP local file header magic (`PK\x03\x04`).
pub fn has_zip_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == b"PK\x03\x04"
}

/// Check Windows PE executable `MZ` magic header.
pub fn has_pe_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && &bytes[..2] == b"MZ"
}

/// Ordered candidate setup executable names inside `extracted` for target CPU `arch`.
pub fn preferred_setup_exe_names_for_arch(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86_64" => &["VBCABLE_Setup_x64.exe"],
        "aarch64" => &["VBCABLE_Setup_arm64.exe", "VBCABLE_Setup_x64.exe"],
        "x86" => &["VBCABLE_Setup.exe"],
        _ => &["VBCABLE_Setup_x64.exe", "VBCABLE_Setup.exe"],
    }
}

/// Extract the newest `VBCABLE_Driver_Pack*.zip` download URL from `vb-audio.com/Cable/index.htm` HTML.
pub fn extract_vbcable_zip_url(html: &str) -> Option<String> {
    let bytes = html.as_bytes();
    let mut best: Option<(u32, String)> = None;
    let mut i = 0;
    while i + 5 <= bytes.len() {
        if bytes[i..i + 4].eq_ignore_ascii_case(b"href")
            && (i == 0 || bytes[i - 1].is_ascii_whitespace() || bytes[i - 1] == b'<')
        {
            let mut j = i + 4;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let (val, next_i) = if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                    let quote = bytes[j];
                    let start = j + 1;
                    let mut end = start;
                    while end < bytes.len() && bytes[end] != quote {
                        end += 1;
                    }
                    (&html[start..end], (end + 1).min(bytes.len()))
                } else {
                    let start = j;
                    let mut end = start;
                    while end < bytes.len()
                        && !bytes[end].is_ascii_whitespace()
                        && !matches!(bytes[end], b'>' | b'"' | b'\'')
                    {
                        end += 1;
                    }
                    (&html[start..end], end)
                };
                if let Some((ver, url)) = normalize_vbcable_href(val)
                    && best.as_ref().is_none_or(|(best_ver, _)| ver > *best_ver)
                {
                    best = Some((ver, url));
                }
                i = next_i;
                continue;
            }
        }
        i += 1;
    }
    best.map(|(_, u)| u)
}

fn normalize_vbcable_href(raw: &str) -> Option<(u32, String)> {
    let s = raw.trim();
    if s.is_empty()
        || s.contains("..")
        || s.contains('\\')
        || s.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let rel_stripped = s.strip_prefix("./").unwrap_or(s);
    let file_part = if let Some(rest) = strip_prefix_ignore_case(s, "https://download.vb-audio.com/Download_CABLE/") {
        rest
    } else if let Some(rest) = strip_prefix_ignore_case(s, "http://download.vb-audio.com/Download_CABLE/") {
        rest
    } else if let Some(rest) = strip_prefix_ignore_case(s, "//download.vb-audio.com/Download_CABLE/") {
        rest
    } else if let Some(rest) = strip_prefix_ignore_case(rel_stripped, "Download_CABLE/") {
        rest
    } else {
        strip_prefix_ignore_case(rel_stripped, "/Download_CABLE/")?
    };

    if file_part.contains('/')
        || file_part.contains('?')
        || file_part.contains('#')
        || !file_part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return None;
    }
    let mid = strip_prefix_ignore_case(file_part, "VBCABLE_Driver_Pack")?;
    let ver_str = strip_suffix_ignore_case(mid, ".zip")?;
    let ver = ver_str
        .bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0u32, |acc, b| acc.saturating_mul(10).saturating_add((b - b'0') as u32));
    let url = format!("https://download.vb-audio.com/Download_CABLE/{file_part}");
    if !is_allowed_vbcable_url(&url) {
        return None;
    }
    Some((ver, url))
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn strip_suffix_ignore_case<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let split = s.len().checked_sub(suffix.len())?;
    if s.is_char_boundary(split) && s[split..].eq_ignore_ascii_case(suffix) {
        Some(&s[..split])
    } else {
        None
    }
}

fn user_agent() -> String {
    format!("VeloxDesk/{}", env!("CARGO_PKG_VERSION"))
}

fn build_index_agent() -> ureq::Agent {
    let cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(INDEX_TIMEOUT))
        .timeout_recv_response(Some(INDEX_TIMEOUT))
        .timeout_recv_body(Some(INDEX_TIMEOUT))
        .timeout_global(Some(INDEX_TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(cfg)
}

fn build_download_agent() -> ureq::Agent {
    let cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_body(Some(DOWNLOAD_RECV_TIMEOUT))
        .timeout_global(Some(DOWNLOAD_GLOBAL_TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(cfg)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn get_with_validated_redirects(
    agent: &ureq::Agent,
    initial_url: &str,
    accept: &str,
) -> Result<ureq::http::Response<ureq::Body>> {
    let mut url = initial_url.to_string();
    let ua = user_agent();

    for hop in 0..=MAX_REDIRECTS {
        if !is_allowed_vbcable_url(&url) {
            bail!("refusing disallowed VB-CABLE URL: {url}");
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
        if status != 200 {
            bail!("unexpected HTTP status {status} from {url}");
        }
        return Ok(resp);
    }
    bail!("redirect loop exceeded for {initial_url}")
}

#[cfg_attr(not(windows), allow(dead_code))]
fn discover_candidate_urls() -> Vec<String> {
    let mut urls = Vec::with_capacity(FALLBACK_ZIP_URLS.len() + 1);
    let agent = build_index_agent();
    match get_with_validated_redirects(&agent, VBCABLE_INDEX_URL, "text/html,*/*") {
        Ok(mut resp) => {
            let mut html = String::new();
            if resp.body_mut().as_reader().take(2 * 1024 * 1024).read_to_string(&mut html).is_ok()
                && let Some(discovered) = extract_vbcable_zip_url(&html)
            {
                urls.push(discovered);
            }
        }
        Err(e) => {
            tracing::debug!("VB-CABLE index discovery failed, falling back to pinned URLs: {e:#}");
        }
    }
    for &fb in FALLBACK_ZIP_URLS {
        if !urls.iter().any(|u| u.eq_ignore_ascii_case(fb)) {
            urls.push(fb.to_string());
        }
    }
    urls
}

#[cfg_attr(not(windows), allow(dead_code))]
fn download_zip_to_path(url: &str, zip_path: &Path) -> Result<()> {
    let agent = build_download_agent();
    let mut resp = get_with_validated_redirects(&agent, url, "application/zip,application/octet-stream,*/*")?;

    if let Some(total) = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        && !(MIN_ZIP_BYTES..=MAX_ZIP_BYTES).contains(&total)
    {
        bail!("VB-CABLE archive Content-Length {total} outside allowed 64 KiB..=32 MiB range");
    }

    let mut file = File::create(zip_path)
        .with_context(|| format!("create temporary archive {}", zip_path.display()))?;
    let mut reader = resp.body_mut().as_reader();
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded = 0u64;
    let mut magic = [0u8; 4];
    let mut magic_len = 0usize;

    loop {
        let n = reader.read(&mut buf).context("read VB-CABLE download stream")?;
        if n == 0 {
            break;
        }
        downloaded = downloaded.checked_add(n as u64).ok_or_else(|| anyhow!("download size overflow"))?;
        if downloaded > MAX_ZIP_BYTES {
            bail!("VB-CABLE download exceeded maximum allowed 32 MiB");
        }
        if magic_len < 4 {
            let take = (4 - magic_len).min(n);
            magic[magic_len..magic_len + take].copy_from_slice(&buf[..take]);
            magic_len += take;
        }
        file.write_all(&buf[..n]).context("write VB-CABLE archive chunk")?;
    }
    file.flush().context("flush VB-CABLE archive")?;
    drop(file);

    if downloaded < MIN_ZIP_BYTES {
        bail!("VB-CABLE archive too small ({downloaded} bytes < 64 KiB)");
    }
    if magic_len < 4 || !has_zip_magic(&magic) {
        bail!("downloaded VB-CABLE archive does not start with valid ZIP 'PK\\x03\\x04' header");
    }
    Ok(())
}

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct InstallResetGuard;

impl Drop for InstallResetGuard {
    fn drop(&mut self) {
        INSTALL_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

pub fn find_and_verify_setup_exe(extracted_dir: &Path, arch: &str) -> Result<PathBuf> {
    let candidates = preferred_setup_exe_names_for_arch(arch);
    for &name in candidates {
        if let Some(found) = find_file_case_insensitive(extracted_dir, name, 3) {
            let mut f = File::open(&found)
                .with_context(|| format!("open VB-CABLE installer {}", found.display()))?;
            let mut hdr = [0u8; 2];
            let n = f.read(&mut hdr).context("read VB-CABLE installer PE header")?;
            if n < 2 || !has_pe_magic(&hdr) {
                bail!("{} does not start with valid Windows PE 'MZ' header", found.display());
            }
            return Ok(found);
        }
    }
    bail!(
        "VB-CABLE setup executable ({}) not found inside {}",
        candidates.join(" / "),
        extracted_dir.display()
    )
}

fn find_file_case_insensitive(dir: &Path, target_name: &str, max_depth: usize) -> Option<PathBuf> {
    let direct = dir.join(target_name);
    if direct.is_file() {
        return Some(direct);
    }
    let entries = fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_file() {
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(target_name))
            {
                return Some(path);
            }
        } else if ft.is_dir() && max_depth > 0 {
            subdirs.push(path);
        }
    }
    for sub in subdirs {
        if let Some(found) = find_file_case_insensitive(&sub, target_name, max_depth - 1) {
            return Some(found);
        }
    }
    None
}

#[cfg(windows)]
fn extract_zip_windows(zip_path: &Path, extracted_dir: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    fs::create_dir_all(extracted_dir)
        .with_context(|| format!("create extraction directory {}", extracted_dir.display()))?;

    let tar_path = PathBuf::from(r"C:\Windows\System32\tar.exe");
    let tar_res = Command::new(&tar_path)
        .arg("-xf")
        .arg(zip_path)
        .arg("-C")
        .arg(extracted_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    if let Ok(out) = &tar_res
        && out.status.success()
    {
        return Ok(());
    }

    let ps_sys = PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
    let ps_bin: &Path = if ps_sys.is_file() { &ps_sys } else { Path::new("powershell.exe") };
    let out = Command::new(ps_bin)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            "Expand-Archive -LiteralPath $env:VDK_ZIP -DestinationPath $env:VDK_OUT -Force",
        ])
        .env("VDK_ZIP", zip_path)
        .env("VDK_OUT", extracted_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("failed to invoke tar.exe and powershell.exe Expand-Archive")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("archive extraction failed: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(windows)]
enum ElevationOutcome {
    Finished { exit_code: u32 },
    Cancelled,
}

#[cfg(windows)]
struct ProcessHandleGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for ProcessHandleGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is a valid process handle returned by `ShellExecuteExW` with `SEE_MASK_NOCLOSEPROCESS`.
            unsafe {
                let _ = windows_sys::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
fn run_elevated_setup_windows(setup_exe: &Path, work_dir: &Path) -> Result<ElevationOutcome> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_CANCELLED, GetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let verb_w: Vec<u16> = "runas\0".encode_utf16().collect();
    let params_w: Vec<u16> = "-i -h\0".encode_utf16().collect();
    let file_w: Vec<u16> = setup_exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let dir_w: Vec<u16> = work_dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    // SAFETY: `SHELLEXECUTEINFOW` is POD and all UTF-16 slices outlive the synchronous `ShellExecuteExW` call.
    let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    sei.fMask = SEE_MASK_NOCLOSEPROCESS;
    sei.lpVerb = verb_w.as_ptr();
    sei.lpFile = file_w.as_ptr();
    sei.lpParameters = params_w.as_ptr();
    sei.lpDirectory = dir_w.as_ptr();
    sei.nShow = SW_HIDE;

    // SAFETY: `sei` is properly initialized with NUL-terminated UTF-16 pointers.
    let ok = unsafe { ShellExecuteExW(&mut sei) };
    if ok == 0 {
        // SAFETY: `GetLastError` reads the calling thread's Win32 error code immediately after `ShellExecuteExW`.
        let err = unsafe { GetLastError() };
        if err == ERROR_CANCELLED {
            return Ok(ElevationOutcome::Cancelled);
        }
        bail!("ShellExecuteExW failed with Win32 error {err}");
    }

    let proc_guard = ProcessHandleGuard(sei.hProcess);
    let mut exit_code: u32 = 0;
    if !proc_guard.0.is_null() {
        // SAFETY: `proc_guard.0` is a valid process handle owned by `proc_guard`.
        let wait_rc = unsafe { WaitForSingleObject(proc_guard.0, 120_000) };
        if wait_rc == WAIT_TIMEOUT {
            bail!("VB-CABLE installer timed out after 120 seconds");
        }
        if wait_rc != WAIT_OBJECT_0 {
            bail!("WaitForSingleObject on VB-CABLE installer failed (status {wait_rc})");
        }
        // SAFETY: process has signaled completion; `exit_code` is a valid out-pointer.
        unsafe {
            let _ = GetExitCodeProcess(proc_guard.0, &mut exit_code);
        }
    }
    Ok(ElevationOutcome::Finished { exit_code })
}

/// Re-scan WASAPI playback devices and return the primary detected virtual cable name, if any.
pub fn rescan_virtual_cable_device_name() -> Option<String> {
    #[cfg(feature = "net")]
    {
        let devices = veloxdesk_audio::list_playback_devices();
        devices
            .iter()
            .find(|d| {
                let lower = d.name.to_ascii_lowercase();
                lower.contains("cable input") || lower.contains("vb-audio")
            })
            .or_else(|| devices.iter().find(|d| d.is_virtual_cable))
            .map(|d| d.name.clone())
    }
    #[cfg(not(feature = "net"))]
    {
        None
    }
}

/// Spawn the background VB-CABLE download & silent UAC installation worker thread,
/// re-scanning `veloxdesk_audio::list_playback_devices()` after completion.
pub fn spawn_install_virtual_mic(proxy: EventLoopProxy<UserEvent>) -> bool {
    spawn_install_virtual_mic_with(proxy, rescan_virtual_cable_device_name)
}

/// Spawn the background VB-CABLE download & silent UAC installation worker thread.
///
/// `rescan_virtual_cable` is called ~800 ms after installer completion to detect the newly
/// registered WASAPI playback endpoint name.
pub fn spawn_install_virtual_mic_with(
    proxy: EventLoopProxy<UserEvent>,
    rescan_virtual_cable: impl FnOnce() -> Option<String> + Send + 'static,
) -> bool {
    if INSTALL_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return false;
    }

    let thread_proxy = proxy.clone();
    let spawned = std::thread::Builder::new()
        .name("veloxdesk-vbcable".into())
        .spawn(move || {
            let _reset = InstallResetGuard;
            run_installer_worker(&thread_proxy, rescan_virtual_cable);
        });

    if let Err(e) = spawned {
        INSTALL_IN_PROGRESS.store(false, Ordering::SeqCst);
        let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Failed(format!(
            "failed to spawn installer thread: {e}"
        ))));
        return false;
    }
    true
}

#[cfg(windows)]
fn run_installer_worker(
    proxy: &EventLoopProxy<UserEvent>,
    rescan_virtual_cable: impl FnOnce() -> Option<String>,
) {
    let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Downloading));

    let pid = std::process::id();
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let temp_dir = std::env::temp_dir().join(format!("veloxdesk-vbcable-{pid}-{millis}"));
    if let Err(e) = fs::create_dir_all(&temp_dir) {
        let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Failed(format!(
            "failed to create temp directory: {e}"
        ))));
        return;
    }
    let temp_guard = TempDirGuard(temp_dir.clone());
    let zip_path = temp_dir.join("vbcable.zip");
    let extracted_dir = temp_dir.join("extracted");

    let candidates = discover_candidate_urls();
    let mut last_err = anyhow!("no VB-CABLE download URLs available");
    let mut downloaded_ok = false;
    for url in &candidates {
        let _ = fs::remove_file(&zip_path);
        match download_zip_to_path(url, &zip_path) {
            Ok(()) => {
                downloaded_ok = true;
                break;
            }
            Err(e) => {
                tracing::warn!(%url, "VB-CABLE candidate download failed: {e:#}");
                last_err = e;
            }
        }
    }
    if !downloaded_ok {
        let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Failed(format!("{last_err:#}"))));
        return;
    }

    let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Extracting));
    if let Err(e) = extract_zip_windows(&zip_path, &extracted_dir) {
        let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Failed(format!("{e:#}"))));
        return;
    }

    let setup_exe = match find_and_verify_setup_exe(&extracted_dir, std::env::consts::ARCH) {
        Ok(p) => p,
        Err(e) => {
            let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Failed(format!("{e:#}"))));
            return;
        }
    };
    let work_dir = setup_exe.parent().unwrap_or(&extracted_dir).to_path_buf();

    let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::WaitingForUac));
    let outcome = match run_elevated_setup_windows(&setup_exe, &work_dir) {
        Ok(o) => o,
        Err(e) => {
            let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Failed(format!("{e:#}"))));
            return;
        }
    };

    // Clean temporary files immediately once elevated setup has finished or been cancelled.
    drop(temp_guard);

    match outcome {
        ElevationOutcome::Cancelled => {
            let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Cancelled));
        }
        ElevationOutcome::Finished { exit_code } => {
            tracing::info!(exit_code, "VB-CABLE silent installer completed");
            std::thread::sleep(Duration::from_millis(800));
            let device_name = rescan_virtual_cable();
            if exit_code != 0 && exit_code != 1 && exit_code != 3010 && device_name.is_none() {
                let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Failed(format!(
                    "VB-CABLE installer exited with code {exit_code}"
                ))));
            } else {
                let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Completed { device_name }));
            }
        }
    }
}

#[cfg(not(windows))]
fn run_installer_worker(
    proxy: &EventLoopProxy<UserEvent>,
    _rescan_virtual_cable: impl FnOnce() -> Option<String>,
) {
    let _ = proxy.send_event(UserEvent::VirtualMic(VirtualMicEvent::Failed(
        "VB-CABLE automatic installer is only supported on Windows".into(),
    )));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_allowlist_enforces_strict_https_vbaudio_hosts() {
        assert!(is_allowed_vbcable_url("https://vb-audio.com/Cable/index.htm"));
        assert!(is_allowed_vbcable_url("https://www.vb-audio.com/Cable/index.htm"));
        assert!(is_allowed_vbcable_url(
            "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip"
        ));
        assert!(is_allowed_vbcable_url(
            "https://DOWNLOAD.VB-AUDIO.COM:443/Download_CABLE/VBCABLE_Driver_Pack46.zip"
        ));

        // Reject HTTP, userinfo, non-443 ports, spoofing domains, IPv6, whitespace, control chars
        assert!(!is_allowed_vbcable_url(
            "http://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip"
        ));
        assert!(!is_allowed_vbcable_url("https://vb-audio.com@evil.example/pack.zip"));
        assert!(!is_allowed_vbcable_url("https://download.vb-audio.com:8443/pack.zip"));
        assert!(!is_allowed_vbcable_url("https://evil-vb-audio.com/pack.zip"));
        assert!(!is_allowed_vbcable_url("https://vb-audio.com.evil.org/pack.zip"));
        assert!(!is_allowed_vbcable_url("https://[::1]/pack.zip"));
        assert!(!is_allowed_vbcable_url("https://download.vb-audio.com/Download_CABLE/pack 46.zip"));
        assert!(!is_allowed_vbcable_url("https://download.vb-audio.com/Download_CABLE/pack.zip\r\n"));
        assert!(!is_allowed_vbcable_url("file:///C:/Windows/System32/calc.exe"));
    }

    #[test]
    fn redirect_resolution_and_allowlist_checks() {
        let r1 = resolve_redirect_url(
            "https://vb-audio.com/Cable/index.htm",
            "//download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip",
        )
        .unwrap();
        assert_eq!(r1, "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip");
        assert!(is_allowed_vbcable_url(&r1));

        let r2 = resolve_redirect_url("https://www.vb-audio.com/Cable/", "/Cable/index.htm").unwrap();
        assert_eq!(r2, "https://www.vb-audio.com/Cable/index.htm");
        assert!(is_allowed_vbcable_url(&r2));

        let r3 = resolve_redirect_url(
            "https://download.vb-audio.com/Download_CABLE/latest",
            "VBCABLE_Driver_Pack46.zip",
        )
        .unwrap();
        assert_eq!(r3, "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip");
        assert!(is_allowed_vbcable_url(&r3));

        // Cross-origin / HTTP downgrade redirects are rejected by allowlist
        let evil = resolve_redirect_url("https://vb-audio.com/Cable/index.htm", "https://evil.org/a.zip").unwrap();
        assert!(!is_allowed_vbcable_url(&evil));
        let downgrade =
            resolve_redirect_url("https://vb-audio.com/Cable/index.htm", "http://download.vb-audio.com/a.zip").unwrap();
        assert!(!is_allowed_vbcable_url(&downgrade));
    }

    #[test]
    fn html_link_extraction_supports_absolute_and_relative_links() {
        let abs_html = r#"
            <html><body>
              <a href="https://download.vb-audio.com/Download_CABLE/VBCABLE_MACDriver_Pack108.dmg">Mac</a>
              <a href="https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack43.zip">Old</a>
              <a href="https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip">Win</a>
            </body></html>
        "#;
        assert_eq!(
            extract_vbcable_zip_url(abs_html).as_deref(),
            Some("https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip")
        );

        let rel_html = r#"<p>Download: <A HREF='Download_CABLE/VBCABLE_Driver_Pack46.zip'>ZIP</A></p>"#;
        assert_eq!(
            extract_vbcable_zip_url(rel_html).as_deref(),
            Some("https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip")
        );

        let root_rel_html = r#"<a href="/Download_CABLE/VBCABLE_Driver_Pack45.zip">Pack45</a>"#;
        assert_eq!(
            extract_vbcable_zip_url(root_rel_html).as_deref(),
            Some("https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip")
        );

        let bad_html = r#"
            <a href="https://evil.example.com/Download_CABLE/VBCABLE_Driver_Pack99.zip">Bad</a>
            <a href="Download_CABLE/../VBCABLE_Driver_Pack99.zip">Traversal</a>
        "#;
        assert_eq!(extract_vbcable_zip_url(bad_html), None);
    }

    #[test]
    fn zip_and_pe_magic_and_setup_exe_verification() {
        assert!(has_zip_magic(b"PK\x03\x04rest_of_zip"));
        assert!(!has_zip_magic(b"PK\x05\x06"));
        assert!(!has_zip_magic(b"PK"));
        assert!(has_pe_magic(b"MZ\x90\x00"));
        assert!(!has_pe_magic(b"ELF"));
        assert!(!has_pe_magic(b"M"));

        assert_eq!(preferred_setup_exe_names_for_arch("x86_64"), &["VBCABLE_Setup_x64.exe"]);
        assert_eq!(
            preferred_setup_exe_names_for_arch("aarch64"),
            &["VBCABLE_Setup_arm64.exe", "VBCABLE_Setup_x64.exe"]
        );
        assert_eq!(preferred_setup_exe_names_for_arch("x86"), &["VBCABLE_Setup.exe"]);

        let dir = std::env::temp_dir().join(format!("vdk-vmic-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let guard = TempDirGuard(dir.clone());

        let exe_path = dir.join("VBCABLE_Setup_x64.exe");
        fs::write(&exe_path, b"not-a-pe").unwrap();
        assert!(find_and_verify_setup_exe(&dir, "x86_64").is_err());

        fs::write(&exe_path, b"MZ\x90\x00\x03\x00\x00\x00").unwrap();
        assert_eq!(find_and_verify_setup_exe(&dir, "x86_64").unwrap(), exe_path);
        assert_eq!(find_and_verify_setup_exe(&dir, "aarch64").unwrap(), exe_path);

        drop(guard);
        assert!(!dir.exists());
    }
}
