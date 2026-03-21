use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, ListState, Paragraph, Wrap},
};

use crate::spec::{PathKind, Spec};

use super::super::app::{Focus, OpsState, TreeCursor};
use super::styles::{border_style, method_color, response_code_style, truncate};

pub(in crate::tui) fn draw(
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
        Some((
            entry.path.clone(),
            entry.kind.clone(),
            entry.operations.len(),
            path_idx,
        ))
    });

    let (path_str, path_kind, op_count, path_idx) = match resolved {
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

    // Build block title: path + optional [WH] tag
    let mut title_spans: Vec<Span> = vec![Span::raw(format!(" {} ", path_str))];
    if path_kind == PathKind::Webhook {
        title_spans.push(Span::styled(
            "[WH] ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }

    if op_count == 0 {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Line::from(title_spans))
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

    // ── Key column width (fixed, for alignment) ───────────────────────────────
    // All labels are padded to this width so values start at the same column.
    const KEY_W: usize = 8;

    fn key(label: &str) -> Span<'static> {
        Span::styled(
            format!("  {:<KEY_W$}", label),
            Style::default().fg(Color::DarkGray),
        )
    }

    let mut lines: Vec<Line> = Vec::new();

    // ── Row 0: method badge + summary / operation_id / fallback ──────────────
    {
        let badge_style = method_color(&op.method).add_modifier(Modifier::BOLD);
        let mut row = vec![
            Span::styled(format!(" {} ", op.method), badge_style),
            Span::raw("  "),
        ];
        if op.deprecated {
            row.push(Span::styled(
                "[deprecated] ",
                Style::default()
                    .fg(Color::LightRed)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(ref sum) = op.summary {
            row.push(Span::styled(
                sum.as_str(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        } else if let Some(ref oid) = op.operation_id {
            row.push(Span::styled(
                oid.as_str(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(row));
    }

    // ── desc ─────────────────────────────────────────────────────────────────
    // Show only the first line, truncated — full text is in detail view.
    if let Some(ref desc) = op.description
        && op.summary.as_deref() != Some(desc.as_str())
    {
        let first_line = desc.lines().next().unwrap_or("").trim();
        if !first_line.is_empty() {
            // Reserve room for the key prefix (2 + KEY_W) and borders (2).
            let max_w = (area.width as usize).saturating_sub(2 + KEY_W + 4);
            lines.push(Line::from(vec![
                key("desc"),
                Span::styled(
                    truncate(first_line, max_w),
                    Style::default().fg(Color::Gray),
                ),
            ]));
        }
    }

    // ── Parameters: one row per location ─────────────────────────────────────
    let locations = vec!["query", "header", "cookie"];

    for loc in locations {
        let group: Vec<_> = op.params.iter().filter(|p| p.location == loc).collect();
        if group.is_empty() {
            continue;
        }
        let mut row = vec![key(loc)];
        for (i, p) in group.iter().enumerate() {
            if i > 0 {
                row.push(Span::raw("  "));
            }
            // Name: white if required, gray otherwise; strikethrough if deprecated
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
            row.push(Span::styled(p.name.clone(), name_style));
            // * suffix for required
            if p.required && !p.deprecated {
                row.push(Span::styled("*", Style::default().fg(Color::Red)));
            }
        }
        lines.push(Line::from(row));
    }

    // ── body ─────────────────────────────────────────────────────────────────
    if let Some(ref rb) = op.request_body {
        let mut row = vec![key("body")];

        // Schema kind + required/optional as a single badge string
        if let Some(ref tree) = rb.schema_tree {
            let req_label = if rb.required { "required" } else { "optional" };
            let badge = format!(" {} {} ", req_label, tree.kind.label());
            row.push(Span::styled(
                badge,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ));
            row.push(Span::raw("  "));
        }

        // Field names (up to 4), then +N more
        if !rb.fields.is_empty() {
            let take = 10.min(rb.fields.len());
            for (i, f) in rb.fields.iter().take(take).enumerate() {
                if i > 0 {
                    row.push(Span::raw("  "));
                }
                row.push(Span::styled(
                    f.name.clone(),
                    Style::default().fg(Color::Gray),
                ));
            }
            if rb.fields.len() > take {
                row.push(Span::styled(
                    format!("  +{}", rb.fields.len() - take),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }

        lines.push(Line::from(row));
    }

    // ── resp ─────────────────────────────────────────────────────────────────
    // All response codes inline on one row, each as a colored badge.
    if !op.responses.is_empty() {
        let mut row = vec![key("resp")];
        for (i, resp) in op.responses.iter().enumerate() {
            if i > 0 {
                row.push(Span::raw(" "));
            }
            row.push(Span::styled(
                format!(" {} ", resp.code),
                response_code_style(&resp.code).add_modifier(Modifier::BOLD),
            ));
            // Short description after the badge (truncated)
            if let Some(ref d) = resp.description {
                row.push(Span::styled(
                    format!(" {}", truncate(d, 20)),
                    Style::default().fg(Color::Gray),
                ));
            }
            // Separate responses visually if there are multiple
            if op.responses.len() > 1 && i < op.responses.len() - 1 {
                row.push(Span::styled("  ", Style::default().fg(Color::DarkGray)));
            }
        }
        lines.push(Line::from(row));
    }

    // ── hint ─────────────────────────────────────────────────────────────────
    // Nudge user toward full-screen detail for schemas / full descriptions.
    let has_schema = op
        .request_body
        .as_ref()
        .and_then(|rb| rb.schema_tree.as_ref())
        .is_some()
        || op.responses.iter().any(|r| r.schema_tree.is_some());
    let has_long_desc = op
        .description
        .as_ref()
        .map(|d| d.lines().count() > 1 || d.len() > 80)
        .unwrap_or(false);

    if has_schema || has_long_desc {
        lines.push(Line::raw("")); // empty spacer
        lines.push(Line::from(vec![
            // Span::raw(format!("  {:KEY_W$}", "")),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " for full detail & schemas",
                Style::default().fg(Color::Gray),
            ),
        ]));
    }

    let detail = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(title_spans))
                .border_style(border_style(is_detail_focused)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(detail, area);
}
