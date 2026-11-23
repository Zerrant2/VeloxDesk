//! Build script emitting Git commit hash and formatted build display string (R6).

use std::path::Path;
use std::process::Command;

fn main() {
    let git_dir = Path::new("../../.git");
    let head_path = git_dir.join("HEAD");
    if head_path.exists() {
        println!("cargo:rerun-if-changed={}", head_path.display());
        if let Ok(head) = std::fs::read_to_string(&head_path)
            && let Some(ref_rel) = head.trim().strip_prefix("ref:")
        {
            let ref_path = git_dir.join(ref_rel.trim());
            if ref_path.exists() {
                println!("cargo:rerun-if-changed={}", ref_path.display());
            }
            let packed = git_dir.join("packed-refs");
            if packed.exists() {
                println!("cargo:rerun-if-changed={}", packed.display());
            }
        }
    }

    let sha = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_string());

    let pkg_ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let display = format!("v{pkg_ver} ({sha})");

    println!("cargo:rustc-env=VELOXDESK_GIT_SHA={sha}");
    println!("cargo:rustc-env=VELOXDESK_BUILD_DISPLAY={display}");
}
