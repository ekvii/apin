use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::spec::Spec;

use super::super::app::{Focus, OpsState, TreeCursor};
use super::path_tree::PathNode;
use super::styles::{border_style, highlight_style, method_color};

/// Per-column data collected before rendering a tree column widget.
struct ColData {
    items: Vec<(String, bool)>,
    selected_in_list: usize,
    title: String,
    search_active: bool,
    search_query: String,
}

const MIN_COL_WIDTH: u16 = 14;

pub(in crate::tui) fn draw(
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
    let window_start = (logical_active + 1).saturating_sub(visible);
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
                ops,
                col_rects[render_idx],
                path_idx,
            );
            continue;
        }

        let is_active = is_tree_focused && col == tree_active_col;

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
                let display = if label == "." {
                    "(self)"
                } else {
                    label.as_str()
                };
                // "__paths__" is a synthetic group node — render as "[PATHS]"
                if label == "__paths__" {
                    return ListItem::new(Line::from(vec![
                        Span::styled(
                            "[PATHS]",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ›", Style::default().fg(Color::DarkGray)),
                    ]));
                }
                // "__webhooks__" is a synthetic group node — render as "[WEBHOOKS]"
                if label == "__webhooks__" {
                    return ListItem::new(Line::from(vec![
                        Span::styled(
                            "[WEBHOOKS]",
                            Style::default()
                                .fg(Color::Magenta)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ›", Style::default().fg(Color::DarkGray)),
                    ]));
                }
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
    ops: &mut OpsState,
    area: Rect,
    path_idx: Option<usize>,
) {
    let spec_idx = specs_state.selected().unwrap_or(0);
    let is_ops_focused = *focus == Focus::Ops;

    // Build filtered list: (original_op_index, method, deprecated).
    let filtered: Vec<(usize, &str, bool)> = {
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
            .map(|(i, op)| (i, op.method.as_str(), op.deprecated))
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
        .map(|(_, method, deprecated)| {
            let base = method_color(method).add_modifier(Modifier::BOLD);
            let style = if *deprecated {
                // For known methods (colored bg), dim the text to DarkGray.
                // For unknown methods (DarkGray bg), keep fg as-is (White) —
                // overriding to DarkGray would make the text invisible.
                let known = matches!(*method, "GET" | "POST" | "PUT" | "PATCH" | "DELETE");
                if known {
                    base.add_modifier(Modifier::CROSSED_OUT).fg(Color::DarkGray)
                } else {
                    base.add_modifier(Modifier::CROSSED_OUT)
                }
            } else {
                base
            };
            let mut spans = vec![Span::styled(format!(" {} ", method), style)];
            if *deprecated {
                spans.push(Span::styled(
                    " [deprecated]",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
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
