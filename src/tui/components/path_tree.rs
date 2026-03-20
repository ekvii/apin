/// A trie built from the path strings of a single spec.  Every path segment
/// is its own node — no chain-collapsing is done.
///
/// Example: given `/api/v1/a/b`, `/api/v2/a/b`, `/api/v2/a/c`:
///
///   root
///   └── "api"
///       ├── "v1"
///       │   └── "a"
///       │       └── "b"   ← leaf (path_index = Some(0))
///       └── "v2"
///           └── "a"
///               ├── "b"   ← leaf (path_index = Some(1))
///               └── "c"   ← leaf (path_index = Some(2))
///
/// Navigating `l` from "api" shows ["v1", "v2"].
/// Navigating `l` from "v2" shows ["a"].
/// Navigating `l` from "a" (under v2) shows ["b", "c"].
///
/// A node is a *leaf* when `children` is empty; `path_index` points back into
/// `Spec::paths`.
#[derive(Debug, Clone)]
pub struct PathNode {
    /// Single path segment label, e.g. `"api"`, `"v2"`, `"{id}"`.
    pub label: String,
    /// Index into `Spec::paths` — `Some` only for leaf nodes.
    pub path_index: Option<usize>,
    /// Sorted children.
    pub children: Vec<PathNode>,
}

impl PathNode {
    fn new_interior(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            path_index: None,
            children: Vec::new(),
        }
    }

    pub fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }
}

/// Build a per-segment trie from a slice of path strings.
pub fn build_tree(paths: &[String]) -> PathNode {
    let mut root = PathNode::new_interior("");

    for (idx, path) in paths.iter().enumerate() {
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        insert(&mut root, &segments, idx);
    }

    sort_children(&mut root);
    root
}

fn insert(node: &mut PathNode, segments: &[&str], path_index: usize) {
    if segments.is_empty() {
        if node.children.is_empty() {
            // Pure leaf — store index directly on this node.
            node.path_index = Some(path_index);
        } else {
            // This node already has children (it's a path prefix for other
            // paths).  Insert a synthetic "." child so the endpoint is
            // selectable without colliding with the children.
            if !node.children.iter().any(|c| c.label == ".") {
                node.children.push(PathNode {
                    label: ".".into(),
                    path_index: Some(path_index),
                    children: vec![],
                });
            }
        }
        return;
    }

    let head = segments[0];
    let tail = &segments[1..];

    // If this node was a pure leaf and we're about to give it children,
    // demote its path_index to a synthetic "." child first.
    if node.path_index.is_some() {
        let demoted_index = node.path_index.take().unwrap();
        node.children.push(PathNode {
            label: ".".into(),
            path_index: Some(demoted_index),
            children: vec![],
        });
    }

    let child_pos = node.children.iter().position(|c| c.label == head);
    let child = match child_pos {
        Some(i) => &mut node.children[i],
        None => {
            node.children.push(PathNode::new_interior(head));
            node.children.last_mut().unwrap()
        }
    };

    insert(child, tail, path_index);
}

fn sort_children(node: &mut PathNode) {
    node.children.sort_by(|a, b| {
        // "." (self-endpoint) always sorts first.
        match (a.label.as_str(), b.label.as_str()) {
            (".", _) => std::cmp::Ordering::Less,
            (_, ".") => std::cmp::Ordering::Greater,
            _ => a.label.cmp(&b.label),
        }
    });
    for child in &mut node.children {
        sort_children(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf_indices(node: &PathNode) -> Vec<(String, usize)> {
        let mut out = vec![];
        collect(node, &mut String::new(), &mut out);
        out
    }

    fn collect(node: &PathNode, path: &mut String, out: &mut Vec<(String, usize)>) {
        if let Some(idx) = node.path_index {
            out.push((path.clone(), idx));
        }
        for child in &node.children {
            let prev = path.len();
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(&child.label);
            collect(child, path, out);
            path.truncate(prev);
        }
    }

    #[test]
    fn self_endpoint_and_child_both_reachable() {
        // /v1/dnsServers        → path_index 0
        // /v1/dnsServers/{id}   → path_index 1
        let paths = vec![
            "/v1/dnsServers".to_string(),
            "/v1/dnsServers/{dnsServerId}".to_string(),
        ];
        let root = build_tree(&paths);
        let leaves = leaf_indices(&root);
        // Both indices must appear exactly once.
        assert!(
            leaves.iter().any(|(_, i)| *i == 0),
            "index 0 missing: {:?}",
            leaves
        );
        assert!(
            leaves.iter().any(|(_, i)| *i == 1),
            "index 1 missing: {:?}",
            leaves
        );
    }

    #[test]
    fn insertion_order_independent() {
        // Insert in reverse order — same result.
        let paths = vec![
            "/v1/dnsServers/{dnsServerId}".to_string(),
            "/v1/dnsServers".to_string(),
        ];
        let root = build_tree(&paths);
        let leaves = leaf_indices(&root);
        assert!(
            leaves.iter().any(|(_, i)| *i == 0),
            "index 0 missing: {:?}",
            leaves
        );
        assert!(
            leaves.iter().any(|(_, i)| *i == 1),
            "index 1 missing: {:?}",
            leaves
        );
    }

    #[test]
    fn empty_paths_produces_empty_root() {
        let root = build_tree(&[]);
        assert!(root.children.is_empty());
        assert!(root.path_index.is_none());
    }

    #[test]
    fn single_path_single_segment() {
        let paths = vec!["/health".to_string()];
        let root = build_tree(&paths);
        assert_eq!(root.children.len(), 1);
        let child = &root.children[0];
        assert_eq!(child.label, "health");
        assert_eq!(child.path_index, Some(0));
        assert!(child.is_leaf());
    }

    #[test]
    fn sort_children_dot_first_then_lexicographic() {
        // Three sibling nodes: "zebra", ".", "alpha" — after sort: ".", "alpha", "zebra"
        let paths = vec![
            "/api".to_string(),       // index 0 — will become "." child
            "/api/zebra".to_string(), // index 1
            "/api/alpha".to_string(), // index 2
        ];
        let root = build_tree(&paths);
        // Navigate into the "api" node.
        let api = root.children.iter().find(|c| c.label == "api").unwrap();
        let labels: Vec<&str> = api.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec![".", "alpha", "zebra"]);
    }

    #[test]
    fn all_leaves_present_in_multi_path_tree() {
        let paths: Vec<String> = vec!["/a/b".to_string(), "/a/c".to_string(), "/d".to_string()];
        let root = build_tree(&paths);
        let leaves = leaf_indices(&root);
        for expected_idx in 0..3 {
            assert!(
                leaves.iter().any(|(_, i)| *i == expected_idx),
                "index {} missing: {:?}",
                expected_idx,
                leaves
            );
        }
        assert_eq!(leaves.len(), 3);
    }

    #[test]
    fn path_index_count_equals_input_count() {
        // Confirm no indices are duplicated or dropped.
        let paths: Vec<String> = (0..5).map(|i| format!("/segment/{}", i)).collect();
        let root = build_tree(&paths);
        let leaves = leaf_indices(&root);
        let mut indices: Vec<usize> = leaves.iter().map(|(_, i)| *i).collect();
        indices.sort();
        assert_eq!(indices, vec![0, 1, 2, 3, 4]);
    }
}
