use std::io::Stdout;

use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use tui_tree_widget::{Tree, TreeItem, TreeState};

use crate::universe::{SchemaKindHint, SchemaNode, Spec};

use super::path_tree::{build_tree, PathNode};

const MIN_COL_WIDTH: u16 = 14;

// ─── Focus ────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
pub(super) enum Focus {
    /// The spec-selector list on the far left (only visible with >1 spec).
    Specs,
    /// The path-tree columns.
    Tree,
    /// The operations list for the selected path.
    Ops,
    /// Maximised detail view for the selected operation (full-screen).
    Detail,
}

// ─── Search state ─────────────────────────────────────────────────────────────

/// Incremental search state attached to the active tree column.
#[derive(Clone, Default)]
struct Search {
    /// The query string the user is typing.
    query: String,
    /// True while the `/` input box is open.
    active: bool,
}

impl Search {
    fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// Case-insensitive substring match.
    fn matches(&self, label: &str) -> bool {
        if self.query.is_empty() {
            return true;
        }
        label
            .to_ascii_lowercase()
            .contains(&self.query.to_ascii_lowercase())
    }
}

// ─── Ops state ────────────────────────────────────────────────────────────────

/// Selection + search state for the operations panel.
#[derive(Default)]
struct OpsState {
    list: ListState,
    search: Search,
    /// The path index this state belongs to.  When the selected path changes
    /// the ops state is reset so the user always starts at the top.
    path_index: Option<usize>,
}

impl OpsState {
    /// Ensure state matches `current_path_index`; reset if it changed.
    fn sync(&mut self, current_path_index: Option<usize>, op_count: usize) {
        if self.path_index != current_path_index {
            self.path_index = current_path_index;
            self.search = Search::default();
            self.list.select(if op_count > 0 { Some(0) } else { None });
        }
    }

    fn selected(&self) -> usize {
        self.list.selected().unwrap_or(0)
    }

    fn move_down(&mut self, filtered_len: usize) {
        if filtered_len == 0 {
            return;
        }
        let next = (self.selected() + 1).min(filtered_len - 1);
        self.list.select(Some(next));
    }

    fn move_up(&mut self) {
        let next = self.selected().saturating_sub(1);
        self.list.select(Some(next));
    }

    fn move_top(&mut self) {
        self.list.select(Some(0));
    }

    fn move_bottom(&mut self, filtered_len: usize) {
        if filtered_len == 0 {
            return;
        }
        self.list.select(Some(filtered_len - 1));
    }

    fn search_push(&mut self, ch: char) {
        self.search.query.push(ch);
        self.clamp(0);
    }

    fn search_pop(&mut self) {
        self.search.query.pop();
        self.clamp(0);
    }

    fn search_commit(&mut self) {
        self.search.active = false;
    }

    fn search_cancel(&mut self) {
        self.search.active = false;
        self.search.query.clear();
        self.clamp(0);
    }

    fn clamp(&mut self, filtered_len: usize) {
        let sel = self.selected();
        if filtered_len == 0 {
            self.list.select(None);
        } else if sel >= filtered_len {
            self.list.select(Some(filtered_len - 1));
        }
    }
}

// ─── Tree cursor ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Level {
    /// Index into the *unfiltered* children list.
    selected: usize,
}

struct TreeCursor {
    root: PathNode,
    /// Stack of per-level selection state (unfiltered indices).
    levels: Vec<Level>,
    /// Which level currently has keyboard focus.
    active_col: usize,
    /// Search state for the active column.  Reset whenever the active column
    /// changes or a navigation event changes the parent node.
    search: Search,
}

impl TreeCursor {
    fn new(root: PathNode) -> Self {
        let levels = if root.children.is_empty() {
            vec![]
        } else {
            vec![Level { selected: 0 }]
        };
        Self {
            root,
            levels,
            active_col: 0,
            search: Search::default(),
        }
    }

    /// Return the node selected at `depth`, walking from root.
    fn node_at_depth(&self, depth: usize) -> Option<&PathNode> {
        let mut node = &self.root;
        for d in 0..=depth {
            let sel = self.levels.get(d)?.selected;
            node = node.children.get(sel)?;
        }
        Some(node)
    }

    fn focused_node(&self) -> Option<&PathNode> {
        self.node_at_depth(self.active_col)
    }

    /// Walk down all open levels from the bottom, following selected children,
    /// until a leaf is reached.  If the bottom node fans out into multiple
    /// leaves (and no further column is open), returns `None`.
    fn selected_path_index(&self) -> Option<usize> {
        let depth = self.levels.len().saturating_sub(1);
        self.deepest_leaf(depth)
    }

    fn deepest_leaf(&self, depth: usize) -> Option<usize> {
        let node = self.node_at_depth(depth)?;
        if node.is_leaf() {
            return node.path_index;
        }
        if self.levels.len() > depth + 1 {
            return self.deepest_leaf(depth + 1);
        }
        single_leaf(node)
    }

    // ── Children helpers ──────────────────────────────────────────────────────

    /// Unfiltered children of the parent node for column `col`.
    fn children_of_col(&self, col: usize) -> &[PathNode] {
        let parent: &PathNode = if col == 0 {
            &self.root
        } else {
            match self.node_at_depth(col - 1) {
                Some(n) => n,
                None => return &[],
            }
        };
        &parent.children
    }

    /// Filtered children for the active column, respecting the current search
    /// query.  Returns `(unfiltered_index, label, is_leaf)` tuples.
    fn filtered_children(&self) -> Vec<(usize, &str, bool)> {
        self.children_of_col(self.active_col)
            .iter()
            .enumerate()
            .filter(|(_, c)| self.search.matches(&c.label))
            .map(|(i, c)| (i, c.label.as_str(), c.is_leaf()))
            .collect()
    }

    /// Index within the filtered list that corresponds to the currently
    /// selected unfiltered index.  Falls back to 0.
    fn filtered_cursor(&self) -> usize {
        let raw = self
            .levels
            .get(self.active_col)
            .map(|l| l.selected)
            .unwrap_or(0);
        self.filtered_children()
            .iter()
            .position(|(i, _, _)| *i == raw)
            .unwrap_or(0)
    }

    /// Apply a movement delta within the filtered list and write back the
    /// unfiltered index.
    fn apply_filtered_move(&mut self, delta: isize) {
        let filtered = self.filtered_children();
        if filtered.is_empty() {
            return;
        }
        let cur = self.filtered_cursor() as isize;
        let next = (cur + delta).max(0).min(filtered.len() as isize - 1) as usize;
        let (raw_idx, _, _) = filtered[next];
        let col = self.active_col;
        if let Some(level) = self.levels.get_mut(col) {
            level.selected = raw_idx;
            self.levels.truncate(col + 1);
            self.open_next_level();
        }
    }

    // ── Navigation ───────────────────────────────────────────────────────────

    fn move_down(&mut self) {
        self.apply_filtered_move(1);
    }

    fn move_up(&mut self) {
        self.apply_filtered_move(-1);
    }

    fn move_top(&mut self) {
        let col = self.active_col;
        let filtered = self.filtered_children();
        if let Some((raw_idx, _, _)) = filtered.first() {
            let raw_idx = *raw_idx;
            if let Some(level) = self.levels.get_mut(col) {
                level.selected = raw_idx;
                self.levels.truncate(col + 1);
                self.open_next_level();
            }
        }
    }

    fn move_bottom(&mut self) {
        let col = self.active_col;
        let filtered = self.filtered_children();
        if let Some((raw_idx, _, _)) = filtered.last() {
            let raw_idx = *raw_idx;
            if let Some(level) = self.levels.get_mut(col) {
                level.selected = raw_idx;
                self.levels.truncate(col + 1);
                self.open_next_level();
            }
        }
    }

    /// Drill right into children.  Returns `true` if the move happened.
    fn move_right(&mut self) -> bool {
        let has_children = self
            .focused_node()
            .map(|n| !n.children.is_empty())
            .unwrap_or(false);
        if !has_children {
            return false;
        }
        let next_col = self.active_col + 1;
        if self.levels.len() <= next_col {
            self.levels.push(Level { selected: 0 });
        }
        self.active_col = next_col;
        self.search = Search::default(); // reset search for new column
        self.open_next_level();
        true
    }

