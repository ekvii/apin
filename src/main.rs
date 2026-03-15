mod inputs;
mod parser;
mod spec;
mod tui;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use futures_util::StreamExt;

use inputs::resolve_inputs;
use spec::load_spec;

#[derive(Parser)]
#[command(name = "apin", version)]
struct Cli {
    /// One or more OpenAPI spec files, directories, or HTTP(S) URLs to load.
    /// Directories are scanned recursively for YAML files that contain
    /// a top-level `openapi:` field.
    /// URLs are probed for well-known spec paths and downloaded to a local file.
    #[arg(required = true)]
    inputs: Vec<String>,

    /// Directory where downloaded specs are stored.
    /// Defaults to the system temporary directory.
    /// A URL is not re-downloaded if its file already exists in this directory.
    #[arg(long, value_name = "DIR")]
    download_dir: Option<PathBuf>,

    /// Re-download specs from URLs even if they already exist locally.
    #[arg(long)]
    force_download: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let download_dir = cli
        .download_dir
        .unwrap_or_else(std::env::temp_dir);

    let specs = resolve_inputs(cli.inputs, download_dir, cli.force_download)
        .filter_map(|result| async move {
            match result {
                Ok(path) => load_spec(path).await.ok(),
                Err(_) => None,
            }
        });

    tui::run(specs).await
}
