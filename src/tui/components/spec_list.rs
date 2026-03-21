use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

use crate::spec::Spec;

use super::super::app::Focus;
use super::styles::{
    accent_text_style, border_style, highlight_style, muted_text_style, primary_text_style,
};

pub(in crate::tui) fn draw(
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
                Span::styled(s.title.as_str(), primary_text_style()),
                Span::raw(" "),
                Span::styled(format!("v{}", s.version), accent_text_style()),
                Span::raw(" "),
                Span::styled(format!("[{}]", file_name), muted_text_style()),
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
