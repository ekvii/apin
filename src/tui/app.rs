use std::io::Stdout;

use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    widgets::ListState,
};

use crate::universe::Spec;

use super::components::detail::{DetailView, Search};
use super::components::path_tree::{PathNode, build_tree};

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

// ─── Ops state ────────────────────────────────────────────────────────────────

/// Selection + search state for the operations panel.
#[derive(Default)]
pub(super) struct OpsState {
    pub(super) list: ListState,
    pub(super) search: Search,
    /// The path index this state belongs to.  When the selected path changes
    /// the ops state is reset so the user always starts at the top.
    path_index: Option<usize>,
}

impl OpsState {
    /// Ensure state matches `current_path_index`; reset if it changed.
    pub(super) fn sync(&mut self, current_path_index: Option<usize>, op_count: usize) {
        if self.path_index != current_path_index {
            self.path_index = current_path_index;
            self.search = Search::default();
            self.list.select(if op_count > 0 { Some(0) } else { None });
        }
    }

    pub(super) fn selected(&self) -> usize {
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

    pub(super) fn clamp(&mut self, filtered_len: usize) {
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
pub(super) struct Level {
    /// Index into the *unfiltered* children list.
    pub(super) selected: usize,
}

pub(super) struct TreeCursor {
    pub(super) root: PathNode,
    /// Stack of per-level selection state (unfiltered indices).
    pub(super) levels: Vec<Level>,
    /// Which level currently has keyboard focus.
    pub(super) active_col: usize,
    /// Search state for the active column.  Reset whenever the active column
    /// changes or a navigation event changes the parent node.
    pub(super) search: Search,
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
    pub(super) fn node_at_depth(&self, depth: usize) -> Option<&PathNode> {
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
    pub(super) fn selected_path_index(&self) -> Option<usize> {
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
    pub(super) fn filtered_children(&self) -> Vec<(usize, &str, bool)> {
        self.children_of_col(self.active_col)
            .iter()
            .enumerate()
            .filter(|(_, c)| self.search.matches(&c.label))
            .map(|(i, c)| (i, c.label.as_str(), c.is_leaf()))
            .collect()
    }

    /// Index within the filtered list that corresponds to the currently
    /// selected unfiltered index.  Falls back to 0.
    pub(super) fn filtered_cursor(&self) -> usize {
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

    pub(super) fn col_count(&self) -> usize {
        self.levels.len()
    }

    pub(super) fn breadcrumb(&self, col: usize) -> String {
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
    /// Full-screen operation detail panel state and rendering.
    pub(super) detail: DetailView,
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
            detail: DetailView::default(),
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
            Focus::Detail => self.detail.search.active,
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
        self.detail.sync_schema_tree(key);
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
            self.detail.scroll = 0;
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
            self.detail.scroll = 0;
            self.sync_schema_tree_state();
        }
    }

    // ── Detail view ───────────────────────────────────────────────────────────

    pub(super) fn detail_in_tree(&self) -> bool {
        self.detail.in_tree()
    }

    pub(super) fn detail_back(&mut self) {
        self.focus = Focus::Ops;
        self.detail.back();
    }

    pub(super) fn detail_scroll_down(&mut self, n: usize) {
        self.detail.scroll_down(n);
    }

    pub(super) fn detail_scroll_up(&mut self, n: usize) {
        self.detail.scroll_up(n);
    }

    pub(super) fn detail_scroll_half_down(&mut self) {
        self.detail.scroll_half_down();
    }

    pub(super) fn detail_scroll_half_up(&mut self) {
        self.detail.scroll_half_up();
    }

    pub(super) fn detail_scroll_top(&mut self) {
        self.detail.scroll_top();
    }

    pub(super) fn detail_scroll_bottom(&mut self) {
        self.detail.scroll_bottom();
    }

    pub(super) fn detail_focus_tree(&mut self) {
        self.detail.focus_tree();
    }

    pub(super) fn detail_unfocus_tree(&mut self) {
        self.detail.unfocus_tree();
    }

    // ── Schema tree navigation (when detail_in_tree is true) ──────────────────

    pub(super) fn schema_tree_key_down(&mut self) {
        self.detail.schema_tree_key_down();
    }

    pub(super) fn schema_tree_key_up(&mut self) {
        self.detail.schema_tree_key_up();
    }

    pub(super) fn schema_tree_key_left(&mut self) {
        self.detail.schema_tree_key_left();
    }

    pub(super) fn schema_tree_key_right(&mut self) {
        self.detail.schema_tree_key_right();
    }

    // ── Detail search ─────────────────────────────────────────────────────────

    pub(super) fn detail_search_open(&mut self) {
        self.detail.search_open();
    }

    pub(super) fn detail_search_push(&mut self, ch: char) {
        self.detail.search_push(ch);
    }

    pub(super) fn detail_search_pop(&mut self) {
        self.detail.search_pop();
    }

    pub(super) fn detail_search_clear(&mut self) {
        self.detail.search_clear();
    }

    pub(super) fn detail_search_cancel(&mut self) {
        self.detail.search_cancel();
    }

    pub(super) fn detail_search_enter(&mut self) {
        self.detail.search_enter();
    }

    pub(super) fn detail_search_next(&mut self) {
        self.detail.search_next();
    }

    pub(super) fn detail_search_prev(&mut self) {
        self.detail.search_prev();
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
            detail,
        } = self;

        terminal.draw(|frame| {
            draw_frame(frame, specs, focus, specs_state, trees, ops, detail);
        })?;
        Ok(())
    }
}

fn draw_frame(
    frame: &mut Frame,
    specs: &[Spec],
    focus: &Focus,
    specs_state: &mut ListState,
    trees: &[TreeCursor],
    ops: &mut OpsState,
    detail: &mut DetailView,
) {
    use super::components::{hint_bar, path_detail, spec_list, tree_columns};

    let area = frame.area();

    // Full-screen detail mode: skip the normal layout entirely.
    if *focus == Focus::Detail {
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);
        detail.draw(frame, specs, specs_state, trees, ops, vert[0]);
        hint_bar::draw(frame, focus, ops, &detail.in_tree, &detail.search, vert[1]);
        return;
    }

    let multi_spec = specs.len() > 1;

    let content_area = if multi_spec {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(area);
        spec_list::draw(frame, specs, focus, specs_state, cols[0]);
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

    tree_columns::draw(frame, specs, focus, specs_state, trees, ops, vert[0]);
    path_detail::draw(frame, specs, focus, specs_state, trees, ops, vert[1]);
    hint_bar::draw(frame, focus, ops, &detail.in_tree, &detail.search, vert[2]);
}
