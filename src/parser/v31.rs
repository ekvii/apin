use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use oas3::spec::{
    Components, ObjectOrReference, ObjectSchema, Operation as Oas3Operation, PathItem,
};

use crate::spec::{
    BodyField, Operation, Param, PathEntry, RequestBody, SchemaKindHint, SchemaNode, Spec,
};

/// Parse an OpenAPI 3.1.x document from a YAML/JSON string and convert it
/// into our internal [`Spec`] representation.
pub fn parse(file_path: String, content: String) -> Result<Spec> {
    // Parse the raw YAML value tree once for inline $ref resolution.
    let raw_value: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse '{}' as YAML", file_path))?;

    let api: oas3::Spec = oas3::from_yaml(&content)
        .with_context(|| format!("failed to parse '{}' as OpenAPI 3.1.x", file_path))?;

    let components = api.components.as_ref();

    let schemas_value: Option<&serde_yaml::Value> =
        raw_value.get("components").and_then(|c| c.get("schemas"));

    let paths = api
        .paths
        .as_ref()
        .map(|p| {
            p.iter()
                .map(|(path_str, item)| {
                    convert_path_entry(path_str.clone(), item, components, schemas_value)
                })
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

fn convert_path_entry(
    path_str: String,
    item: &PathItem,
    components: Option<&Components>,
    schemas_value: Option<&serde_yaml::Value>,
) -> PathEntry {
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
            maybe_op.map(|op| convert_operation(method.to_string(), op, components, schemas_value))
        })
        .collect();

    PathEntry {
        path: path_str,
        operations,
    }
}

fn convert_operation(
    method: String,
    op: &Oas3Operation,
    components: Option<&Components>,
    schemas_value: Option<&serde_yaml::Value>,
) -> Operation {
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

    let responses = op
        .responses
        .as_ref()
        .map(|r| {
            r.iter()
                .map(|(code, ref_or_resp)| {
                    let desc = match ref_or_resp {
                        ObjectOrReference::Object(resp) => resp
                            .description
                            .as_deref()
                            .map(str::trim)
                            .filter(|d| !d.is_empty())
                            .map(str::to_string),
                        ObjectOrReference::Ref { .. } => None,
                    };
                    (code.clone(), desc)
                })
                .collect()
        })
        .unwrap_or_default();

    let request_body = op
        .request_body
        .as_ref()
        .map(|rb| resolve_request_body(rb, components, schemas_value));

    Operation {
        method,
        summary: op.summary.clone(),
        description: op.description.clone(),
        operation_id: op.operation_id.clone(),
        params,
        request_body,
        responses,
    }
}

// ─── Request body resolution ─────────────────────────────────────────────────

fn resolve_request_body(
    rb: &ObjectOrReference<oas3::spec::RequestBody>,
    components: Option<&Components>,
    schemas_value: Option<&serde_yaml::Value>,
) -> RequestBody {
    let body = match rb {
        ObjectOrReference::Object(b) => b,
        ObjectOrReference::Ref { ref_path, .. } => {
            let resolved = ref_path
                .strip_prefix("#/components/requestBodies/")
                .and_then(|name| components?.request_bodies.get(name))
                .and_then(|oor| match oor {
                    ObjectOrReference::Object(b) => Some(b),
                    ObjectOrReference::Ref { .. } => None,
                });
            match resolved {
                Some(b) => b,
                None => {
                    return RequestBody {
                        description: Some(format!("$ref: {}", ref_path)),
                        fields: vec![],
                        schema_tree: None,
                    };
                }
            }
        }
    };

    let schema_ref = body
        .content
        .get("application/json")
        .or_else(|| body.content.values().next())
        .and_then(|mt| mt.schema.as_ref());

    let fields = schema_ref
        .map(|sr| extract_fields(sr, components))
        .unwrap_or_default();

    let schema_tree = schema_ref.and_then(|sr| build_schema_tree(sr, schemas_value, components));

    RequestBody {
        description: body.description.clone(),
        fields,
        schema_tree,
    }
}

/// Resolve the schema ref to a cleaned `serde_yaml::Value`, then walk it
/// recursively to build a `SchemaNode` tree.
fn build_schema_tree(
    schema_ref: &ObjectOrReference<ObjectSchema>,
    schemas_value: Option<&serde_yaml::Value>,
    components: Option<&Components>,
) -> Option<SchemaNode> {
    let resolved_schema = resolve_schema(schema_ref, components)?;

    // Determine root label from $ref name if available.
    let root_label = match schema_ref {
        ObjectOrReference::Ref { ref_path, .. } => ref_path
            .strip_prefix("#/components/schemas/")
            .unwrap_or("body")
            .to_string(),
        ObjectOrReference::Object(_) => "body".to_string(),
    };

    let base_value: serde_yaml::Value = serde_yaml::to_value(resolved_schema).ok()?;
    let resolved = resolve_refs_in_value(base_value, schemas_value, &mut HashSet::new());
    let clean = strip_noise_keys(resolved);
    Some(value_to_schema_node(root_label, &clean, &HashSet::new()))
}

// ─── serde_yaml::Value helpers ───────────────────────────────────────────────

fn resolve_refs_in_value(
    val: serde_yaml::Value,
    schemas: Option<&serde_yaml::Value>,
    visited: &mut HashSet<String>,
) -> serde_yaml::Value {
    match val {
        serde_yaml::Value::Mapping(mut map) => {
            let ref_key = serde_yaml::Value::String("$ref".to_string());
            if let Some(serde_yaml::Value::String(ref ref_str)) = map.get(&ref_key).cloned()
                && let Some(name) = ref_str.strip_prefix("#/components/schemas/")
            {
                if !visited.contains(name) && let Some(target) = schemas.and_then(|s| s.get(name)) {
                    visited.insert(name.to_string());
                    let resolved =
                        resolve_refs_in_value(target.clone(), schemas, visited);
                    visited.remove(name);
                    return resolved;
                }
                map.remove(&ref_key);
                let name_key = serde_yaml::Value::String("type".to_string());
                map.insert(name_key, serde_yaml::Value::String(name.to_string()));
                return serde_yaml::Value::Mapping(map);
            }
            let resolved_map: serde_yaml::Mapping = map
                .into_iter()
                .map(|(k, v)| (k, resolve_refs_in_value(v, schemas, visited)))
                .collect();
            serde_yaml::Value::Mapping(resolved_map)
        }
        serde_yaml::Value::Sequence(seq) => serde_yaml::Value::Sequence(
            seq.into_iter()
                .map(|v| resolve_refs_in_value(v, schemas, visited))
                .collect(),
        ),
        other => other,
    }
}

fn strip_noise_keys(val: serde_yaml::Value) -> serde_yaml::Value {
    const NOISE: &[&str] = &[
        "title",
        "additionalProperties",
        "format",
        "pattern",
        "minLength",
        "maxLength",
        "minimum",
        "maximum",
        "default",
        "example",
        "extensions",
        "nullable",
        "readOnly",
        "writeOnly",
        "xml",
        "externalDocs",
    ];

    match val {
        serde_yaml::Value::Mapping(map) => {
            let cleaned: serde_yaml::Mapping = map
                .into_iter()
                .filter(|(k, _)| {
                    if let serde_yaml::Value::String(s) = k {
                        !NOISE.contains(&s.as_str())
                    } else {
                        true
                    }
                })
                .map(|(k, v)| (k, strip_noise_keys(v)))
                .collect();
            serde_yaml::Value::Mapping(cleaned)
        }
        serde_yaml::Value::Sequence(seq) => {
            serde_yaml::Value::Sequence(seq.into_iter().map(strip_noise_keys).collect())
        }
        other => other,
    }
}

// ─── SchemaNode tree builder ─────────────────────────────────────────────────

fn value_to_schema_node(
    label: String,
    val: &serde_yaml::Value,
    required_set: &HashSet<String>,
) -> SchemaNode {
    let required = required_set.contains(&label);

    let mapping = match val {
        serde_yaml::Value::Mapping(m) => m,
        _ => {
            return SchemaNode {
                label,
                kind: SchemaKindHint::Unknown,
                description: None,
                required,
                children: vec![],
            };
        }
    };

    let description: Option<String> = mapping
        .get(serde_yaml::Value::String("description".into()))
        .and_then(|v| match v {
            serde_yaml::Value::String(s) => Some(s.lines().next().unwrap_or("").trim().to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty());

    let str_val = |key: &str| -> Option<&str> {
        mapping
            .get(serde_yaml::Value::String(key.into()))
            .and_then(|v| {
                if let serde_yaml::Value::String(s) = v {
                    Some(s.as_str())
                } else {
                    None
                }
            })
    };

    // ── allOf / anyOf / oneOf ─────────────────────────────────────────────────
    for (kw, hint) in &[
        ("allOf", SchemaKindHint::AllOf),
        ("anyOf", SchemaKindHint::AnyOf),
        ("oneOf", SchemaKindHint::OneOf),
    ] {
        if let Some(serde_yaml::Value::Sequence(branches)) =
            mapping.get(serde_yaml::Value::String((*kw).into()))
        {
            let children: Vec<SchemaNode> = branches
                .iter()
                .enumerate()
                .map(|(i, branch)| {
                    value_to_schema_node(format!("{}[{}]", kw, i), branch, &HashSet::new())
                })
                .collect();
            return SchemaNode {
                label,
                kind: hint.clone(),
                description,
                required,
                children,
            };
        }
    }

    // ── Object with properties ────────────────────────────────────────────────
    if let Some(serde_yaml::Value::Mapping(props)) =
        mapping.get(serde_yaml::Value::String("properties".into()))
    {
        let child_required: HashSet<String> = mapping
            .get(serde_yaml::Value::String("required".into()))
            .and_then(|v| {
                if let serde_yaml::Value::Sequence(seq) = v {
                    Some(
                        seq.iter()
                            .filter_map(|s| {
                                if let serde_yaml::Value::String(name) = s {
                                    Some(name.clone())
                                } else {
                                    None
                                }
                            })
                            .collect(),
                    )
                } else {
                    None
                }
            })
            .unwrap_or_default();

        let children: Vec<SchemaNode> = props
            .iter()
            .map(|(k, v)| {
                let prop_name = match k {
                    serde_yaml::Value::String(s) => s.clone(),
                    _ => "?".to_string(),
                };
                value_to_schema_node(prop_name, v, &child_required)
            })
            .collect();

        return SchemaNode {
            label,
            kind: SchemaKindHint::Object,
            description,
            required,
            children,
        };
    }

    // ── Array with items ──────────────────────────────────────────────────────
    if let Some(items_val) = mapping.get(serde_yaml::Value::String("items".into())) {
        let item_node = value_to_schema_node("items".to_string(), items_val, &HashSet::new());
        return SchemaNode {
            label,
            kind: SchemaKindHint::Array,
            description,
            required,
            children: vec![item_node],
        };
    }

    // ── Primitive ─────────────────────────────────────────────────────────────
    if let Some(type_str) = str_val("type") {
        return SchemaNode {
            label,
            kind: SchemaKindHint::Primitive(type_str.to_string()),
            description,
            required,
            children: vec![],
        };
    }

    // ── Fallback ──────────────────────────────────────────────────────────────
    SchemaNode {
        label,
        kind: SchemaKindHint::Unknown,
        description,
        required,
        children: vec![],
    }
}

fn extract_fields(
    schema_ref: &ObjectOrReference<ObjectSchema>,
    components: Option<&Components>,
) -> Vec<BodyField> {
    let schema = match resolve_schema(schema_ref, components) {
        Some(s) => s,
        None => return vec![],
    };
    schema
        .properties
        .keys()
        .map(|name| BodyField { name: name.clone() })
        .collect()
}

fn resolve_schema<'a>(
    r: &'a ObjectOrReference<ObjectSchema>,
    components: Option<&'a Components>,
) -> Option<&'a ObjectSchema> {
    match r {
        ObjectOrReference::Object(s) => Some(s),
        ObjectOrReference::Ref { ref_path, .. } => {
            let name = ref_path.strip_prefix("#/components/schemas/")?;
            match components?.schemas.get(name)? {
                ObjectOrReference::Object(s) => Some(s),
                ObjectOrReference::Ref { .. } => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/openapi31_fixture.yaml"
    );

    #[test]
    fn vpc_post_has_body_fields() {
        let content = std::fs::read_to_string(FIXTURE).expect("fixture not found");
        let spec = parse("fixture.yaml".to_string(), content).expect("parse failed");
        let vpc_path = spec
            .paths
            .iter()
            .find(|p| p.path == "/v1/vpcs")
            .expect("/v1/vpcs not found");
        let post = vpc_path
            .operations
            .iter()
            .find(|o| o.method == "POST")
            .expect("POST not found");
        let rb = post.request_body.as_ref().expect("request_body is None");
        assert!(!rb.fields.is_empty(), "expected fields but got none");
        assert!(
            rb.fields.iter().any(|f| f.name == "name"),
            "expected 'name' field"
        );
    }

    #[test]
    fn all_mutating_operations_have_body_fields() {
        let content = std::fs::read_to_string(FIXTURE).expect("fixture not found");
        let spec = parse("fixture.yaml".to_string(), content).expect("parse failed");
        let empty: Vec<_> = spec
            .paths
            .iter()
            .flat_map(|p| p.operations.iter().map(move |o| (p, o)))
            .filter(|(_, o)| matches!(o.method.as_str(), "POST" | "PUT" | "PATCH"))
            .filter(|(_, o)| {
                o.request_body
                    .as_ref()
                    .map(|rb| rb.fields.is_empty())
                    .unwrap_or(false)
            })
            .map(|(p, o)| format!("{} {}", o.method, p.path))
            .collect();
        assert!(
            empty.is_empty(),
            "operations with empty body fields: {:?}",
            empty
        );
    }

    #[test]
    fn vpc_post_schema_tree_populated() {
        let content = std::fs::read_to_string(FIXTURE).expect("fixture not found");
        let spec = parse("fixture.yaml".to_string(), content).unwrap();
        let post = spec
            .paths
            .iter()
            .find(|p| p.path == "/v1/vpcs")
            .unwrap()
            .operations
            .iter()
            .find(|o| o.method == "POST")
            .unwrap();
        let tree = post
            .request_body
            .as_ref()
            .unwrap()
            .schema_tree
            .as_ref()
            .expect("schema_tree should be Some");
        // The root should be an object with named children (properties).
        assert!(
            !tree.children.is_empty(),
            "expected schema_tree to have children"
        );
        assert!(
            tree.children.iter().any(|c| c.label == "name"),
            "expected 'name' child in schema_tree"
        );
        assert!(
            tree.children.iter().any(|c| c.label == "projectId"),
            "expected 'projectId' child in schema_tree"
        );
    }
}
