use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tokio::fs;

use crate::parser;

/// A single parameter (query, path, header, cookie) extracted from an operation.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    /// `query`, `path`, `header`, or `cookie`
    pub location: String,
    pub required: bool,
    /// Optional description; stored for future display use.
    #[allow(dead_code)]
    pub description: Option<String>,
}

/// One HTTP operation (GET, POST, …) on a path.
#[derive(Debug, Clone)]
pub struct Operation {
    /// Upper-case method string, e.g. `"GET"`.
    pub method: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub operation_id: Option<String>,
    pub params: Vec<Param>,
    /// True when a requestBody is defined.
    pub has_request_body: bool,
    /// HTTP status codes declared in `responses`, e.g. `["200", "404"]`.
    pub response_codes: Vec<String>,
}

/// One path entry: the path string plus all its operations.
#[derive(Debug, Clone)]
pub struct PathEntry {
    pub path: String,
    pub operations: Vec<Operation>,
}

/// A parsed, normalised representation of a single OpenAPI spec file.
#[derive(Debug, Clone)]
pub struct Spec {
    pub file_path: PathBuf,
    /// Stored for future use (e.g. a status/info panel).
    #[allow(dead_code)]
    pub openapi_version: String,
    pub title: String,
    pub version: String,
    /// Stored for future use.
    #[allow(dead_code)]
    pub description: String,
    pub paths: Vec<PathEntry>,
}

/// The collection of all loaded specs.
#[derive(Debug, Default)]
pub struct Universe {
    pub specs: Vec<Spec>,
}

/// Load and parse every file in `files` concurrently.
///
/// Version detection works by reading the raw `openapi:` field from the YAML
/// before full parsing so we can route to the right library:
///   - `3.0.*`  → [`parser::v30`] (openapiv3, the most mature 3.0 library)
///   - `3.1.*`  → [`parser::v31`] (oas3)
///   - anything else → user-friendly [`anyhow`] error
pub async fn load_universe(files: Vec<String>) -> Result<Universe> {
    // Spawn one task per file, all running concurrently.
    let handles: Vec<_> = files
        .into_iter()
        .map(|file| tokio::spawn(async move { load_spec(file).await }))
        .collect();

    let mut specs = Vec::with_capacity(handles.len());
    for handle in handles {
        // Propagate both join errors and parse errors with useful context.
        let spec = handle.await.context("task panicked while loading spec")??;
        specs.push(spec);
    }

    Ok(Universe { specs })
}

/// Read a single file, sniff its OpenAPI version, and dispatch to the
/// appropriate parser module.
async fn load_spec(file_path: String) -> Result<Spec> {
    let content = fs::read_to_string(&file_path)
        .await
        .with_context(|| format!("could not read file '{}'", file_path))?;

    let version = sniff_version(&content)
        .with_context(|| format!("'{}' does not look like an OpenAPI document", file_path))?;

    if version.starts_with("3.0") {
        parser::v30::parse(file_path, content).await
    } else if version.starts_with("3.1") {
        parser::v31::parse(file_path, content).await
    } else {
        bail!(
            "'{}' uses unsupported OpenAPI version '{}' (only 3.0.x and 3.1.x are supported)",
            file_path,
            version
        )
    }
}

/// Extract the value of the top-level `openapi:` key without fully
/// deserialising the document.  Works for both YAML and JSON.
fn sniff_version(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        // YAML: `openapi: "3.1.0"` or `openapi: 3.1.0`
        if let Some(rest) = trimmed.strip_prefix("openapi:") {
            let v = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
        // JSON: `"openapi": "3.1.0"`
        if trimmed.contains("\"openapi\"") {
            if let Some(colon) = trimmed.find(':') {
                let v = trimmed[colon + 1..]
                    .trim()
                    .trim_matches('"')
                    .trim_end_matches(',')
                    .to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}
