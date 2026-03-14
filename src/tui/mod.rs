mod app;
mod events;
mod path_tree;

use anyhow::{Context, Result};
use crossterm::{
    event::{Event, EventStream},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::{Stream, StreamExt};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::universe::Spec;

use app::ApinApp;
use events::{Action, EventHandler};

/// Terminal lifecycle management
pub async fn launch(
    specs: impl Stream<Item = anyhow::Result<Spec>> + Send + 'static,
) -> Result<()> {
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
    specs: impl Stream<Item = anyhow::Result<Spec>> + Send + 'static,
) -> Result<()> {
    let mut handler = EventHandler::new();
    let mut events = EventStream::new();
    let mut specs_done = false;

    tokio::pin!(specs);

    loop {
        app.draw()?;

        tokio::select! {
            // A new spec arrived — push it into the app.
            result = specs.next(), if !specs_done => {
                match result {
                    Some(Ok(spec)) => { app.push_spec(spec); }
                    Some(Err(_)) => {} // load error — skip
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
