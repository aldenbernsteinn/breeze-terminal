//! Process classification. The actual child enumeration (per-OS) lives in the
//! platform layer and calls [`classify`] on each child's name.

use crate::session_state::TOOL_PROCESS_NAMES;

/// What a child process is, for lifecycle decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildType {
    /// The sleep-inhibitor; left running on purpose.
    Caffeinate,
    /// A language server (reaped when idle).
    Lsp,
    /// An active tool invocation (keeps the session at full power).
    Tool,
    /// Anything else.
    Other,
}

/// Classify a child process by its name.
pub fn classify(name: &str) -> ChildType {
    match name {
        "caffeinate" => ChildType::Caffeinate,
        "clangd" | "sourcekit-lsp" => ChildType::Lsp,
        n if TOOL_PROCESS_NAMES.contains(n) => ChildType::Tool,
        _ => ChildType::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caffeinate_is_caffeinate() {
        assert_eq!(classify("caffeinate"), ChildType::Caffeinate);
    }

    #[test]
    fn language_servers_are_lsp() {
        assert_eq!(classify("clangd"), ChildType::Lsp);
        assert_eq!(classify("sourcekit-lsp"), ChildType::Lsp);
    }

    #[test]
    fn known_tools_are_tool() {
        assert_eq!(classify("rg"), ChildType::Tool);
        assert_eq!(classify("cargo"), ChildType::Tool);
        assert_eq!(classify("git"), ChildType::Tool);
    }

    #[test]
    fn unknown_is_other() {
        assert_eq!(classify("Finder"), ChildType::Other);
        assert_eq!(classify(""), ChildType::Other);
    }
}
