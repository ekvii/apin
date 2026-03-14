use ratatui::{layout::Rect, style::Style, widgets::Paragraph, Frame};

use super::super::app::{Focus, OpsState};
use super::detail::Search;

pub(crate) fn draw(
    frame: &mut Frame,
    focus: &Focus,
    _ops: &OpsState,
    detail_in_tree: &bool,
    detail_search: &Search,
    area: Rect,
) {
    let text = if detail_search.active && *focus == Focus::Detail {
        " type to search  Enter: jump to first  Esc: cancel  Backspace: delete  Ctrl+U: clear"
    } else {
        match focus {
            Focus::Detail if *detail_in_tree => {
                " j/k: navigate  h/l: collapse/expand  f: unfocus  q: quit"
            }
            Focus::Detail => {
                " j/k: scroll  Ctrl-D/U: half-page  f: focus schema  /: search  n/N: next/prev  gg/G: top/bottom  Esc/h: back  q: quit"
            }
            Focus::Ops => {
                " j/k: navigate  Ctrl-D/U: half-page  gg/G: top/bottom  /: search  Enter: expand  h: back  q/Esc: quit"
            }
            _ => {
                " j/k: navigate  Ctrl-D/U: half-page  gg/G: top/bottom  h/l: columns  /: search  Enter: detail  q/Esc: quit"
            }
        }
    };
    let hint = Paragraph::new(text).style(Style::default().fg(ratatui::style::Color::DarkGray));
    frame.render_widget(hint, area);
}
