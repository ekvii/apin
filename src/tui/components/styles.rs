use ratatui::style::{Color, Modifier, Style};

// ─── Panel chrome ─────────────────────────────────────────────────────────────

pub(in crate::tui) fn border_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Secondary (response-tree) border: focused=green, unfocused=dark-gray.
pub(in crate::tui) fn secondary_border_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Border that highlights yellow when a search is active.
pub(in crate::tui) fn search_border_style() -> Style {
    Style::default().fg(Color::Yellow)
}

pub(in crate::tui) fn highlight_style(active: bool) -> Style {
    if active {
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    }
}

/// Highlight style used for a secondary tree (e.g. response schema) that needs
/// a visually distinct focused color from the primary tree.
pub(in crate::tui) fn secondary_highlight_style(active: bool) -> Style {
    if active {
        Style::default()
            .bg(Color::Green)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    }
}

/// Search result highlight (applied to matched text lines).
pub(in crate::tui) fn search_match_style() -> Style {
    Style::default().bg(Color::Yellow).fg(Color::Black)
}

// ─── Method & status badges ───────────────────────────────────────────────────

pub(in crate::tui) fn method_color(method: &str) -> Style {
    match method {
        "GET" => Style::default().fg(Color::Black).bg(Color::Green),
        "POST" => Style::default().fg(Color::Black).bg(Color::Blue),
        "PUT" => Style::default().fg(Color::Black).bg(Color::Yellow),
        "PATCH" => Style::default().fg(Color::Black).bg(Color::Cyan),
        "DELETE" => Style::default().fg(Color::Black).bg(Color::Red),
        _ => Style::default().fg(Color::White).bg(Color::DarkGray),
    }
}

pub(in crate::tui) fn response_code_style(code: &str) -> Style {
    let base = Style::default().add_modifier(Modifier::BOLD);
    match code.chars().next() {
        Some('2') => base.fg(Color::Black).bg(Color::Green),
        Some('3') => base.fg(Color::Black).bg(Color::Cyan),
        Some('4') => base.fg(Color::Black).bg(Color::Yellow),
        Some('5') => base.fg(Color::Black).bg(Color::Red),
        _ => base.fg(Color::White).bg(Color::DarkGray),
    }
}

/// Badge for webhook entries: black-on-magenta bold.
pub(in crate::tui) fn webhook_badge_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Magenta)
        .add_modifier(Modifier::BOLD)
}

/// Badge for request body (required / optional): black-on-magenta bold.
pub(in crate::tui) fn body_badge_style() -> Style {
    webhook_badge_style()
}

// ─── Status indicators ────────────────────────────────────────────────────────

/// "[deprecated]" text label or struck-through name.
pub(in crate::tui) fn deprecated_label_style() -> Style {
    Style::default().fg(Color::LightRed)
}

/// Name/badge of a deprecated item — struck-through and dimmed.
pub(in crate::tui) fn deprecated_name_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::CROSSED_OUT)
}

/// "[required]" text label or required asterisk.
pub(in crate::tui) fn required_label_style() -> Style {
    Style::default().fg(Color::Red)
}

/// Name of a required field — bold white.
pub(in crate::tui) fn required_name_style() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

/// "(required)" annotation after a section header — green bold.
pub(in crate::tui) fn required_annotation_style() -> Style {
    Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD)
}

/// "(optional)" annotation after a section header — dimmed.
pub(in crate::tui) fn optional_annotation_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

// ─── Text hierarchy ───────────────────────────────────────────────────────────

/// Primary content text: bold white. Used for titles, summaries, paths.
pub(in crate::tui) fn primary_text_style() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

/// Secondary content text: plain gray. Used for descriptions and body text.
pub(in crate::tui) fn secondary_text_style() -> Style {
    Style::default().fg(Color::Gray)
}

/// Muted / metadata text: dark gray. Used for labels, column headers, dividers,
/// placeholder messages, and hint prose.
pub(in crate::tui) fn muted_text_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Accent text: cyan. Used for operation IDs, tag chips, optional field names,
/// schema items-type values, version strings, and similar highlights.
pub(in crate::tui) fn accent_text_style() -> Style {
    Style::default().fg(Color::Cyan)
}

/// Type label text: blue. Used for schema/parameter type strings.
pub(in crate::tui) fn type_label_style() -> Style {
    Style::default().fg(Color::Blue)
}

/// Schema root-kind value: magenta bold. Used for the top-level type label in
/// schema root headers (e.g. "object", "array").
pub(in crate::tui) fn schema_root_kind_style() -> Style {
    Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD)
}

// ─── Section headers ──────────────────────────────────────────────────────────

/// "PARAMETERS" section header.
pub(in crate::tui) fn params_header_style() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

/// "REQUEST BODY" section header.
pub(in crate::tui) fn request_body_header_style() -> Style {
    Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD)
}

/// "RESPONSES" section header.
pub(in crate::tui) fn responses_header_style() -> Style {
    Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD)
}

// ─── Keybinding hints ─────────────────────────────────────────────────────────

/// The key itself: e.g. "[f]", "[j/k]", "Enter". Yellow.
pub(in crate::tui) fn hint_key_style() -> Style {
    Style::default().fg(Color::Yellow)
}

/// The surrounding prose for a keybinding hint. Same as `muted_text_style`.
pub(in crate::tui) fn hint_text_style() -> Style {
    muted_text_style()
}

// ─── Tree column nodes ────────────────────────────────────────────────────────

/// Regular (leaf) list item in tree columns.
pub(in crate::tui) fn list_item_style() -> Style {
    Style::default().fg(Color::White)
}

/// "[PATHS]" synthetic group node.
pub(in crate::tui) fn paths_group_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// "[WEBHOOKS]" synthetic group node.
pub(in crate::tui) fn webhooks_group_style() -> Style {
    Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD)
}

/// Navigation arrow "›" between tree levels.
pub(in crate::tui) fn nav_arrow_style() -> Style {
    muted_text_style()
}

// ─── Parameter location group badges ─────────────────────────────────────────

/// Returns (badge_bg_color) for the given parameter location string.
pub(in crate::tui) fn param_location_color(location: &str) -> Color {
    match location {
        "path" => Color::Magenta,
        "query" => Color::Cyan,
        "header" => Color::Blue,
        "cookie" => Color::Green,
        _ => Color::DarkGray,
    }
}

/// The text style for the content inside a parameter-location badge chip.
pub(in crate::tui) fn param_location_badge_style(location: &str) -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(param_location_color(location))
        .add_modifier(Modifier::BOLD)
}

// ─── Utilities ────────────────────────────────────────────────────────────────

pub(in crate::tui) fn truncate(s: &str, max: usize) -> String {
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
