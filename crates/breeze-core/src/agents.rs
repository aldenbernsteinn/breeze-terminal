//! The AI coding agents Breeze recognizes — used to detect a resumable/managed
//! agent under a pane's shell (for throttling and the orphan scan).

/// Process (binary) names of the AI coding agents Breeze recognizes. Single
/// source of truth for detection and the orphan scan.
pub const AGENT_PROCESS_NAMES: &[&str] = &["claude", "codex", "gemini", "opencode"];

/// The agent names plus the Node runtime several of them run under — used when
/// validating a throttled PID by name (a Node-based agent reports as `node`).
pub const AGENT_THROTTLE_NAMES: &[&str] = &["claude", "codex", "gemini", "opencode", "node"];

/// Whether `name` is one of the recognized built-in agent processes.
pub fn is_agent_process(name: &str) -> bool {
    AGENT_PROCESS_NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_built_in_agents() {
        for name in AGENT_PROCESS_NAMES {
            assert!(is_agent_process(name));
        }
        assert!(!is_agent_process("bash"));
        assert!(!is_agent_process(""));
    }
}
