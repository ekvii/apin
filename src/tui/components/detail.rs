use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use tui_tree_widget::{Tree, TreeItem, TreeState};

use crate::universe::{SchemaKindHint, SchemaNode, Spec};

use super::super::app::{OpsState, TreeCursor};

// ─── Search state ─────────────────────────────────────────────────────────────

/// Incremental search state for the detail view.
#[derive(Clone, Default)]
pub(crate) struct Search {
    pub(crate) query: String,
    pub(crate) active: bool,
}

impl Search {
    pub(crate) fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// Case-insensitive substring match.
    pub(crate) fn matches(&self, label: &str) -> bool {
        if self.query.is_empty() {
            return true;
        }
        label
            .to_ascii_lowercase()
            .contains(&self.query.to_ascii_lowercase())
    }
}

// ─── DetailView ───────────────────────────────────────────────────────────────

/// All state and rendering logic for the full-screen operation detail panel.
pub(crate) struct DetailView {
    /// Scroll offset (in virtual lines).
    pub(crate) scroll: usize,
    /// Whether the cursor is currently inside the inline schema tree widget.
    pub(crate) in_tree: bool,
    /// Virtual line index where the inline schema tree starts (cached from last draw).
    tree_start: usize,
    /// Number of currently visible rows in the inline schema tree (cached from last draw).
    tree_len: usize,
    /// Fold/selection state for the schema tree widget.
    schema_tree_state: TreeState<usize>,
    /// The operation key for which `schema_tree_state` was last initialised.
    /// When it changes we reset the tree so nodes start collapsed.
    schema_tree_op_key: Option<(usize, usize, usize)>,
    /// Visible height of the detail view (rows), cached for half-page scroll.
    view_height: usize,
    /// Incremental search state.
    pub(crate) search: Search,
    /// Virtual line indices of lines that match the current search query.
    pub(crate) search_matches: Vec<usize>,
    /// Which match the cursor is currently on (index into `search_matches`).
    search_cursor: usize,
}

impl Default for DetailView {
    fn default() -> Self {
        Self {
            scroll: 0,
            in_tree: false,
            tree_start: 0,
            tree_len: 0,
            schema_tree_state: TreeState::default(),
            schema_tree_op_key: None,
            view_height: 0,
            search: Search::default(),
            search_matches: Vec::new(),
            search_cursor: 0,
        }
    }
}

impl DetailView {
    // ── State sync ────────────────────────────────────────────────────────────

    /// Reset the schema tree state when the selected operation changes.
    pub(crate) fn sync_schema_tree(&mut self, op_key: Option<(usize, usize, usize)>) {
        if self.schema_tree_op_key != op_key {
            self.schema_tree_op_key = op_key;
            self.schema_tree_state = TreeState::default();
            self.in_tree = false;
            self.tree_start = 0;
            self.tree_len = 0;
        }
    }

    // ── Action methods (called from events.rs) ────────────────────────────────

    pub(crate) fn in_tree(&self) -> bool {
        self.in_tree
    }

    pub(crate) fn back(&mut self) {
        self.scroll = 0;
        self.in_tree = false;
    }

    pub(crate) fn scroll_down(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_add(n);
    }

    pub(crate) fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    pub(crate) fn scroll_half_down(&mut self) {
        let half = (self.view_height / 2).max(1);
        self.scroll += half;
    }

    pub(crate) fn scroll_half_up(&mut self) {
        let half = (self.view_height / 2).max(1);
        self.scroll = self.scroll.saturating_sub(half);
    }

    pub(crate) fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    pub(crate) fn scroll_bottom(&mut self) {
        self.scroll = usize::MAX / 2;
    }

    pub(crate) fn focus_tree(&mut self) {
        if self.tree_len > 0 {
            self.in_tree = true;
            self.schema_tree_state.select_first();
        }
    }

    pub(crate) fn unfocus_tree(&mut self) {
        self.in_tree = false;
    }

