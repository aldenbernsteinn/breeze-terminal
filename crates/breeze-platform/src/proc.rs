//! Process inspection: names, children, resident size, CPU times, working
//! directory, and a system-wide agent-process scan. Backed by libproc on macOS,
//! `/proc` on Linux, and ToolHelp/PSAPI on Windows.

use std::collections::HashSet;

/// Process name, or `"unknown"` if it can't be read.
pub fn process_name(pid: i32) -> String {
    imp::process_name(pid)
}

/// Invoke `f` for each direct child PID of `parent`.
pub fn for_each_child(parent: i32, f: impl FnMut(i32)) {
    imp::for_each_child(parent, f)
}

/// Invoke `f(child_pid, name)` for each direct child of `parent`.
pub fn for_each_child_with_names(parent: i32, mut f: impl FnMut(i32, &str)) {
    imp::for_each_child(parent, |pid| {
        let name = process_name(pid);
        f(pid, &name);
    })
}

/// True if `parent` has at least one child whose name is neither ignored nor
/// `"unknown"` — i.e. a real tool is running.
pub fn has_active_child(parent: i32, ignored: &HashSet<&str>) -> bool {
    let mut active = false;
    imp::for_each_child(parent, |pid| {
        if active {
            return;
        }
        let name = process_name(pid);
        if name != "unknown" && !ignored.contains(name.as_str()) {
            active = true;
        }
    });
    active
}

/// Number of direct children.
pub fn child_count(parent: i32) -> i64 {
    let mut n = 0;
    imp::for_each_child(parent, |_| n += 1);
    n
}

/// Resident set size in bytes (0 on failure).
pub fn rss_bytes(pid: i32) -> u64 {
    imp::task_info(pid).map(|t| t.rss_bytes).unwrap_or(0)
}

/// Cumulative (user, system) CPU time in nanoseconds, if readable.
pub fn cpu_times(pid: i32) -> Option<(u64, u64)> {
    imp::task_info(pid).map(|t| (t.user_ns, t.system_ns))
}

/// Process working directory, if readable.
pub fn working_directory(pid: i32) -> Option<String> {
    imp::working_directory(pid)
}

/// Raw per-process accounting.
pub struct TaskInfo {
    pub user_ns: u64,
    pub system_ns: u64,
    pub rss_bytes: u64,
}

/// Search the process tree under `root` for a descendant named `name`, walking
/// up to `max_depth` levels. Children are collected before grandchildren are
/// scanned (no nested enumeration). Returns the first match found.
pub fn find_descendant_named(root: i32, name: &str, max_depth: u32) -> Option<i32> {
    if max_depth == 0 {
        return None;
    }
    let mut children: Vec<(i32, String)> = Vec::new();
    for_each_child_with_names(root, |pid, n| children.push((pid, n.to_string())));
    for (pid, child_name) in &children {
        if child_name == name {
            return Some(*pid);
        }
    }
    for (pid, _) in &children {
        if let Some(found) = find_descendant_named(*pid, name, max_depth - 1) {
            return Some(found);
        }
    }
    None
}

/// Find a recognized agent process running under a pane's shell (child or
/// grandchild) — the first one matching any of [`AGENT_PROCESS_NAMES`].
pub fn find_agent_pid(shell_pid: i32) -> Option<i32> {
    breeze_core::agents::AGENT_PROCESS_NAMES
        .iter()
        .find_map(|name| find_descendant_named(shell_pid, name, 2))
}

/// All system-wide agent processes. Cached for 60s — a full PID scan is hundreds
/// of syscalls.
pub fn find_all_agent_processes() -> Vec<i32> {
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static CACHE: OnceLock<Mutex<Option<(Vec<i32>, Instant)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().unwrap();
    if let Some((ref pids, when)) = *guard {
        if when.elapsed() < Duration::from_secs(60) {
            return pids.clone();
        }
    }
    let mut pids = Vec::new();
    for name in breeze_core::agents::AGENT_PROCESS_NAMES {
        pids.extend(imp::scan_named(name));
    }
    pids.sort_unstable();
    pids.dedup();
    *guard = Some((pids.clone(), Instant::now()));
    pids
}

fn buf_to_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(target_os = "macos")]
mod imp {
    pub use super::TaskInfo;
    use super::buf_to_string;
    use std::os::raw::c_void;

    pub fn process_name(pid: i32) -> String {
        let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let r = unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
        if r <= 0 {
            return "unknown".to_string();
        }
        buf_to_string(&buf)
    }

