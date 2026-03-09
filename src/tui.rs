use std::io::Stdout;

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::universe::{PathEntry, Spec, Universe};

// ─── Terminal lifecycle ───────────────────────────────────────────────────────

/// Set up the terminal, run the TUI event loop, then restore the terminal.
/// Any error from the loop is surfaced only after the terminal is cleaned up,
/// so the message is always readable in the normal shell.
pub fn launch(universe: Universe) -> Result<()> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen).context("failed to enter alternate screen")?;

    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;

    let result = run(&mut terminal, universe);

    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;
    terminal.show_cursor().context("failed to restore cursor")?;

    result
}

// ─── Application state ───────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Pane {
    Specs,
    Paths,
}

struct App {
    universe: Universe,
    /// Which pane currently has focus.
    active_pane: Pane,
    /// Selection state for the left (specs) list.
    specs_state: ListState,
    /// Selection state for the right (paths) list.
    paths_state: ListState,
}

impl App {
    fn new(universe: Universe) -> Self {
        let mut specs_state = ListState::default();
        if !universe.specs.is_empty() {
            specs_state.select(Some(0));
        }
        let mut paths_state = ListState::default();
        if universe
            .specs
            .first()
            .map(|s| !s.paths.is_empty())
            .unwrap_or(false)
        {
            paths_state.select(Some(0));
        }
        let initial_pane = if universe.specs.len() == 1 {
            Pane::Paths
        } else {
            Pane::Specs
        };
        Self {
            universe,
            active_pane: initial_pane,
            specs_state,
            paths_state,
        }
    }

    fn selected_spec_index(&self) -> Option<usize> {
        self.specs_state.selected()
    }

    fn selected_spec(&self) -> Option<&Spec> {
        self.selected_spec_index()
            .and_then(|i| self.universe.specs.get(i))
    }

    fn current_paths(&self) -> &[PathEntry] {
        self.selected_spec()
            .map(|s| s.paths.as_slice())
            .unwrap_or(&[])
    }

    fn selected_path_entry(&self) -> Option<&PathEntry> {
        let idx = self.paths_state.selected()?;
        self.selected_spec()?.paths.get(idx)
    }

    // ── Navigation helpers ────────────────────────────────────────────────────

    fn move_down(&mut self) {
        match self.active_pane {
            Pane::Specs => {
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
                // Reset paths cursor when the selected spec changes.
                let has_paths = self
                    .universe
                    .specs
                    .get(next)
                    .map(|s| !s.paths.is_empty())
                    .unwrap_or(false);
                self.paths_state
                    .select(if has_paths { Some(0) } else { None });
            }
            Pane::Paths => {
                let len = self.current_paths().len();
                if len == 0 {
                    return;
                }
                let next = self
                    .paths_state
                    .selected()
                    .map(|i| (i + 1).min(len - 1))
                    .unwrap_or(0);
                self.paths_state.select(Some(next));
            }
        }
    }

    fn move_up(&mut self) {
        match self.active_pane {
            Pane::Specs => {
                let next = self
                    .specs_state
                    .selected()
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                self.specs_state.select(Some(next));
                let has_paths = self
                    .universe
                    .specs
                    .get(next)
                    .map(|s| !s.paths.is_empty())
                    .unwrap_or(false);
                self.paths_state
                    .select(if has_paths { Some(0) } else { None });
            }
            Pane::Paths => {
                let next = self
                    .paths_state
                    .selected()
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                self.paths_state.select(Some(next));
            }
        }
    }

    fn move_top(&mut self) {
        match self.active_pane {
            Pane::Specs => self.specs_state.select(Some(0)),
            Pane::Paths => self.paths_state.select(Some(0)),
        }
    }

    fn move_bottom(&mut self) {
        match self.active_pane {
            Pane::Specs => {
                let last = self.universe.specs.len().saturating_sub(1);
                self.specs_state.select(Some(last));
            }
            Pane::Paths => {
                let last = self.current_paths().len().saturating_sub(1);
                self.paths_state.select(Some(last));
            }
        }
    }
}