    /// Move focus one column to the left.  Returns `true` if the move happened.
    fn move_left(&mut self) -> bool {
        if self.active_col == 0 {
            return false;
        }
        self.active_col -= 1;
        self.search = Search::default();
        true
    }

    fn open_next_level(&mut self) {
        let has_children = self
            .focused_node()
            .map(|n| !n.children.is_empty())
            .unwrap_or(false);
        if has_children && self.levels.len() == self.active_col + 1 {
            self.levels.push(Level { selected: 0 });
        }
    }

    fn col_count(&self) -> usize {
        self.levels.len()
    }

    fn breadcrumb(&self, col: usize) -> String {
        if col == 0 {
            return String::new();
        }
        (0..col)
            .filter_map(|d| self.node_at_depth(d).map(|n| n.label.as_str()))
            .collect::<Vec<_>>()
            .join("/")
    }

    // ── Search helpers ────────────────────────────────────────────────────────

    fn search_push(&mut self, ch: char) {
        self.search.query.push(ch);
        self.search_clamp_selection();
    }

    fn search_pop(&mut self) {
        self.search.query.pop();
        self.search_clamp_selection();
    }

    /// After the query changes, make sure the selected item is still visible in
    /// the filtered list.  If the current selection is no longer in the filtered
    /// results, jump to the first match.
    fn search_clamp_selection(&mut self) {
        let col = self.active_col;
        let filtered = self.filtered_children();
        if filtered.is_empty() {
            return;
        }
        let raw = self.levels.get(col).map(|l| l.selected).unwrap_or(0);
        // If current selection is still visible, keep it.
        if filtered.iter().any(|(i, _, _)| *i == raw) {
            return;
        }
        // Otherwise jump to first match.
        let (first_raw, _, _) = filtered[0];
        if let Some(level) = self.levels.get_mut(col) {
            level.selected = first_raw;
            self.levels.truncate(col + 1);
            self.open_next_level();
        }
    }

    fn search_commit(&mut self) {
        // Keep selection, just close the input box.
        self.search.active = false;
    }

