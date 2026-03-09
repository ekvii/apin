use std::path::PathBuf;

use anyhow::{Context, Result};
use openapiv3::{OpenAPI, Parameter, ReferenceOr};

use crate::universe::{Operation, Param, PathEntry, Spec};

/// Parse an OpenAPI 3.0.x document from a YAML/JSON string and convert it
/// into our internal [`Spec`] representation.
pub async fn parse(file_path: String, content: String) -> Result<Spec> {
    let api: OpenAPI = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse '{}' as OpenAPI 3.0.x", file_path))?;

    let paths = api
        .paths
        .paths
        .iter()
        .map(|(path_str, ref_or_item)| {
            let operations = match ref_or_item {
                ReferenceOr::Item(item) => item
                    .iter()
                    .map(|(method, op)| {
                        let params = op
                            .parameters
                            .iter()
                            .filter_map(|p| resolve_param(p))
                            .collect();

                        let response_codes = op
                            .responses
                            .responses
                            .keys()
                            .map(|sc| sc.to_string())
                            .collect();

                        Operation {
                            method: method.to_uppercase(),
                            summary: op.summary.clone(),
                            description: op.description.clone(),
                            operation_id: op.operation_id.clone(),
                            params,
                            has_request_body: op.request_body.is_some(),
                            response_codes,
                        }
                    })
                    .collect(),
                ReferenceOr::Reference { .. } => vec![],
            };

            PathEntry {
                path: path_str.clone(),
                operations,
            }
        })
        .collect();

    Ok(Spec {
        file_path: PathBuf::from(file_path),
        openapi_version: api.openapi,
        title: api.info.title,
        version: api.info.version,
        description: api.info.description.unwrap_or_default(),
        paths,
    })
}

/// Resolve a `ReferenceOr<Parameter>` to a `Param`, ignoring `$ref` entries
/// (we don't resolve component references at this stage).
fn resolve_param(p: &ReferenceOr<Parameter>) -> Option<Param> {
    let param = match p {
        ReferenceOr::Item(param) => param,
        ReferenceOr::Reference { .. } => return None,
    };

    let (data, location) = match param {
        Parameter::Query { parameter_data, .. } => (parameter_data, "query"),
        Parameter::Path { parameter_data, .. } => (parameter_data, "path"),
        Parameter::Header { parameter_data, .. } => (parameter_data, "header"),
        Parameter::Cookie { parameter_data, .. } => (parameter_data, "cookie"),
    };

    Some(Param {
        name: data.name.clone(),
        location: location.to_string(),
        required: data.required,
        description: data.description.clone(),
    })
}