    pub fn for_each_child(parent: i32, mut f: impl FnMut(i32)) {
        let mut buf = [0i32; 128];
        // macOS returns the number of PIDs here (not a byte count). Clamp to the
        // buffer and filter pid > 0 — correct under either interpretation since
        // the buffer is zero-initialised.
        let r = unsafe {
            libc::proc_listchildpids(
                parent,
                buf.as_mut_ptr() as *mut c_void,
                (buf.len() * std::mem::size_of::<i32>()) as i32,
            )
        };
        if r <= 0 {
            return;
        }
        let count = (r as usize).min(buf.len());
        for &pid in buf.iter().take(count) {
            if pid > 0 {
                f(pid);
            }
        }
    }

    pub fn task_info(pid: i32) -> Option<TaskInfo> {
        let mut ti: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let r = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, &mut ti as *mut _ as *mut c_void, size)
        };
        if r != size {
            return None;
        }
        Some(TaskInfo {
            user_ns: ti.pti_total_user,
            system_ns: ti.pti_total_system,
            rss_bytes: ti.pti_resident_size,
        })
    }

    pub fn working_directory(pid: i32) -> Option<String> {
        let mut vpi: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
        let r = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                &mut vpi as *mut _ as *mut c_void,
                size,
            )
        };
        if r != size {
            return None;
        }
        let path_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                vpi.pvi_cdir.vip_path.as_ptr() as *const u8,
                vpi.pvi_cdir.vip_path.len(),
            )
        };
        let path = buf_to_string(path_bytes);
        if path.is_empty() {
            None
        } else {
            Some(path)
        }
    }

    pub fn scan_named(target: &str) -> Vec<i32> {
        let cap = 4096usize;
        let mut buf = vec![0i32; cap];
        let bytes = unsafe {
            libc::proc_listallpids(
                buf.as_mut_ptr() as *mut c_void,
                (cap * std::mem::size_of::<i32>()) as i32,
            )
        };
        if bytes <= 0 {
            return Vec::new();
        }
        let count = (bytes as usize).min(cap);
        let mut out = Vec::new();
        for &p in buf.iter().take(count) {
            if p > 0 && process_name(p) == target {
                out.push(p);
            }
        }
        out
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub use super::TaskInfo;
    use std::fs;

    pub fn process_name(pid: i32) -> String {
        match fs::read_to_string(format!("/proc/{pid}/comm")) {
            Ok(s) => {
                let t = s.trim_end_matches('\n');
                if t.is_empty() {
                    "unknown".to_string()
                } else {
                    t.to_string()
                }
            }
            Err(_) => "unknown".to_string(),
        }
    }

    pub fn for_each_child(parent: i32, mut f: impl FnMut(i32)) {
        // /proc/<pid>/task/<tid>/children lists direct children space-separated.
        let dir = format!("/proc/{parent}/task");
        let Ok(tasks) = fs::read_dir(&dir) else { return };
        for task in tasks.flatten() {
            let children = task.path().join("children");
            if let Ok(s) = fs::read_to_string(&children) {
                for tok in s.split_whitespace() {
                    if let Ok(pid) = tok.parse::<i32>() {
                        if pid > 0 {
                            f(pid);
                        }
                    }
                }
            }
        }
    }

    pub fn task_info(pid: i32) -> Option<TaskInfo> {
        // /proc/<pid>/stat: utime (14), stime (15) in clock ticks; rss (24) in pages.
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // comm (field 2) may contain spaces/parens; split after the trailing ')'.
        let close = stat.rfind(')')?;
        let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        // rest[0] = state (field 3); utime = field 14 → rest[11]; stime → rest[12]; rss pages → rest[21].
        let utime: u64 = rest.get(11)?.parse().ok()?;
        let stime: u64 = rest.get(12)?.parse().ok()?;
        let rss_pages: u64 = rest.get(21)?.parse().ok()?;
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        let ns_per_tick = if hz > 0 { 1_000_000_000 / hz } else { 0 };
        Some(TaskInfo {
            user_ns: utime * ns_per_tick,
            system_ns: stime * ns_per_tick,
            rss_bytes: rss_pages * page,
        })
    }

    pub fn working_directory(pid: i32) -> Option<String> {
        fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
    }

    pub fn scan_named(target: &str) -> Vec<i32> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir("/proc") else { return out };
        for e in entries.flatten() {
            if let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) {
                if process_name(pid) == target {
                    out.push(pid);
                }
            }
        }
        out
    }
}

