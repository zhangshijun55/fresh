//! Pure helpers used by `WidgetCommand` dispatch.
//!
//! These are factored out of the plugin-dispatch module so they can
//! be tested without spinning up an `Editor`. The widget runtime's
//! state mutations are intentionally pure functions of (current
//! widget state, requested action) → next state — the dispatcher
//! reads from the registry, calls these, and fires events.

use fresh_core::api::{TreeNode, WidgetSpec};
use fresh_core::text_property::TextPropertyEntry;

/// Locate a widget node in a spec tree by its stable `key`. Returns
/// the matched node, or `None` if no widget has that key.
///
/// Walks `Row`/`Col` children. Skips `Raw`/`HintBar`/`Spacer` (those
/// kinds either have no key worth dispatching to, or no interactive
/// behaviour at all).
pub fn find_widget_by_key<'a>(spec: &'a WidgetSpec, target: &str) -> Option<&'a WidgetSpec> {
    if target.is_empty() {
        return None;
    }
    match spec {
        WidgetSpec::Row { children, .. } | WidgetSpec::Col { children, .. } => {
            for c in children {
                if let Some(found) = find_widget_by_key(c, target) {
                    return Some(found);
                }
            }
            None
        }
        WidgetSpec::Toggle { key: Some(k), .. }
        | WidgetSpec::Button { key: Some(k), .. }
        | WidgetSpec::TextInput { key: Some(k), .. }
        | WidgetSpec::List { key: Some(k), .. }
        | WidgetSpec::Tree { key: Some(k), .. }
            if k == target =>
        {
            Some(spec)
        }
        _ => None,
    }
}

/// Apply a non-printable editing key to a `(value, cursor)` pair,
/// returning `(new_value, new_cursor)`. `cursor` is a UTF-8 byte
/// offset clamped to `[0, value.len()]`.
///
/// Recognised keys: `"Backspace"`, `"Delete"`, `"Left"`, `"Right"`,
/// `"Home"`, `"End"`. Any other key string is a no-op.
///
/// All boundary handling respects UTF-8 char boundaries so the
/// renderer's cursor-byte logic doesn't land in the middle of a
/// multi-byte character. (`Left`/`Right` step by *grapheme* later
/// — for v1 we step by char, which is wrong for combining marks
/// but acceptable until a higher-fidelity grapheme iterator lands.)
pub fn apply_text_input_key(value: &str, cursor: usize, key: &str) -> (String, usize) {
    let cursor = cursor.min(value.len());
    match key {
        "Backspace" => {
            if cursor == 0 {
                return (value.to_string(), 0);
            }
            // Find the start of the previous char.
            let mut prev = cursor - 1;
            while prev > 0 && !value.is_char_boundary(prev) {
                prev -= 1;
            }
            let mut new_value = String::with_capacity(value.len() - (cursor - prev));
            new_value.push_str(&value[..prev]);
            new_value.push_str(&value[cursor..]);
            (new_value, prev)
        }
        "Delete" => {
            if cursor >= value.len() {
                return (value.to_string(), cursor);
            }
            // Find the start of the next char.
            let mut next = cursor + 1;
            while next < value.len() && !value.is_char_boundary(next) {
                next += 1;
            }
            let mut new_value = String::with_capacity(value.len() - (next - cursor));
            new_value.push_str(&value[..cursor]);
            new_value.push_str(&value[next..]);
            (new_value, cursor)
        }
        "Left" => {
            if cursor == 0 {
                return (value.to_string(), 0);
            }
            let mut prev = cursor - 1;
            while prev > 0 && !value.is_char_boundary(prev) {
                prev -= 1;
            }
            (value.to_string(), prev)
        }
        "Right" => {
            if cursor >= value.len() {
                return (value.to_string(), value.len());
            }
            let mut next = cursor + 1;
            while next < value.len() && !value.is_char_boundary(next) {
                next += 1;
            }
            (value.to_string(), next)
        }
        "Home" => (value.to_string(), 0),
        "End" => (value.to_string(), value.len()),
        _ => (value.to_string(), cursor),
    }
}