    // ── Schema tree navigation ────────────────────────────────────────────────

    pub(crate) fn schema_tree_key_down(&mut self) {
        self.schema_tree_state.key_down();
    }

    pub(crate) fn schema_tree_key_up(&mut self) {
        self.schema_tree_state.key_up();
    }

    pub(crate) fn schema_tree_key_left(&mut self) {
        self.schema_tree_state.key_left();
    }

    pub(crate) fn schema_tree_key_right(&mut self) {
        self.schema_tree_state.key_right();
    }

    // ── Search ────────────────────────────────────────────────────────────────

    pub(crate) fn search_open(&mut self) {
        self.search.active = true;
        self.search.query.clear();
        self.search_matches.clear();
        self.search_cursor = 0;
    }

    pub(crate) fn search_push(&mut self, ch: char) {
        self.search.query.push(ch);
        if let Some(&line) = self.search_matches.first() {
            self.scroll = line;
            self.search_cursor = 0;
        }
    }

    pub(crate) fn search_pop(&mut self) {
        self.search.query.pop();
        if let Some(&line) = self.search_matches.first() {
            self.scroll = line;
            self.search_cursor = 0;
        }
    }

    pub(crate) fn search_clear(&mut self) {
        self.search.query.clear();
        self.search_matches.clear();
        self.search_cursor = 0;
    }

    pub(crate) fn search_cancel(&mut self) {
        self.search.active = false;
        self.search.query.clear();
        self.search_matches.clear();
        self.search_cursor = 0;
    }

    pub(crate) fn search_enter(&mut self) {
        self.search.active = false;
        if let Some(&line) = self.search_matches.first() {
            self.scroll = line;
        }
    }

    pub(crate) fn search_next(&mut self) {
        if !self.search_matches.is_empty() {
            self.search_cursor = (self.search_cursor + 1) % self.search_matches.len();
            self.scroll = self.search_matches[self.search_cursor];
        }
    }

    pub(crate) fn search_prev(&mut self) {
        if !self.search_matches.is_empty() {
            let len = self.search_matches.len();
            self.search_cursor = self.search_cursor.checked_sub(1).unwrap_or(len - 1);
            self.scroll = self.search_matches[self.search_cursor];
        }
    }

    // ── Draw ──────────────────────────────────────────────────────────────────

    /// Render the full-screen detail panel into `area`.
    pub(crate) fn draw(
        &mut self,
        frame: &mut Frame,
        specs: &[Spec],
        specs_state: &ratatui::widgets::ListState,
        trees: &[TreeCursor],
        ops: &OpsState,
        area: Rect,
    ) {
        let spec_idx = specs_state.selected().unwrap_or(0);

        // ── Resolve operation ─────────────────────────────────────────────────
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
            Some(o) => o.clone(),
            None => return,
        };

        // ── Determine if we have a schema tree to show ────────────────────────
        let has_schema = op
            .request_body
            .as_ref()
            .and_then(|rb| rb.schema_tree.as_ref())
            .is_some();

        // ── Build content lines (text portion) ────────────────────────────────
        let inner_w = (area.width.saturating_sub(2)) as usize;
        let divider: String = "─".repeat(inner_w);

        let mut lines: Vec<Line> = Vec::new();

        // ┌─ METHOD  PATH ──────────────────────────────────────────────────────┐
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

        // ── Summary ───────────────────────────────────────────────────────────
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

        // ── Operation ID ──────────────────────────────────────────────────────
        if let Some(ref oid) = op.operation_id {
            lines.push(Line::from(vec![
                Span::styled("  Operation ID ", Style::default().fg(Color::DarkGray)),
                Span::styled(oid.clone(), Style::default().fg(Color::Cyan)),
            ]));
        }

        // ── Description ───────────────────────────────────────────────────────
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

