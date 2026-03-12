mod path_tree;

use std::io::Stdout;

use anyhow::{Context, Result};
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use tokio::sync::mpsc;
use tui_tree_widget::{Tree, TreeItem, TreeState};

use crate::universe::{SchemaNode, Spec, Universe};
use path_tree::{build_tree, PathNode};

// ─── Terminal lifecycle ───────────────────────────────────────────────────────

pub async fn launch(
    universe: Universe,
    spec_rx: mpsc::UnboundedReceiver<anyhow::Result<Spec>>,
) -> Result<()> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen).context("failed to enter alternate screen")?;

    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;

    let result = run(&mut terminal, universe, spec_rx).await;

    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;
    terminal.show_cursor().context("failed to restore cursor")?;

    result
}

// ─── Focus ───────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Focus {
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

// ─── Ops state ───────────────────────────────────────────────────────────────

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
        if filtered_len == 0 { return; }
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
        if filtered_len == 0 { return; }
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

// ─── Tree cursor ─────────────────────────────────────────────────────────────

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

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn single_leaf(node: &PathNode) -> Option<usize> {
    if node.is_leaf() {
        return node.path_index;
    }
    if node.children.len() != 1 {
        return None;
    }
    single_leaf(&node.children[0])
}

// ─── Application state ───────────────────────────────────────────────────────

struct App {
    universe: Universe,
    focus: Focus,
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
}

impl App {
    fn new(universe: Universe) -> Self {
        let mut specs_state = ListState::default();
        if !universe.specs.is_empty() {
            specs_state.select(Some(0));
        }

        let trees: Vec<TreeCursor> = universe
            .specs
            .iter()
            .map(|s| {
                let path_strings: Vec<String> = s.paths.iter().map(|p| p.path.clone()).collect();
                let root = build_tree(&path_strings);
                let mut cursor = TreeCursor::new(root);
                cursor.open_next_level();
                cursor
            })
            .collect();

        let focus = if universe.specs.len() == 1 {
            Focus::Tree
        } else {
            Focus::Specs
        };

        Self {
            universe,
            focus,
            specs_state,
            trees,
            ops: OpsState::default(),
            detail_scroll: 0,
            detail_in_tree: false,
            detail_tree_start: 0,
            detail_tree_len: 0,
            schema_tree_state: TreeState::default(),
            schema_tree_op_key: None,
        }
    }

    fn selected_spec_index(&self) -> usize {
        self.specs_state.selected().unwrap_or(0)
    }

    fn selected_spec(&self) -> Option<&Spec> {
        self.universe.specs.get(self.selected_spec_index())
    }

    fn tree(&self) -> Option<&TreeCursor> {
        self.trees.get(self.selected_spec_index())
    }

    fn tree_mut(&mut self) -> Option<&mut TreeCursor> {
        let idx = self.selected_spec_index();
        self.trees.get_mut(idx)
    }

    fn is_searching(&self) -> bool {
        match self.focus {
            Focus::Tree => self.tree().map(|t| t.search.active).unwrap_or(false),
            Focus::Ops => self.ops.search.active,
            Focus::Specs | Focus::Detail => false,
        }
    }