    fn search_cancel(&mut self) {
        self.search.active = false;
        self.search.query.clear();
        self.search_clamp_selection();
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn single_leaf(node: &PathNode) -> Option<usize> {
    if node.is_leaf() {
        return node.path_index;
    }
    if node.children.len() != 1 {
        return None;
    }
    single_leaf(&node.children[0])
}

// ─── Application state ────────────────────────────────────────────────────────

pub(super) struct ApinApp {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    specs: Vec<Spec>,
    pub(super) focus: Focus,
    specs_state: ListState,
    trees: Vec<TreeCursor>,
    ops: OpsState,
    /// Scroll offset (in virtual lines) for the full-screen detail view.
    detail_scroll: usize,
    /// Whether the cursor is currently inside the inline schema tree widget.
    detail_in_tree: bool,
    /// Virtual line index where the inline schema tree starts (cached from last draw).
    detail_tree_start: usize,
    /// Number of currently visible rows in the inline schema tree (cached from last draw).
    detail_tree_len: usize,
    /// Fold/selection state for the schema tree widget in the detail view.
    schema_tree_state: TreeState<usize>,
    /// The operation index for which `schema_tree_state` was last initialised.
    /// When it changes we reset the tree state so nodes start collapsed.
    schema_tree_op_key: Option<(usize, usize, usize)>, // (spec_idx, path_idx, op_idx)
    /// Visible height of the detail view (rows), cached from last draw for half-page scroll.
    detail_view_height: usize,
    /// Incremental search state for the detail view.
    detail_search: Search,
    /// Virtual line indices of lines that match the current detail search query.
    detail_search_matches: Vec<usize>,
    /// Which match the cursor is currently on (index into detail_search_matches).
    detail_search_cursor: usize,
}

impl ApinApp {
    pub(super) fn new(terminal: Terminal<CrosstermBackend<Stdout>>) -> Self {
        Self {
            terminal,
            specs: Vec::new(),
            focus: Focus::Specs,
            specs_state: ListState::default(),
            trees: Vec::new(),
            ops: OpsState::default(),
            detail_scroll: 0,
            detail_in_tree: false,
            detail_tree_start: 0,
            detail_tree_len: 0,
            schema_tree_state: TreeState::default(),
            schema_tree_op_key: None,
            detail_view_height: 0,
            detail_search: Search::default(),
            detail_search_matches: Vec::new(),
            detail_search_cursor: 0,
        }
    }

    fn selected_spec_index(&self) -> usize {
        self.specs_state.selected().unwrap_or(0)
    }

    fn selected_spec(&self) -> Option<&Spec> {
        self.specs.get(self.selected_spec_index())
    }

    fn tree(&self) -> Option<&TreeCursor> {
        self.trees.get(self.selected_spec_index())
    }

    fn tree_mut(&mut self) -> Option<&mut TreeCursor> {
        let idx = self.selected_spec_index();
        self.trees.get_mut(idx)
    }

    pub(super) fn is_searching(&self) -> bool {
        match self.focus {
            Focus::Tree => self.tree().map(|t| t.search.active).unwrap_or(false),
            Focus::Ops => self.ops.search.active,
            Focus::Detail => self.detail_search.active,
            Focus::Specs => false,
        }
    }

    /// Filtered operations for the selected path, respecting ops search query.
    /// Returns `(original_index, method, label)` tuples.
    pub(super) fn filtered_ops(&self) -> Vec<(usize, &str, &str)> {
        let Some(spec) = self.selected_spec() else {
            return vec![];
        };
        let Some(tree) = self.tree() else {
            return vec![];
        };
        let Some(idx) = tree.selected_path_index() else {
            return vec![];
        };
        let Some(entry) = spec.paths.get(idx) else {
            return vec![];
        };
        entry
            .operations
            .iter()
            .enumerate()
            .filter(|(_, op)| {
                let label = op
                    .summary
                    .as_deref()
                    .or(op.operation_id.as_deref())
                    .unwrap_or(&op.method);
                self.ops.search.matches(&op.method) || self.ops.search.matches(label)
            })
            .map(|(i, op)| {
                let label = op
                    .summary
                    .as_deref()
                    .or(op.operation_id.as_deref())
                    .unwrap_or("");
                (i, op.method.as_str(), label)
            })
            .collect()
    }

    /// The `Operation` currently selected in the ops panel (considering filter).
    fn selected_operation_index(&self) -> Option<usize> {
        let filtered = self.filtered_ops();
        filtered.get(self.ops.selected()).map(|(i, _, _)| *i)
    }

    /// Add a newly-loaded spec and build its tree cursor.  The spec-list
    /// selection is left unchanged so the user keeps their current context.
    pub(super) fn push_spec(&mut self, spec: Spec) {
        let path_strings: Vec<String> = spec.paths.iter().map(|p| p.path.clone()).collect();
        let root = build_tree(&path_strings);
        let mut cursor = TreeCursor::new(root);
        cursor.open_next_level();
        self.trees.push(cursor);
        self.specs.push(spec);
        // If this is the very first spec, select it.
        if self.specs.len() == 1 {
            self.specs_state.select(Some(0));
        }
    }

    /// If the currently-selected operation differs from when the schema tree
    /// state was last initialised, reset it so nodes start collapsed to first
    /// level (root open, children visible but not expanded).
    fn sync_schema_tree_state(&mut self) {
        let key = self.current_op_key();
        if self.schema_tree_op_key != key {
            self.schema_tree_op_key = key;
            self.schema_tree_state = TreeState::default();
            self.detail_in_tree = false;
            self.detail_tree_start = 0;
            self.detail_tree_len = 0;
        }
    }

    /// Returns `(spec_idx, path_idx, op_idx)` for the currently selected
    /// operation, or `None` if nothing is selected.
    fn current_op_key(&self) -> Option<(usize, usize, usize)> {
        let spec_idx = self.selected_spec_index();
        let path_idx = self.tree()?.selected_path_index()?;
        let op_idx = self.selected_operation_index()?;
        Some((spec_idx, path_idx, op_idx))
    }

    // ─── Public action methods (called from the event loop in mod.rs) ─────────

    // ── Specs navigation ──────────────────────────────────────────────────────

    pub(super) fn specs_move_down(&mut self) {
        let len = self.specs.len();
        if len == 0 {
            return;
        }
        let next = self
            .specs_state
            .selected()
            .map(|i| (i + 1).min(len - 1))
            .unwrap_or(0);
        self.specs_state.select(Some(next));
    }

    pub(super) fn specs_move_up(&mut self) {
        let next = self
            .specs_state
            .selected()
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0);
        self.specs_state.select(Some(next));
    }

    pub(super) fn specs_move_top(&mut self) {
        self.specs_state.select(Some(0));
    }

    pub(super) fn specs_move_bottom(&mut self) {
        let last = self.specs.len().saturating_sub(1);
        self.specs_state.select(Some(last));
    }

    // ── Tree navigation ───────────────────────────────────────────────────────

    pub(super) fn tree_move_down(&mut self) {
        if let Some(t) = self.tree_mut() {
            t.move_down();
        }
    }

    pub(super) fn tree_move_up(&mut self) {
        if let Some(t) = self.tree_mut() {
            t.move_up();
        }
    }

    pub(super) fn tree_move_top(&mut self) {
        if let Some(t) = self.tree_mut() {
            t.move_top();
        }
    }

    pub(super) fn tree_move_bottom(&mut self) {
        if let Some(t) = self.tree_mut() {
            t.move_bottom();
        }
    }

    /// Try to move right within tree columns; if at a leaf, move focus to Ops.
    pub(super) fn tree_move_right_or_ops(&mut self) {
        let moved = if let Some(t) = self.tree_mut() {
            t.move_right()
        } else {
            false
        };
        if !moved {
            let path_idx = self.tree().and_then(|t| t.selected_path_index());
            let op_count = path_idx
                .and_then(|i| self.selected_spec()?.paths.get(i))
                .map(|e| e.operations.len())
                .unwrap_or(0);
            if op_count > 0 {
                self.ops.sync(path_idx, op_count);
                self.focus = Focus::Ops;
            }
        }
    }

    /// Try to move left within tree columns; if at col 0 and multi-spec, move to Specs.
    pub(super) fn tree_move_left_or_specs(&mut self) {
        let moved = if let Some(t) = self.tree_mut() {
            t.move_left()
        } else {
            false
        };
        if !moved && self.specs.len() > 1 {
            self.focus = Focus::Specs;
        }
    }

    // ── Tree search ───────────────────────────────────────────────────────────

    pub(super) fn tree_search_open(&mut self) {
        if let Some(t) = self.tree_mut() {
            t.search.active = true;
        }
    }

    pub(super) fn tree_search_push(&mut self, ch: char) {
        if let Some(t) = self.tree_mut() {
            t.search_push(ch);
        }
    }

    pub(super) fn tree_search_pop(&mut self) {
        if let Some(t) = self.tree_mut() {
            t.search_pop();
        }
    }

    pub(super) fn tree_search_clear(&mut self) {
        if let Some(t) = self.tree_mut() {
            t.search.query.clear();
            t.search_clamp_selection();
        }
    }

    pub(super) fn tree_search_commit(&mut self) {
        if let Some(t) = self.tree_mut() {
            t.search_commit();
        }
    }

    pub(super) fn tree_search_cancel(&mut self) {
        if let Some(t) = self.tree_mut() {
            t.search_cancel();
        }
    }

    // ── Ops navigation ────────────────────────────────────────────────────────

    pub(super) fn ops_move_down(&mut self) {
        let flen = self.filtered_ops().len();
        self.ops.move_down(flen);
    }

    pub(super) fn ops_move_up(&mut self) {
        self.ops.move_up();
    }

    pub(super) fn ops_move_top(&mut self) {
        self.ops.move_top();
    }

    pub(super) fn ops_move_bottom(&mut self) {
        let flen = self.filtered_ops().len();
        self.ops.move_bottom(flen);
    }

    // ── Ops search ────────────────────────────────────────────────────────────

    pub(super) fn ops_search_open(&mut self) {
        self.ops.search.active = true;
    }

    pub(super) fn ops_search_push(&mut self, ch: char) {
        self.ops.search_push(ch);
        let flen = self.filtered_ops().len();
        self.ops.clamp(flen);
    }

    pub(super) fn ops_search_pop(&mut self) {
        self.ops.search_pop();
        let flen = self.filtered_ops().len();
        self.ops.clamp(flen);
    }

    pub(super) fn ops_search_clear(&mut self) {
        self.ops.search.query.clear();
        let flen = self.filtered_ops().len();
        self.ops.clamp(flen);
    }

    pub(super) fn ops_search_commit(&mut self) {
        self.ops.search_commit();
    }

    pub(super) fn ops_search_cancel(&mut self) {
        self.ops.search_cancel();
    }

    // ── Focus changes ─────────────────────────────────────────────────────────

    pub(super) fn focus_tree(&mut self) {
        self.focus = Focus::Tree;
        if let Some(t) = self.tree_mut() {
            t.active_col = 0;
        }
    }

    // ── Enter / detail transitions ────────────────────────────────────────────

    pub(super) fn enter_detail_from_ops(&mut self) {
        if self.selected_operation_index().is_some() {
            self.focus = Focus::Detail;
            self.detail_scroll = 0;
            self.sync_schema_tree_state();
        }
    }

    pub(super) fn enter_detail_from_tree(&mut self) {
        let path_idx = self.tree().and_then(|t| t.selected_path_index());
        let op_count = path_idx
            .and_then(|i| self.selected_spec()?.paths.get(i))
            .map(|e| e.operations.len())
            .unwrap_or(0);
        if op_count > 0 {
            self.ops.sync(path_idx, op_count);
            self.focus = Focus::Detail;
            self.detail_scroll = 0;
            self.sync_schema_tree_state();
        }
    }

    // ── Detail view ───────────────────────────────────────────────────────────

    pub(super) fn detail_in_tree(&self) -> bool {
        self.detail_in_tree
    }

    pub(super) fn detail_back(&mut self) {
        self.focus = Focus::Ops;
        self.detail_scroll = 0;
        self.detail_in_tree = false;
    }

    pub(super) fn detail_scroll_down(&mut self, n: usize) {
        self.detail_scroll = self.detail_scroll.saturating_add(n);
    }

    pub(super) fn detail_scroll_up(&mut self, n: usize) {
        self.detail_scroll = self.detail_scroll.saturating_sub(n);
    }

    pub(super) fn detail_scroll_half_down(&mut self) {
        let half = (self.detail_view_height / 2).max(1);
        self.detail_scroll += half;
    }

    pub(super) fn detail_scroll_half_up(&mut self) {
        let half = (self.detail_view_height / 2).max(1);
        self.detail_scroll = self.detail_scroll.saturating_sub(half);
    }

    pub(super) fn detail_scroll_top(&mut self) {
        self.detail_scroll = 0;
    }

    pub(super) fn detail_scroll_bottom(&mut self) {
        self.detail_scroll = usize::MAX / 2;
    }

    pub(super) fn detail_focus_tree(&mut self) {
        if self.detail_tree_len > 0 {
            self.detail_in_tree = true;
            self.schema_tree_state.select_first();
        }
    }

    pub(super) fn detail_unfocus_tree(&mut self) {
        self.detail_in_tree = false;
    }

    // ── Schema tree navigation (when detail_in_tree is true) ──────────────────

    pub(super) fn schema_tree_key_down(&mut self) {
        self.schema_tree_state.key_down();
    }

    pub(super) fn schema_tree_key_up(&mut self) {
        self.schema_tree_state.key_up();
    }

    pub(super) fn schema_tree_key_left(&mut self) {
        self.schema_tree_state.key_left();
    }

    pub(super) fn schema_tree_key_right(&mut self) {
        self.schema_tree_state.key_right();
    }

    // ── Detail search ─────────────────────────────────────────────────────────

    pub(super) fn detail_search_open(&mut self) {
        self.detail_search.active = true;
        self.detail_search.query.clear();
        self.detail_search_matches.clear();
        self.detail_search_cursor = 0;
    }

    pub(super) fn detail_search_push(&mut self, ch: char) {
        self.detail_search.query.push(ch);
        if let Some(&line) = self.detail_search_matches.first() {
            self.detail_scroll = line;
            self.detail_search_cursor = 0;
        }
    }

    pub(super) fn detail_search_pop(&mut self) {
        self.detail_search.query.pop();
        if let Some(&line) = self.detail_search_matches.first() {
            self.detail_scroll = line;
            self.detail_search_cursor = 0;
        }
    }

    pub(super) fn detail_search_clear(&mut self) {
        self.detail_search.query.clear();
        self.detail_search_matches.clear();
        self.detail_search_cursor = 0;
    }

    pub(super) fn detail_search_cancel(&mut self) {
        self.detail_search.active = false;
        self.detail_search.query.clear();
        self.detail_search_matches.clear();
        self.detail_search_cursor = 0;
    }

    pub(super) fn detail_search_enter(&mut self) {
        self.detail_search.active = false;
        if let Some(&line) = self.detail_search_matches.first() {
            self.detail_scroll = line;
        }
    }

    pub(super) fn detail_search_next(&mut self) {
        if !self.detail_search_matches.is_empty() {
            self.detail_search_cursor =
                (self.detail_search_cursor + 1) % self.detail_search_matches.len();
            self.detail_scroll = self.detail_search_matches[self.detail_search_cursor];
        }
    }

    pub(super) fn detail_search_prev(&mut self) {
        if !self.detail_search_matches.is_empty() {
            let len = self.detail_search_matches.len();
            self.detail_search_cursor = self.detail_search_cursor.checked_sub(1).unwrap_or(len - 1);
            self.detail_scroll = self.detail_search_matches[self.detail_search_cursor];
        }
    }

    // ─── Draw ─────────────────────────────────────────────────────────────────

    pub(super) fn draw(&mut self) -> anyhow::Result<()> {
        // Destructure to avoid borrow conflict: `terminal` needs `&mut` for
        // `.draw()`, while the closure borrows the rest of the fields.
        let Self {
            terminal,
            specs,
            focus,
            specs_state,
            trees,
            ops,
            detail_scroll,
            detail_in_tree,
            detail_tree_start,
            detail_tree_len,
            schema_tree_state,
            schema_tree_op_key: _,
            detail_view_height,
            detail_search,
            detail_search_matches,
            detail_search_cursor,
        } = self;

        terminal.draw(|frame| {
            draw_frame(
                frame,
                specs,
                focus,
                specs_state,
                trees,
                ops,
                detail_scroll,
                detail_in_tree,
                detail_tree_start,
                detail_tree_len,
                schema_tree_state,
                detail_view_height,
                detail_search,
                detail_search_matches,
                detail_search_cursor,
            );
        })?;
        Ok(())
    }
}

// ─── Draw frame (top-level) ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn draw_frame(
    frame: &mut Frame,
    specs: &[Spec],
    focus: &Focus,
    specs_state: &mut ListState,
    trees: &[TreeCursor],
    ops: &mut OpsState,
    detail_scroll: &mut usize,
    detail_in_tree: &mut bool,
    detail_tree_start: &mut usize,
    detail_tree_len: &mut usize,
    schema_tree_state: &mut TreeState<usize>,
    detail_view_height: &mut usize,
    detail_search: &mut Search,
    detail_search_matches: &mut Vec<usize>,
    detail_search_cursor: &mut usize,
) {
    let area = frame.area();

    // Full-screen detail mode: skip the normal layout entirely.
    if *focus == Focus::Detail {
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);
        draw_detail_fullscreen(
            frame,
            specs,
            focus,
            specs_state,
            trees,
            ops,
            detail_scroll,
            detail_in_tree,
            detail_tree_start,
            detail_tree_len,
            schema_tree_state,
            detail_view_height,
            detail_search,
            detail_search_matches,
            detail_search_cursor,
            vert[0],
        );
        draw_hint(frame, focus, ops, detail_in_tree, detail_search, vert[1]);
        return;
    }

    let multi_spec = specs.len() > 1;

    let content_area = if multi_spec {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(area);
        draw_spec_list(frame, specs, focus, specs_state, cols[0]);
        cols[1]
    } else {
        area
    };

    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(content_area);

    draw_tree_columns(frame, specs, focus, specs_state, trees, ops, vert[0]);
    draw_path_detail(frame, specs, focus, specs_state, trees, ops, vert[1]);
    draw_hint(frame, focus, ops, detail_in_tree, detail_search, vert[2]);
}

