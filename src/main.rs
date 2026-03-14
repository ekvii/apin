mod parser;
mod tui;
mod universe;

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use async_walkdir::WalkDir;
use clap::Parser;
use futures_util::{Stream, TryStreamExt, future, stream};
use tokio::fs;
use tokio::io::AsyncReadExt;

use universe::load_spec;

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
    let specs = resolve_inputs(cli.paths)
        .map_ok(load_spec)
        .try_buffer_unordered(8);
    tui::launch(specs).await
}

/// Resolves CLI inputs into a merged stream of recursively found OpenAPI spec paths.
fn resolve_inputs(inputs: Vec<String>) -> impl Stream<Item = Result<String>> + Send + 'static {
    let streams_of_spec_paths = inputs.into_iter().map(|input| {
        let (is_file, is_dir) = {
            let p = Path::new(&input);
            (p.is_file(), p.is_dir())
        };
        if is_file {
            // input is a spec file path
            Box::pin(stream::once(future::ready(Ok(input))))
        } else if is_dir {
            // input is a directory with potential spec files
            Box::pin(collect_openapi_files(input))
        } else {
            Box::pin(stream::once(future::ready(Err(anyhow!(
                "'{}' not found — make sure the path is correct",
                input
            ))))) as std::pin::Pin<Box<dyn Stream<Item = Result<String>> + Send>>
        }
    });
    stream::select_all(streams_of_spec_paths)
}

/// Recursively yields all OpenAPI spec files as a stream of paths.
fn collect_openapi_files(root: String) -> impl Stream<Item = Result<String>> + Send + 'static {
    WalkDir::new(&root)
        .map_err(|e| anyhow!(e))
        .try_filter_map(|entry| async move {
            let path = entry.path();
            if !is_yaml(&path) {
                return Ok(None);
            }
            has_openapi_field(path).await
        })
}

fn is_yaml(p: &std::path::Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(str::to_lowercase),
        Some(s) if s == "yaml" || s == "yml"
    )
}

/// Returns the path as a `String` if the file's first 4 KB contains a line starting with `openapi:`,
/// or `None` otherwise.
async fn has_openapi_field(path: PathBuf) -> Result<Option<String>> {
    let path_str = path.to_str().map(str::to_string);
    let mut reader = fs::File::open(&path)
        .await
        .map_err(|e| anyhow!(e).context(format!("cannot open '{}'", path_str.as_deref().unwrap_or_default())))?
        .take(4096);
    let mut buf = Vec::with_capacity(4096);
    reader.read_to_end(&mut buf).await.map_err(|e| anyhow!(e))?;
    let head = std::str::from_utf8(&buf).unwrap_or("");
    let found = head.lines().any(|l| l.trim_start().starts_with("openapi:"));
    Ok(found.then_some(path_str).flatten())
}
