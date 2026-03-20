use ratatui::style::{Color, Modifier, Style};

pub(crate) fn border_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

pub(crate) fn highlight_style(active: bool) -> Style {
    if active {
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    }
}

pub(crate) fn method_color(method: &str) -> Style {
    match method {
        "GET" => Style::default().fg(Color::Black).bg(Color::Green),
        "POST" => Style::default().fg(Color::Black).bg(Color::Blue),
        "PUT" => Style::default().fg(Color::Black).bg(Color::Yellow),
        "PATCH" => Style::default().fg(Color::Black).bg(Color::Cyan),
        "DELETE" => Style::default().fg(Color::Black).bg(Color::Red),
        _ => Style::default().fg(Color::White).bg(Color::DarkGray),
    }
}

pub(crate) fn response_code_style(code: &str) -> Style {
    let base = Style::default().add_modifier(Modifier::BOLD);
    match code.chars().next() {
        Some('2') => base.fg(Color::Black).bg(Color::Green),
        Some('3') => base.fg(Color::Black).bg(Color::Cyan),
        Some('4') => base.fg(Color::Black).bg(Color::Yellow),
        Some('5') => base.fg(Color::Black).bg(Color::Red),
        _ => base.fg(Color::White).bg(Color::DarkGray),
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
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
    use ratatui::style::{Color, Modifier};

    // ── truncate ─────────────────────────────────────────────────────────────

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length_unchanged() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string_gets_ellipsis() {
        let result = truncate("hello world", 8);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 8);
    }

    #[test]
    fn truncate_max_one_returns_ellipsis_only() {
        let result = truncate("abc", 1);
        assert_eq!(result, "…");
    }

    #[test]
    fn truncate_max_zero_returns_ellipsis() {
        // saturating_sub(1) on 0 = 0 chars taken, then push '…'
        let result = truncate("abc", 0);
        assert_eq!(result, "…");
    }

    #[test]
    fn truncate_unicode_counts_chars_not_bytes() {
        // "日本語" = 3 chars; max=3 should keep it as-is.
        assert_eq!(truncate("日本語", 3), "日本語");
        // max=2 → 1 char taken + ellipsis
        let r = truncate("日本語", 2);
        assert_eq!(r.chars().count(), 2);
        assert!(r.ends_with('…'));
    }

    // ── method_color ─────────────────────────────────────────────────────────

    #[test]
    fn method_color_get_is_green_bg() {
        let s = method_color("GET");
        assert_eq!(s.bg, Some(Color::Green));
        assert_eq!(s.fg, Some(Color::Black));
    }

    #[test]
    fn method_color_post_is_blue_bg() {
        let s = method_color("POST");
        assert_eq!(s.bg, Some(Color::Blue));
    }

    #[test]
    fn method_color_put_is_yellow_bg() {
        let s = method_color("PUT");
        assert_eq!(s.bg, Some(Color::Yellow));
    }

    #[test]
    fn method_color_patch_is_cyan_bg() {
        let s = method_color("PATCH");
        assert_eq!(s.bg, Some(Color::Cyan));
    }

    #[test]
    fn method_color_delete_is_red_bg() {
        let s = method_color("DELETE");
        assert_eq!(s.bg, Some(Color::Red));
    }

    #[test]
    fn method_color_unknown_is_dark_gray_bg() {
        let s = method_color("OPTIONS");
        assert_eq!(s.bg, Some(Color::DarkGray));
        assert_eq!(s.fg, Some(Color::White));
    }

    // ── response_code_style ───────────────────────────────────────────────────

    #[test]
    fn response_code_2xx_is_green() {
        let s = response_code_style("200");
        assert_eq!(s.bg, Some(Color::Green));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn response_code_3xx_is_cyan() {
        let s = response_code_style("301");
        assert_eq!(s.bg, Some(Color::Cyan));
    }

    #[test]
    fn response_code_4xx_is_yellow() {
        let s = response_code_style("404");
        assert_eq!(s.bg, Some(Color::Yellow));
    }

    #[test]
    fn response_code_5xx_is_red() {
        let s = response_code_style("500");
        assert_eq!(s.bg, Some(Color::Red));
    }

    #[test]
    fn response_code_unknown_is_dark_gray() {
        let s = response_code_style("default");
        assert_eq!(s.bg, Some(Color::DarkGray));
    }

    // ── border_style ─────────────────────────────────────────────────────────

    #[test]
    fn border_style_active_is_cyan() {
        let s = border_style(true);
        assert_eq!(s.fg, Some(Color::Cyan));
    }

    #[test]
    fn border_style_inactive_is_dark_gray() {
        let s = border_style(false);
        assert_eq!(s.fg, Some(Color::DarkGray));
    }

    // ── highlight_style ───────────────────────────────────────────────────────

    #[test]
    fn highlight_style_active_cyan_bg_bold() {
        let s = highlight_style(true);
        assert_eq!(s.bg, Some(Color::Cyan));
        assert_eq!(s.fg, Some(Color::Black));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn highlight_style_inactive_dark_gray_bg() {
        let s = highlight_style(false);
        assert_eq!(s.bg, Some(Color::DarkGray));
        assert_eq!(s.fg, Some(Color::White));
    }
}
