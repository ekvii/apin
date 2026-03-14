use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, ListState, Paragraph, Wrap},
    Frame,
};

use crate::universe::Spec;

use super::super::app::{Focus, OpsState, TreeCursor};
use super::styles::{border_style, method_color, response_code_style, truncate};

pub(crate) fn draw(
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
    if op.summary.is_some()
        && let Some(ref oid) = op.operation_id
    {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("id      ", Style::default().fg(Color::DarkGray)),
                Span::styled(oid.as_str(), Style::default().fg(Color::Gray)),
            ]));
    }

    // ── Description ───────────────────────────────────────────────────────────
    if let Some(ref desc) = op.description
        && op.summary.as_deref() != Some(desc.as_str())
    {
            lines.push(Line::raw(""));
            for desc_line in desc.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {}", desc_line),
                    Style::default().fg(Color::DarkGray),
                )));
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
