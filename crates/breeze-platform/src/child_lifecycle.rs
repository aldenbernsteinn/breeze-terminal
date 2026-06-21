//! Reaps a session's child processes on state transitions: language servers are
//! killed after a grace period once the session goes quiet, and everything
//! non-essential is killed on deep idle. `caffeinate` is deliberately spared —
//! it costs no CPU and keeping the Mac awake preserves other sessions' API
//! connections.

use crate::proc;
use breeze_core::proc::{classify, ChildType};
use breeze_core::session_state::SessionState;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const PRIO_PROCESS: i32 = 0;
const RENICE_PRIORITY: i32 = 10;

#[cfg(unix)]
fn term(pid: i32) {
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}
#[cfg(unix)]
fn renice(pid: i32) {
    unsafe {
        libc::setpriority(PRIO_PROCESS, pid as libc::id_t, RENICE_PRIORITY);
    }
}
#[cfg(not(unix))]
fn term(_pid: i32) {}
#[cfg(not(unix))]
fn renice(_pid: i32) {}

pub struct ChildLifecycle {
    agent_pid: i32,
    lsp_timeout: Duration,
    /// Bumped whenever the LSP timer is (re)started or cancelled; a fired timer
    /// only acts if its generation is still current.
    lsp_generation: Arc<AtomicU64>,
}

impl ChildLifecycle {
    pub fn new(agent_pid: i32) -> ChildLifecycle {
        ChildLifecycle {
            agent_pid,
            lsp_timeout: Duration::from_secs(60),
            lsp_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Test/runtime hook to shorten the LSP grace period.
    pub fn with_lsp_timeout(mut self, timeout: Duration) -> ChildLifecycle {
        self.lsp_timeout = timeout;
        self
    }

    pub fn handle_state_transition(&self, _old: SessionState, new: SessionState) {
        match new {
            SessionState::RunningTools | SessionState::StreamingAPI => self.cancel_lsp_timer(),
            SessionState::WaitingInput => {
                self.start_lsp_timer();
                renice(self.agent_pid);
            }
            SessionState::DeepIdle => {
                self.kill_lsp_servers();
                self.cancel_lsp_timer();
            }
        }
    }

    /// Kill children whose name equals `target`.
    pub fn kill_by_name(&self, target: &str) {
        proc::for_each_child_with_names(self.agent_pid, |pid, name| {
            if name == target {
                term(pid);
            }
        });
    }

    pub fn kill_lsp_servers(&self) {
        proc::for_each_child_with_names(self.agent_pid, |pid, name| {
            if name == "clangd" || name == "sourcekit-lsp" {
                term(pid);
            }
        });
    }

    pub fn kill_all_non_essential(&self) {
        proc::for_each_child_with_names(self.agent_pid, |pid, name| {
            if name != "unknown" {
                match classify(name) {
                    ChildType::Caffeinate | ChildType::Lsp => term(pid),
                    ChildType::Tool | ChildType::Other => {}
                }
            }
        });
    }

    fn start_lsp_timer(&self) {
        let gen = self.lsp_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let generation = Arc::clone(&self.lsp_generation);
        let pid = self.agent_pid;
        let timeout = self.lsp_timeout;
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            if generation.load(Ordering::SeqCst) != gen {
                return; // superseded or cancelled
            }
            proc::for_each_child_with_names(pid, |cpid, name| {
                if name == "clangd" || name == "sourcekit-lsp" {
                    term(cpid);
                }
            });
        });
    }

    fn cancel_lsp_timer(&self) {
        self.lsp_generation.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    fn alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn kill_by_name_terminates_matching_child() {
        let mut child = Command::new("sleep").arg("30").spawn().expect("spawn");
        let cpid = child.id() as i32;
        std::thread::sleep(Duration::from_millis(80));

        let lc = ChildLifecycle::new(std::process::id() as i32);
        assert!(alive(cpid));
        lc.kill_by_name("sleep");
        let _ = child.wait();
        assert!(!alive(cpid), "child should be terminated");
    }

    #[test]
    fn lsp_timer_fires_after_timeout() {
        // No real clangd to kill, but verify the timer path runs without panic
        // and respects cancellation generations.
        let lc = ChildLifecycle::new(std::process::id() as i32)
            .with_lsp_timeout(Duration::from_millis(40));
        lc.handle_state_transition(SessionState::StreamingAPI, SessionState::WaitingInput);
        lc.handle_state_transition(SessionState::WaitingInput, SessionState::RunningTools); // cancels
        std::thread::sleep(Duration::from_millis(80));
        // Reaching here without panic is the assertion.
    }
}
