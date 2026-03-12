mod parser;
mod tui;
mod universe;

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use async_walkdir::WalkDir;
use clap::Parser;
use futures_util::{FutureExt, TryFutureExt, TryStreamExt, future, stream};
use tokio::fs;
use tokio::io::AsyncReadExt;

use universe::{Universe, spawn_spec_loader};

#[derive(Parser)]
#[command(name = "apin", version)]
struct Cli {
    /// One or more OpenAPI spec files or directories to load.
    /// Directories are scanned recursively for YAML files that contain
    /// a top-level `openapi:` field.
    #[arg(required = true)]
    paths: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let inputs = resolve_inputs(cli.paths);
    let spec_rx = spawn_spec_loader(inputs);
    tui::launch(Universe::default(), spec_rx).await
}

// ─── Input resolution ─────────────────────────────────────────────────────────

/// Resolves CLI inputs into a merged stream of file paths.  Each input becomes
/// its own stream; all streams are interleaved via `select_all` so paths are
/// yielded as they are discovered across all inputs concurrently.
fn resolve_inputs(
    inputs: Vec<String>,
) -> impl futures_util::Stream<Item = Result<String>> + Send + 'static {
    let streams = inputs.into_iter().map(|input| {
        let (is_file, is_dir) = {
            let p = Path::new(&input);
            (p.is_file(), p.is_dir())
        };
        let s: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<String>> + Send>> =
            if is_file {
                Box::pin(stream::once(future::ready(Ok(input))))
            } else if is_dir {
                Box::pin(collect_openapi_files(input))
            } else {
                Box::pin(stream::once(future::ready(Err(anyhow!(
                    "'{}' not found — make sure the path is correct",
                    input
                )))))
            };
        s
    });
    stream::select_all(streams)
}

// ─── Directory scan ───────────────────────────────────────────────────────────

/// Recursively yields all `.yaml` / `.yml` files under `root` whose first
/// 4 KB contains a top-level `openapi:` field, as a stream of paths.
/// Files are yielded as they are discovered — no intermediate collection.
fn collect_openapi_files(
    root: String,
) -> impl futures_util::Stream<Item = Result<String>> + Send + 'static {
    WalkDir::new(&root)
        .map_err(|e| anyhow!(e))
        .try_filter_map(|entry| {
            let path = entry.path();
            if !is_yaml(&path) {
                return future::ready(Ok(None)).left_future();
            }
            has_openapi_field(path.clone())
                .map_ok(move |yes| yes.then(|| path.to_str().map(str::to_string)).flatten())
                .right_future()
        })
}

fn is_yaml(p: &std::path::Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("yaml") | Some("yml")
    )
}

// ─── OpenAPI header sniff ─────────────────────────────────────────────────────

/// Returns `true` if the file's first 4 KB contains a line starting with
/// `openapi:`.
///
/// Expressed as a combinator chain — no `async` block, no `.await`.
/// The read buffer and reader are captured inside `poll_fn` so no local
/// borrows escape the closure boundary.
fn has_openapi_field(path: PathBuf) -> impl std::future::Future<Output = Result<bool>> {
    let path_for_err = path.display().to_string();
    fs::File::open(path)
        .map_err(move |e| anyhow!(e).context(format!("cannot open '{}'", path_for_err)))
        .and_then(|file| {
            future::poll_fn({
                let mut buf = Vec::with_capacity(4096);
                let mut reader = file.take(4096);
                move |cx| {
                    let read_fut = reader.read_to_end(&mut buf);
                    tokio::pin!(read_fut);
                    match read_fut.poll(cx) {
                        std::task::Poll::Pending => std::task::Poll::Pending,
                        std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(anyhow!(e))),
                        std::task::Poll::Ready(Ok(_)) => {
                            let head = std::str::from_utf8(&buf).unwrap_or("");
                            std::task::Poll::Ready(Ok(head
                                .lines()
                                .any(|l| l.trim_start().starts_with("openapi:"))))
                        }
                    }
                }
            })
        })
}