    /// Filtered operations for the selected path, respecting ops search query.
    /// Returns `(original_index, method, label)` tuples.
    fn filtered_ops(&self) -> Vec<(usize, &str, &str)> {
        let Some(spec) = self.selected_spec() else { return vec![] };
        let Some(tree) = self.tree() else { return vec![] };
        let Some(idx) = tree.selected_path_index() else { return vec![] };
        let Some(entry) = spec.paths.get(idx) else { return vec![] };
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

    // ── Spec-list navigation ──────────────────────────────────────────────────

    fn specs_move_down(&mut self) {
        let len = self.universe.specs.len();
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

    fn specs_move_up(&mut self) {
        let next = self
            .specs_state
            .selected()
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0);
        self.specs_state.select(Some(next));
    }

    fn specs_move_top(&mut self) {
        self.specs_state.select(Some(0));
    }

    fn specs_move_bottom(&mut self) {
        let last = self.universe.specs.len().saturating_sub(1);
        self.specs_state.select(Some(last));
    }

    /// Add a newly-loaded spec and build its tree cursor.  The spec-list
    /// selection is left unchanged so the user keeps their current context.
    fn push_spec(&mut self, spec: Spec) {
        let path_strings: Vec<String> = spec.paths.iter().map(|p| p.path.clone()).collect();
        let root = build_tree(&path_strings);
        let mut cursor = TreeCursor::new(root);
        cursor.open_next_level();
        self.trees.push(cursor);
        self.universe.push_spec(spec);
        // If this is the very first spec, select it and switch focus to the tree.
        if self.universe.specs.len() == 1 {
            self.specs_state.select(Some(0));
            self.focus = Focus::Tree;
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
}

// ─── Event loop ──────────────────────────────────────────────────────────────

async fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    universe: Universe,
    mut spec_rx: mpsc::UnboundedReceiver<anyhow::Result<Spec>>,
) -> Result<()> {
    let mut app = App::new(universe);
    let mut pending_g = false;
    let mut events = EventStream::new();

    loop {
        terminal.draw(|frame| draw(frame, &mut app))?;

        tokio::select! {
            // A new spec finished loading — add it immediately and redraw.
            result = spec_rx.recv() => {
                match result {
                    Some(Ok(spec)) => app.push_spec(spec),
                    Some(Err(_)) => {} // parse error — silently skip for now
                    None => {}         // channel closed (all specs loaded)
                }
            }

            // A terminal event is ready — handle it.
            maybe_event = events.next() => {
                let Some(event_result) = maybe_event else { break };
                let event = event_result.context("failed to read event")?;

                if let Event::Key(key) = event {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    // ── Search input mode ─────────────────────────────────────
                    if app.is_searching() {
                        match key.code {
                            KeyCode::Esc => match app.focus {
                                Focus::Tree => {
                                    if let Some(t) = app.tree_mut() { t.search_cancel(); }
                                }
                                Focus::Ops => app.ops.search_cancel(),
                                Focus::Specs | Focus::Detail => {}
                            },
                            KeyCode::Enter => match app.focus {
                                Focus::Tree => {
                                    if let Some(t) = app.tree_mut() { t.search_commit(); }
                                }
                                Focus::Ops => app.ops.search_commit(),
                                Focus::Specs | Focus::Detail => {}
                            },
                            KeyCode::Backspace => match app.focus {
                                Focus::Tree => {
                                    if let Some(t) = app.tree_mut() { t.search_pop(); }
                                }
                                Focus::Ops => {
                                    app.ops.search_pop();
                                    let flen = app.filtered_ops().len();
                                    app.ops.clamp(flen);
                                }
                                Focus::Specs | Focus::Detail => {}
                            },
                            KeyCode::Char(ch) => {
                                // Ctrl+U clears the query (Unix readline convention).
                                if key.modifiers.contains(KeyModifiers::CONTROL) && ch == 'u' {
                                    match app.focus {
                                        Focus::Tree => {
                                            if let Some(t) = app.tree_mut() {
                                                t.search.query.clear();
                                                t.search_clamp_selection();
                                            }
                                        }
                                        Focus::Ops => {
                                            app.ops.search.query.clear();
                                            let flen = app.filtered_ops().len();
                                            app.ops.clamp(flen);
                                        }
                                        Focus::Specs | Focus::Detail => {}
                                    }
                                } else {
                                    match app.focus {
                                        Focus::Tree => {
                                            if let Some(t) = app.tree_mut() { t.search_push(ch); }
                                        }
                                        Focus::Ops => {
                                            app.ops.search_push(ch);
                                            let flen = app.filtered_ops().len();
                                            app.ops.clamp(flen);
                                        }
                                        Focus::Specs | Focus::Detail => {}
                                    }
                                }
                            }
                            KeyCode::Down => match app.focus {
                                Focus::Tree => {
                                    if let Some(t) = app.tree_mut() { t.move_down(); }
                                }
                                Focus::Ops => {
                                    let flen = app.filtered_ops().len();
                                    app.ops.move_down(flen);
                                }
                                Focus::Specs | Focus::Detail => {}
                            },
                            KeyCode::Up => match app.focus {
                                Focus::Tree => {
                                    if let Some(t) = app.tree_mut() { t.move_up(); }
                                }
                                Focus::Ops => app.ops.move_up(),
                                Focus::Specs | Focus::Detail => {}
                            },
                            _ => {}
                        }
                        continue;
                    }

                    // ── Detail full-screen mode ───────────────────────────────
                    if app.focus == Focus::Detail {
                        if app.detail_in_tree {
                            // ── Tree is focused: j/k/h/l navigate tree; Esc exits tree focus
                            match key.code {
                                // f: unfocus tree
                                KeyCode::Char('f') => {
                                    app.detail_in_tree = false;
                                }
                                KeyCode::Char('q') => break,
                                KeyCode::Char('j') | KeyCode::Down => {
                                    app.schema_tree_state.key_down();
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    app.schema_tree_state.key_up();
                                }
                                KeyCode::Char('h') | KeyCode::Left => {
                                    app.schema_tree_state.key_left();
                                }
                                KeyCode::Char('l') | KeyCode::Right => {
                                    app.schema_tree_state.key_right();
                                }
                                _ => {}
                            }
                        } else {
                            // ── Normal scrolling; Tab/f/Enter focuses tree if visible
                            match key.code {
                                KeyCode::Backspace => {
                                    app.focus = Focus::Ops;
                                    app.detail_scroll = 0;
                                    app.detail_in_tree = false;
                                }
                                KeyCode::Esc | KeyCode::Char('h') => {
                                    app.focus = Focus::Ops;
                                    app.detail_scroll = 0;
                                    app.detail_in_tree = false;
                                }
                                KeyCode::Char('q') => break,
                                KeyCode::Char('j') | KeyCode::Down => {
                                    app.detail_scroll += 1;
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    app.detail_scroll = app.detail_scroll.saturating_sub(1);
                                }
                                // f: focus the schema tree if it exists
                                KeyCode::Char('f') => {
                                    if app.detail_tree_len > 0 {
                                        app.detail_in_tree = true;
                                        app.schema_tree_state.select_first();
                                    }
                                }
                                KeyCode::Char('g') => pending_g = true,
                                KeyCode::Char('G') => {
                                    app.detail_scroll = usize::MAX / 2;
                                }
                                _ => {}
                            }
                        }
                        continue;
                    }

                    // ── Normal navigation mode ────────────────────────────────

                    if pending_g {
                        pending_g = false;
                        if key.code == KeyCode::Char('g') {
                            match app.focus {
                                Focus::Specs => app.specs_move_top(),
                                Focus::Tree => {
                                    if let Some(t) = app.tree_mut() { t.move_top(); }
                                }
                                Focus::Ops => app.ops.move_top(),
                                Focus::Detail => {
                                    app.detail_scroll = 0;
                                }
                            }
                        }
                        continue;
                    }

                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,

                        // Open search on `/`
                        KeyCode::Char('/') => match app.focus {
                            Focus::Tree => {
                                if let Some(t) = app.tree_mut() {
                                    t.search.active = true;
                                }
                            }
                            Focus::Ops => {
                                app.ops.search.active = true;
                            }
                            Focus::Specs | Focus::Detail => {}
                        },

                        KeyCode::Char('j') | KeyCode::Down => match app.focus {
                            Focus::Specs => app.specs_move_down(),
                            Focus::Tree => {
                                if let Some(t) = app.tree_mut() { t.move_down(); }
                            }
                            Focus::Ops => {
                                let flen = app.filtered_ops().len();
                                app.ops.move_down(flen);
                            }
                            Focus::Detail => {
                                app.detail_scroll = app.detail_scroll.saturating_add(1);
                            }
                        },

                        KeyCode::Char('k') | KeyCode::Up => match app.focus {
                            Focus::Specs => app.specs_move_up(),
                            Focus::Tree => {
                                if let Some(t) = app.tree_mut() { t.move_up(); }
                            }
                            Focus::Ops => app.ops.move_up(),
                            Focus::Detail => {
                                app.detail_scroll = app.detail_scroll.saturating_sub(1);
                            }
                        },

                        KeyCode::Char('g') => pending_g = true,

                        KeyCode::Char('G') => match app.focus {
                            Focus::Specs => app.specs_move_bottom(),
                            Focus::Tree => {
                                if let Some(t) = app.tree_mut() { t.move_bottom(); }
                            }
                            Focus::Ops => {
                                let flen = app.filtered_ops().len();
                                app.ops.move_bottom(flen);
                            }
                            Focus::Detail => {
                                app.detail_scroll = usize::MAX / 2;
                            }
                        },

                        KeyCode::Enter => match app.focus {
                            Focus::Ops => {
                                // Enter detail full-screen mode for the selected operation.
                                if app.selected_operation_index().is_some() {
                                    app.focus = Focus::Detail;
                                    app.detail_scroll = 0;
                                    app.sync_schema_tree_state();
                                }
                            }
                            Focus::Tree => {
                                // If a path is selected and its detail is already visible
                                // (ops panel showing), Enter jumps straight to Detail.
                                let path_idx = app.tree().and_then(|t| t.selected_path_index());
                                let op_count = path_idx
                                    .and_then(|i| app.selected_spec()?.paths.get(i))
                                    .map(|e| e.operations.len())
                                    .unwrap_or(0);
                                if op_count > 0 {
                                    app.ops.sync(path_idx, op_count);
                                    app.focus = Focus::Detail;
                                    app.detail_scroll = 0;
                                    app.sync_schema_tree_state();
                                }
                            }
                            Focus::Specs | Focus::Detail => {}
                        },

                        KeyCode::Char('l') | KeyCode::Right | KeyCode::Tab => match app.focus {
                            Focus::Specs => {
                                app.focus = Focus::Tree;
                                if let Some(t) = app.tree_mut() { t.active_col = 0; }
                            }
                            Focus::Tree => {
                                // Try to drill into tree children first.
                                let moved = if let Some(t) = app.tree_mut() {
                                    t.move_right()
                                } else {
                                    false
                                };
                                // If at a leaf (can't go right in tree), move to Ops.
                                if !moved {
                                    let path_idx = app.tree()
                                        .and_then(|t| t.selected_path_index());
                                    let op_count = path_idx
                                        .and_then(|i| app.selected_spec()?.paths.get(i))
                                        .map(|e| e.operations.len())
                                        .unwrap_or(0);
                                    if op_count > 0 {
                                        app.ops.sync(path_idx, op_count);
                                        app.focus = Focus::Ops;
                                    }
                                }
                            }
                            Focus::Ops | Focus::Detail => {
                                // Already the rightmost panel — no-op.
                            }
                        },

                        KeyCode::Char('h') | KeyCode::Left => match app.focus {
                            Focus::Specs => {}
                            Focus::Tree => {
                                let moved = if let Some(t) = app.tree_mut() {
                                    t.move_left()
                                } else {
                                    false
                                };
                                if !moved && app.universe.specs.len() > 1 {
                                    app.focus = Focus::Specs;
                                }
                            }
                            Focus::Ops => {
                                app.focus = Focus::Tree;
                            }
                            Focus::Detail => {
                                app.focus = Focus::Ops;
                                app.detail_scroll = 0;
                            }
                        },

                        KeyCode::Backspace => match app.focus {
                            Focus::Detail => {
                                app.focus = Focus::Ops;
                                app.detail_scroll = 0;
                            }
                            _ => {}
                        },

                        _ => {}
                    }
                }
            }
        }
    }

    Ok(())
}

// ─── Drawing ─────────────────────────────────────────────────────────────────

/// Minimum column width in terminal cells (including borders).
const MIN_COL_WIDTH: u16 = 14;

fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Full-screen detail mode: skip the normal layout entirely.
    if app.focus == Focus::Detail {
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);
        draw_detail_fullscreen(frame, app, vert[0]);
        draw_hint(frame, app, vert[1]);
        return;
    }

    let multi_spec = app.universe.specs.len() > 1;

    let content_area = if multi_spec {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(area);
        draw_spec_list(frame, app, cols[0]);
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

    draw_tree_columns(frame, app, vert[0]);
    draw_path_detail(frame, app, vert[1]);
    draw_hint(frame, app, vert[2]);
}