// ── Spec list ─────────────────────────────────────────────────────────────────

fn draw_spec_list(
    frame: &mut Frame,
    specs: &[Spec],
    focus: &Focus,
    specs_state: &mut ListState,
    area: Rect,
) {
    let active = *focus == Focus::Specs;

    let items: Vec<ListItem> = specs
        .iter()
        .map(|s| {
            let file_name = s
                .file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");
            ListItem::new(Line::from(vec![
                Span::styled(
                    s.title.as_str(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(format!("v{}", s.version), Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(
                    format!("[{}]", file_name),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Specs ")
                .border_style(border_style(active)),
        )
        .highlight_style(highlight_style(active));

    frame.render_stateful_widget(list, area, specs_state);
}

// ── Tree columns ──────────────────────────────────────────────────────────────

fn draw_tree_columns(
    frame: &mut Frame,
    specs: &[Spec],
    focus: &Focus,
    specs_state: &ListState,
    trees: &[TreeCursor],
    ops: &mut OpsState,
    area: Rect,
) {
    let spec_idx = specs_state.selected().unwrap_or(0);
    let is_tree_focused = *focus == Focus::Tree;
    let is_ops_focused = *focus == Focus::Ops;

    let tree_col_count = trees.get(spec_idx).map(|t| t.col_count()).unwrap_or(0);
    let tree_active_col = trees.get(spec_idx).map(|t| t.active_col).unwrap_or(0);

    let selected_spec = specs.get(spec_idx);
    let spec_title = selected_spec.map(|s| {
        let fname = s
            .file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");
        format!(" {} v{} [{}] ", s.title, s.version, fname)
    });

    // Sync ops state based on currently selected path.
    let path_idx = trees.get(spec_idx).and_then(|t| t.selected_path_index());
    let op_count = path_idx
        .and_then(|i| selected_spec?.paths.get(i))
        .map(|e| e.operations.len())
        .unwrap_or(0);
    ops.sync(path_idx, op_count);

    // Whether to show an ops column — only when a concrete path is selected.
    let show_ops = op_count > 0;

    // Total logical columns: tree columns + optional ops column.
    let total_cols = tree_col_count + if show_ops { 1 } else { 0 };

    if total_cols == 0 {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(spec_title.unwrap_or_else(|| " Paths ".into()))
            .border_style(border_style(false));
        frame.render_widget(
            Paragraph::new(Span::styled(
                "(no paths)",
                Style::default().fg(Color::DarkGray),
            ))
            .block(block),
            area,
        );
        return;
    }

    // The "active" column index for windowing purposes.
    // Focus::Ops logically occupies column `tree_col_count`.
    let logical_active = if is_ops_focused {
        tree_col_count
    } else {
        tree_active_col
    };

    // Slide the window so the active column is always visible.
    let max_visible = ((area.width / MIN_COL_WIDTH) as usize).max(1);
    let visible = total_cols.min(max_visible);
    let window_start = if logical_active + 1 <= visible {
        0
    } else {
        logical_active + 1 - visible
    };
    let window_end = (window_start + visible).min(total_cols);

    let constraints: Vec<Constraint> = (0..visible)
        .map(|_| Constraint::Ratio(1, visible as u32))
        .collect();
    let col_rects = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (render_idx, col) in (window_start..window_end).enumerate() {
        // The ops column is logically at index `tree_col_count`.
        if show_ops && col == tree_col_count {
            draw_ops_column(
                frame,
                specs,
                focus,
                specs_state,
                trees,
                ops,
                col_rects[render_idx],
                path_idx,
                op_count,
            );
            continue;
        }

        let is_active = is_tree_focused && col == tree_active_col;

        struct ColData {
            items: Vec<(String, bool)>,
            selected_in_list: usize,
            title: String,
            search_active: bool,
            search_query: String,
        }

        let data: ColData = {
            let tree = match trees.get(spec_idx) {
                Some(t) => t,
                None => continue,
            };

            let title = if col == 0 {
                spec_title.clone().unwrap_or_else(|| " Paths ".into())
            } else {
                format!(" /{} ", tree.breadcrumb(col))
            };

            if is_active && !tree.search.is_empty() {
                let filtered = tree.filtered_children();
                let cursor = tree.filtered_cursor();
                ColData {
                    items: filtered
                        .iter()
                        .map(|(_, lbl, leaf)| (lbl.to_string(), *leaf))
                        .collect(),
                    selected_in_list: cursor,
                    title,
                    search_active: tree.search.active,
                    search_query: tree.search.query.clone(),
                }
            } else {
                let parent: &PathNode = if col == 0 {
                    &tree.root
                } else {
                    match tree.node_at_depth(col - 1) {
                        Some(n) => n,
                        None => continue,
                    }
                };
                let selected_raw = tree.levels.get(col).map(|l| l.selected).unwrap_or(0);
                ColData {
                    items: parent
                        .children
                        .iter()
                        .map(|c| (c.label.clone(), c.is_leaf()))
                        .collect(),
                    selected_in_list: selected_raw,
                    title,
                    search_active: is_active && tree.search.active,
                    search_query: if is_active {
                        tree.search.query.clone()
                    } else {
                        String::new()
                    },
                }
            }
        };

        let col_title = if is_active && (!data.search_query.is_empty() || data.search_active) {
            let indicator = if data.search_active { "_" } else { "" };
            format!(
                "{} /{}{} ",
                data.title.trim_end(),
                data.search_query,
                indicator
            )
        } else {
            data.title.clone()
        };

        let list_items: Vec<ListItem> = data
            .items
            .iter()
            .map(|(label, is_leaf)| {
                // "." is a synthetic self-endpoint node — display it as "(self)"
                // so the user understands it represents the parent path itself.
                let display = if label == "." {
                    "(self)"
                } else {
                    label.as_str()
                };
                if *is_leaf {
                    ListItem::new(Span::styled(display, Style::default().fg(Color::White)))
                } else {
                    ListItem::new(Line::from(vec![
                        Span::raw(display),
                        Span::styled(" ›", Style::default().fg(Color::DarkGray)),
                    ]))
                }
            })
            .collect();

        let list = List::new(list_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(col_title)
                    .border_style(if is_active && data.search_active {
                        Style::default().fg(Color::Yellow)
                    } else {
                        border_style(is_active)
                    }),
            )
            .highlight_style(highlight_style(is_active));

        let mut state = ListState::default();
        state.select(Some(data.selected_in_list));
        frame.render_stateful_widget(list, col_rects[render_idx], &mut state);
    }
}

/// Render the operations column — the final column in the path tree.
/// Shows only HTTP method badges, one per row.
fn draw_ops_column(
    frame: &mut Frame,
    specs: &[Spec],
    focus: &Focus,
    specs_state: &ListState,
    trees: &[TreeCursor],
    ops: &mut OpsState,
    area: Rect,
    path_idx: Option<usize>,
    _op_count: usize,
) {
    let spec_idx = specs_state.selected().unwrap_or(0);
    let is_ops_focused = *focus == Focus::Ops;
    let _ = trees; // unused in this path after restructure

    // Build filtered list: (original_op_index, method).
    let filtered: Vec<(usize, &str)> = {
        let spec = match specs.get(spec_idx) {
            Some(s) => s,
            None => return,
        };
        let idx = match path_idx {
            Some(i) => i,
            None => return,
        };
        let entry = match spec.paths.get(idx) {
            Some(e) => e,
            None => return,
        };
        entry
            .operations
            .iter()
            .enumerate()
            .filter(|(_, op)| ops.search.matches(&op.method))
            .map(|(i, op)| (i, op.method.as_str()))
            .collect()
    };

    let title = if !ops.search.query.is_empty() || ops.search.active {
        let cursor = if ops.search.active { "_" } else { "" };
        format!(" Methods /{}{} ", ops.search.query, cursor)
    } else {
        " Methods ".to_string()
    };

    let items: Vec<ListItem> = filtered
        .iter()
        .map(|(_, method)| {
            ListItem::new(Line::from(vec![Span::styled(
                format!(" {} ", method),
                method_color(method).add_modifier(Modifier::BOLD),
            )]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(if ops.search.active {
                    Style::default().fg(Color::Yellow)
                } else {
                    border_style(is_ops_focused)
                }),
        )
        .highlight_style(highlight_style(is_ops_focused));

    ops.clamp(filtered.len());
    frame.render_stateful_widget(list, area, &mut ops.list);
}

// ── Detail full-screen ────────────────────────────────────────────────────────

/// Renders the selected operation as a maximised, scrollable detail panel that
/// occupies the full terminal (minus the hint bar).
#[allow(clippy::too_many_arguments)]
fn draw_detail_fullscreen(
    frame: &mut Frame,
    specs: &[Spec],
    _focus: &Focus,
    specs_state: &ListState,
    trees: &[TreeCursor],
    ops: &OpsState,
    detail_scroll: &mut usize,
    detail_in_tree: &mut bool,
    detail_tree_start: &mut usize,
    detail_tree_len: &mut usize,
    schema_tree_state: &mut TreeState<usize>,
    detail_view_height: &mut usize,
    detail_search: &mut Search,
    detail_search_matches: &mut Vec<usize>,
    detail_search_cursor: &mut usize,
    area: Rect,
) {
    let spec_idx = specs_state.selected().unwrap_or(0);

    // ── Resolve operation ──────────────────────────────────────────────────────
    let resolved = trees.get(spec_idx).and_then(|t| {
        let path_idx = t.selected_path_index()?;
        let entry = specs.get(spec_idx)?.paths.get(path_idx)?;
        Some((entry.path.clone(), path_idx))
    });

    let (path_str, path_idx) = match resolved {
        Some(v) => v,
        None => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Detail ")
                .border_style(border_style(true));
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "no operation selected",
                    Style::default().fg(Color::DarkGray),
                ))
                .block(block),
                area,
            );
            return;
        }
    };

    let op_idx = {
        let filtered: Vec<(usize, &str, &str)> = {
            let spec = match specs.get(spec_idx) {
                Some(s) => s,
                None => return,
            };
            let tree = match trees.get(spec_idx) {
                Some(t) => t,
                None => return,
            };
            let idx = match tree.selected_path_index() {
                Some(i) => i,
                None => return,
            };
            let entry = match spec.paths.get(idx) {
                Some(e) => e,
                None => return,
            };
            entry
                .operations
                .iter()
                .enumerate()
                .filter(|(_, op)| {
                    let label = op
                        .summary
                        .as_deref()
                        .or(op.operation_id.as_deref())
                        .unwrap_or(&op.method);
                    ops.search.matches(&op.method) || ops.search.matches(label)
                })
                .map(|(i, op)| {
                    let label = op
                        .summary
                        .as_deref()
                        .or(op.operation_id.as_deref())
                        .unwrap_or("");
                    (i, op.method.as_str(), label)
                })
                .collect()
        };
        filtered
            .get(ops.selected())
            .map(|(i, _, _)| *i)
            .unwrap_or(0)
    };

    let op = match specs
        .get(spec_idx)
        .and_then(|s| s.paths.get(path_idx))
        .and_then(|e| e.operations.get(op_idx))
    {
        Some(o) => o.clone(),
        None => return,
    };

    // ── Determine if we have a schema tree to show ─────────────────────────────
    let has_schema = op
        .request_body
        .as_ref()
        .and_then(|rb| rb.schema_tree.as_ref())
        .is_some();

    // ── Build content lines (text portion) ────────────────────────────────────
    let inner_w = (area.width.saturating_sub(2)) as usize;
    let divider: String = "─".repeat(inner_w);

    let mut lines: Vec<Line> = Vec::new();

    // ┌─ METHOD  PATH ──────────────────────────────────────────────────────────┐
    let badge_style = method_color(&op.method).add_modifier(Modifier::BOLD);
    lines.push(Line::from(vec![
        Span::styled(format!(" {} ", op.method), badge_style),
        Span::raw("  "),
        Span::styled(
            path_str.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::raw(""));

    // ── Summary ───────────────────────────────────────────────────────────────
    if let Some(ref sum) = op.summary {
        lines.push(Line::from(vec![
            Span::styled("  Summary      ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                sum.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // ── Operation ID ──────────────────────────────────────────────────────────
    if let Some(ref oid) = op.operation_id {
        lines.push(Line::from(vec![
            Span::styled("  Operation ID ", Style::default().fg(Color::DarkGray)),
            Span::styled(oid.clone(), Style::default().fg(Color::Cyan)),
        ]));
    }

    // ── Description ───────────────────────────────────────────────────────────
    if let Some(ref desc) = op.description {
        if op.summary.as_deref() != Some(desc.as_str()) {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "  Description",
                Style::default().fg(Color::DarkGray),
            )));
            for desc_line in desc.lines() {
                lines.push(Line::from(Span::styled(
                    format!("    {}", desc_line),
                    Style::default().fg(Color::Gray),
                )));
            }
        }
    }

    // ── Parameters ────────────────────────────────────────────────────────────
    if !op.params.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            divider.clone(),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  PARAMETERS",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::raw(""));

        let locations = [
            ("path", Color::Magenta),
            ("query", Color::Cyan),
            ("header", Color::Blue),
            ("cookie", Color::Green),
        ];

        for (loc, loc_color) in locations {
            let group: Vec<_> = op.params.iter().filter(|p| p.location == loc).collect();
            if group.is_empty() {
                continue;
            }

            // Location header
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!(" {} ", loc.to_uppercase()),
                    Style::default()
                        .fg(Color::Black)
                        .bg(loc_color)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));

            // Column headers
            lines.push(Line::from(vec![Span::styled(
                "    name                 type       req   description",
                Style::default().fg(Color::DarkGray),
            )]));

            for p in &group {
                let name_style = if p.required {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let req_span = if p.required {
                    Span::styled(
                        " yes  ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(" no   ", Style::default().fg(Color::DarkGray))
                };
                let name_padded = format!(
                    "{:<22}",
                    if p.required {
                        format!("{}*", p.name)
                    } else {
                        p.name.clone()
                    }
                );
                let mut param_row = vec![
                    Span::raw("    "),
                    Span::styled(name_padded, name_style),
                    Span::styled(
                        format!("{:<10} ", "string"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    req_span,
                ];
                if let Some(ref desc) = p.description {
                    param_row.push(Span::styled(desc.clone(), Style::default().fg(Color::Gray)));
                }
                lines.push(Line::from(param_row));
            }
            lines.push(Line::raw(""));
        }
    }

    // ── Request body header ────────────────────────────────────────────────────
    let schema_node_opt = op
        .request_body
        .as_ref()
        .and_then(|rb| rb.schema_tree.as_ref());

    let (_schema_header_lines, owned_effective_children, tree_id_start) =
        if let Some(ref node) = schema_node_opt {
            let (hdr, ch, id_start) = schema_effective_roots(node);
            (hdr, ch.to_vec(), id_start)
        } else {
            (vec![], vec![], 0)
        };

    if let Some(ref rb) = op.request_body {
        lines.push(Line::from(Span::styled(
            divider.clone(),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  REQUEST BODY",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::raw(""));

        if let Some(ref desc) = rb.description {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(desc.clone(), Style::default().fg(Color::Gray)),
            ]));
            lines.push(Line::raw(""));
        }

        if !has_schema {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    "(schema not available)",
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            lines.push(Line::raw(""));
        } else {
            let hint_style = Style::default().fg(Color::DarkGray);
            lines.push(Line::from(vec![
                Span::styled("  ", hint_style),
                Span::styled("[f]", Style::default().fg(Color::Yellow)),
                Span::styled(" focus/unfocus  ", hint_style),
                Span::styled("[j/k]", Style::default().fg(Color::Yellow)),
                Span::styled(" navigate  ", hint_style),
                Span::styled("[h/l]", Style::default().fg(Color::Yellow)),
                Span::styled(" collapse/expand", hint_style),
            ]));
            lines.push(Line::raw(""));
        }
    }

    // ── Responses (lines_below) ───────────────────────────────────────────────
    let mut lines_below: Vec<Line> = Vec::new();

    if !op.responses.is_empty() {
        let target: &mut Vec<Line> = if has_schema {
            &mut lines_below
        } else {
            &mut lines
        };
        target.push(Line::from(Span::styled(
            divider.clone(),
            Style::default().fg(Color::DarkGray),
        )));
        target.push(Line::from(Span::styled(
            "  RESPONSES",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
        target.push(Line::raw(""));
        target.push(Line::from(Span::styled(
            "    code    description",
            Style::default().fg(Color::DarkGray),
        )));

        for (code, desc) in &op.responses {
            let badge = Span::styled(
                format!(" {} ", code),
                response_code_style(code).add_modifier(Modifier::BOLD),
            );
            let desc_span = if let Some(d) = desc {
                Span::styled(format!("  {}", d), Style::default().fg(Color::Gray))
            } else {
                Span::raw("")
            };
            target.push(Line::from(vec![Span::raw("    "), badge, desc_span]));
        }
        target.push(Line::raw(""));
    }

    // ── Virtual scroll layout ─────────────────────────────────────────────────
    let tree_len: usize = if !owned_effective_children.is_empty() {
        let opened = schema_tree_state.opened().clone();
        count_visible_tree_rows(&owned_effective_children, tree_id_start, &opened)
    } else {
        0
    };

    let lines_above_len = lines.len();
    let tree_start = lines_above_len;
    let tree_end = tree_start + tree_len;

    *detail_tree_start = tree_start;
    *detail_tree_len = tree_len;

    if !detail_search.query.is_empty() {
        let q = detail_search.query.to_ascii_lowercase();
        let mut matches: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| line_plain_text(l).to_ascii_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        let below_start = tree_end;
        for (i, l) in lines_below.iter().enumerate() {
            if line_plain_text(l).to_ascii_lowercase().contains(&q) {
                matches.push(below_start + i);
            }
        }
        if *detail_search_matches != matches {
            *detail_search_matches = matches;
            if *detail_search_cursor >= detail_search_matches.len() {
                *detail_search_cursor = 0;
            }
        }
    } else {
        detail_search_matches.clear();
        *detail_search_cursor = 0;
    }

    let total_virtual = tree_end + lines_below.len();

    let in_tree_indicator = if *detail_in_tree { "●" } else { "○" };
    let search_indicator = if !detail_search.query.is_empty() {
        let cursor = if detail_search.active { "_" } else { "" };
        let n = detail_search_matches.len();
        let cur = if n > 0 { *detail_search_cursor + 1 } else { 0 };
        format!("  /{}{} [{}/{}]", detail_search.query, cursor, cur, n)
    } else if detail_search.active {
        "  /_".to_string()
    } else {
        String::new()
    };
    let title = format!(
        " {} {}  [line {}/{}] {}{}",
        op.method,
        path_str,
        detail_scroll.saturating_add(1).min(total_virtual),
        total_virtual.max(1),
        in_tree_indicator,
        search_indicator,
    );

    let outer_block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(Color::Cyan));

    let inner_area = outer_block.inner(area);
    frame.render_widget(outer_block, area);

    let view_h = inner_area.height as usize;
    if view_h == 0 {
        return;
    }
    *detail_view_height = view_h;

    let max_scroll = total_virtual.saturating_sub(view_h);
    if *detail_scroll > max_scroll {
        *detail_scroll = max_scroll;
    }

    let scroll = *detail_scroll;
    let view_end = scroll + view_h;

    if tree_len == 0 || !has_schema {
        let matches = detail_search_matches.clone();
        let mut combined: Vec<Line> = lines.into_iter().chain(lines_below.into_iter()).collect();
        highlight_matched_lines(&mut combined, 0, &matches);
        let para = Paragraph::new(combined)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0));
        frame.render_widget(para, inner_area);
        return;
    }

    // ── Tree is present: compute split layout ─────────────────────────────────
    let above_rows = if scroll < tree_start {
        (tree_start - scroll).min(view_h)
    } else {
        0
    };
    let tree_visible_start = tree_start.max(scroll);
    let tree_visible_end = tree_end.min(view_end);
    let tree_rows = if tree_visible_end > tree_visible_start {
        tree_visible_end - tree_visible_start
    } else {
        0
    };
    let below_rows = view_h.saturating_sub(above_rows + tree_rows);

    if !*detail_in_tree && tree_rows > 0 {
        let desired_offset = scroll.saturating_sub(tree_start);
        let current_offset = schema_tree_state.get_offset();
        if desired_offset > current_offset {
            schema_tree_state.scroll_down(desired_offset - current_offset);
        } else if desired_offset < current_offset {
            schema_tree_state.scroll_up(current_offset - desired_offset);
        }
    }

    let mut constraints: Vec<Constraint> = Vec::new();
    if above_rows > 0 {
        constraints.push(Constraint::Length(above_rows as u16));
    }
    if tree_rows > 0 {
        constraints.push(Constraint::Length(tree_rows as u16));
    }
    if below_rows > 0 {
        constraints.push(Constraint::Length(below_rows as u16));
    }
    if constraints.is_empty() {
        return;
    }

    let rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner_area);

    let matches = detail_search_matches.clone();
    let mut rect_idx = 0usize;

    // ── Render lines_above ────────────────────────────────────────────────────
    if above_rows > 0 {
        let above_scroll = scroll;
        highlight_matched_lines(&mut lines, 0, &matches);
        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((above_scroll as u16, 0));
        frame.render_widget(para, rects[rect_idx]);
        rect_idx += 1;
    }

    // ── Render tree widget ────────────────────────────────────────────────────
    if tree_rows > 0 {
        let tree_items = schema_children_to_tree_items(&owned_effective_children, tree_id_start);
        let in_tree = *detail_in_tree;
        let tree_block = Block::default()
            .borders(Borders::LEFT)
            .border_style(if in_tree {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            });

        match Tree::new(&tree_items) {
            Ok(tree_widget) => {
                let tree_widget = tree_widget
                    .block(tree_block)
                    .highlight_style(if in_tree {
                        Style::default()
                            .bg(Color::Cyan)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().bg(Color::DarkGray).fg(Color::White)
                    })
                    .highlight_symbol("  ");
                frame.render_stateful_widget(tree_widget, rects[rect_idx], schema_tree_state);
            }
            Err(_) => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "(schema display error)",
                        Style::default().fg(Color::DarkGray),
                    )),
                    rects[rect_idx],
                );
            }
        }
        rect_idx += 1;
    }

    // ── Render lines_below ────────────────────────────────────────────────────
    if below_rows > 0 {
        let below_scroll = if scroll >= tree_end {
            scroll - tree_end
        } else {
            0
        };
        highlight_matched_lines(&mut lines_below, tree_end, &matches);
        let para = Paragraph::new(lines_below)
            .wrap(Wrap { trim: false })
            .scroll((below_scroll as u16, 0));
        frame.render_widget(para, rects[rect_idx]);
    }
}

// ── Schema tree helpers ───────────────────────────────────────────────────────

fn schema_effective_roots(node: &SchemaNode) -> (Vec<Line<'static>>, &[SchemaNode], usize) {
    let mut header: Vec<Line<'static>> = Vec::new();

    let root_kind = node.kind.label().to_string();
    let mut root_spans: Vec<Span<'static>> = vec![
        Span::styled("  type  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            root_kind,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(ref desc) = node.description {
        root_spans.push(Span::styled(
            format!("   {}", truncate(desc, 80)),
            Style::default().fg(Color::Gray),
        ));
    }
    header.push(Line::from(root_spans));

    if node.kind == SchemaKindHint::Array {
        if let Some(items) = node.children.first() {
            let items_kind = items.kind.label().to_string();
            let mut items_spans: Vec<Span<'static>> = vec![
                Span::styled("  items ", Style::default().fg(Color::DarkGray)),
                Span::styled(items_kind, Style::default().fg(Color::Cyan)),
            ];
            if let Some(ref desc) = items.description {
                items_spans.push(Span::styled(
                    format!("   {}", truncate(desc, 72)),
                    Style::default().fg(Color::Gray),
                ));
            }
            header.push(Line::from(items_spans));

            let id_start = 2usize;
            return (header, &items.children, id_start);
        }
    }

    (header, &node.children, 1usize)
}

fn schema_children_to_tree_items(
    children: &[SchemaNode],
    id_start: usize,
) -> Vec<TreeItem<'static, usize>> {
    let mut counter = id_start;
    children
        .iter()
        .map(|child| schema_node_to_tree_item(child, &mut counter))
        .collect()
}

fn schema_node_to_tree_item(node: &SchemaNode, counter: &mut usize) -> TreeItem<'static, usize> {
    let id = *counter;
    *counter += 1;

    let kind_label = node.kind.label();
    let req_marker = if node.required { "*" } else { "" };

    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(
            format!("{}{}", node.label.clone(), req_marker),
            if node.required {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            },
        ),
        Span::styled(
            format!("  {}", kind_label),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    if let Some(ref desc) = node.description {
        spans.push(Span::styled(
            format!("  — {}", truncate(desc, 60)),
            Style::default().fg(Color::Gray),
        ));
    }

    let text = Line::from(spans);

    if node.children.is_empty() {
        TreeItem::new_leaf(id, text)
    } else {
        let children: Vec<TreeItem<'static, usize>> = node
            .children
            .iter()
            .map(|child| schema_node_to_tree_item(child, counter))
            .collect();
        TreeItem::new(id, text, children)
            .unwrap_or_else(|_| TreeItem::new_leaf(id, Line::from(Span::raw(node.label.clone()))))
    }
}

fn count_visible_tree_rows(
    children: &[SchemaNode],
    id_start: usize,
    opened: &std::collections::HashSet<Vec<usize>>,
) -> usize {
    let mut counter = id_start;
    let mut total = 0usize;
    for child in children {
        total += count_node_rows(child, &[], &mut counter, opened);
    }
    total
}

fn count_node_rows(
    node: &SchemaNode,
    parent_path: &[usize],
    id_counter: &mut usize,
    opened: &std::collections::HashSet<Vec<usize>>,
) -> usize {
    let my_id = *id_counter;
    *id_counter += 1;

    let mut count = 1usize;

    let my_path: Vec<usize> = parent_path
        .iter()
        .chain(std::iter::once(&my_id))
        .cloned()
        .collect();

    if !node.children.is_empty() && opened.contains(&my_path) {
        for child in &node.children {
            count += count_node_rows(child, &my_path, id_counter, opened);
        }
    } else {
        fn skip_subtree(node: &SchemaNode, counter: &mut usize) {
            *counter += 1;
            for child in &node.children {
                skip_subtree(child, counter);
            }
        }
        for child in &node.children {
            skip_subtree(child, id_counter);
        }
    }

    count
}

// ── Path detail ───────────────────────────────────────────────────────────────

fn draw_path_detail(
    frame: &mut Frame,
    specs: &[Spec],
    focus: &Focus,
    specs_state: &ListState,
    trees: &[TreeCursor],
    ops: &OpsState,
    area: Rect,
) {
    let spec_idx = specs_state.selected().unwrap_or(0);
    let is_detail_focused = matches!(focus, Focus::Ops | Focus::Tree);

    let resolved = trees.get(spec_idx).and_then(|t| {
        let path_idx = t.selected_path_index()?;
        let entry = specs.get(spec_idx)?.paths.get(path_idx)?;
        Some((entry.path.clone(), entry.operations.len(), path_idx))
    });

    let (path_str, op_count, path_idx) = match resolved {
        Some(v) => v,
        None => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Detail ")
                .border_style(border_style(false));
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "select a path",
                    Style::default().fg(Color::DarkGray),
                ))
                .block(block),
                area,
            );
            return;
        }
    };

    let detail_title = format!(" {} ", path_str);

    if op_count == 0 {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(detail_title)
            .border_style(border_style(is_detail_focused));
        frame.render_widget(
            Paragraph::new(Span::styled(
                "no operations",
                Style::default().fg(Color::DarkGray),
            ))
            .block(block),
            area,
        );
        return;
    }

    // Resolve selected op index via filtered ops
    let op_idx = {
        let spec = match specs.get(spec_idx) {
            Some(s) => s,
            None => return,
        };
        let tree = match trees.get(spec_idx) {
            Some(t) => t,
            None => return,
        };
        let idx = match tree.selected_path_index() {
            Some(i) => i,
            None => return,
        };
        let entry = match spec.paths.get(idx) {
            Some(e) => e,
            None => return,
        };
        let filtered: Vec<(usize, &str, &str)> = entry
            .operations
            .iter()
            .enumerate()
            .filter(|(_, op)| {
                let label = op
                    .summary
                    .as_deref()
                    .or(op.operation_id.as_deref())
                    .unwrap_or(&op.method);
                ops.search.matches(&op.method) || ops.search.matches(label)
            })
            .map(|(i, op)| {
                let label = op
                    .summary
                    .as_deref()
                    .or(op.operation_id.as_deref())
                    .unwrap_or("");
                (i, op.method.as_str(), label)
            })
            .collect();
        filtered
            .get(ops.selected())
            .map(|(i, _, _)| *i)
            .unwrap_or(0)
    };

    let op = match specs
        .get(spec_idx)
        .and_then(|s| s.paths.get(path_idx))
        .and_then(|e| e.operations.get(op_idx))
    {
        Some(o) => o,
        None => return,
    };

    let mut lines: Vec<Line> = Vec::new();

    // ── Header: method badge + summary/operation_id ───────────────────────────
    let badge_style = method_color(&op.method).add_modifier(Modifier::BOLD);
    let mut header = vec![
        Span::styled(format!(" {} ", op.method), badge_style),
        Span::raw("  "),
    ];
    if let Some(ref sum) = op.summary {
        header.push(Span::styled(
            sum.as_str(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    } else if let Some(ref oid) = op.operation_id {
        header.push(Span::styled(
            oid.as_str(),
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(header));

    // ── Operation ID (when summary is also present) ────────────────────────────
    if op.summary.is_some() {
        if let Some(ref oid) = op.operation_id {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("id      ", Style::default().fg(Color::DarkGray)),
                Span::styled(oid.as_str(), Style::default().fg(Color::Gray)),
            ]));
        }
    }

    // ── Description ───────────────────────────────────────────────────────────
    if let Some(ref desc) = op.description {
        if op.summary.as_deref() != Some(desc.as_str()) {
            lines.push(Line::raw(""));
            for desc_line in desc.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {}", desc_line),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }

    lines.push(Line::raw(""));

    // ── Parameters ────────────────────────────────────────────────────────────
    if !op.params.is_empty() {
        let locations = ["path", "query", "header", "cookie"];
        for loc in locations {
            let group: Vec<_> = op.params.iter().filter(|p| p.location == loc).collect();
            if group.is_empty() {
                continue;
            }
            let label = format!("{:<8}", loc);
            let mut param_line = vec![
                Span::raw("  "),
                Span::styled(label, Style::default().fg(Color::DarkGray)),
            ];
            for (i, p) in group.iter().enumerate() {
                if i > 0 {
                    param_line.push(Span::raw("  "));
                }
                let name_style = if p.required {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                param_line.push(Span::styled(
                    if p.required {
                        format!("{}*", p.name)
                    } else {
                        p.name.clone()
                    },
                    name_style,
                ));
                if let Some(ref desc) = p.description {
                    param_line.push(Span::styled(
                        format!(" ({})", truncate(desc, 30)),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
            lines.push(Line::from(param_line));
        }
    }

    // ── Request body ──────────────────────────────────────────────────────────
    if let Some(ref rb) = op.request_body {
        let summary = if rb.fields.is_empty() {
            "request body".to_string()
        } else {
            let names: Vec<&str> = rb.fields.iter().take(3).map(|f| f.name.as_str()).collect();
            let suffix = if rb.fields.len() > 3 {
                format!(" +{} more", rb.fields.len() - 3)
            } else {
                String::new()
            };
            format!("{}{}", names.join(", "), suffix)
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("body    ", Style::default().fg(Color::DarkGray)),
            Span::styled(summary, Style::default().fg(Color::Magenta)),
        ]));
    }

    // ── Responses ─────────────────────────────────────────────────────────────
    if !op.responses.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("responses", Style::default().fg(Color::DarkGray)),
        ]));
        for (code, desc) in &op.responses {
            let mut resp_line = vec![
                Span::raw("    "),
                Span::styled(format!(" {} ", code), response_code_style(code)),
            ];
            if let Some(d) = desc {
                resp_line.push(Span::raw("  "));
                resp_line.push(Span::styled(d.clone(), Style::default().fg(Color::Gray)));
            }
            lines.push(Line::from(resp_line));
        }
    }

    let detail = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(detail_title)
                .border_style(border_style(is_detail_focused)),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(detail, area);
}

// ── Hint bar ──────────────────────────────────────────────────────────────────

fn draw_hint(
    frame: &mut Frame,
    focus: &Focus,
    _ops: &OpsState,
    detail_in_tree: &bool,
    detail_search: &Search,
    area: Rect,
) {
    let text = if detail_search.active && *focus == Focus::Detail {
        " type to search  Enter: jump to first  Esc: cancel  Backspace: delete  Ctrl+U: clear"
    } else {
        match focus {
            Focus::Detail if *detail_in_tree => " j/k: navigate  h/l: collapse/expand  f: unfocus  q: quit",
            Focus::Detail => " j/k: scroll  Ctrl-D/U: half-page  f: focus schema  /: search  n/N: next/prev  gg/G: top/bottom  Esc/h: back  q: quit",
            Focus::Ops => " j/k: navigate  Ctrl-D/U: half-page  gg/G: top/bottom  /: search  Enter: expand  h: back  q/Esc: quit",
            _ => " j/k: navigate  Ctrl-D/U: half-page  gg/G: top/bottom  h/l: columns  /: search  Enter: detail  q/Esc: quit",
        }
    };
    let hint = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(hint, area);
}

// ─── Style helpers ────────────────────────────────────────────────────────────

fn line_plain_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn highlight_matched_lines(lines: &mut [Line], offset: usize, matches: &[usize]) {
    if matches.is_empty() {
        return;
    }
    for (i, line) in lines.iter_mut().enumerate() {
        if matches.binary_search(&(offset + i)).is_ok() {
            let style = Style::default().bg(Color::Yellow).fg(Color::Black);
            for span in &mut line.spans {
                span.style = span.style.patch(style);
            }
        }
    }
}

fn border_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn highlight_style(active: bool) -> Style {
    if active {
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    }
}

fn method_color(method: &str) -> Style {
    match method {
        "GET" => Style::default().fg(Color::Black).bg(Color::Green),
        "POST" => Style::default().fg(Color::Black).bg(Color::Blue),
        "PUT" => Style::default().fg(Color::Black).bg(Color::Yellow),
        "PATCH" => Style::default().fg(Color::Black).bg(Color::Cyan),
        "DELETE" => Style::default().fg(Color::Black).bg(Color::Red),
        _ => Style::default().fg(Color::Black).bg(Color::DarkGray),
    }
}

fn response_code_style(code: &str) -> Style {
    let base = Style::default().add_modifier(Modifier::BOLD);
    match code.chars().next() {
        Some('2') => base.fg(Color::Black).bg(Color::Green),
        Some('3') => base.fg(Color::Black).bg(Color::Cyan),
        Some('4') => base.fg(Color::Black).bg(Color::Yellow),
        Some('5') => base.fg(Color::Black).bg(Color::Red),
        _ => base.fg(Color::White).bg(Color::DarkGray),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
