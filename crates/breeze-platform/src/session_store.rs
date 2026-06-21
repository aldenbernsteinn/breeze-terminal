//! Reads/writes the saved-session file (`~/.breeze/session.json`) used to
//! recover a window's gridspaces on relaunch. Writes are atomic (temp file +
//! rename) so a crash mid-write can't corrupt an existing session.

use breeze_core::session::SavedGridspace;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `~/.breeze/session.json`.
pub fn session_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".breeze/session.json")
}

/// Persist the gridspaces. An empty list removes the file (next launch is fresh).
pub fn save(gridspaces: &[SavedGridspace]) -> std::io::Result<()> {
    save_to(&session_path(), gridspaces)
}

/// Load saved gridspaces, or `None` if absent/unreadable/corrupt.
pub fn load() -> Option<Vec<SavedGridspace>> {
    load_from(&session_path())
}

/// Remove the session file.
pub fn clear() -> std::io::Result<()> {
    clear_at(&session_path())
}

pub fn save_to(path: &Path, gridspaces: &[SavedGridspace]) -> std::io::Result<()> {
    if gridspaces.is_empty() {
        return clear_at(path);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec(gridspaces)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Atomic: write a temp file in the same dir, then rename over the target.
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn load_from(path: &Path) -> Option<Vec<SavedGridspace>> {
    let data = std::fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

pub fn clear_at(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use breeze_core::split_tree::{PaneID, SplitDirection, SplitNode};

    fn tmp_path() -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("breeze-sess-{}-{}/session.json", std::process::id(), n))
    }

    fn sample() -> Vec<SavedGridspace> {
        vec![
            SavedGridspace {
                name: "a".to_string(),
                pane_count: 2,
                working_directories: Some(vec![Some("/tmp".to_string()), None]),
                split_tree: Some(SplitNode::split(
                    SplitDirection::Horizontal,
                    0.6,
                    SplitNode::leaf(PaneID::new(1)),
                    SplitNode::leaf(PaneID::new(2)),
                )),
                pane_ids: Some(vec![1, 2]),
                focused_pane_index: Some(1),
            },
            SavedGridspace {
                name: "b".to_string(),
                pane_count: 1,
                working_directories: None,
                split_tree: None,
                pane_ids: None,
                focused_pane_index: None,
            },
        ]
    }

    #[test]
    fn save_load_round_trip() {
        let path = tmp_path();
        let want = sample();
        save_to(&path, &want).unwrap();
        let got = load_from(&path).expect("load");
        assert_eq!(got, want);
    }

    #[test]
    fn empty_clears_and_load_absent_is_none() {
        let path = tmp_path();
        save_to(&path, &sample()).unwrap();
        assert!(path.exists());
        // Saving an empty list removes the file.
        save_to(&path, &[]).unwrap();
        assert!(!path.exists());
        assert!(load_from(&path).is_none());
    }

    #[test]
    fn corrupt_file_loads_as_none() {
        let path = tmp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        assert!(load_from(&path).is_none());
    }
}
