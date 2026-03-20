use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use tui_tree_widget::{Tree, TreeItem, TreeState};

use crate::spec::{PathKind, SchemaKindHint, SchemaNode, Spec};

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
#[derive(Default)]
pub(crate) struct DetailView {
    /// Scroll offset (in virtual lines).
    pub(crate) scroll: usize,
    /// Whether the cursor is currently inside the request-body schema tree widget.
    pub(crate) in_tree: bool,
    /// Virtual line index where the request-body schema tree starts (cached from last draw).
    tree_start: usize,
    /// Number of currently visible rows in the request-body schema tree (cached from last draw).
    tree_len: usize,
    /// Fold/selection state for the request-body schema tree widget.
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

    // ── Response schema trees ─────────────────────────────────────────────────
    /// One `TreeState` per response that has a `schema_tree` (same order as
    /// responses with schemas, indexed by their 1-based hotkey N-1).
    resp_tree_states: Vec<TreeState<usize>>,
    /// Virtual start line of each response tree slot (cached from last draw).
    resp_tree_starts: Vec<usize>,
    /// Visible row-count of each response tree slot (cached from last draw).
    resp_tree_lens: Vec<usize>,
    /// Index into `resp_tree_states` that currently has keyboard focus, or `None`.
    pub(crate) focused_resp_tree: Option<usize>,
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
            self.resp_tree_states.clear();
            self.resp_tree_starts.clear();
            self.resp_tree_lens.clear();
            self.focused_resp_tree = None;
        }
    }

    // ── Action methods (called from events.rs) ────────────────────────────────

    pub(crate) fn in_tree(&self) -> bool {
        self.in_tree
    }

    pub(crate) fn back(&mut self) {
        self.scroll = 0;
        self.in_tree = false;
        self.focused_resp_tree = None;
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

    // ── Response tree focus / navigation ─────────────────────────────────────

    /// Returns `true` if any response tree currently has keyboard focus.
    pub(crate) fn in_resp_tree(&self) -> bool {
        self.focused_resp_tree.is_some()
    }

    /// Focus response tree `idx` (0-based, matching the 1-based hotkey digit).
    /// Ignored if `idx` is out of range.
    pub(crate) fn focus_resp_tree(&mut self, idx: usize) {
        if idx < self.resp_tree_states.len() {
            self.focused_resp_tree = Some(idx);
            self.in_tree = false;
            self.resp_tree_states[idx].select_first();
            // Scroll the viewport so the focused tree is visible.
            if idx < self.resp_tree_starts.len() {
                self.scroll = self.resp_tree_starts[idx];
            }
        }
    }

    /// Remove focus from whatever response tree is currently focused.
    pub(crate) fn unfocus_resp_tree(&mut self) {
        self.focused_resp_tree = None;
    }

    pub(crate) fn resp_tree_key_down(&mut self) {
        if let Some(i) = self.focused_resp_tree
            && let Some(s) = self.resp_tree_states.get_mut(i)
        {
            s.key_down();
        }
    }

    pub(crate) fn resp_tree_key_up(&mut self) {
        if let Some(i) = self.focused_resp_tree
            && let Some(s) = self.resp_tree_states.get_mut(i)
        {
            s.key_up();
        }
    }

    pub(crate) fn resp_tree_key_left(&mut self) {
        if let Some(i) = self.focused_resp_tree
            && let Some(s) = self.resp_tree_states.get_mut(i)
        {
            s.key_left();
        }
    }

    pub(crate) fn resp_tree_key_right(&mut self) {
        if let Some(i) = self.focused_resp_tree
            && let Some(s) = self.resp_tree_states.get_mut(i)
        {
            s.key_right();
        }
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

        let is_webhook = specs
            .get(spec_idx)
            .and_then(|s| s.paths.get(path_idx))
            .map(|e| e.kind == PathKind::Webhook)
            .unwrap_or(false);

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
        let mut title_line = vec![
            Span::styled(format!(" {} ", op.method), badge_style),
            Span::raw("  "),
        ];
        // Gap A: [WH] tag for webhook entries
        if is_webhook {
            title_line.push(Span::styled(
                "[WH] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        title_line.push(Span::styled(
            path_str.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
        // Gap C: [DEPRECATED] badge for the operation
        if op.deprecated {
            title_line.push(Span::raw("  "));
            title_line.push(Span::styled(
                "[deprecated]",
                Style::default().fg(Color::LightRed),
            ));
        }
        lines.push(Line::from(title_line));

        // Gap B: tags chips
        if !op.tags.is_empty() {
            let mut tag_spans: Vec<Span> = vec![Span::raw("  ")];
            for tag in &op.tags {
                tag_spans.push(Span::styled(
                    format!("[{}]", tag),
                    Style::default().fg(Color::Cyan),
                ));
                tag_spans.push(Span::raw("  "));
            }
            lines.push(Line::from(tag_spans));
        }
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
        if let Some(ref desc) = op.description
            && op.summary.as_deref() != Some(desc.as_str())
        {
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
                    "    name                 type       tags   description",
                    Style::default().fg(Color::DarkGray),
                )]));

                for p in &group {
                    let name_style = if p.deprecated {
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::CROSSED_OUT)
                    } else if p.required {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    let name_padded = format!("{:<22}", p.name.clone());
                    // Bug E: use actual schema type instead of hardcoded "string"
                    let type_str = p.schema_type.as_deref().unwrap_or("string");
                    let mut param_row = vec![
                        Span::raw("    "),
                        Span::styled(name_padded, name_style),
                        Span::styled(
                            format!("{:<10} ", type_str),
                            Style::default().fg(Color::Blue),
                        ),
                    ];
                    if p.required {
                        param_row.push(Span::styled(
                            "[required]",
                            Style::default().fg(Color::Red),
                        ));
                        param_row.push(Span::raw(" "));
                    }
                    if p.deprecated {
                        param_row.push(Span::styled(
                            "[deprecated]",
                            Style::default().fg(Color::LightRed),
                        ));
                        param_row.push(Span::raw(" "));
                    }
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
            if let Some(node) = schema_node_opt {
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
            // Gap M: show (required) / (optional) in REQUEST BODY header
            let req_label = if rb.required {
                Span::styled(
                    " (required)",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(" (optional)", Style::default().fg(Color::DarkGray))
            };
            lines.push(Line::from(vec![
                Span::styled(
                    "  REQUEST BODY",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
                req_label,
            ]));
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

        // ── Responses ────────────────────────────────────────────────────────
        // Build per-response schema tree children (only for responses that have
        // a schema_tree).  We assign a 1-based hotkey to each such response in
        // the order they appear.
        struct RespTreeSlot {
            /// Index into `op.responses` this slot belongs to.
            resp_idx: usize,
            /// 1-based hotkey digit shown in the hint (1, 2, 3, …).
            hotkey: usize,
            owned_children: Vec<SchemaNode>,
            id_start: usize,
        }

        let mut resp_tree_slots: Vec<RespTreeSlot> = Vec::new();
        {
            let mut hotkey = 1usize;
            for (resp_idx, resp) in op.responses.iter().enumerate() {
                if let Some(node) = resp.schema_tree.as_ref() {
                    let (_, ch, id_start) = schema_effective_roots(node);
                    resp_tree_slots.push(RespTreeSlot {
                        resp_idx,
                        hotkey,
                        owned_children: ch.to_vec(),
                        id_start,
                    });
                    hotkey += 1;
                }
            }
        }

        // Ensure we have enough TreeState slots (grow on demand, never shrink
        // within an operation — sync_schema_tree resets on op change).
        while self.resp_tree_states.len() < resp_tree_slots.len() {
            self.resp_tree_states.push(TreeState::default());
        }

        // Build the responses text lines.  For responses that have a schema tree
        // we embed a hotkey hint on the response row instead of static lines.
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

            // Assign hotkeys by matching slots
            let hotkey_for_resp: std::collections::HashMap<usize, usize> = resp_tree_slots
                .iter()
                .map(|s| (s.resp_idx, s.hotkey))
                .collect();

            for (resp_idx, resp) in op.responses.iter().enumerate() {
                let badge = Span::styled(
                    format!(" {} ", resp.code),
                    response_code_style(&resp.code).add_modifier(Modifier::BOLD),
                );
                let desc_span = if let Some(ref d) = resp.description {
                    Span::styled(format!("  {}", d), Style::default().fg(Color::Gray))
                } else {
                    Span::raw("")
                };
                let mut row = vec![Span::raw("    "), badge, desc_span];

                // If this response has a schema tree, show the hotkey hint.
                if let Some(&hk) = hotkey_for_resp.get(&resp_idx) {
                    row.push(Span::raw("  "));
                    row.push(Span::styled(
                        format!("[{}]", hk),
                        Style::default().fg(Color::Yellow),
                    ));
                    row.push(Span::styled(
                        " schema",
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                target.push(Line::from(row));
            }
            target.push(Line::raw(""));
        }

        // ── Virtual scroll layout ─────────────────────────────────────────────
        // Layout (virtual line order):
        //   lines_above  (text lines before req-body tree)
        //   [req_body_tree if has_schema]
        //   lines_below  (responses text block, possibly at beginning of lines)
        //   [resp_tree_0]
        //   [resp_tree_1]
        //   ...
        //   [resp_tree_N]
        //
        // Note: when !has_schema, responses go into `lines` (not lines_below),
        // so `lines_below` is empty and resp trees appear after `lines`.

        let req_tree_len: usize = if !owned_effective_children.is_empty() {
            let opened = self.schema_tree_state.opened().clone();
            count_visible_tree_rows(&owned_effective_children, tree_id_start, &opened)
        } else {
            0
        };

        let lines_above_len = lines.len();
        let req_tree_start = lines_above_len;
        let req_tree_end = req_tree_start + req_tree_len;

        self.tree_start = req_tree_start;
        self.tree_len = req_tree_len;

        let below_start = req_tree_end;
        let below_end = below_start + lines_below.len();

        // Compute each response tree's row count and virtual start.
        // Only the focused slot occupies space; all others are hidden (len = 0).
        let mut resp_tree_lens_new: Vec<usize> = Vec::new();
        let mut resp_tree_starts_new: Vec<usize> = Vec::new();
        let mut cursor = below_end;
        for (slot_idx, slot) in resp_tree_slots.iter().enumerate() {
            let is_focused = self.focused_resp_tree == Some(slot_idx);
            let len = if !is_focused || slot.owned_children.is_empty() {
                0
            } else {
                let opened = if let Some(s) = self.resp_tree_states.get(slot_idx) {
                    s.opened().clone()
                } else {
                    Default::default()
                };
                count_visible_tree_rows(&slot.owned_children, slot.id_start, &opened)
            };
            resp_tree_starts_new.push(cursor);
            resp_tree_lens_new.push(len);
            cursor += len;
        }
        let total_virtual = cursor;

        // If a resp tree is focused, reserve blank padding lines after it.
        const FOOTER_PAD: usize = 3;
        let resp_tree_pad_start = total_virtual;
        let total_virtual = if self.focused_resp_tree.is_some() && resp_tree_lens_new.iter().any(|&l| l > 0) {
            total_virtual + FOOTER_PAD
        } else {
            total_virtual
        };

        // Write back cached positions (needed by focus_resp_tree scroll jump).
        self.resp_tree_starts = resp_tree_starts_new.clone();
        self.resp_tree_lens = resp_tree_lens_new.clone();

        if !self.search.query.is_empty() {
            let q = self.search.query.to_ascii_lowercase();
            let mut matches: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, l)| line_plain_text(l).to_ascii_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect();
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

        let in_tree_indicator = if self.in_tree {
            "●"
        } else if self.focused_resp_tree.is_some() {
            "◆"
        } else {
            "○"
        };
        let search_indicator = if !self.search.query.is_empty() {
            let cursor_sym = if self.search.active { "_" } else { "" };
            let n = self.search_matches.len();
            let cur = if n > 0 { self.search_cursor + 1 } else { 0 };
            format!("  /{}{} [{}/{}]", self.search.query, cursor_sym, cur, n)
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

        // ── Fast path: no tree widgets at all ─────────────────────────────────
        let any_req_tree = req_tree_len > 0 && has_schema;
        let any_resp_tree = resp_tree_lens_new.iter().any(|&l| l > 0);

        if !any_req_tree && !any_resp_tree {
            let matches = self.search_matches.clone();
            let mut combined: Vec<Line> = lines.into_iter().chain(lines_below).collect();
            highlight_matched_lines(&mut combined, 0, &matches);
            let para = Paragraph::new(combined)
                .wrap(Wrap { trim: false })
                .scroll((scroll as u16, 0));
            frame.render_widget(para, inner_area);
            return;
        }

        // ── Build the ordered list of layout segments ─────────────────────────
        // Each segment is: (virtual_start, virtual_end, SegmentKind)
        enum Seg {
            TextAbove,
            ReqTree,
            TextBelow,
            RespTree(usize), // slot index
            Pad,             // blank trailing lines after focused resp tree
        }

        let mut segments: Vec<(usize, usize, Seg)> = Vec::new();
        if lines_above_len > 0 {
            segments.push((0, lines_above_len, Seg::TextAbove));
        }
        if any_req_tree {
            segments.push((req_tree_start, req_tree_end, Seg::ReqTree));
        }
        if !lines_below.is_empty() {
            segments.push((below_start, below_end, Seg::TextBelow));
        }
        for (i, (&start, &len)) in resp_tree_starts_new
            .iter()
            .zip(resp_tree_lens_new.iter())
            .enumerate()
        {
            if len > 0 {
                segments.push((start, start + len, Seg::RespTree(i)));
            }
        }
        // Blank padding after the focused resp tree (so there is space between
        // the last tree row and the bottom border).
        if resp_tree_pad_start < total_virtual {
            segments.push((resp_tree_pad_start, total_virtual, Seg::Pad));
        }

        // Passive scroll sync for req-body tree (unchanged logic).
        if !self.in_tree && any_req_tree {
            let desired_offset = scroll.saturating_sub(req_tree_start);
            let current_offset = self.schema_tree_state.get_offset();
            if desired_offset > current_offset {
                self.schema_tree_state
                    .scroll_down(desired_offset - current_offset);
            } else if desired_offset < current_offset {
                self.schema_tree_state
                    .scroll_up(current_offset - desired_offset);
            }
        }

        // ── Build ratatui constraints ─────────────────────────────────────────
        let mut constraints: Vec<Constraint> = Vec::new();
        for (vstart, vend, _) in &segments {
            let seg_len = *vend - *vstart;
            let visible_start = (*vstart).max(scroll);
            let visible_end = (*vend).min(view_end);
            let rows = visible_end.saturating_sub(visible_start).min(seg_len);
            if rows > 0 {
                constraints.push(Constraint::Length(rows as u16));
            }
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

        for (vstart, vend, seg) in segments {
            let visible_start = vstart.max(scroll);
            let visible_end = vend.min(view_end);
            let rows = visible_end.saturating_sub(visible_start);
            if rows == 0 {
                continue;
            }
            let rect = rects[rect_idx];
            rect_idx += 1;

            match seg {
                Seg::TextAbove => {
                    highlight_matched_lines(&mut lines, 0, &matches);
                    let para = Paragraph::new(lines.clone())
                        .wrap(Wrap { trim: false })
                        .scroll((scroll as u16, 0));
                    frame.render_widget(para, rect);
                }
                Seg::TextBelow => {
                    let below_scroll = scroll.saturating_sub(below_start);
                    highlight_matched_lines(&mut lines_below, below_start, &matches);
                    let para = Paragraph::new(lines_below.clone())
                        .wrap(Wrap { trim: false })
                        .scroll((below_scroll as u16, 0));
                    frame.render_widget(para, rect);
                }
                Seg::ReqTree => {
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
                                rect,
                                &mut self.schema_tree_state,
                            );
                        }
                        Err(_) => {
                            frame.render_widget(
                                Paragraph::new(Span::styled(
                                    "(schema display error)",
                                    Style::default().fg(Color::DarkGray),
                                )),
                                rect,
                            );
                        }
                    }
                }
                Seg::RespTree(slot_idx) => {
                    let slot = &resp_tree_slots[slot_idx];
                    let tree_items =
                        schema_children_to_tree_items(&slot.owned_children, slot.id_start);
                    let is_focused = self.focused_resp_tree == Some(slot_idx);
                    let tree_block = Block::default()
                        .borders(Borders::LEFT)
                        .border_style(if is_focused {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        });
                    if let Some(state) = self.resp_tree_states.get_mut(slot_idx) {
                        match Tree::new(&tree_items) {
                            Ok(tree_widget) => {
                                let tree_widget = tree_widget
                                    .block(tree_block)
                                    .highlight_style(if is_focused {
                                        Style::default()
                                            .bg(Color::Green)
                                            .fg(Color::Black)
                                            .add_modifier(Modifier::BOLD)
                                    } else {
                                        Style::default().bg(Color::DarkGray).fg(Color::White)
                                    })
                                    .highlight_symbol("  ");
                                frame.render_stateful_widget(tree_widget, rect, state);
                            }
                            Err(_) => {
                                frame.render_widget(
                                    Paragraph::new(Span::styled(
                                        "(schema display error)",
                                        Style::default().fg(Color::DarkGray),
                                    )),
                                    rect,
                                );
                            }
                        }
                    }
                }
                Seg::Pad => {
                    frame.render_widget(Paragraph::new(""), rect);
                }
            }
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

    if node.kind == SchemaKindHint::Array
        && let Some(items) = node.children.first()
    {
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

    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(
            node.label.clone(),
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
            Style::default().fg(Color::Blue),
        ),
    ];

    if node.required {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[required]",
            Style::default().fg(Color::Red),
        ));
    }

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
        _ => Style::default().fg(Color::White).bg(Color::DarkGray),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Search::matches ───────────────────────────────────────────────────────

    #[test]
    fn search_matches_empty_query_always_true() {
        let s = Search::default();
        assert!(s.matches("anything"));
        assert!(s.matches(""));
    }

    #[test]
    fn search_matches_case_insensitive() {
        let s = Search { query: "GET".into(), active: false };
        assert!(s.matches("get"));
        assert!(s.matches("GET"));
        assert!(s.matches("getUser"));
        assert!(!s.matches("post"));
    }

    #[test]
    fn search_matches_substring() {
        let s = Search { query: "user".into(), active: false };
        assert!(s.matches("listUsers"));
        assert!(s.matches("USER"));
        assert!(!s.matches("account"));
    }

    #[test]
    fn search_is_empty_reflects_query() {
        let mut s = Search::default();
        assert!(s.is_empty());
        s.query.push('x');
        assert!(!s.is_empty());
    }

    // ── DetailView scroll helpers ─────────────────────────────────────────────

    #[test]
    fn detail_scroll_down_and_up() {
        let mut d = DetailView::default();
        d.scroll_down(5);
        assert_eq!(d.scroll, 5);
        d.scroll_up(3);
        assert_eq!(d.scroll, 2);
    }

    #[test]
    fn detail_scroll_up_does_not_underflow() {
        let mut d = DetailView::default();
        d.scroll_up(10); // scroll is 0, should stay 0
        assert_eq!(d.scroll, 0);
    }

    #[test]
    fn detail_scroll_top_resets_to_zero() {
        let mut d = DetailView::default();
        d.scroll = 42;
        d.scroll_top();
        assert_eq!(d.scroll, 0);
    }

    #[test]
    fn detail_scroll_bottom_sets_large_value() {
        let mut d = DetailView::default();
        d.scroll_bottom();
        // Just check it's a large value (won't overflow).
        assert!(d.scroll > 0);
    }

    #[test]
    fn detail_back_clears_in_tree_and_scroll() {
        let mut d = DetailView::default();
        d.in_tree = true;
        d.scroll = 99;
        d.focused_resp_tree = Some(0);
        d.back();
        assert_eq!(d.scroll, 0);
        assert!(!d.in_tree);
        assert!(d.focused_resp_tree.is_none());
    }

    // ── DetailView search helpers ─────────────────────────────────────────────

    #[test]
    fn detail_search_cancel_clears_everything() {
        let mut d = DetailView::default();
        d.search.active = true;
        d.search.query = "hello".into();
        d.search_matches = vec![1, 2, 3];
        d.search_cancel();
        assert!(!d.search.active);
        assert!(d.search.query.is_empty());
        assert!(d.search_matches.is_empty());
    }

    #[test]
    fn detail_search_next_cycles_through_matches() {
        let mut d = DetailView::default();
        d.search_matches = vec![0, 5, 10];
        d.search_cursor = 0;
        d.scroll = 0;

        d.search_next();
        assert_eq!(d.search_cursor, 1);
        assert_eq!(d.scroll, 5);

        d.search_next();
        assert_eq!(d.search_cursor, 2);
        assert_eq!(d.scroll, 10);

        // Wraps around.
        d.search_next();
        assert_eq!(d.search_cursor, 0);
        assert_eq!(d.scroll, 0);
    }

    #[test]
    fn detail_search_prev_cycles_backwards() {
        let mut d = DetailView::default();
        d.search_matches = vec![0, 5, 10];
        d.search_cursor = 0;
        d.scroll = 0;

        // Going prev from 0 wraps to last.
        d.search_prev();
        assert_eq!(d.search_cursor, 2);
        assert_eq!(d.scroll, 10);

        d.search_prev();
        assert_eq!(d.search_cursor, 1);
        assert_eq!(d.scroll, 5);
    }

    #[test]
    fn detail_search_next_noop_when_no_matches() {
        let mut d = DetailView::default();
        d.scroll = 7;
        d.search_next(); // no matches — scroll unchanged
        assert_eq!(d.scroll, 7);
    }

    // ── DetailView sync_schema_tree ───────────────────────────────────────────

    #[test]
    fn sync_schema_tree_resets_on_key_change() {
        let mut d = DetailView::default();
        d.in_tree = true;
        d.tree_len = 5;
        d.scroll = 20;
        d.sync_schema_tree(Some((0, 0, 0)));
        // Key is now set — call again with same key: should NOT reset.
        d.in_tree = true;
        d.sync_schema_tree(Some((0, 0, 0)));
        assert!(d.in_tree, "should not reset when key unchanged");

        // Call with a different key: should reset.
        d.sync_schema_tree(Some((0, 0, 1)));
        assert!(!d.in_tree, "should reset when key changes");
        assert_eq!(d.tree_len, 0);
        assert!(d.focused_resp_tree.is_none());
    }

    #[test]
    fn sync_schema_tree_resets_on_none_to_some() {
        let mut d = DetailView::default();
        // Initial key is None; setting to Some should trigger reset.
        d.in_tree = true;
        d.sync_schema_tree(Some((1, 2, 3)));
        assert!(!d.in_tree);
    }
}
