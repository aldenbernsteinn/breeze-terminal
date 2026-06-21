//! Named split-layout presets, stored as JSON under `~/.breeze/layouts/`. Each
//! preset is a saved `SplitNode` arrangement the user can recall. Cross-platform
//! (plain file IO), like `session_store`.

use breeze_core::split_tree::SplitNode;
use std::path::{Path, PathBuf};

pub fn layouts_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".breeze/layouts")
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

pub fn save(name: &str, tree: &SplitNode) -> std::io::Result<()> {
    save_in(&layouts_dir(), name, tree)
}
pub fn load(name: &str) -> Option<SplitNode> {
    load_in(&layouts_dir(), name)
}
pub fn list() -> Vec<String> {
    list_in(&layouts_dir())
}
pub fn delete(name: &str) -> std::io::Result<()> {
    delete_in(&layouts_dir(), name)
}

pub fn save_in(dir: &Path, name: &str, tree: &SplitNode) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(tree)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(dir.join(format!("{}.json", sanitize(name))), json)
}

pub fn load_in(dir: &Path, name: &str) -> Option<SplitNode> {
    let data = std::fs::read(dir.join(format!("{}.json", sanitize(name)))).ok()?;
    serde_json::from_slice(&data).ok()
}

pub fn list_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("json") {
                p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

pub fn delete_in(dir: &Path, name: &str) -> std::io::Result<()> {
    match std::fs::remove_file(dir.join(format!("{}.json", sanitize(name)))) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use breeze_core::split_tree::{PaneID, SplitDirection, SplitNode};

    fn tmp() -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("breeze-layouts-{}-{}", std::process::id(), n))
    }

    #[test]
    fn save_list_load_delete() {
        let dir = tmp();
        let tree = SplitNode::split(
            SplitDirection::Vertical,
            0.5,
            SplitNode::leaf(PaneID::new(0)),
            SplitNode::leaf(PaneID::new(1)),
        );
        save_in(&dir, "two up", &tree).unwrap();
        // Name is sanitized ("two up" → "two_up").
        assert_eq!(list_in(&dir), vec!["two_up".to_string()]);
        assert_eq!(load_in(&dir, "two up"), Some(tree));
        delete_in(&dir, "two up").unwrap();
        assert!(list_in(&dir).is_empty());
        assert_eq!(load_in(&dir, "two up"), None);
    }
}
