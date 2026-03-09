use std::path::PathBuf;

use anyhow::{Context, Result};
use oas3::spec::{ObjectOrReference, Operation as Oas3Operation, PathItem};

use crate::universe::{Operation, Param, PathEntry, Spec};

/// Parse an OpenAPI 3.1.x document from a YAML/JSON string and convert it
/// into our internal [`Spec`] representation.
pub async fn parse(file_path: String, content: String) -> Result<Spec> {
    let api: oas3::Spec = oas3::from_yaml(&content)
        .with_context(|| format!("failed to parse '{}' as OpenAPI 3.1.x", file_path))?;

    let paths = api
        .paths
        .as_ref()
        .map(|p| {
            p.iter()
                .map(|(path_str, item)| convert_path_entry(path_str.clone(), item))
                .collect()
        })
        .unwrap_or_default();

    Ok(Spec {
        file_path: PathBuf::from(file_path),
        openapi_version: api.openapi,
        title: api.info.title,
        version: api.info.version,
        description: api.info.description.unwrap_or_default(),
        paths,
    })
}

fn convert_path_entry(path_str: String, item: &PathItem) -> PathEntry {
    let method_ops: &[(&str, Option<&Oas3Operation>)] = &[
        ("GET", item.get.as_ref()),
        ("PUT", item.put.as_ref()),
        ("POST", item.post.as_ref()),
        ("DELETE", item.delete.as_ref()),
        ("OPTIONS", item.options.as_ref()),
        ("HEAD", item.head.as_ref()),
        ("PATCH", item.patch.as_ref()),
        ("TRACE", item.trace.as_ref()),
    ];

    let operations = method_ops
        .iter()
        .filter_map(|(method, maybe_op)| {
            maybe_op.map(|op| convert_operation(method.to_string(), op))
        })
        .collect();

    PathEntry {
        path: path_str,
        operations,
    }
}

fn convert_operation(method: String, op: &Oas3Operation) -> Operation {
    let params = op
        .parameters
        .iter()
        .filter_map(|oor| match oor {
            ObjectOrReference::Object(p) => Some(Param {
                name: p.name.clone(),
                location: format!("{:?}", p.location).to_lowercase(),
                required: p.required.unwrap_or(false),
                description: p.description.clone(),
            }),
            ObjectOrReference::Ref { .. } => None,
        })
        .collect();

    let response_codes = op
        .responses
        .as_ref()
        .map(|r| r.keys().cloned().collect())
        .unwrap_or_default();

    Operation {
        method,
        summary: op.summary.clone(),
        description: op.description.clone(),
        operation_id: op.operation_id.clone(),
        params,
        has_request_body: op.request_body.is_some(),
        response_codes,
    }
}
