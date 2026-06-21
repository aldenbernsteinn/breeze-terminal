//! A modal confirmation prompt (e.g. before closing a tab/pane). Esc cancels,
//! Enter confirms. Pure state — the window renders it and routes keys.

#[derive(Default)]
pub struct ConfirmDialog {
    open: bool,
    message: String,
    confirm_label: String,
}

impl ConfirmDialog {
    pub fn new() -> ConfirmDialog {
        ConfirmDialog::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Label for the confirm/primary button (e.g. "Close", "Delete").
    pub fn confirm_label(&self) -> &str {
        &self.confirm_label
    }

    /// Show the prompt with `message` and a default "Confirm" button label.
    pub fn ask(&mut self, message: impl Into<String>) {
        self.ask_labeled(message, "Confirm");
    }

    /// Show the prompt with `message` and an explicit confirm-button label.
    pub fn ask_labeled(&mut self, message: impl Into<String>, confirm_label: impl Into<String>) {
        self.open = true;
        self.message = message.into();
        self.confirm_label = confirm_label.into();
    }

    /// Confirm (Enter / Close button). Returns true if a prompt was open — the
    /// caller then performs the action.
    pub fn confirm(&mut self) -> bool {
        let was_open = self.open;
        self.open = false;
        was_open
    }

    /// Cancel (Esc). Dismisses without acting.
    pub fn cancel(&mut self) {
        self.open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_opens_with_message() {
        let mut d = ConfirmDialog::new();
        assert!(!d.is_open());
        d.ask("Close this tab?");
        assert!(d.is_open());
        assert_eq!(d.message(), "Close this tab?");
    }

    #[test]
    fn confirm_closes_and_reports_open() {
        let mut d = ConfirmDialog::new();
        d.ask("x");
        assert!(d.confirm()); // was open → act
        assert!(!d.is_open());
        assert!(!d.confirm()); // already closed → don't act
    }

    #[test]
    fn cancel_dismisses_without_action() {
        let mut d = ConfirmDialog::new();
        d.ask("x");
        d.cancel();
        assert!(!d.is_open());
    }

    #[test]
    fn ask_labeled_sets_message_and_button_label() {
        let mut d = ConfirmDialog::new();
        d.ask_labeled("Close \"Tab 2\"?", "Close");
        assert!(d.is_open());
        assert_eq!(d.message(), "Close \"Tab 2\"?");
        assert_eq!(d.confirm_label(), "Close");
        // The plain `ask` keeps a sensible default label.
        d.ask("Something?");
        assert_eq!(d.confirm_label(), "Confirm");
    }
}