/// In-place mutate a `Toggle`'s `checked` field by walking the
/// spec tree and matching on `widget_key`. Used by the
/// `WidgetMutate::SetChecked` IPC fast path.
///
/// Returns true when a matching Toggle was found and updated.
pub fn set_toggle_checked_in_spec(
    spec: &mut WidgetSpec,
    widget_key: &str,
    new_checked: bool,
) -> bool {
    if widget_key.is_empty() {
        return false;
    }
    match spec {
        WidgetSpec::Row { children, .. } | WidgetSpec::Col { children, .. } => {
            for c in children {
                if set_toggle_checked_in_spec(c, widget_key, new_checked) {
                    return true;
                }
            }
            false
        }
        WidgetSpec::Toggle { checked, key, .. } => {
            if key.as_deref() == Some(widget_key) {
                *checked = new_checked;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// In-place mutate a `List`'s `items` and `item_keys` fields.
/// Returns true when a matching List was found and updated.
pub fn set_list_items_in_spec(
    spec: &mut WidgetSpec,
    widget_key: &str,
    new_items: Vec<TextPropertyEntry>,
    new_item_keys: Vec<String>,
) -> bool {
    if widget_key.is_empty() {
        return false;
    }
    match spec {
        WidgetSpec::Row { children, .. } | WidgetSpec::Col { children, .. } => {
            // Workaround: take ownership of items to avoid double-borrow on recursive call
            for c in children.iter_mut() {
                if c.contains_key(widget_key) {
                    return set_list_items_in_spec(c, widget_key, new_items, new_item_keys);
                }
            }
            false
        }
        WidgetSpec::List {
            items,
            item_keys,
            key,
            ..
        } => {
            if key.as_deref() == Some(widget_key) {
                *items = new_items;
                *item_keys = new_item_keys;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// In-place mutate a `Tree`'s `nodes` and `item_keys` fields.
/// Returns true when a matching Tree was found and updated.
///
/// Note: this does *not* touch instance state (selected_index,
/// scroll, expanded_keys). The renderer will clamp the previous
/// selection to a now-visible node and orphan-discard expanded
/// keys that no longer match any item key on the next render.
pub fn set_tree_nodes_in_spec(
    spec: &mut WidgetSpec,
    widget_key: &str,
    new_nodes: Vec<TreeNode>,
    new_item_keys: Vec<String>,
) -> bool {
    if widget_key.is_empty() {
        return false;
    }
    match spec {
        WidgetSpec::Row { children, .. } | WidgetSpec::Col { children, .. } => {
            for c in children.iter_mut() {
                if c.contains_key(widget_key) {
                    return set_tree_nodes_in_spec(c, widget_key, new_nodes, new_item_keys);
                }
            }
            false
        }
        WidgetSpec::Tree {
            nodes,
            item_keys,
            key,
            ..
        } => {
            if key.as_deref() == Some(widget_key) {
                *nodes = new_nodes;
                *item_keys = new_item_keys;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Resolve the absolute `nodes` index of the parent of `child_idx`
/// in a `Tree`. The parent of a node at depth `d` is the most recent
/// earlier node at depth `d - 1`. Returns `None` for top-level nodes
/// (depth 0) and for out-of-range indices.
pub fn tree_parent_index(nodes: &[TreeNode], child_idx: usize) -> Option<usize> {
    let child = nodes.get(child_idx)?;
    if child.depth == 0 {
        return None;
    }
    let target_depth = child.depth - 1;
    nodes[..child_idx]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, n)| n.depth == target_depth)
        .map(|(i, _)| i)
}

/// Recursive helper for `set_*_in_spec` — does this
/// subtree contain a widget (any kind) with `widget_key`?
trait ContainsKey {
    fn contains_key(&self, widget_key: &str) -> bool;
}

impl ContainsKey for WidgetSpec {
    fn contains_key(&self, widget_key: &str) -> bool {
        match self {
            WidgetSpec::Row { children, .. } | WidgetSpec::Col { children, .. } => {
                children.iter().any(|c| c.contains_key(widget_key))
            }
            WidgetSpec::Toggle { key, .. }
            | WidgetSpec::Button { key, .. }
            | WidgetSpec::TextInput { key, .. }
            | WidgetSpec::List { key, .. }
            | WidgetSpec::Tree { key, .. } => key.as_deref() == Some(widget_key),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toggle_with_key(k: &str) -> WidgetSpec {
        WidgetSpec::Toggle {
            checked: false,
            label: "T".into(),
            focused: false,
            key: Some(k.into()),
        }
    }

    #[test]
    fn find_widget_by_key_finds_top_level_match() {
        let spec = toggle_with_key("a");
        assert!(find_widget_by_key(&spec, "a").is_some());
        assert!(find_widget_by_key(&spec, "b").is_none());
    }

    #[test]
    fn find_widget_by_key_recurses_into_row() {
        let spec = WidgetSpec::Row {
            children: vec![toggle_with_key("a"), toggle_with_key("b")],
            key: None,
        };
        assert!(find_widget_by_key(&spec, "b").is_some());
    }

    #[test]
    fn find_widget_by_key_returns_none_for_empty_target() {
        let spec = toggle_with_key("a");
        assert!(find_widget_by_key(&spec, "").is_none());
    }

    #[test]
    fn backspace_at_start_is_noop() {
        assert_eq!(
            apply_text_input_key("hello", 0, "Backspace"),
            ("hello".into(), 0)
        );
    }

    #[test]
    fn backspace_in_middle_removes_previous_char() {
        assert_eq!(
            apply_text_input_key("hello", 3, "Backspace"),
            ("helo".into(), 2)
        );
    }

    #[test]
    fn backspace_at_end_removes_last_char() {
        assert_eq!(
            apply_text_input_key("hello", 5, "Backspace"),
            ("hell".into(), 4)
        );
    }

    #[test]
    fn delete_at_end_is_noop() {
        assert_eq!(
            apply_text_input_key("hello", 5, "Delete"),
            ("hello".into(), 5)
        );
    }

    #[test]
    fn delete_in_middle_removes_next_char() {
        assert_eq!(
            apply_text_input_key("hello", 2, "Delete"),
            ("helo".into(), 2)
        );
    }

    #[test]
    fn left_decrements_cursor() {
        assert_eq!(apply_text_input_key("abc", 2, "Left"), ("abc".into(), 1));
    }

    #[test]
    fn right_increments_cursor_until_end() {
        assert_eq!(apply_text_input_key("abc", 1, "Right"), ("abc".into(), 2));
        assert_eq!(apply_text_input_key("abc", 3, "Right"), ("abc".into(), 3));
    }

    #[test]
    fn home_jumps_to_zero() {
        assert_eq!(apply_text_input_key("abc", 2, "Home"), ("abc".into(), 0));
    }

    #[test]
    fn end_jumps_to_value_len() {
        assert_eq!(apply_text_input_key("abc", 1, "End"), ("abc".into(), 3));
    }

    #[test]
    fn unknown_key_is_noop() {
        assert_eq!(apply_text_input_key("abc", 1, "Wat"), ("abc".into(), 1));
    }

    #[test]
    fn backspace_handles_multibyte_chars() {
        // "héllo" — 'é' is 2 bytes (0xC3 0xA9).
        let s = "héllo";
        // Cursor after 'é' (byte 3). Backspace removes 'é'.
        let (new_value, new_cursor) = apply_text_input_key(s, 3, "Backspace");
        assert_eq!(new_value, "hllo");
        assert_eq!(new_cursor, 1);
    }

    #[test]
    fn left_handles_multibyte_chars() {
        let s = "héllo";
        // From byte 3 (after 'é'), Left goes to byte 1 (before 'é').
        let (_, cursor) = apply_text_input_key(s, 3, "Left");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn right_handles_multibyte_chars() {
        let s = "héllo";
        // From byte 1 (before 'é'), Right goes to byte 3 (after 'é').
        let (_, cursor) = apply_text_input_key(s, 1, "Right");
        assert_eq!(cursor, 3);
    }

    fn node(text: &str, depth: u32, has_children: bool) -> TreeNode {
        TreeNode {
            text: TextPropertyEntry::text(text),
            depth,
            has_children,
        }
    }

    #[test]
    fn tree_parent_index_top_level_returns_none() {
        let nodes = vec![node("root", 0, true)];
        assert!(tree_parent_index(&nodes, 0).is_none());
    }

    #[test]
    fn tree_parent_index_finds_immediate_parent() {
        let nodes = vec![
            node("root", 0, true),
            node("child", 1, false),
            node("child2", 1, false),
        ];
        assert_eq!(tree_parent_index(&nodes, 1), Some(0));
        assert_eq!(tree_parent_index(&nodes, 2), Some(0));
    }

    #[test]
    fn tree_parent_index_skips_intermediate_siblings() {
        // root, child, grandchild → grandchild's parent is child (idx 1).
        let nodes = vec![
            node("root", 0, true),
            node("child", 1, true),
            node("grand", 2, false),
        ];
        assert_eq!(tree_parent_index(&nodes, 2), Some(1));
    }

    #[test]
    fn tree_parent_index_finds_parent_across_unrelated_subtree() {
        // root_a, child_a, root_b, child_b — child_b's parent is root_b (idx 2),
        // not root_a.
        let nodes = vec![
            node("a", 0, true),
            node("a.0", 1, false),
            node("b", 0, true),
            node("b.0", 1, false),
        ];
        assert_eq!(tree_parent_index(&nodes, 3), Some(2));
    }

    #[test]
    fn set_tree_nodes_in_spec_replaces_nodes() {
        let mut spec = WidgetSpec::Tree {
            nodes: vec![node("old", 0, false)],
            item_keys: vec!["k0".into()],
            selected_index: -1,
            visible_rows: 5,
            expanded_keys: vec![],
            key: Some("t".into()),
        };
        let new_nodes = vec![node("new1", 0, false), node("new2", 0, false)];
        let new_keys = vec!["a".to_string(), "b".to_string()];
        let ok = set_tree_nodes_in_spec(&mut spec, "t", new_nodes.clone(), new_keys.clone());
        assert!(ok);
        match &spec {
            WidgetSpec::Tree {
                nodes, item_keys, ..
            } => {
                assert_eq!(nodes.len(), 2);
                assert_eq!(item_keys, &new_keys);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn set_tree_nodes_in_spec_returns_false_for_unknown_key() {
        let mut spec = WidgetSpec::Tree {
            nodes: vec![node("a", 0, false)],
            item_keys: vec!["k".into()],
            selected_index: -1,
            visible_rows: 5,
            expanded_keys: vec![],
            key: Some("real".into()),
        };
        assert!(!set_tree_nodes_in_spec(&mut spec, "wrong", vec![], vec![]));
    }
}
