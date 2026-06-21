//! Low-level process suspend/resume — the primitive the duty-cycle throttle and
//! deep-idle freeze are built on.
//!
//! - unix: `SIGSTOP` / `SIGCONT`.
//! - windows: suspend/resume every thread of the process (ToolHelp snapshot).

/// Suspend the process. Returns whether the request was issued successfully.
pub fn suspend(pid: i32) -> bool {
    imp::suspend(pid)
}

/// Resume the process. Returns whether the request was issued successfully.
pub fn resume(pid: i32) -> bool {
    imp::resume(pid)
}

/// Politely terminate the process (SIGTERM / TerminateProcess).
pub fn terminate(pid: i32) -> bool {
    imp::terminate(pid)
}

#[cfg(unix)]
mod imp {
    pub fn suspend(pid: i32) -> bool {
        unsafe { libc::kill(pid, libc::SIGSTOP) == 0 }
    }
    pub fn resume(pid: i32) -> bool {
        unsafe { libc::kill(pid, libc::SIGCONT) == 0 }
    }
    pub fn terminate(pid: i32) -> bool {
        unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
    };

    fn for_each_thread(pid: i32, mut f: impl FnMut(u32)) -> bool {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snap == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut ok = Thread32First(snap, &mut entry);
            let mut touched = false;
            while ok != 0 {
                if entry.th32OwnerProcessID == pid as u32 {
                    f(entry.th32ThreadID);
                    touched = true;
                }
                ok = Thread32Next(snap, &mut entry);
            }
            CloseHandle(snap);
            touched
        }
    }

    pub fn suspend(pid: i32) -> bool {
        for_each_thread(pid, |tid| unsafe {
            let h = OpenThread(THREAD_SUSPEND_RESUME, 0, tid);
            if !h.is_null() {
                SuspendThread(h);
                CloseHandle(h);
            }
        })
    }

    pub fn resume(pid: i32) -> bool {
        for_each_thread(pid, |tid| unsafe {
            let h = OpenThread(THREAD_SUSPEND_RESUME, 0, tid);
            if !h.is_null() {
                ResumeThread(h);
                CloseHandle(h);
            }
        })
    }

    pub fn terminate(pid: i32) -> bool {
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
            if h.is_null() {
                return false;
            }
            let ok = TerminateProcess(h, 1) != 0;
            CloseHandle(h);
            ok
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    /// Spawn a real long-running child, suspend it, resume it, then kill it.
    /// Verifies the signals are accepted by the OS for a live process.
    #[test]
    fn suspend_resume_real_child() {
        let mut child = Command::new("sleep").arg("30").spawn().expect("spawn sleep");
        let pid = child.id() as i32;
        std::thread::sleep(Duration::from_millis(50));

        assert!(suspend(pid), "suspend should succeed on a live pid");
        assert!(resume(pid), "resume should succeed on a live pid");

        // Cleanup.
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn signal_to_dead_pid_fails() {
        // PID 0x7FFFFFFF is overwhelmingly unlikely to exist.
        assert!(!suspend(i32::MAX));
    }

    #[test]
    fn terminate_kills_a_real_child() {
        let mut child = Command::new("sleep").arg("30").spawn().expect("spawn");
        let pid = child.id() as i32;
        std::thread::sleep(Duration::from_millis(50));
        assert!(terminate(pid), "terminate should succeed on a live pid");
        let _ = child.wait();
        // After reaping, the pid is gone.
        assert!(!terminate(pid));
    }
}
