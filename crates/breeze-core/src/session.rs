//! Persisted session model: the saved layout of one gridspace (a window of
//! panes) so a session can be recovered on relaunch. Serializes to the same
//! JSON the app reads/writes. Optional fields keep older files loadable.

use crate::split_tree::SplitNode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedGridspace {
    pub name: String,
    #[serde(rename = "paneCount")]
    pub pane_count: i64,

    // All optional for backward-compat: older files without these keys still
    // decode. nil/None omits the key on write. (Unknown keys from older files —
    // e.g. a former `agentRunning`/`resume` — are ignored.)
    #[serde(rename = "workingDirectories", default, skip_serializing_if = "Option::is_none")]
    pub working_directories: Option<Vec<Option<String>>>,
    #[serde(rename = "splitTree", default, skip_serializing_if = "Option::is_none")]
    pub split_tree: Option<SplitNode>,
    #[serde(rename = "paneIDs", default, skip_serializing_if = "Option::is_none")]
    pub pane_ids: Option<Vec<i64>>,
    #[serde(rename = "focusedPaneIndex", default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_index: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split_tree::{PaneID, SplitDirection, SplitNode};

    fn sample() -> SavedGridspace {
        SavedGridspace {
            name: "work".to_string(),
            pane_count: 2,
            working_directories: Some(vec![Some("/tmp".to_string()), None]),
            split_tree: Some(SplitNode::split(
                SplitDirection::Vertical,
                0.5,
                SplitNode::leaf(PaneID::new(1)),
                SplitNode::leaf(PaneID::new(2)),
            )),
            pane_ids: Some(vec![1, 2]),
            focused_pane_index: Some(0),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let list = vec![sample()];
        let json = serde_json::to_string(&list).unwrap();
        let back: Vec<SavedGridspace> = serde_json::from_str(&json).unwrap();
        assert_eq!(list, back);
    }

    #[test]
    fn json_field_names_are_camelcase() {
        let v = serde_json::to_value(sample()).unwrap();
        assert_eq!(v["name"], "work");
        assert_eq!(v["paneCount"], 2);
        assert_eq!(v["paneIDs"], serde_json::json!([1, 2]));
        assert_eq!(v["focusedPaneIndex"], 0);
        // Nested split tree keeps its own serialized shape.
        assert_eq!(v["splitTree"]["type"], "split");
        assert_eq!(v["splitTree"]["first"]["paneID"]["id"], 1);
    }

    #[test]
    fn nil_optionals_are_omitted() {
        let g = SavedGridspace {
            name: "g".to_string(),
            pane_count: 1,
            working_directories: None,
            split_tree: None,
            pane_ids: None,
            focused_pane_index: None,
        };
        let v = serde_json::to_value(&g).unwrap();
        assert!(v.get("splitTree").is_none());
        assert!(v.get("focusedPaneIndex").is_none());
    }

    #[test]
    fn old_file_without_optional_keys_still_decodes() {
        // A minimal early-format entry: only the required keys.
        let json = r#"[{"name":"old","paneCount":1,"agentRunning":[false]}]"#;
        let list: Vec<SavedGridspace> = serde_json::from_str(json).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].pane_count, 1);
        assert!(list[0].split_tree.is_none());
        assert!(list[0].pane_ids.is_none());
    }
}
