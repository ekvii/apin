use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::TryFutureExt;
use tokio::fs;

use crate::parser;

/// A single field extracted from a request-body schema.
#[derive(Debug, Clone)]
pub struct BodyField {
    pub name: String,
}

// ─── Schema tree ─────────────────────────────────────────────────────────────

/// The kind of a schema node, used for display hints.
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaKindHint {
    Object,
    Array,
    AllOf,
    AnyOf,
    OneOf,
    Primitive(String), // "string" | "integer" | "number" | "boolean" | ...
    Unknown,
}

impl SchemaKindHint {
    pub fn label(&self) -> &str {
        match self {
            SchemaKindHint::Object => "object",
            SchemaKindHint::Array => "array",
            SchemaKindHint::AllOf => "allOf",
            SchemaKindHint::AnyOf => "anyOf",
            SchemaKindHint::OneOf => "oneOf",
            SchemaKindHint::Primitive(s) => s.as_str(),
            SchemaKindHint::Unknown => "?",
        }
    }
}

/// A node in the schema tree, built from the resolved `serde_yaml::Value`.
///
/// Each node represents one logical schema entity: a named property, an array
/// item, an allOf/anyOf/oneOf branch, etc.  The `children` list is non-empty
/// for objects, arrays, and composition keywords.
#[derive(Debug, Clone)]
pub struct SchemaNode {
    /// Display label: property name, "items", "allOf[0]", etc.
    pub label: String,
    /// The resolved type/kind of this node.
    pub kind: SchemaKindHint,
    /// Short description (trimmed to one line).
    pub description: Option<String>,
    /// Whether this field is required (only meaningful for object properties).
    pub required: bool,
    /// Child nodes (properties, items, allOf branches, …).
    pub children: Vec<SchemaNode>,
}

impl SchemaNode {}

/// Parsed request-body metadata.
#[derive(Debug, Clone)]
pub struct RequestBody {
    /// Optional description from the requestBody object itself.
    pub description: Option<String>,
    /// Top-level fields of the body schema (empty if the schema is a $ref
    /// that could not be resolved, or uses a non-object schema kind).
    pub fields: Vec<BodyField>,
    /// Structured schema tree for the collapsible detail view.
    pub schema_tree: Option<SchemaNode>,
}

/// A single parameter (query, path, header, cookie) extracted from an operation.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    /// `query`, `path`, `header`, or `cookie`
    pub location: String,
    pub required: bool,
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
    /// Parsed request body, if present.
    pub request_body: Option<RequestBody>,
    /// HTTP status codes paired with their description, e.g. `[("200", Some("OK")), ("404", None)]`.
    pub responses: Vec<(String, Option<String>)>,
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

/// Read a single file, sniff its OpenAPI version, and dispatch to the
/// appropriate parser module.
///
/// Expressed as a combinator chain — no internal `.await` points.
pub fn load_spec(file_path: String) -> impl std::future::Future<Output = Result<Spec>> {
    let path_for_read_err = file_path.clone();
    fs::read_to_string(file_path.clone())
        // Map the io::Error into anyhow with context before entering and_then.
        .map_err(move |e| {
            anyhow!(e).context(format!("could not read file '{}'", path_for_read_err))
        })
        .and_then(move |content| {
            // Parsing is CPU-bound; run it off the async executor.
            let fp = file_path.clone();
            tokio::task::spawn_blocking(move || {
                let version = sniff_version(&content)
                    .with_context(|| format!("'{}' does not look like an OpenAPI document", fp))?;
                if version.starts_with("3.0") {
                    parser::v30::parse(fp, content)
                } else if version.starts_with("3.1") {
                    parser::v31::parse(fp, content)
                } else {
                    bail!(
                        "'{}' uses unsupported OpenAPI version '{}' \
                         (only 3.0.x and 3.1.x are supported)",
                        fp,
                        version
                    )
                }
            })
            // Flatten JoinError into anyhow, then the inner Result<Spec>.
            .map_err(|e| anyhow!(e))
            .and_then(std::future::ready)
        })
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
