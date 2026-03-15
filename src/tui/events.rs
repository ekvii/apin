use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::app::{ApinApp, Focus};

// ─── Action ───────────────────────────────────────────────────────────────────

/// What the event loop should do after handling a key event.
pub(super) enum Action {
    /// Keep running.
    Continue,
    /// Exit the event loop cleanly.
    Quit,
}

// ─── EventHandler ─────────────────────────────────────────────────────────────

/// Stateful key-event handler.  Owns the cross-event state (`pending_g`) that
/// must persist between keystrokes.
pub(super) struct EventHandler {
    /// Set when `g` is pressed; cleared on the next key.  When the next key is
    /// also `g` the cursor jumps to the top (`gg` vim binding).
    pending_g: bool,
}

impl EventHandler {
    pub(super) fn new() -> Self {
        Self { pending_g: false }
    }

    /// Handle a single key event, mutating `app` as needed.
    pub(super) fn handle(&mut self, app: &mut ApinApp, key: KeyEvent) -> Action {
        if key.kind != KeyEventKind::Press {
            return Action::Continue;
        }

        // ── Loading screen — only quit is accepted ────────────────────────────
        if app.is_loading() {
            return match key.code {
                KeyCode::Char('q') => Action::Quit,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
                _ => Action::Continue,
            };
        }

        // ── Search input mode ─────────────────────────────────────────────────
        if app.is_searching() {
            return handle_search_key(app, key);
        }

        // ── Detail full-screen mode ───────────────────────────────────────────
        if app.focus == Focus::Detail {
            return handle_detail_key(app, key, &mut self.pending_g);
        }

        // ── Normal navigation mode ────────────────────────────────────────────
        handle_normal_key(app, key, &mut self.pending_g)
    }
}

// ─── Search input mode ────────────────────────────────────────────────────────

fn handle_search_key(app: &mut ApinApp, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc => match app.focus {
            Focus::Tree => app.tree_search_cancel(),
            Focus::Ops => app.ops_search_cancel(),
            Focus::Detail => app.detail_search_cancel(),
            Focus::Specs => {}
        },
        KeyCode::Enter => match app.focus {
            Focus::Tree => app.tree_search_commit(),
            Focus::Ops => app.ops_search_commit(),
            // In detail, Enter closes the input and jumps to the first match.
            Focus::Detail => app.detail_search_enter(),
            Focus::Specs => {}
        },
        KeyCode::Backspace => match app.focus {
            Focus::Tree => app.tree_search_pop(),
            Focus::Ops => app.ops_search_pop(),
            Focus::Detail => app.detail_search_pop(),
            Focus::Specs => {}
        },
        KeyCode::Char(ch) => {
            // Ctrl+U clears the query (Unix readline convention).
            if key.modifiers.contains(KeyModifiers::CONTROL) && ch == 'u' {
                match app.focus {
                    Focus::Tree => app.tree_search_clear(),
                    Focus::Ops => app.ops_search_clear(),
                    Focus::Detail => app.detail_search_clear(),
                    Focus::Specs => {}
                }
            } else {
                match app.focus {
                    Focus::Tree => app.tree_search_push(ch),
                    Focus::Ops => app.ops_search_push(ch),
                    Focus::Detail => app.detail_search_push(ch),
                    Focus::Specs => {}
                }
            }
        }
        // Arrow keys still move the cursor while search is open.
        KeyCode::Down => match app.focus {
            Focus::Tree => app.tree_move_down(),
            Focus::Ops => app.ops_move_down(),
            Focus::Specs | Focus::Detail => {}
        },
        KeyCode::Up => match app.focus {
            Focus::Tree => app.tree_move_up(),
            Focus::Ops => app.ops_move_up(),
            Focus::Specs | Focus::Detail => {}
        },
        _ => {}
    }
    Action::Continue
}

// ─── Detail full-screen mode ──────────────────────────────────────────────────