#[cfg(windows)]
mod imp {
    pub use super::TaskInfo;
    use super::buf_to_string;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    fn name_of_entry(entry: &PROCESSENTRY32) -> String {
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(entry.szExeFile.as_ptr() as *const u8, entry.szExeFile.len())
        };
        let mut n = buf_to_string(bytes);
        // Drop a trailing .exe to match unix-style bare names.
        if let Some(stripped) = n.strip_suffix(".exe") {
            n = stripped.to_string();
        }
        n
    }

    fn with_snapshot<R>(f: impl FnOnce(isize, &mut PROCESSENTRY32) -> R, default: R) -> R {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return default;
            }
            let mut entry: PROCESSENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
            let r = f(snap, &mut entry);
            CloseHandle(snap);
            r
        }
    }

    pub fn process_name(pid: i32) -> String {
        with_snapshot(
            |snap, entry| unsafe {
                let mut ok = Process32First(snap, entry);
                while ok != 0 {
                    if entry.th32ProcessID == pid as u32 {
                        return name_of_entry(entry);
                    }
                    ok = Process32Next(snap, entry);
                }
                "unknown".to_string()
            },
            "unknown".to_string(),
        )
    }

    pub fn for_each_child(parent: i32, mut f: impl FnMut(i32)) {
        let mut kids = Vec::new();
        with_snapshot(
            |snap, entry| unsafe {
                let mut ok = Process32First(snap, entry);
                while ok != 0 {
                    if entry.th32ParentProcessID == parent as u32 && entry.th32ProcessID > 0 {
                        kids.push(entry.th32ProcessID as i32);
                    }
                    ok = Process32Next(snap, entry);
                }
            },
            (),
        );
        for pid in kids {
            f(pid);
        }
    }

    pub fn task_info(pid: i32) -> Option<TaskInfo> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
            if h.is_null() {
                return None;
            }
            let (mut creation, mut exit, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) =
                std::mem::zeroed();
            let times_ok = GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user);
            let mut mem: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            mem.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            let mem_ok =
                GetProcessMemoryInfo(h, &mut mem, std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32);
            CloseHandle(h);
            if times_ok == 0 || mem_ok == 0 {
                return None;
            }
            // FILETIME is 100-ns ticks → ×100 for ns.
            let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) * 100;
            Some(TaskInfo {
                user_ns: ft(user),
                system_ns: ft(kernel),
                rss_bytes: mem.WorkingSetSize as u64,
            })
        }
    }

    pub fn working_directory(_pid: i32) -> Option<String> {
        // Reading another process's cwd requires remote-memory tricks; not needed
        // for throttling. Left unimplemented.
        None
    }

    pub fn scan_named(target: &str) -> Vec<i32> {
        let mut out = Vec::new();
        with_snapshot(
            |snap, entry| unsafe {
                let mut ok = Process32First(snap, entry);
                while ok != 0 {
                    if name_of_entry(entry) == target {
                        out.push(entry.th32ProcessID as i32);
                    }
                    ok = Process32Next(snap, entry);
                }
            },
            (),
        );
        out
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    #[test]
    fn own_process_name_and_cwd() {
        let me = std::process::id() as i32;
        let name = process_name(me);
        assert_ne!(name, "unknown");
        // The test binary's cwd should resolve to something.
        assert!(working_directory(me).is_some());
    }

    #[test]
    fn cpu_and_rss_readable_for_self() {
        let me = std::process::id() as i32;
        assert!(rss_bytes(me) > 0);
        let (u, s) = cpu_times(me).expect("cpu times");
        let _ = (u, s); // both monotonic counters, no strict assertion
    }

    #[test]
    fn child_enumeration_finds_spawned_child() {
        let mut child = Command::new("sleep").arg("30").spawn().expect("spawn");
        let cpid = child.id() as i32;
        std::thread::sleep(Duration::from_millis(80));

        let me = std::process::id() as i32;
        let count = child_count(me);
        assert!(count >= 1, "expected >=1 child, got {count}");

        let mut seen = false;
        for_each_child_with_names(me, |pid, name| {
            if pid == cpid {
                seen = true;
                assert_eq!(name, "sleep");
            }
        });
        assert!(seen, "spawned child not found in enumeration");

        // A "sleep" child is active (not ignored, not unknown).
        let ignored: HashSet<&str> = ["caffeinate"].into_iter().collect();
        assert!(has_active_child(me, &ignored));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn finds_descendant_by_name() {
        let mut child = Command::new("sleep").arg("30").spawn().expect("spawn");
        let cpid = child.id() as i32;
        std::thread::sleep(Duration::from_millis(80));

        let me = std::process::id() as i32;
        assert_eq!(find_descendant_named(me, "sleep", 2), Some(cpid));
        assert_eq!(find_descendant_named(me, "no-such-proc", 2), None);
        // depth 0 finds nothing.
        assert_eq!(find_descendant_named(me, "sleep", 0), None);
        // No agent descendant here.
        assert_eq!(find_agent_pid(me), None);

        let _ = child.kill();
        let _ = child.wait();
    }
}