// ── Spec list ─────────────────────────────────────────────────────────────────

fn draw_spec_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let active = app.focus == Focus::Specs;

    let items: Vec<ListItem> = app
        .universe
        .specs
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

    frame.render_stateful_widget(list, area, &mut app.specs_state);
}

// ── Tree columns ──────────────────────────────────────────────────────────────

fn draw_tree_columns(frame: &mut Frame, app: &mut App, area: Rect) {
    let spec_idx = app.selected_spec_index();
    let is_tree_focused = app.focus == Focus::Tree;
    let is_ops_focused = app.focus == Focus::Ops;

    let tree_col_count = app.trees.get(spec_idx).map(|t| t.col_count()).unwrap_or(0);
    let tree_active_col = app.trees.get(spec_idx).map(|t| t.active_col).unwrap_or(0);

    let spec_title = app.selected_spec().map(|s| {
        let fname = s
            .file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");
        format!(" {} v{} [{}] ", s.title, s.version, fname)
    });

    // Sync ops state based on currently selected path.
    let path_idx = app.tree().and_then(|t| t.selected_path_index());
    let op_count = path_idx
        .and_then(|i| app.selected_spec()?.paths.get(i))
        .map(|e| e.operations.len())
        .unwrap_or(0);
    app.ops.sync(path_idx, op_count);

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
    let logical_active = if is_ops_focused { tree_col_count } else { tree_active_col };

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
            draw_ops_column(frame, app, col_rects[render_idx], path_idx, op_count);
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
            let tree = match app.trees.get(spec_idx) {
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
            format!("{} /{}{} ", data.title.trim_end(), data.search_query, indicator)
        } else {
            data.title.clone()
        };

        let list_items: Vec<ListItem> = data
            .items
            .iter()
            .map(|(label, is_leaf)| {
                // "." is a synthetic self-endpoint node — display it as "(self)"
                // so the user understands it represents the parent path itself.
                let display = if label == "." { "(self)" } else { label.as_str() };
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
    app: &mut App,
    area: Rect,
    path_idx: Option<usize>,
    _op_count: usize,
) {
    let is_ops_focused = app.focus == Focus::Ops;

    // Build filtered list: (original_op_index, method).
    let filtered: Vec<(usize, &str)> = {
        let spec = match app.selected_spec() {
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
            .filter(|(_, op)| app.ops.search.matches(&op.method))
            .map(|(i, op)| (i, op.method.as_str()))
            .collect()
    };

    let title = if is_ops_focused && (!app.ops.search.query.is_empty() || app.ops.search.active) {
        let cursor = if app.ops.search.active { "_" } else { "" };
        format!(" Methods /{}{} ", app.ops.search.query, cursor)
    } else {
        " Methods ".to_string()
    };

    let items: Vec<ListItem> = filtered
        .iter()
        .map(|(_, method)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", method),
                    method_color(method).add_modifier(Modifier::BOLD),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(if is_ops_focused && app.ops.search.active {
                    Style::default().fg(Color::Yellow)
                } else {
                    border_style(is_ops_focused)
                }),
        )
        .highlight_style(highlight_style(is_ops_focused));

    app.ops.clamp(filtered.len());
    frame.render_stateful_widget(list, area, &mut app.ops.list);
}

// ── Detail full-screen ────────────────────────────────────────────────────────

/// Renders the selected operation as a maximised, scrollable detail panel that
/// occupies the full terminal (minus the hint bar).
fn draw_detail_fullscreen(frame: &mut Frame, app: &mut App, area: Rect) {
    // ── Resolve operation ──────────────────────────────────────────────────────
    let resolved = app.tree().and_then(|t| {
        let path_idx = t.selected_path_index()?;
        let entry = app.selected_spec()?.paths.get(path_idx)?;
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
                Paragraph::new(Span::styled("no operation selected", Style::default().fg(Color::DarkGray)))
                    .block(block),
                area,
            );
            return;
        }
    };

    let op_idx = app.selected_operation_index().unwrap_or(0);
    let op = match app
        .selected_spec()
        .and_then(|s| s.paths.get(path_idx))
        .and_then(|e| e.operations.get(op_idx))
    {
        Some(o) => o.clone(),
        None => return,
    };

    // ── Determine if we have a schema tree to show ─────────────────────────────
    let has_schema = op.request_body.as_ref().and_then(|rb| rb.schema_tree.as_ref()).is_some();

    // ── Build content lines (text portion) ────────────────────────────────────
    let inner_w = (area.width.saturating_sub(2)) as usize;
    let divider: String = "─".repeat(inner_w);

    let mut lines: Vec<Line> = Vec::new();

    // ┌─ METHOD  PATH ──────────────────────────────────────────────────────────┐
    let badge_style = method_color(&op.method).add_modifier(Modifier::BOLD);
    lines.push(Line::from(vec![
        Span::styled(format!(" {} ", op.method), badge_style),
        Span::raw("  "),
        Span::styled(path_str.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::raw(""));

    // ── Summary ───────────────────────────────────────────────────────────────
    if let Some(ref sum) = op.summary {
        lines.push(Line::from(vec![
            Span::styled("  Summary      ", Style::default().fg(Color::DarkGray)),
            Span::styled(sum.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
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
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::raw(""));

        let locations = [
            ("path",   Color::Magenta),
            ("query",  Color::Cyan),
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
                    Style::default().fg(Color::Black).bg(loc_color).add_modifier(Modifier::BOLD),
                ),
            ]));

            // Column headers
            lines.push(Line::from(vec![
                Span::styled("    name                 type       req   description", Style::default().fg(Color::DarkGray)),
            ]));

            for p in &group {
                let name_style = if p.required {
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let req_span = if p.required {
                    Span::styled(" yes  ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
                } else {
                    Span::styled(" no   ", Style::default().fg(Color::DarkGray))
                };
                let name_padded = format!("{:<22}", if p.required { format!("{}*", p.name) } else { p.name.clone() });
                let mut param_row = vec![
                    Span::raw("    "),
                    Span::styled(name_padded, name_style),
                    Span::styled(format!("{:<10} ", "string"), Style::default().fg(Color::DarkGray)),
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
    // The schema tree itself is rendered separately below; here we just emit
    // the section header, optional description, and schema type summary.
    // When a schema is present, we also show a focus hint and the schema header.
    let schema_node_opt = op
        .request_body
        .as_ref()
        .and_then(|rb| rb.schema_tree.as_ref());

    // Pre-compute effective roots and header lines so we can embed them in lines_above.
    // We clone the children into an owned Vec to avoid borrow-checker lifetime issues
    // when we later need to borrow `op` again for the responses section.
    let (_schema_header_lines, owned_effective_children, tree_id_start) =
        if let Some(ref node) = schema_node_opt {
            let (hdr, ch, id_start) = schema_effective_roots(node);
            (hdr, ch.to_vec(), id_start)
        } else {
            (vec![], vec![], 0)
        };
    let _effective_children: &[SchemaNode] = &owned_effective_children;

    if let Some(ref rb) = op.request_body {
        lines.push(Line::from(Span::styled(
            divider.clone(),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  REQUEST BODY",
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
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
                Span::styled("(schema not available)", Style::default().fg(Color::DarkGray)),
            ]));
            lines.push(Line::raw(""));
        } else {
            // Focus hint.
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
    // When a schema is present, responses go into `lines_below` so they appear
    // after the inline tree widget in the virtual scroll model.
    // When no schema, they go directly into `lines`.
    let mut lines_below: Vec<Line> = Vec::new();

    if !op.responses.is_empty() {
        let target: &mut Vec<Line> = if has_schema { &mut lines_below } else { &mut lines };
        target.push(Line::from(Span::styled(
            divider.clone(),
            Style::default().fg(Color::DarkGray),
        )));
        target.push(Line::from(Span::styled(
            "  RESPONSES",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
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
            target.push(Line::from(vec![
                Span::raw("    "),
                badge,
                desc_span,
            ]));
        }
        target.push(Line::raw(""));
    }

    // ── Virtual scroll layout ─────────────────────────────────────────────────
    // lines      = lines_above (everything before the inline schema tree)
    // lines_below = lines after the tree (responses)
    // tree occupies virtual lines [tree_start .. tree_start + tree_len)
    //
    // Virtual coordinate system:
    //   0 .. lines_above.len()                         → lines_above
    //   lines_above.len() .. lines_above.len()+tree_len → tree widget rows
    //   (lines_above.len()+tree_len) ..                 → lines_below
    //
    // detail_scroll is the virtual line index of the first visible row.

    // Compute visible tree node count based on current open state.
    let tree_len: usize = if !owned_effective_children.is_empty() {
        let opened = app.schema_tree_state.opened().clone();
        count_visible_tree_rows(&owned_effective_children, tree_id_start, &opened)
    } else {
        0
    };

    let lines_above_len = lines.len();
    let tree_start = lines_above_len;
    let tree_end = tree_start + tree_len;

    // Cache for keyboard handler.
    app.detail_tree_start = tree_start;
    app.detail_tree_len = tree_len;

    // Total virtual lines.
    let total_virtual = tree_end + lines_below.len();

    // Build outer block.
    let in_tree_indicator = if app.detail_in_tree { "●" } else { "○" };
    let title = format!(
        " {} {}  [line {}/{}] {} ",
        op.method, path_str,
        app.detail_scroll.saturating_add(1).min(total_virtual),
        total_virtual.max(1),
        in_tree_indicator,
    );

    let outer_block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD))
        .border_style(Style::default().fg(Color::Cyan));

    let inner_area = outer_block.inner(area);
    frame.render_widget(outer_block, area);

    let view_h = inner_area.height as usize;
    if view_h == 0 {
        return;
    }

    // Clamp scroll so we never scroll past the last line.
    let max_scroll = total_virtual.saturating_sub(view_h);
    if app.detail_scroll > max_scroll {
        app.detail_scroll = max_scroll;
    }

    let scroll = app.detail_scroll;
    let view_end = scroll + view_h; // exclusive

    // Determine which regions are visible.
    // above_visible: virtual lines [scroll .. min(view_end, tree_start)] that come from lines_above
    // tree_visible:  tree is visible if scroll < tree_end && view_end > tree_start
    // below_visible: virtual lines [max(scroll, tree_end) .. view_end] from lines_below

    if tree_len == 0 || !has_schema {
        // No tree: render everything as a single scrollable paragraph.
        let combined: Vec<Line> = lines.into_iter().chain(lines_below.into_iter()).collect();
        let para = Paragraph::new(combined)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0));
        frame.render_widget(para, inner_area);
        return;
    }

    // ── Tree is present: compute split layout ─────────────────────────────────
    // We need to render up to three zones in inner_area:
    //   [above_rect] lines from lines_above
    //   [tree_rect]  tree widget
    //   [below_rect] lines from lines_below

    // How many rows of lines_above are visible?
    let above_rows = if scroll < tree_start {
        (tree_start - scroll).min(view_h)
    } else {
        0
    };
    // Tree widget height in screen rows.
    let tree_visible_start = tree_start.max(scroll);
    let tree_visible_end = tree_end.min(view_end);
    let tree_rows = if tree_visible_end > tree_visible_start {
        tree_visible_end - tree_visible_start
    } else {
        0
    };
    // Lines-below rows.
    let below_rows = view_h.saturating_sub(above_rows + tree_rows);

    // When NOT in tree-focus mode, sync the tree widget's internal scroll
    // offset so that the tree scrolls along with the rest of the detail view.
    // When in tree-focus mode, the tree manages its own offset via key_down/key_up.
    if !app.detail_in_tree && tree_rows > 0 {
        let desired_offset = scroll.saturating_sub(tree_start);
        let current_offset = app.schema_tree_state.get_offset();
        if desired_offset > current_offset {
            app.schema_tree_state.scroll_down(desired_offset - current_offset);
        } else if desired_offset < current_offset {
            app.schema_tree_state.scroll_up(current_offset - desired_offset);
        }
    }

    // Build constraints for the layout.
    let mut constraints: Vec<Constraint> = Vec::new();
    if above_rows > 0 { constraints.push(Constraint::Length(above_rows as u16)); }
    if tree_rows > 0  { constraints.push(Constraint::Length(tree_rows as u16)); }
    if below_rows > 0 { constraints.push(Constraint::Length(below_rows as u16)); }
    if constraints.is_empty() { return; }

    let rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner_area);

    let mut rect_idx = 0usize;

    // ── Render lines_above ────────────────────────────────────────────────────
    if above_rows > 0 {
        let above_scroll = scroll; // first line of lines_above to show
        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((above_scroll as u16, 0));
        frame.render_widget(para, rects[rect_idx]);
        rect_idx += 1;
    }

    // ── Render tree widget ────────────────────────────────────────────────────
    if tree_rows > 0 {
        let tree_items = schema_children_to_tree_items(&owned_effective_children, tree_id_start);
        let in_tree = app.detail_in_tree;
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
                        Style::default()
                            .bg(Color::DarkGray)
                            .fg(Color::White)
                    })
                    .highlight_symbol("  ");
                frame.render_stateful_widget(
                    tree_widget,
                    rects[rect_idx],
                    &mut app.schema_tree_state,
                );
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
        // How far into lines_below are we scrolled?
        let below_scroll = if scroll >= tree_end {
            scroll - tree_end
        } else {
            0
        };
        let para = Paragraph::new(lines_below)
            .wrap(Wrap { trim: false })
            .scroll((below_scroll as u16, 0));
        frame.render_widget(para, rects[rect_idx]);
    }
}

// ── Schema tree helpers ───────────────────────────────────────────────────────

/// Determine which nodes are the "effective roots" to display in the tree and
/// what header information to show above it.
///
/// Rules:
/// - The real root node (the $ref name or "body") is never shown as a tree row.
///   Its label/kind/description become styled text lines returned in `header`.
/// - If the real root is an **Array**, we also skip the `items` child node and
///   promote its children (the item's properties) as the tree roots.  The items
///   node info is added to `header`.
/// - Otherwise the real root's children become the tree roots.
///
/// IDs: root=0 (skipped), root.children[i] get DFS ids starting at 1.
/// For the array case: root=0, items=1 (skipped), items.children[i] get ids
/// starting at 2.
fn schema_effective_roots(
    node: &SchemaNode,
) -> (Vec<Line<'static>>, &[SchemaNode], usize) {
    use crate::universe::SchemaKindHint;

    let mut header: Vec<Line<'static>> = Vec::new();

    // Root summary line.
    let root_kind = node.kind.label().to_string();
    let mut root_spans: Vec<Span<'static>> = vec![
        Span::styled("  type  ", Style::default().fg(Color::DarkGray)),
        Span::styled(root_kind, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
    ];
    if let Some(ref desc) = node.description {
        root_spans.push(Span::styled(
            format!("   {}", truncate(desc, 80)),
            Style::default().fg(Color::Gray),
        ));
    }
    header.push(Line::from(root_spans));

    // For array: also show the items kind.
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

            // id=0 is root (skipped), id=1 is items (skipped).
            // items.children start at id=2.
            // Compute id_start = 2 (root consumed 1, items consumed 1).
            let id_start = 2usize;
            return (header, &items.children, id_start);
        }
    }

    // Non-array (or empty array): root's children are the tree items.
    // id=0 is root (skipped), children start at id=1.
    (header, &node.children, 1usize)
}

/// Build `TreeItem`s for the effective root children, assigning DFS pre-order
/// IDs starting from `id_start`.
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
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
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
        TreeItem::new(id, text, children).unwrap_or_else(|_| {
            TreeItem::new_leaf(id, Line::from(Span::raw(node.label.clone())))
        })
    }
}

/// Count the number of visible rows in the *displayed* tree (i.e. after
/// skipping the root and optional items node) given the current open set.
///
/// `children` are the effective top-level nodes (as returned by
/// `schema_effective_roots`).  `id_start` is the DFS id of `children[0]`.
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

    // This node is 1 visible row.
    let mut count = 1usize;

    // The open path for this node: parent_path + [my_id].
    // tui-tree-widget stores just the identifier path from the top-level items.
    // Since our top-level items have IDs starting at id_start, the path to a
    // top-level item is vec![item_id], and to its child vec![item_id, child_id].
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
        // Skip IDs for invisible descendants.
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

fn draw_path_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let is_detail_focused = matches!(app.focus, Focus::Ops | Focus::Tree);

    // Resolve the selected operation.
    let resolved = app.tree().and_then(|t| {
        let path_idx = t.selected_path_index()?;
        let entry = app.selected_spec()?.paths.get(path_idx)?;
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
                Paragraph::new(Span::styled("select a path", Style::default().fg(Color::DarkGray)))
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
            Paragraph::new(Span::styled("no operations", Style::default().fg(Color::DarkGray)))
                .block(block),
            area,
        );
        return;
    }

    // Which operation to display: use ops panel selection if available, else first.
    let op_idx = app.selected_operation_index().unwrap_or(0);

    let op = match app.selected_spec().and_then(|s| s.paths.get(path_idx)).and_then(|e| e.operations.get(op_idx)) {
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
        header.push(Span::styled(sum.as_str(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)));
    } else if let Some(ref oid) = op.operation_id {
        header.push(Span::styled(oid.as_str(), Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD)));
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
            // Render description line-by-line so it wraps naturally in the panel.
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
        // Group by location.
        let locations = ["path", "query", "header", "cookie"];
        for loc in locations {
            let group: Vec<_> = op.params.iter().filter(|p| p.location == loc).collect();
            if group.is_empty() { continue; }
            let label = format!("{:<8}", loc);
            let mut param_line = vec![
                Span::raw("  "),
                Span::styled(label, Style::default().fg(Color::DarkGray)),
            ];
            for (i, p) in group.iter().enumerate() {
                if i > 0 { param_line.push(Span::raw("  ")); }
                let name_style = if p.required {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                param_line.push(Span::styled(
                    if p.required { format!("{}*", p.name) } else { p.name.clone() },
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

fn draw_hint(frame: &mut Frame, app: &App, area: Rect) {
    let text = if app.is_searching() {
        " type to filter  Enter: confirm  Esc: cancel  ↑/↓: move  Ctrl+U: clear"
    } else {
        match app.focus {
            Focus::Detail if app.detail_in_tree => " j/k: navigate  h/l: collapse/expand  Esc: unfocus  q: quit",
            Focus::Detail => " j/k: scroll  f/Tab: focus schema  gg/G: top/bottom  Backspace/Esc/h: back  q: quit",
            Focus::Ops => " j/k: navigate  gg/G: top/bottom  /: search  Enter: expand  h: back  q/Esc: quit",
            _ => " j/k: navigate  gg/G: top/bottom  h/l: columns  /: search  Enter: detail  q/Esc: quit",
        }
    };
    let hint = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(hint, area);
}

// ─── Style helpers ────────────────────────────────────────────────────────────

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
