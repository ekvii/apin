mod parser;
mod tui;
mod universe;

use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tokio::fs;

use universe::load_universe;

/// apin — OpenAPI universe browser
#[derive(Parser)]
#[command(name = "apin", version)]
struct Cli {
    /// One or more OpenAPI spec files or directories to load.
    /// Directories are scanned recursively for YAML files that contain
    /// a top-level `openapi:` field.
    paths: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.paths.is_empty() {
        bail!("no paths specified — run with --help for usage");
    }

    // Resolve every argument to a flat list of spec file paths before
    // touching the terminal so errors are printed cleanly to the shell.
    let mut files: Vec<String> = Vec::new();
    for input in &cli.paths {
        let p = Path::new(input);
        if !p.exists() {
            bail!("'{}' not found — make sure the path is correct", input);
        }
        if p.is_file() {
            files.push(input.clone());
        } else if p.is_dir() {
            let found = collect_openapi_files(p)
                .await
                .with_context(|| format!("failed to scan directory '{}'", input))?;
            if found.is_empty() {
                bail!("no OpenAPI YAML files found in '{}'", input);
            }
            files.extend(found);
        } else {
            bail!("'{}' is neither a file nor a directory", input);
        }
    }

    if files.is_empty() {
        bail!("no OpenAPI spec files found");
    }

    let universe = load_universe(files)
        .await
        .context("failed to load specs")?;

    tui::launch(universe)
}

/// Recursively walk `dir` and return paths of every `.yaml` / `.yml` file
/// whose content contains a top-level `openapi:` field.
async fn collect_openapi_files(dir: &Path) -> Result<Vec<String>> {
    let mut results = Vec::new();
    collect_recursive(dir, &mut results).await?;
    results.sort();
    Ok(results)
}

fn collect_recursive<'a>(
    dir: &'a Path,
    out: &'a mut Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let mut entries = fs::read_dir(dir)
            .await
            .with_context(|| format!("cannot read directory '{}'", dir.display()))?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                collect_recursive(&path, out).await?;
            } else if is_yaml(&path) && has_openapi_field(&path).await? {
                if let Some(s) = path.to_str() {
                    out.push(s.to_string());
                }
            }
        }
        Ok(())
    })
}

fn is_yaml(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("yaml") | Some("yml")
    )
}

/// Returns true if the file contains a line that starts with `openapi:`.
/// Reads only the first 4 KB to avoid loading huge files just for the check.
async fn has_openapi_field(p: &Path) -> Result<bool> {
    use tokio::io::AsyncReadExt;
    let mut file = fs::File::open(p)
        .await
        .with_context(|| format!("cannot open '{}'", p.display()))?;

    let mut buf = vec![0u8; 4096];
    let n = file.read(&mut buf).await?;
    let head = std::str::from_utf8(&buf[..n]).unwrap_or("");

    Ok(head.lines().any(|line| line.trim_start().starts_with("openapi:")))
}
