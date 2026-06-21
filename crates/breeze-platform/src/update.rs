//! Opt-in update check against GitHub Releases. No telemetry: a single HTTPS GET
//! on launch (via the system `curl`), and nothing is installed without the user
//! confirming the prompt. Self-replace is macOS-only (uses `hdiutil`/`open`);
//! other platforms get a no-op `apply`.

use std::process::Command;

/// Public repo whose Releases we check.
const REPO: &str = "aldenbernsteinn/breeze-terminal";

/// This build's version, straight from Cargo.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A published release newer than the running build.
pub struct Available {
    pub version: String,
    pub dmg_url: String,
}

/// Ask GitHub for the latest release. Returns it only when it's newer than this
/// build AND ships a `.dmg` asset. `None` on any error or when up to date — the
/// check is best-effort and never surfaces failures.
pub fn check() -> Option<Available> {
    let api = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let out = Command::new("curl")
        .args(["-fsSL", "-H", "User-Agent: breeze", &api])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let latest = json.get("tag_name")?.as_str()?.trim_start_matches('v').to_string();
    if !is_newer(&latest, current_version()) {
        return None;
    }
    let dmg_url = json
        .get("assets")?
        .as_array()?
        .iter()
        .filter_map(|a| a.get("browser_download_url")?.as_str())
        .find(|u| u.ends_with(".dmg"))?
        .to_string();
    Some(Available { version: latest, dmg_url })
}

/// `a` is a newer version than `b`, compared by numeric dotted components.
fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| {
        s.split(['.', '-'])
            .map(|x| x.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parse(a) > parse(b)
}

/// Download the DMG, swap the running `.app` in place, and relaunch. Exits the
/// process on success. macOS only.
#[cfg(target_os = "macos")]
pub fn apply(dmg_url: &str) -> std::io::Result<()> {
    use std::path::PathBuf;
    let dmg = "/tmp/Breeze-update.dmg";
    let mount = "/Volumes/Breeze-update";

    let ok = Command::new("curl").args(["-fsSL", "-o", dmg, dmg_url]).status()?.success();
    if !ok {
        return Err(std::io::Error::other("download failed"));
    }
    let _ = Command::new("hdiutil").args(["detach", mount]).status();
    let mounted = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-mountpoint", mount, dmg])
        .status()?
        .success();
    if !mounted {
        return Err(std::io::Error::other("mount failed"));
    }

    // Resolve the running .app bundle from the executable path.
    let exe = std::env::current_exe()?;
    let app_root: PathBuf = exe
        .ancestors()
        .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/Applications/Breeze.app"));
    let app_str = app_root.to_string_lossy().to_string();

    let src = format!("{mount}/Breeze.app");
    let _ = Command::new("rm").args(["-rf", &app_str]).status();
    let copied = Command::new("cp").args(["-R", &src, &app_str]).status()?.success();
    let _ = Command::new("hdiutil").args(["detach", mount]).status();
    if !copied {
        return Err(std::io::Error::other("install failed"));
    }

    let _ = Command::new("open").arg(&app_root).status();
    std::process::exit(0);
}

/// Non-macOS: no self-replace yet.
#[cfg(not(target_os = "macos"))]
pub fn apply(_dmg_url: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn newer_comparison() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.1"));
    }
}
