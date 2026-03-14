use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

use crate::universe::Spec;

use super::super::app::Focus;
use super::styles::{border_style, highlight_style};

pub(crate) fn draw(
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