        // ── Parameters ────────────────────────────────────────────────────────
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
                        param_row
                            .push(Span::styled(desc.clone(), Style::default().fg(Color::Gray)));
                    }
                    lines.push(Line::from(param_row));
                }
                lines.push(Line::raw(""));
            }
        }

        // ── Request body header ───────────────────────────────────────────────
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

        // ── Responses (lines_below) ───────────────────────────────────────────
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

        // ── Virtual scroll layout ─────────────────────────────────────────────
        let tree_len: usize = if !owned_effective_children.is_empty() {
            let opened = self.schema_tree_state.opened().clone();
            count_visible_tree_rows(&owned_effective_children, tree_id_start, &opened)
        } else {
            0
        };

        let lines_above_len = lines.len();
        let tree_start = lines_above_len;
        let tree_end = tree_start + tree_len;

        self.tree_start = tree_start;
        self.tree_len = tree_len;

        if !self.search.query.is_empty() {
            let q = self.search.query.to_ascii_lowercase();
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
            if self.search_matches != matches {
                self.search_matches = matches;
                if self.search_cursor >= self.search_matches.len() {
                    self.search_cursor = 0;
                }
            }
        } else {
            self.search_matches.clear();
            self.search_cursor = 0;
        }

        let total_virtual = tree_end + lines_below.len();

        let in_tree_indicator = if self.in_tree { "●" } else { "○" };
        let search_indicator = if !self.search.query.is_empty() {
            let cursor = if self.search.active { "_" } else { "" };
            let n = self.search_matches.len();
            let cur = if n > 0 { self.search_cursor + 1 } else { 0 };
            format!("  /{}{} [{}/{}]", self.search.query, cursor, cur, n)
        } else if self.search.active {
            "  /_".to_string()
        } else {
            String::new()
        };
        let title = format!(
            " {} {}  [line {}/{}] {}{}",
            op.method,
            path_str,
            self.scroll.saturating_add(1).min(total_virtual),
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
        self.view_height = view_h;

        let max_scroll = total_virtual.saturating_sub(view_h);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }

        let scroll = self.scroll;
        let view_end = scroll + view_h;

        if tree_len == 0 || !has_schema {
            let matches = self.search_matches.clone();
            let mut combined: Vec<Line> =
                lines.into_iter().chain(lines_below.into_iter()).collect();
            highlight_matched_lines(&mut combined, 0, &matches);
            let para = Paragraph::new(combined)
                .wrap(Wrap { trim: false })
                .scroll((scroll as u16, 0));
            frame.render_widget(para, inner_area);
            return;
        }

        // ── Tree is present: compute split layout ─────────────────────────────
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

        if !self.in_tree && tree_rows > 0 {
            let desired_offset = scroll.saturating_sub(tree_start);
            let current_offset = self.schema_tree_state.get_offset();
            if desired_offset > current_offset {
                self.schema_tree_state
                    .scroll_down(desired_offset - current_offset);
            } else if desired_offset < current_offset {
                self.schema_tree_state
                    .scroll_up(current_offset - desired_offset);
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

        let matches = self.search_matches.clone();
        let mut rect_idx = 0usize;

        // ── Render lines_above ────────────────────────────────────────────────
        if above_rows > 0 {
            let above_scroll = scroll;
            highlight_matched_lines(&mut lines, 0, &matches);
            let para = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((above_scroll as u16, 0));
            frame.render_widget(para, rects[rect_idx]);
            rect_idx += 1;
        }

        // ── Render tree widget ────────────────────────────────────────────────
        if tree_rows > 0 {
            let tree_items =
                schema_children_to_tree_items(&owned_effective_children, tree_id_start);
            let in_tree = self.in_tree;
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
                    frame.render_stateful_widget(
                        tree_widget,
                        rects[rect_idx],
                        &mut self.schema_tree_state,
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

        // ── Render lines_below ────────────────────────────────────────────────
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
}

// ─── Schema tree helpers ──────────────────────────────────────────────────────

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
