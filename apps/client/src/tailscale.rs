//! Native Tailscale CLI integration: auto-detects installed `tailscale`, queries
//! `tailscale status --json` without flashing a console window, launches/logs in,
//! and opens external URLs cleanly.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use veloxdesk_ui::{Language, TailscaleInfo, TailscalePeer, tr};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn apply_no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn apply_no_window(_cmd: &mut Command) {}

/// Locate the `tailscale` CLI executable on `PATH` or in standard platform install directories.
pub fn find_tailscale_exe() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "tailscale.exe" } else { "tailscale" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    let mut fallbacks: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        fallbacks.push(PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe"));
        fallbacks.push(PathBuf::from(r"C:\Program Files (x86)\Tailscale\tailscale.exe"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            fallbacks.push(PathBuf::from(local).join("Tailscale").join("tailscale.exe"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        fallbacks.push(PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale"));
        fallbacks.push(PathBuf::from("/usr/local/bin/tailscale"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        fallbacks.push(PathBuf::from("/usr/bin/tailscale"));
        fallbacks.push(PathBuf::from("/usr/local/bin/tailscale"));
    }

    fallbacks.into_iter().find(|p| p.is_file())
}

/// Pick the best Tailscale IP from a `TailscaleIPs` JSON array (preferring IPv4 `100.x.y.z`).
fn pick_preferred_ip(ips: Option<&Value>) -> Option<String> {
    let arr = ips?.as_array()?;
    let strs: Vec<&str> = arr.iter().filter_map(Value::as_str).filter(|s| !s.is_empty()).collect();
    if let Some(v4) = strs.iter().find(|s| s.parse::<Ipv4Addr>().is_ok()) {
        return Some((*v4).to_string());
    }
    strs.first().map(|s| (*s).to_string())
}

/// Extract a clean display name from `HostName` (falling back to the first label of `DNSName`).
fn extract_name(node: &Value) -> String {
    if let Some(host) = node.get("HostName").and_then(Value::as_str)
        && !host.trim().is_empty()
    {
        return host.trim().to_string();
    }
    if let Some(dns) = node.get("DNSName").and_then(Value::as_str) {
        let short = dns.trim_end_matches('.').split('.').next().unwrap_or("").trim();
        if !short.is_empty() {
            return short.to_string();
        }
    }
    String::new()
}

/// Parse the JSON output of `tailscale status --json` into a [`TailscaleInfo`] snapshot.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_status_json(raw: &str) -> TailscaleInfo {
    parse_status_json_for(raw, Language::En)
}

/// Parse `tailscale status --json` with localized status/error text.
pub fn parse_status_json_for(raw: &str, lang: Language) -> TailscaleInfo {
    let t = tr(lang);
    let val: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            return TailscaleInfo {
                installed: true,
                running: false,
                error: Some(t.fmt_ts_parse_err(&e.to_string())),
                ..Default::default()
            };
        }
    };

    let state = val.get("BackendState").and_then(Value::as_str).unwrap_or("");
    let running = state == "Running";

    let self_node = val.get("Self");
    let self_ip = self_node.and_then(|n| pick_preferred_ip(n.get("TailscaleIPs")));
    let self_name = self_node.map(extract_name).filter(|s| !s.is_empty());

    let mut peers = Vec::new();
    if let Some(peer_map) = val.get("Peer").and_then(Value::as_object) {
        for node in peer_map.values() {
            let Some(ip) = pick_preferred_ip(node.get("TailscaleIPs")) else { continue };
            let mut name = extract_name(node);
            if name.is_empty() {
                name = ip.clone();
            }
            let os = node.get("OS").and_then(Value::as_str).unwrap_or("").to_string();
            let online = node.get("Online").and_then(Value::as_bool).unwrap_or(false);
            peers.push(TailscalePeer { name, ip, os, online });
        }
    }

    // Online peers first, then alphabetically by name (case-insensitive), then by IP.
    peers.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
            .then_with(|| a.ip.cmp(&b.ip))
    });

    let error = if running {
        None
    } else {
        Some(match state {
            "NeedsLogin" => t.ts_needs_login.into(),
            "Stopped" => t.ts_stopped.into(),
            "Starting" => t.ts_starting.into(),
            "NoState" | "" => t.ts_daemon_not_running.into(),
            other => t.fmt_ts_state(other),
        })
    };

    TailscaleInfo { installed: true, running, self_ip, self_name, peers, error }
}

/// Query local Tailscale status via `tailscale status --json` (non-interactive, no console flash).
pub fn query_status_for(lang: Language) -> TailscaleInfo {
    let t = tr(lang);
    let Some(exe) = find_tailscale_exe() else {
        return TailscaleInfo { installed: false, ..Default::default() };
    };
    let mut cmd = Command::new(&exe);
    cmd.args(["status", "--json"]);
    apply_no_window(&mut cmd);

    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return TailscaleInfo {
                installed: true,
                running: false,
                error: Some(t.fmt_ts_cannot_run(&e.to_string())),
                ..Default::default()
            };
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    if !stdout.trim().is_empty() && serde_json::from_str::<Value>(&stdout).is_ok() {
        return parse_status_json_for(&stdout, lang);
    }

    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    TailscaleInfo {
        installed: true,
        running: false,
        error: Some(if stderr.is_empty() {
            t.ts_not_running.into()
        } else {
            stderr
        }),
        ..Default::default()
    }
}

/// Launch the Tailscale GUI (`tailscale-ipn.exe` on Windows) or invoke `tailscale up`.
pub fn start_tailscale_for(lang: Language) -> Result<(), String> {
    let t = tr(lang);
    let exe = find_tailscale_exe().ok_or_else(|| t.ts_not_installed.to_string())?;

    #[cfg(windows)]
    {
        if let Some(ipn) = exe.parent().map(|d: &Path| d.join("tailscale-ipn.exe"))
            && ipn.is_file()
            && Command::new(&ipn).spawn().is_ok()
        {
            return Ok(());
        }
    }
    #[cfg(not(windows))]
    let _ = Path::new("");

    let mut cmd = Command::new(&exe);
    cmd.arg("up");
    apply_no_window(&mut cmd);
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| t.fmt_ts_failed_launch(&e.to_string()))
}

/// Validate that `url` is a clean `https://` URL with no control characters or whitespace.
fn is_safe_open_url(url: &str) -> bool {
    url.starts_with("https://")
        && url.len() > "https://".len()
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Open an external HTTPS URL in the platform's default browser without invoking a shell.
pub fn open_url(url: &str) {
    if !is_safe_open_url(url) {
        tracing::warn!(%url, "refusing to open unsafe or non-https URL");
        return;
    }
    #[cfg(windows)]
    {
        use std::ptr::{null, null_mut};
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let open: Vec<u16> = "open\0".encode_utf16().collect();
        let target: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `open` and `target` are NUL-terminated UTF-16 strings valid for the call;
        // `ShellExecuteW` opens the URL directly via Windows Shell without spawning `cmd.exe`.
        unsafe {
            let _ = ShellExecuteW(null_mut(), open.as_ptr(), target.as_ptr(), null(), null(), SW_SHOWNORMAL);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_running_status_prefers_ipv4_and_sorts_online_first() {
        let sample = r#"{
            "BackendState": "Running",
            "Self": {
                "HostName": "my-pc",
                "DNSName": "my-pc.tailnet.ts.net.",
                "TailscaleIPs": ["fd7a:115c:a1e0::1", "100.64.1.10"]
            },
            "Peer": {
                "node-z": {
                    "HostName": "z-offline",
                    "OS": "linux",
                    "TailscaleIPs": ["100.64.2.30"],
                    "Online": false
                },
                "node-b": {
                    "HostName": "Beta-Rig",
                    "OS": "windows",
                    "TailscaleIPs": ["fd7a:115c:a1e0::2", "100.64.2.20"],
                    "Online": true
                },
                "node-a": {
                    "HostName": "",
                    "DNSName": "alpha-mac.tailnet.ts.net.",
                    "OS": "macOS",
                    "TailscaleIPs": ["100.64.2.10"],
                    "Online": true
                },
                "node-a-off": {
                    "HostName": "a-offline",
                    "OS": "android",
                    "TailscaleIPs": ["fd7a:115c:a1e0::9"],
                    "Online": false
                }
            }
        }"#;

        let info = parse_status_json(sample);
        assert!(info.installed);
        assert!(info.running);
        assert!(info.error.is_none());
        assert_eq!(info.self_ip.as_deref(), Some("100.64.1.10"));
        assert_eq!(info.self_name.as_deref(), Some("my-pc"));
        assert_eq!(info.peers.len(), 4);

        // Online peers first, sorted case-insensitively: alpha-mac, Beta-Rig, then offline: a-offline, z-offline.
        assert_eq!(info.peers[0].name, "alpha-mac");
        assert!(info.peers[0].online);
        assert_eq!(info.peers[0].ip, "100.64.2.10");

        assert_eq!(info.peers[1].name, "Beta-Rig");
        assert!(info.peers[1].online);
        assert_eq!(info.peers[1].ip, "100.64.2.20");
        assert_eq!(info.peers[1].os, "windows");

        assert_eq!(info.peers[2].name, "a-offline");
        assert!(!info.peers[2].online);
        assert_eq!(info.peers[2].ip, "fd7a:115c:a1e0::9");

        assert_eq!(info.peers[3].name, "z-offline");
        assert!(!info.peers[3].online);
    }

    #[test]
    fn parse_needs_login_and_stopped_states() {
        let login = parse_status_json(r#"{"BackendState": "NeedsLogin", "Self": null, "Peer": null}"#);
        assert!(login.installed);
        assert!(!login.running);
        assert_eq!(login.error.as_deref(), Some("Tailscale needs login"));
        assert!(login.peers.is_empty());

        let stopped = parse_status_json(r#"{"BackendState": "Stopped"}"#);
        assert!(stopped.installed);
        assert!(!stopped.running);
        assert_eq!(stopped.error.as_deref(), Some("Tailscale is stopped"));
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        let bad = parse_status_json("not json at all");
        assert!(bad.installed);
        assert!(!bad.running);
        assert!(bad.error.as_deref().unwrap_or("").contains("failed to parse"));
    }

    #[test]
    fn open_url_rejects_non_https_and_control_chars() {
        assert!(is_safe_open_url("https://tailscale.com/download"));
        assert!(is_safe_open_url("https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.2.0"));
        assert!(!is_safe_open_url("http://github.com/Zerrant2/VeloxDesk"));
        assert!(!is_safe_open_url("https://"));
        assert!(!is_safe_open_url("https://example.com/a b"));
        assert!(!is_safe_open_url("https://example.com/a\ncalc.exe"));
        assert!(!is_safe_open_url("file:///C:/Windows/System32/calc.exe"));
    }
}
