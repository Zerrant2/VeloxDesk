//! Build script emitting Git commit hash and formatted build display string.
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=VELOXDESK_GIT_SHA");

    let sha = std::env::var("VELOXDESK_GIT_SHA")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=7", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "dev".to_string());

    let pkg_ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.3.0".to_string());
    let display = format!("v{pkg_ver} ({sha})");

    println!("cargo:rustc-env=VELOXDESK_GIT_SHA={sha}");
    println!("cargo:rustc-env=VELOXDESK_BUILD_DISPLAY={display}");
}
