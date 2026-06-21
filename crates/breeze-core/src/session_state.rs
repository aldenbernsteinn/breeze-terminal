//! Pure session-state decision logic. The child-process scan is injected as a
//! closure so the core has zero OS dependency and is fully unit-testable;
//! `breeze-platform` supplies the real scan.

use std::collections::HashSet;
use std::sync::LazyLock;

/// The four duty-cycle states a session can be in. `as_str()` is the stable
/// label used in logging and persisted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    RunningTools,
    StreamingAPI,
    WaitingInput,
    DeepIdle,
}

impl SessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::RunningTools => "TOOLS",
            SessionState::StreamingAPI => "STREAMING",
            SessionState::WaitingInput => "WAITING",
            SessionState::DeepIdle => "IDLE",
        }
    }
}

/// Child-process names that indicate tool execution.
pub static TOOL_PROCESS_NAMES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "rg", "git", "node", "python", "python3", "ruby", "bash", "zsh", "sh",
        "swift", "clang", "gcc", "make", "cargo", "go", "npm", "npx", "bun",
        "curl", "find", "grep", "sed", "awk",
    ]
    .into_iter()
    .collect()
});

/// Child names ignored when detecting tool activity.
pub static IGNORED_CHILD_NAMES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["caffeinate"].into_iter().collect());

/// Evaluate the session state from elapsed-time signals.
///
/// `has_active_child` is the injected child scan (the platform layer applies
/// [`IGNORED_CHILD_NAMES`]); `skip_child_scan` lets a subsampled poll avoid the
/// scan cost. `freeze_guard` always forces a real scan before `DeepIdle` so a
/// silent-but-running tool is never frozen mid-run.
pub fn evaluate<F: Fn() -> bool>(
    time_since_last_output: f64,
    time_since_last_keystroke: f64,
    current_state: SessionState,
    skip_child_scan: bool,
    has_active_child: F,
) -> SessionState {
    // Already deep idle: process is SIGSTOP'd; only wake on fresh output.
    if current_state == SessionState::DeepIdle {
        return if time_since_last_output < 2.0 {
            SessionState::StreamingAPI
        } else {
            SessionState::DeepIdle
        };
    }

    let has_tools = if skip_child_scan { false } else { has_active_child() };

    if has_tools {
        SessionState::RunningTools
    } else if time_since_last_output < 2.0 {
        SessionState::StreamingAPI
    } else if time_since_last_keystroke > 90.0 && time_since_last_output > 90.0 {
        // Truly abandoned: no input AND no output for 90s.
        freeze_guard(&has_active_child)
    } else if time_since_last_output > 30.0 {
        // Output silence for 30s → freeze early to save poll cycles.
        freeze_guard(&has_active_child)
    } else {
        SessionState::WaitingInput
    }
}

/// Final gate before `DeepIdle`: a skipped scan could otherwise freeze a silent
/// long-running tool, so force one real scan here.
fn freeze_guard<F: Fn() -> bool>(has_active_child: &F) -> SessionState {
    if has_active_child() {
        SessionState::RunningTools
    } else {
        SessionState::DeepIdle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_values() {
        assert_eq!(SessionState::RunningTools.as_str(), "TOOLS");
        assert_eq!(SessionState::StreamingAPI.as_str(), "STREAMING");
        assert_eq!(SessionState::WaitingInput.as_str(), "WAITING");
        assert_eq!(SessionState::DeepIdle.as_str(), "IDLE");
    }

    #[test]
    fn deep_idle_wakes_on_recent_output() {
        assert_eq!(
            evaluate(0.5, 200.0, SessionState::DeepIdle, false, || true),
            SessionState::StreamingAPI
        );
    }

    #[test]
    fn deep_idle_stays_when_silent() {
        assert_eq!(
            evaluate(50.0, 200.0, SessionState::DeepIdle, false, || true),
            SessionState::DeepIdle
        );
    }

    #[test]
    fn tools_win_when_child_active() {
        assert_eq!(
            evaluate(0.1, 0.1, SessionState::WaitingInput, false, || true),
            SessionState::RunningTools
        );
    }

    #[test]
    fn streaming_on_recent_output_no_tools() {
        assert_eq!(
            evaluate(1.0, 50.0, SessionState::WaitingInput, false, || false),
            SessionState::StreamingAPI
        );
    }

    #[test]
    fn waiting_when_active_but_no_tools_no_recent_output() {
        assert_eq!(
            evaluate(10.0, 10.0, SessionState::WaitingInput, false, || false),
            SessionState::WaitingInput
        );
    }

    #[test]
    fn idle_after_90s_no_input_no_output() {
        assert_eq!(
            evaluate(100.0, 100.0, SessionState::WaitingInput, false, || false),
            SessionState::DeepIdle
        );
    }

    #[test]
    fn freeze_early_after_30s_output_silence() {
        // keystroke recent (so the 90/90 branch is skipped), output silent 40s
        assert_eq!(
            evaluate(40.0, 5.0, SessionState::WaitingInput, false, || false),
            SessionState::DeepIdle
        );
    }

    #[test]
    fn freeze_guard_keeps_tools_if_child_alive() {
        // Would freeze (90/90), but a real scan finds a live child → tools.
        assert_eq!(
            evaluate(100.0, 100.0, SessionState::WaitingInput, false, || true),
            SessionState::RunningTools
        );
    }

    #[test]
    fn skip_child_scan_assumes_no_tools() {
        // skip=true → has_tools forced false even though closure would say true;
        // recent output → streaming (not tools).
        assert_eq!(
            evaluate(1.0, 1.0, SessionState::WaitingInput, true, || true),
            SessionState::StreamingAPI
        );
    }
}