// ─── Event loop ──────────────────────────────────────────────────────────────

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, universe: Universe) -> Result<()> {
    let mut app = App::new(universe);
    let mut pending_g = false;

    loop {
        terminal.draw(|frame| draw(frame, &mut app))?;

        if let Event::Key(key) = event::read().context("failed to read event")? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if pending_g {
                pending_g = false;
                if key.code == KeyCode::Char('g') {
                    app.move_top();
                }
                // Any other key after 'g' is discarded (no partial-command action).
                continue;
            }

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('j') | KeyCode::Down => app.move_down(),
                KeyCode::Char('k') | KeyCode::Up => app.move_up(),
                KeyCode::Char('g') => pending_g = true,
                KeyCode::Char('G') => app.move_bottom(),
                KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => {
                    app.active_pane = Pane::Paths;
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    if app.universe.specs.len() > 1 {
                        app.active_pane = Pane::Specs;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

// ─── Drawing ─────────────────────────────────────────────────────────────────

fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Right split: paths list (top, ~40%), path detail (middle, fills), hint bar (1 row)
    let right_area = if app.universe.specs.len() == 1 {
        // Single spec — right column fills the whole terminal.
        area
    } else {
        // Multiple specs — outer split: left (specs) 25% | right 75%
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
            .split(area);
        draw_spec_list(frame, app, panes[0]);
        panes[1]
    };

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(right_area);

    draw_paths(frame, app, right[0]);
    draw_path_detail(frame, app, right[1]);
    draw_hint(frame, right[2]);
}

/// Left: the scrollable list of spec files.
/// Each item is a single line: title (bold)  v1.0  [filename]
fn draw_spec_list(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let active = app.active_pane == Pane::Specs;

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
        .highlight_style(highlight_style(active))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut app.specs_state);
}

/// Right top: the scrollable list of paths for the selected spec.
fn draw_paths(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let active = app.active_pane == Pane::Paths;

    let title = app
        .selected_spec()
        .map(|s| {
            let file_name = s
                .file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");
            format!(" {} / v{} / {} ", s.title, s.version, file_name)
        })
        .unwrap_or_else(|| " Paths ".into());

    // Collect to a local Vec before borrowing app.paths_state mutably.
    let path_strings: Vec<String> = app.current_paths().iter().map(|e| e.path.clone()).collect();

    let path_items: Vec<ListItem> = if path_strings.is_empty() {
        vec![ListItem::new("  (no paths)")]
    } else {
        path_strings
            .iter()
            .map(|p| ListItem::new(p.as_str()))
            .collect()
    };

    let list = List::new(path_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(border_style(active)),
        )
        .highlight_style(highlight_style(active))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut app.paths_state);
}

/// Right middle: detail panel for the currently selected path.
/// Shows each HTTP method as a coloured badge, followed by summary,
/// parameters, request-body indicator, and response codes.
fn draw_path_detail(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let active = app.active_pane == Pane::Paths;

    let content: Vec<Line> = match app.selected_path_entry() {
        None => vec![Line::from(Span::styled(
            "select a path",
            Style::default().fg(Color::DarkGray),
        ))],
        Some(entry) => {
            if entry.operations.is_empty() {
                vec![Line::from(Span::styled(
                    "no operations",
                    Style::default().fg(Color::DarkGray),
                ))]
            } else {
                let mut lines: Vec<Line> = Vec::new();

                for op in &entry.operations {
                    // ── Method badge + summary ──────────────────────────────
                    let badge_style = method_color(&op.method);
                    let mut method_line = vec![
                        Span::styled(
                            format!(" {} ", op.method),
                            badge_style.add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                    ];
                    if let Some(ref sum) = op.summary {
                        method_line.push(Span::styled(
                            sum.as_str(),
                            Style::default().fg(Color::White),
                        ));
                    } else if let Some(ref oid) = op.operation_id {
                        method_line
                            .push(Span::styled(oid.as_str(), Style::default().fg(Color::Gray)));
                    }
                    lines.push(Line::from(method_line));

                    // ── Description (if different from summary) ─────────────
                    if let Some(ref desc) = op.description {
                        if op.summary.as_deref() != Some(desc.as_str()) {
                            lines.push(Line::from(Span::styled(
                                format!("  {}", truncate(desc, 72)),
                                Style::default().fg(Color::DarkGray),
                            )));
                        }
                    }

                    // ── Parameters ─────────────────────────────────────────
                    if !op.params.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled("params  ", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                op.params
                                    .iter()
                                    .map(|p| {
                                        if p.required {
                                            format!("{}* ({})", p.name, p.location)
                                        } else {
                                            format!("{} ({})", p.name, p.location)
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("  "),
                                Style::default().fg(Color::Yellow),
                            ),
                        ]));
                    }

                    // ── Request body ───────────────────────────────────────
                    if op.has_request_body {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled("body    ", Style::default().fg(Color::DarkGray)),
                            Span::styled("request body", Style::default().fg(Color::Magenta)),
                        ]));
                    }

                    // ── Responses ──────────────────────────────────────────
                    if !op.response_codes.is_empty() {
                        let badges: Vec<Span> = op
                            .response_codes
                            .iter()
                            .flat_map(|code| {
                                [
                                    Span::styled(format!(" {} ", code), response_code_style(code)),
                                    Span::raw(" "),
                                ]
                            })
                            .collect();

                        let mut resp_line = vec![
                            Span::raw("  "),
                            Span::styled("resp    ", Style::default().fg(Color::DarkGray)),
                        ];
                        resp_line.extend(badges);
                        lines.push(Line::from(resp_line));
                    }

                    // Blank separator between operations
                    lines.push(Line::raw(""));
                }

                lines
            }
        }
    };

    let detail_title = app
        .selected_path_entry()
        .map(|e| format!(" {} ", e.path))
        .unwrap_or_else(|| " Detail ".into());

    let detail = Paragraph::new(content)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(detail_title)
                .border_style(border_style(active)),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(detail, area);
}

/// Bottom hint bar spanning the right column.
fn draw_hint(frame: &mut Frame, area: ratatui::layout::Rect) {
    let hint =
        Paragraph::new(" j/k: navigate  gg/G: top/bottom  h/l or ←/→: switch pane  q/Esc: quit")
            .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(hint, area);
}

// ─── Style helpers ───────────────────────────────────────────────────────────

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

/// Pick a foreground colour for an HTTP method badge.
fn method_color(method: &str) -> Style {
    match method {
        "GET" => Style::default().fg(Color::Black).bg(Color::Green),
        "POST" => Style::default().fg(Color::Black).bg(Color::Blue),
        "PUT" => Style::default().fg(Color::Black).bg(Color::Yellow),
        "PATCH" => Style::default().fg(Color::Black).bg(Color::Cyan),
        "DELETE" => Style::default().fg(Color::Black).bg(Color::Red),
        "HEAD" | "OPTIONS" | "TRACE" => Style::default().fg(Color::Black).bg(Color::DarkGray),
        _ => Style::default().fg(Color::White),
    }
}

/// Colour for HTTP response code badges (2xx green, 4xx yellow, 5xx red, else gray).
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

/// Truncate a string to at most `max` chars, appending `…` if cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
