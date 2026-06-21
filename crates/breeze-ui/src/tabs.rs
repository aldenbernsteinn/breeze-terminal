//! Tab strip state: the ordered set of open gridspaces (windows of panes) and
//! which one is active. Pure logic; the windowing layer renders + drives it.

#[derive(Debug, Default)]
pub struct TabManager {
    ids: Vec<u64>,
    active: usize,
    next_id: u64,
}

impl TabManager {
    pub fn new() -> TabManager {
        TabManager::default()
    }

    pub fn count(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Active tab index, or `None` when there are no tabs.
    pub fn active_index(&self) -> Option<usize> {
        if self.ids.is_empty() {
            None
        } else {
            Some(self.active)
        }
    }

    pub fn active_id(&self) -> Option<u64> {
        self.ids.get(self.active).copied()
    }

    pub fn ids(&self) -> &[u64] {
        &self.ids
    }

    /// Open a new tab; it becomes active. Returns its id.
    pub fn open(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.ids.push(id);
        self.active = self.ids.len() - 1;
        id
    }

    /// Close the active tab; the next (or previous) tab becomes active. Returns
    /// the closed id.
    pub fn close_active(&mut self) -> Option<u64> {
        if self.ids.is_empty() {
            return None;
        }
        let removed = self.ids.remove(self.active);
        if self.active >= self.ids.len() {
            self.active = self.ids.len().saturating_sub(1);
        }
        Some(removed)
    }

    /// Select a tab by index. Returns false if out of range.
    pub fn select(&mut self, index: usize) -> bool {
        if index < self.ids.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    /// Activate the next tab, wrapping.
    pub fn next(&mut self) {
        if !self.ids.is_empty() {
            self.active = (self.active + 1) % self.ids.len();
        }
    }

    /// Activate the previous tab, wrapping.
    pub fn prev(&mut self) {
        if !self.ids.is_empty() {
            self.active = (self.active + self.ids.len() - 1) % self.ids.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_become_active_with_unique_ids() {
        let mut t = TabManager::new();
        assert_eq!(t.active_index(), None);
        let a = t.open();
        let b = t.open();
        let c = t.open();
        assert_eq!(t.count(), 3);
        assert_eq!(t.active_index(), Some(2));
        assert_eq!(t.active_id(), Some(c));
        assert!(a != b && b != c);
    }

    #[test]
    fn select_and_wrap_navigation() {
        let mut t = TabManager::new();
        for _ in 0..3 {
            t.open();
        }
        assert!(t.select(0));
        assert!(!t.select(9));
        t.prev(); // wraps to last
        assert_eq!(t.active_index(), Some(2));
        t.next(); // wraps to first
        assert_eq!(t.active_index(), Some(0));
    }

    #[test]
    fn close_active_clamps_index() {
        let mut t = TabManager::new();
        let ids: Vec<u64> = (0..3).map(|_| t.open()).collect();
        // active is the last (index 2); closing it clamps active to new last (1).
        let closed = t.close_active();
        assert_eq!(closed, Some(ids[2]));
        assert_eq!(t.count(), 2);
        assert_eq!(t.active_index(), Some(1));
        // Close from the middle: select 0, close → active stays valid.
        t.select(0);
        t.close_active();
        assert_eq!(t.active_id(), Some(ids[1]));
        t.close_active();
        assert_eq!(t.active_index(), None);
        assert_eq!(t.close_active(), None);
    }
}
