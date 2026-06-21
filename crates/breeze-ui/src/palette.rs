//! Command/settings palette state: an overlay with a query box that fuzzy-
//! filters a list of commands. Pure logic — the window renders `visible()` and
//! drives it from key events.

use breeze_core::fuzzy::fuzzy_score;

pub struct Palette {
    open: bool,
    items: Vec<String>,
    query: String,
    /// Indices into `items`, ranked best-first for the current query.
    filtered: Vec<usize>,
    selected: usize,
}

impl Palette {
    pub fn new(items: Vec<String>) -> Palette {
        let mut p = Palette { open: false, items, query: String::new(), filtered: Vec::new(), selected: 0 };
        p.refilter();
        p
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.refilter();
    }

    /// Replace the command list (e.g. switch between command and settings modes).
    pub fn set_items(&mut self, items: Vec<String>) {
        self.items = items;
        self.selected = 0;
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.refilter();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.refilter();
    }

    /// Move the selection by `delta`, clamped to the visible range.
    pub fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.filtered.len() as i32 - 1;
        self.selected = (self.selected as i32 + delta).clamp(0, max) as usize;
    }

    /// The visible (filtered, ranked) command titles.
    pub fn visible(&self) -> Vec<&str> {
        self.filtered.iter().map(|&i| self.items[i].as_str()).collect()
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The currently highlighted command, if any.
    pub fn selected_item(&self) -> Option<&str> {
        self.filtered.get(self.selected).map(|&i| self.items[i].as_str())
    }

    fn refilter(&mut self) {
        let q: Vec<char> = self.query.to_lowercase().chars().collect();
        let mut scored: Vec<(usize, i32)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| fuzzy_score(&q, &item.to_lowercase()).map(|s| (i, s)))
            .collect();
        scored.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        self.filtered = scored.into_iter().map(|(i, _)| i).collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette::new(vec![
            "New Tab".into(),
            "New Window".into(),
            "Close Pane".into(),
            "Split Right".into(),
            "Settings".into(),
        ])
    }

    #[test]
    fn open_close_resets_query() {
        let mut p = palette();
        assert!(!p.is_open());
        p.push_char('x');
        p.open();
        assert!(p.is_open());
        assert_eq!(p.query(), "");
        p.close();
        assert!(!p.is_open());
    }

    #[test]
    fn typing_filters_and_backspace_restores() {
        let mut p = palette();
        p.open();
        p.push_char('n');
        p.push_char('w');
        let vis = p.visible();
        assert!(vis.contains(&"New Window"));
        assert!(!vis.contains(&"Settings"));
        p.backspace();
        p.backspace();
        // Empty query → all items visible again.
        assert_eq!(p.visible().len(), 5);
    }

    #[test]
    fn selection_moves_and_clamps_and_activates() {
        let mut p = palette();
        p.open();
        assert_eq!(p.selected_index(), 0);
        p.move_selection(-5); // clamps at 0
        assert_eq!(p.selected_index(), 0);
        p.move_selection(2);
        assert_eq!(p.selected_index(), 2);
        p.move_selection(100); // clamps at last
        assert_eq!(p.selected_index(), p.visible().len() - 1);
        assert!(p.selected_item().is_some());
    }

    #[test]
    fn no_match_yields_no_selection() {
        let mut p = palette();
        p.open();
        for c in "zzzzz".chars() {
            p.push_char(c);
        }
        assert!(p.visible().is_empty());
        assert_eq!(p.selected_item(), None);
    }
}
