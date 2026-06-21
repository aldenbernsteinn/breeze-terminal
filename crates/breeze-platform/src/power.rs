//! Whether the OS is in a battery-saving / low-power state. When true, the
//! session manager tightens its duty cycles (see `throttle_action`). One
//! interface, a real implementation per OS — never macOS-only with a no-op.

/// True when the system is in a user- or OS-engaged low-power / battery-saver
/// mode. Best-effort and cheap; safe to poll.
pub fn low_power_active() -> bool {
    imp::low_power_active()
}

#[cfg(target_os = "macos")]
mod imp {
    pub fn low_power_active() -> bool {
        // `pmset -g` prints a `lowpowermode <0|1>` line reflecting the current
        // Low Power Mode setting.
        let out = match std::process::Command::new("pmset").arg("-g").output() {
            Ok(o) => o,
            Err(_) => return false,
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| {
                let l = l.trim();
                l.strip_prefix("lowpowermode")
                    .map(|rest| rest.trim().starts_with('1'))
            })
            .unwrap_or(false)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn low_power_active() -> bool {
        // The ACPI platform profile exposes the active power posture; "low-power"
        // is the battery-saver equivalent.
        matches!(
            std::fs::read_to_string("/sys/firmware/acpi/platform_profile")
                .map(|s| s.trim().to_string())
                .as_deref(),
            Ok("low-power")
        )
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::Power::GetSystemPowerStatus;

    pub fn low_power_active() -> bool {
        unsafe {
            let mut status = std::mem::zeroed();
            if GetSystemPowerStatus(&mut status) == 0 {
                return false;
            }
            // SystemStatusFlag bit 0 set => battery saver is engaged.
            (status.SystemStatusFlag & 0x01) != 0
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    pub fn low_power_active() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_cheap_and_total() {
        // Returns a definite bool on this host without panicking; we can't assert
        // the value (depends on the machine's current power posture).
        let _ = low_power_active();
    }
}