fn handle_detail_key(app: &mut ApinApp, key: KeyEvent, pending_g: &mut bool) -> Action {
    if app.detail_in_tree() {
        match key.code {
            KeyCode::Char('f') => app.detail_unfocus_tree(),
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('j') | KeyCode::Down => app.schema_tree_key_down(),
            KeyCode::Char('k') | KeyCode::Up => app.schema_tree_key_up(),
            KeyCode::Char('h') | KeyCode::Left => app.schema_tree_key_left(),
            KeyCode::Char('l') | KeyCode::Right => app.schema_tree_key_right(),
            _ => {}
        }
    } else {
        match key.code {
            KeyCode::Backspace | KeyCode::Esc | KeyCode::Char('h') => app.detail_back(),
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('j') | KeyCode::Down => app.detail_scroll_down(1),
            KeyCode::Char('k') | KeyCode::Up => app.detail_scroll_up(1),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.detail_scroll_half_down();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.detail_scroll_half_up();
            }
            KeyCode::Char('f') => app.detail_focus_tree(),
            KeyCode::Char('/') => app.detail_search_open(),
            KeyCode::Char('n') => app.detail_search_next(),
            KeyCode::Char('N') => app.detail_search_prev(),
            KeyCode::Char('g') => *pending_g = true,
            KeyCode::Char('G') => app.detail_scroll_bottom(),
            _ => {}
        }
    }
    Action::Continue
}

// ─── Normal navigation mode ───────────────────────────────────────────────────

fn handle_normal_key(app: &mut ApinApp, key: KeyEvent, pending_g: &mut bool) -> Action {
    if *pending_g {
        *pending_g = false;
        if key.code == KeyCode::Char('g') {
            match app.focus {
                Focus::Specs => app.specs_move_top(),
                Focus::Tree => app.tree_move_top(),
                Focus::Ops => app.ops_move_top(),
                Focus::Detail => app.detail_scroll_top(),
            }
        }
        return Action::Continue;
    }

    match key.code {
        KeyCode::Char('q') => return Action::Quit,

        KeyCode::Char('/') => match app.focus {
            Focus::Tree => app.tree_search_open(),
            Focus::Ops => app.ops_search_open(),
            Focus::Specs | Focus::Detail => {}
        },

        KeyCode::Char('j') | KeyCode::Down => match app.focus {
            Focus::Specs => app.specs_move_down(),
            Focus::Tree => app.tree_move_down(),
            Focus::Ops => app.ops_move_down(),
            Focus::Detail => app.detail_scroll_down(1),
        },

        KeyCode::Char('k') | KeyCode::Up => match app.focus {
            Focus::Specs => app.specs_move_up(),
            Focus::Tree => app.tree_move_up(),
            Focus::Ops => app.ops_move_up(),
            Focus::Detail => app.detail_scroll_up(1),
        },

        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => match app.focus {
            Focus::Specs => {
                for _ in 0..10 {
                    app.specs_move_down();
                }
            }
            Focus::Tree => {
                for _ in 0..10 {
                    app.tree_move_down();
                }
            }
            Focus::Ops => {
                for _ in 0..10 {
                    app.ops_move_down();
                }
            }
            Focus::Detail => app.detail_scroll_half_down(),
        },

        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => match app.focus {
            Focus::Specs => {
                for _ in 0..10 {
                    app.specs_move_up();
                }
            }
            Focus::Tree => {
                for _ in 0..10 {
                    app.tree_move_up();
                }
            }
            Focus::Ops => {
                for _ in 0..10 {
                    app.ops_move_up();
                }
            }
            Focus::Detail => app.detail_scroll_half_up(),
        },

        KeyCode::Char('g') => *pending_g = true,

        KeyCode::Char('G') => match app.focus {
            Focus::Specs => app.specs_move_bottom(),
            Focus::Tree => app.tree_move_bottom(),
            Focus::Ops => app.ops_move_bottom(),
            Focus::Detail => app.detail_scroll_bottom(),
        },

        KeyCode::Enter => match app.focus {
            Focus::Ops => app.enter_detail_from_ops(),
            Focus::Tree => app.enter_detail_from_tree(),
            Focus::Specs | Focus::Detail => {}
        },

        KeyCode::Char('l') | KeyCode::Right | KeyCode::Tab => match app.focus {
            Focus::Specs => app.focus_tree(),
            Focus::Tree => app.tree_move_right_or_ops(),
            Focus::Ops | Focus::Detail => {}
        },

        KeyCode::Char('h') | KeyCode::Left => match app.focus {
            Focus::Specs => {}
            Focus::Tree => app.tree_move_left_or_specs(),
            Focus::Ops => app.focus = Focus::Tree,
            Focus::Detail => app.detail_back(),
        },

        KeyCode::Backspace => {
            if app.focus == Focus::Detail {
                app.detail_back();
            }
        }

        _ => {}
    }

    Action::Continue
}
