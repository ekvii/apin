use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, ListState, Paragraph, Wrap},
};

use crate::spec::{Operation, Param, PathKind, RequestBody, Response, Spec};

use super::super::app::{Focus, OpsState, TreeCursor};
use super::styles::{border_style, method_color, response_code_style, truncate};

// ── Key column ────────────────────────────────────────────────────────────────

const KEY_W: usize = 8;

fn key(label: &str) -> Span<'static> {
    Span::styled(
        format!("  {:<KEY_W$}", label),
        Style::default().fg(Color::DarkGray),
    )
}

// ── Per-block line builders ───────────────────────────────────────────────────

/// `[METHOD]  [deprecated]  summary / operation_id`
fn header_line(op: &Operation) -> Line<'static> {
    let badge_style = method_color(&op.method).add_modifier(Modifier::BOLD);
    let mut spans = vec![
        Span::styled(format!(" {} ", op.method), badge_style),
        Span::raw("  "),
    ];
    if op.deprecated {
        spans.push(Span::styled(
            "[deprecated] ",
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(ref sum) = op.summary {
        spans.push(Span::styled(
            sum.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    } else if let Some(ref oid) = op.operation_id {
        spans.push(Span::styled(
            oid.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

/// `  desc    <first line of description, truncated>`
/// Returns `None` when there is nothing worth showing.
fn desc_line(op: &Operation, area_width: u16) -> Option<Line<'static>> {
    let desc = op.description.as_deref()?;
    // Skip when description duplicates the summary.
    if op.summary.as_deref() == Some(desc) {
        return None;
    }
    let first = desc.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return None;
    }
    let max_w = (area_width as usize).saturating_sub(2 + KEY_W + 4);
    Some(Line::from(vec![
        key("desc"),
        Span::styled(truncate(first, max_w), Style::default().fg(Color::Gray)),
    ]))
}

/// `  <loc>   name*  other  ~~deprecated~~`
/// One line per non-empty location group among query / header / cookie.
fn param_lines(params: &[Param]) -> Vec<Line<'static>> {
    const LOCATIONS: &[&str] = &["query", "header", "cookie"];

    LOCATIONS
        .iter()
        .filter_map(|&loc| {
            let group: Vec<&Param> = params.iter().filter(|p| p.location == loc).collect();
            if group.is_empty() {
                return None;
            }
            let mut spans = vec![key(loc)];
            for (i, p) in group.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("  "));
                }
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
                spans.push(Span::styled(p.name.clone(), name_style));
                if p.required && !p.deprecated {
                    spans.push(Span::styled("*", Style::default().fg(Color::Red)));
                }
            }
            Some(Line::from(spans))
        })
        .collect()
}

/// `  body    [required object]  field  field  +N`
/// Returns `None` when there is no request body.
fn body_line(rb: &RequestBody) -> Option<Line<'static>> {
    let mut spans = vec![key("body")];

    if let Some(ref tree) = rb.schema_tree {
        let req_label = if rb.required { "required" } else { "optional" };
        let badge = format!(" {} {} ", req_label, tree.kind.label());
        spans.push(Span::styled(
            badge,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    }

    if !rb.fields.is_empty() {
        let take = 10.min(rb.fields.len());
        for (i, f) in rb.fields.iter().take(take).enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(
                f.name.clone(),
                Style::default().fg(Color::Gray),
            ));
        }
        if rb.fields.len() > take {
            spans.push(Span::styled(
                format!("  +{}", rb.fields.len() - take),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    Some(Line::from(spans))
}

/// `  resp    [200] OK  [404] Not Found  [500] …`
/// Returns `None` when there are no responses.
fn resp_line(responses: &[Response]) -> Option<Line<'static>> {
    if responses.is_empty() {
        return None;
    }
    let mut spans = vec![key("resp")];
    for (i, resp) in responses.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!(" {} ", resp.code),
            response_code_style(&resp.code).add_modifier(Modifier::BOLD),
        ));
        if let Some(ref d) = resp.description {
            spans.push(Span::styled(
                format!(" {}", truncate(d, 20)),
                Style::default().fg(Color::Gray),
            ));
        }
        if responses.len() > 1 && i < responses.len() - 1 {
            spans.push(Span::styled("  ", Style::default().fg(Color::DarkGray)));
        }
    }
    Some(Line::from(spans))
}

/// `Enter for full detail & schemas` — shown when there is more to see.
fn hint_line(op: &Operation) -> Option<Line<'static>> {
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

    if !has_schema && !has_long_desc {
        return None;
    }
    Some(Line::from(vec![
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
    ]))
}

// ── Public draw entry point ───────────────────────────────────────────────────

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

    // Title: path string + optional [WH] webhook badge.
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

    // Resolve the selected operation index through the filtered ops list.
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

    // Assemble lines from individual block builders.
    let mut lines: Vec<Line> = Vec::new();

    lines.push(header_line(op));
    lines.extend(desc_line(op, area.width));
    lines.extend(param_lines(&op.params));
    lines.extend(op.request_body.as_ref().and_then(body_line));
    lines.extend(resp_line(&op.responses));
    if let Some(hint) = hint_line(op) {
        lines.push(Line::raw(""));
        lines.push(hint);
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
