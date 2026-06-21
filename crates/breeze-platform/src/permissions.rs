//! Per-OS capability check for inspecting/controlling other processes (used to
//! gate the cross-session orphan scan). One interface, a real implementation
//! per OS — never macOS-only with a no-op fallback.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessState {
    /// We can scan/inspect processes as needed.
    Granted,
    /// The OS is withholding access; `request`/`open_settings` can help.
    Denied,
    /// The concept doesn't apply on this OS.
    NotApplicable,
}

/// Whether the app can inspect processes beyond the bare minimum (the reach the
/// orphan scan wants).
pub fn process_scan_access() -> AccessState {
    imp::process_scan_access()
}

/// Open the OS settings page where the user can grant access (best-effort).
pub fn open_settings() {
    imp::open_settings();
}

/// Short, OS-appropriate guidance to show in the in-app permission notice.
pub fn guidance() -> &'static str {
    imp::guidance()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::AccessState;
    use std::path::PathBuf;

    pub fn process_scan_access() -> AccessState {
        // Full Disk Access lets us read the TCC database directory; use it as
        // the FDA signal (own-user process listing works regardless, but the
        // full orphan sweep wants FDA).
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let tcc = home.join("Library/Application Support/com.apple.TCC/TCC.db");
        if std::fs::File::open(&tcc).is_ok() {
            AccessState::Granted
        } else {
            AccessState::Denied
        }
    }

    pub fn open_settings() {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
            .spawn();
    }

    pub fn guidance() -> &'static str {
        "Grant Breeze Full Disk Access in System Settings ▸ Privacy & Security ▸ Full Disk Access."
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::AccessState;

    pub fn process_scan_access() -> AccessState {
        // Own-user /proc is always readable; cross-process inspection is limited
        // by Yama ptrace_scope. scope 0 = unrestricted; >0 = restricted.
        match std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope") {
            Ok(s) if s.trim() == "0" => AccessState::Granted,
            Ok(_) => AccessState::Denied,
            // No Yama (file absent) → unrestricted.
            Err(_) => AccessState::Granted,
        }
    }

    pub fn open_settings() {
        // No GUI settings page; guidance covers the sysctl.
    }

    pub fn guidance() -> &'static str {
        "Set kernel.yama.ptrace_scope=0 (sudo sysctl -w kernel.yama.ptrace_scope=0) to inspect other processes."
    }
}

#[cfg(windows)]
mod imp {
    use super::AccessState;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    pub fn process_scan_access() -> AccessState {
        // Own-session processes are queryable without elevation; treat that as
        // Granted. (Cross-session/other-user inspection needs SeDebugPrivilege.)
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, std::process::id());
            if !h.is_null() {
                CloseHandle(h);
                AccessState::Granted
            } else {
                AccessState::Denied
            }
        }
    }

    pub fn open_settings() {
        // No applicable settings page; elevation is the remedy.
    }

    pub fn guidance() -> &'static str {
        "Run Breeze as administrator to inspect processes in other sessions."
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    use super::AccessState;
    pub fn process_scan_access() -> AccessState {
        AccessState::NotApplicable
    }
    pub fn open_settings() {}
    pub fn guidance() -> &'static str {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_state_is_decisive_on_this_host() {
        // On macOS/Linux/Windows it returns a definite Granted/Denied (never
        // NotApplicable), and the call never panics.
        let s = process_scan_access();
        assert!(matches!(s, AccessState::Granted | AccessState::Denied));
        assert!(!guidance().is_empty());
    }
}
