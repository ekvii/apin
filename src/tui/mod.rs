mod app;
mod components;
mod events;

use anyhow::{Context, Result};
use crossterm::{
    event::{Event, EventStream},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::{Stream, StreamExt};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::time::{Duration, interval};

use crate::spec::Spec;

use app::ApinApp;
use events::{Action, EventHandler};

/// Set up the terminal and run the TUI event loop.
/// Specs are consumed from the stream as they arrive; the loading screen is
/// shown until the first spec is received.
pub async fn run(specs: impl Stream<Item = Spec> + Send + 'static) -> Result<()> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen).context("failed to enter alternate screen")?;

    let backend = CrosstermBackend::new(out);
    let terminal = Terminal::new(backend).context("failed to create terminal")?;
    let mut app = ApinApp::new(terminal);

    let result = event_loop(&mut app, specs).await;

    disable_raw_mode().context("failed to disable raw mode")?;
    let mut stdout = std::io::stdout();
    execute!(stdout, LeaveAlternateScreen).context("failed to leave alternate screen")?;

    result
}

async fn event_loop(
    app: &mut ApinApp,
    specs: impl Stream<Item = Spec> + Send + 'static,
) -> Result<()> {
    let mut handler = EventHandler::new();
    let mut events = EventStream::new();
    let mut specs_done = false;
    let mut spinner = interval(Duration::from_millis(80));

    tokio::pin!(specs);

    loop {
        app.draw()?;

        tokio::select! {
            // Advance the spinner frame.
            _ = spinner.tick(), if app.is_loading() => {
                app.spinner_tick();
            }

            // A new spec arrived — push it into the app.
            result = specs.next(), if !specs_done => {
                match result {
                    Some(spec) => { app.push_spec(spec); }
                    None => { specs_done = true; }
                }
            }

            // A terminal event is ready — handle it.
            maybe_event = events.next() => {
                let Some(event_result) = maybe_event else { break };
                let event = event_result.context("failed to read event")?;

                if let Event::Key(key) = event {
                    match handler.handle(app, key) {
                        Action::Continue => {}
                        Action::Quit => break,
                    }
                }
            }
        }
    }

    Ok(())
}
