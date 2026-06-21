//! A terminal with the efficiency engine attached: once an agent process
//! appears under the shell, a [`SessionManager`] is bound to it so its CPU gets
//! state-throttled. Keystrokes and output are fed to the detector.

use crate::proc;
use crate::session_manager::SessionManager;
use crate::terminal::Terminal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a pane's child output must be quiet before the pane counts as
/// "settled" — long enough to ignore a momentary pause, short enough to notice
/// when a background process actually finishes.
const SETTLE_QUIET: Duration = Duration::from_millis(600);

pub struct ManagedTerminal {
    terminal: Terminal,
    session: Arc<Mutex<Option<SessionManager>>>,
    /// Set to the agent's RSS (GB) when it crosses the high-memory threshold.
    memory_alert: Arc<Mutex<Option<f64>>>,
}

impl ManagedTerminal {
    /// Spawn a shell-hosted terminal whose output is fed to the (later-attached)
    /// session detector.
    pub fn spawn(
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> std::io::Result<ManagedTerminal> {
        ManagedTerminal::spawn_with_notify(program, args, env, cwd, cols, rows, breeze_vt::Vt::DEFAULT_HISTORY, None)
    }

    /// Like [`ManagedTerminal::spawn`], but `notify` is also called on each
    /// chunk of output — a UI uses it to wake its event loop for a redraw
    /// (event-driven, no idle spin) — and the scrollback depth is explicit.
    pub fn spawn_with_notify(
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
        history: usize,
        mut notify: Option<Box<dyn FnMut() + Send>>,
    ) -> std::io::Result<ManagedTerminal> {
        let session: Arc<Mutex<Option<SessionManager>>> = Arc::new(Mutex::new(None));
        let out_session = Arc::clone(&session);
        let on_output = Box::new(move || {
            if let Ok(guard) = out_session.lock() {
                if let Some(sm) = guard.as_ref() {
                    sm.record_output();
                }
            }
            if let Some(n) = notify.as_mut() {
                n();
            }
        });
        let terminal =
            Terminal::spawn_with_output(program, args, env, cwd, cols, rows, history, Some(on_output))?;
        Ok(ManagedTerminal { terminal, session, memory_alert: Arc::new(Mutex::new(None)) })
    }

    /// If no engine is attached yet, look for an agent process under the shell
    /// and bind a [`SessionManager`] to it. Returns whether an engine is now
    /// attached. Cheap to call repeatedly (e.g. on a UI tick).
    pub fn poll_attach(&self) -> bool {
        if self.session.lock().unwrap().is_some() {
            return true;
        }
        let Some(shell_pid) = self.terminal.child_pid() else {
            return false;
        };
        if let Some(agent) = proc::find_agent_pid(shell_pid as i32) {
            let sm = SessionManager::new(agent);
            let alert = Arc::clone(&self.memory_alert);
            sm.on_memory_alert(move |gb| {
                if let Ok(mut a) = alert.lock() {
                    *a = Some(gb);
                }
            });
            sm.start();
            *self.session.lock().unwrap() = Some(sm);
            true
        } else {
            false
        }
    }

    /// True once the engine is bound to an agent process.
    pub fn is_attached(&self) -> bool {
        self.session.lock().unwrap().is_some()
    }

    fn record_keystroke(&self) {
        if let Some(sm) = self.session.lock().unwrap().as_ref() {
            sm.record_keystroke();
        }
    }

    /// Tab/pane went off-screen: freeze the agent (deep idle → ~0% CPU).
    pub fn enter_background(&self) {
        if let Some(sm) = self.session.lock().unwrap().as_ref() {
            sm.force_deep_idle();
        }
    }

    /// Tab/pane became visible again: resume from the forced freeze.
    pub fn enter_foreground(&self) {
        if let Some(sm) = self.session.lock().unwrap().as_ref() {
            sm.resume_from_forced();
        }
    }

    /// Propagate the system low-power state to the throttle (tighter duty cycles).
    pub fn set_low_power(&self, enabled: bool) {
        if let Some(sm) = self.session.lock().unwrap().as_ref() {
            sm.set_low_power_mode(enabled);
        }
    }

    /// Write input; also notifies the detector of a keystroke (typing boost).
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.record_keystroke();
        self.terminal.write(bytes)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        self.terminal.resize(cols, rows)
    }

    pub fn scroll(&self, delta: i32) {
        self.terminal.scroll(delta);
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.terminal.child_pid()
    }

    /// Whether the underlying shell has exited (its output stream ended).
    pub fn has_exited(&self) -> bool {
        self.terminal.has_exited()
    }

    /// Whether the pane is idle — its shell has no active child (sitting at a
    /// prompt, nothing running). Used to decide which panes a shrink may drop.
    pub fn is_idle(&self) -> bool {
        match self.terminal.child_pid() {
            Some(pid) => !crate::proc::has_active_child(
                pid as i32,
                &breeze_core::session_state::IGNORED_CHILD_NAMES,
            ),
            None => true,
        }
    }

    /// Scrollback viewport position: `(offset_from_bottom, total_lines)`.
    pub fn scroll_position(&self) -> (usize, usize) {
        self.terminal.scroll_position()
    }

    /// The agent's RSS in GB if it has crossed the high-memory threshold.
    pub fn memory_alert_gb(&self) -> Option<f64> {
        *self.memory_alert.lock().unwrap()
    }

    /// The shell's current working directory (via libproc), if readable.
    pub fn cwd(&self) -> Option<String> {
        self.terminal.child_pid().and_then(|pid| crate::proc::working_directory(pid as i32))
    }

    pub fn row_text(&self, row: usize) -> String {
        self.terminal.row_text(row)
    }

    pub fn screen_text(&self) -> String {
        self.terminal.screen_text()
    }

    /// The visible screen as colored cells (for the color renderer).
    pub fn screen_cells(&self) -> Vec<Vec<breeze_vt::Cell>> {
        self.terminal.screen_cells()
    }

    /// Cursor position `(row, col)` in visible-screen coordinates.
    pub fn cursor_pos(&self) -> (usize, usize) {
        self.terminal.cursor_pos()
    }

    pub fn title(&self) -> Option<String> {
        self.terminal.title()
    }

    /// Whether the pane has unacknowledged output that has gone quiet — a
    /// background process finished or paused on its own. A pane that keeps
    /// emitting output (or has none) is not settled.
    pub fn is_settled(&self) -> bool {
        self.terminal.is_settled(SETTLE_QUIET)
    }

    /// Acknowledge current activity (the pane is focused/visible), clearing the
    /// settled state until new output arrives.
    pub fn ack_activity(&self) {
        self.terminal.ack();
    }

    /// Print a notice into this pane's grid (display-only, not sent to the child).
    pub fn display(&self, text: &str) {
        self.terminal.display(text);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn no_agent_means_not_attached() {
        // A plain `cat` has no agent child, so the engine never attaches.
        let mt = ManagedTerminal::spawn("/bin/cat", &[], &[("TERM", "xterm-256color")], None, 40, 10)
            .expect("spawn");
        std::thread::sleep(Duration::from_millis(80));
        assert!(!mt.poll_attach());
        assert!(!mt.is_attached());
    }
}
