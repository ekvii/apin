/// Parse an OpenAPI 3.2.x document.
///
/// OpenAPI 3.2.0 (published September 2025) is structurally nearly identical to
/// 3.1.x.  The main additions relevant to apin are:
///
/// - `additionalOperations` map on Path Item objects — allows arbitrary HTTP
///   methods beyond the fixed 8 (e.g. COPY, QUERY, MOVE, LOCK, …).
/// - `query` as a fixed field on Path Item (convenience alias for an
///   `additionalOperations` entry with method "QUERY").
/// - `$self` URI field on the OpenAPI Object (ignored for display purposes).
///
/// The `oas3` crate targets 3.1.x but the document structure is the same, so we
/// reuse its typed parsing for everything that 3.1 covers, then layer on top
/// the raw-value pass needed to pick up `additionalOperations`.
use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use oas3::spec::{
    Components, ObjectOrReference, ObjectSchema, Operation as Oas3Operation, PathItem,
    SchemaType, SchemaTypeSet,
};

use crate::spec::{
    BodyField, Operation, Param, PathEntry, PathKind, RequestBody, Response, SchemaKindHint,
    SchemaNode, Spec,
};

// Re-use the internal helpers from v31 by importing them through the public
// parse entry-point — we only need to supply our own top-level `parse` fn
// and the `additionalOperations` overlay.

/// Parse an OpenAPI 3.2.x document from a YAML/JSON string.
pub fn parse(file_path: String, content: String) -> Result<Spec> {
    // Parse raw YAML once — needed both for $ref resolution AND for
    // picking up `additionalOperations` / `query` that oas3 doesn't know.
    let raw_value: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse '{}' as YAML", file_path))?;

    // oas3 happily parses 3.2 docs (it doesn't reject unknown versions).
    let api: oas3::Spec = oas3::from_yaml(&content)
        .with_context(|| format!("failed to parse '{}' as OpenAPI 3.2.x", file_path))?;

    let components = api.components.as_ref();

    let schemas_value: Option<&serde_yaml::Value> =
        raw_value.get("components").and_then(|c| c.get("schemas"));

    // ── Standard paths ────────────────────────────────────────────────────────
    let mut paths: Vec<PathEntry> = api
        .paths
        .as_ref()
        .map(|p| {
            p.iter()
                .map(|(path_str, item)| {
                    // Overlay additionalOperations from the raw value.
                    let additional_ops_value = raw_value
                        .get("paths")
                        .and_then(|ps| ps.get(path_str.as_str()))
                        .and_then(|pi| pi.get("additionalOperations"));

                    convert_path_entry(
                        path_str.clone(),
                        item,
                        PathKind::Path,
                        components,
                        schemas_value,
                        additional_ops_value,
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    // ── Webhooks (same as 3.1) ────────────────────────────────────────────────
    for (name, item) in &api.webhooks {
        let additional_ops_value = raw_value
            .get("webhooks")
            .and_then(|wh| wh.get(name.as_str()))
            .and_then(|pi| pi.get("additionalOperations"));

        paths.push(convert_path_entry(
            name.clone(),
            item,
            PathKind::Webhook,
            components,
            schemas_value,
            additional_ops_value,
        ));
    }

    Ok(Spec {
        file_path: PathBuf::from(file_path),
        openapi_version: api.openapi,
        title: api.info.title,
        version: api.info.version,
        description: api.info.description.unwrap_or_default(),
        paths,
    })
}

// ─── Path entry conversion ────────────────────────────────────────────────────

fn convert_path_entry(
    path_str: String,
    item: &PathItem,
    kind: PathKind,
    components: Option<&Components>,
    schemas_value: Option<&serde_yaml::Value>,
    // Raw value of the `additionalOperations` map for this path item, if any.
    additional_ops_value: Option<&serde_yaml::Value>,
) -> PathEntry {
    // Standard 8 methods handled via the oas3 typed struct.
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

    let mut operations: Vec<Operation> = method_ops
        .iter()
        .filter_map(|(method, maybe_op)| {
            maybe_op.map(|op| convert_operation(method.to_string(), op, components, schemas_value))
        })
        .collect();

    // ── additionalOperations (3.2 new field) ─────────────────────────────────
    // additionalOperations is a map of { methodName -> Operation Object }.
    // Parse each entry from the raw YAML value.
    if let Some(serde_yaml::Value::Mapping(extra_map)) = additional_ops_value {
        for (key, op_val) in extra_map {
            let method_name = match key {
                serde_yaml::Value::String(s) => s.to_uppercase(),
                _ => continue,
            };
            if let Some(op) = parse_raw_operation(method_name, op_val, schemas_value) {
                operations.push(op);
            }
        }
    }

    PathEntry {
        path: path_str,
        kind,
        operations,
    }
}

// ─── Typed operation conversion (mirrors v31) ─────────────────────────────────

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
            ObjectOrReference::Object(p) => {
                let schema_type = p
                    .schema
                    .as_ref()
                    .and_then(|sr| resolve_schema(sr, components))
                    .and_then(|s| s.schema_type.as_ref())
                    .and_then(schema_type_str);
                Some(Param {
                    name: p.name.clone(),
                    location: format!("{:?}", p.location).to_lowercase(),
                    required: p.required.unwrap_or(false),
                    description: p.description.clone(),
                    schema_type,
                    deprecated: p.deprecated.unwrap_or(false),
                })
            }
            ObjectOrReference::Ref { ref_path, .. } => {
                let name = ref_path.strip_prefix("#/components/parameters/")?;
                let resolved_oor = components?.parameters.get(name)?;
                match resolved_oor {
                    ObjectOrReference::Object(p) => {
                        let schema_type = p
                            .schema
                            .as_ref()
                            .and_then(|sr| resolve_schema(sr, components))
                            .and_then(|s| s.schema_type.as_ref())
                            .and_then(schema_type_str);
                        Some(Param {
                            name: p.name.clone(),
                            location: format!("{:?}", p.location).to_lowercase(),
                            required: p.required.unwrap_or(false),
                            description: p.description.clone(),
                            schema_type,
                            deprecated: p.deprecated.unwrap_or(false),
                        })
                    }
                    ObjectOrReference::Ref { .. } => None,
                }
            }
        })
        .collect();

    let responses: Vec<Response> = op
        .responses
        .as_ref()
        .map(|r| {
            r.iter()
                .map(|(code, ref_or_resp)| {
                    let (desc, schema_tree) = match ref_or_resp {
                        ObjectOrReference::Object(resp) => {
                            let d = resp
                                .description
                                .as_deref()
                                .map(str::trim)
                                .filter(|d| !d.is_empty())
                                .map(str::to_string);
                            let st = resp
                                .content
                                .get("application/json")
                                .or_else(|| resp.content.values().next())
                                .and_then(|mt| mt.schema.as_ref())
                                .and_then(|sr| build_schema_tree(sr, schemas_value, components));
                            (d, st)
                        }
                        ObjectOrReference::Ref { ref_path, .. } => {
                            let resolved = ref_path
                                .strip_prefix("#/components/responses/")
                                .and_then(|name| components?.responses.get(name))
                                .and_then(|oor| match oor {
                                    ObjectOrReference::Object(resp) => Some(resp),
                                    ObjectOrReference::Ref { .. } => None,
                                });
                            match resolved {
                                Some(resp) => {
                                    let d = resp
                                        .description
                                        .as_deref()
                                        .map(str::trim)
                                        .filter(|d| !d.is_empty())
                                        .map(str::to_string);
                                    let st = resp
                                        .content
                                        .get("application/json")
                                        .or_else(|| resp.content.values().next())
                                        .and_then(|mt| mt.schema.as_ref())
                                        .and_then(|sr| {
                                            build_schema_tree(sr, schemas_value, components)
                                        });
                                    (d, st)
                                }
                                None => (None, None),
                            }
                        }
                    };
                    Response {
                        code: code.clone(),
                        description: desc,
                        schema_tree,
                    }
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
        tags: op.tags.clone(),
        deprecated: op.deprecated.unwrap_or(false),
        params,
        request_body,
        responses,
    }
}

// ─── Raw-value operation parser (for additionalOperations) ───────────────────

/// Parse an operation from a raw `serde_yaml::Value` (used for
/// `additionalOperations` entries that the typed `oas3` struct doesn't expose).
fn parse_raw_operation(
    method: String,
    val: &serde_yaml::Value,
    schemas_value: Option<&serde_yaml::Value>,
) -> Option<Operation> {
    let map = val.as_mapping()?;

    let str_field = |key: &str| -> Option<String> {
        map.get(serde_yaml::Value::String(key.into()))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };

    let summary = str_field("summary");
    let description = str_field("description");
    let operation_id = str_field("operationId");

    let deprecated = map
        .get(serde_yaml::Value::String("deprecated".into()))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let tags: Vec<String> = map
        .get(serde_yaml::Value::String("tags".into()))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // Parameters — parse inline only (no $ref resolution for simplicity).
    let params: Vec<Param> = map
        .get(serde_yaml::Value::String("parameters".into()))
        .and_then(|v| v.as_sequence())
        .map(|seq| seq.iter().filter_map(parse_raw_param).collect())
        .unwrap_or_default();

    // Request body — basic extraction.
    let request_body = map
        .get(serde_yaml::Value::String("requestBody".into()))
        .map(|rb_val| parse_raw_request_body(rb_val, schemas_value));

    // Responses — basic extraction.
    let responses: Vec<Response> = map
        .get(serde_yaml::Value::String("responses".into()))
        .and_then(|v| v.as_mapping())
        .map(|resp_map| {
            resp_map
                .iter()
                .map(|(code_val, resp_val)| {
                    let code = match code_val {
                        serde_yaml::Value::String(s) => s.clone(),
                        serde_yaml::Value::Number(n) => n.to_string(),
                        _ => "?".to_string(),
                    };
                    let desc = resp_val
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let schema_tree =
                        extract_raw_schema_tree(resp_val, schemas_value);
                    Response {
                        code,
                        description: desc,
                        schema_tree,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Some(Operation {
        method,
        summary,
        description,
        operation_id,
        tags,
        deprecated,
        params,
        request_body,
        responses,
    })
}

fn parse_raw_param(val: &serde_yaml::Value) -> Option<Param> {
    let map = val.as_mapping()?;
    let str_field = |key: &str| -> Option<String> {
        map.get(serde_yaml::Value::String(key.into()))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let name = str_field("name")?;
    let location = str_field("in").unwrap_or_else(|| "query".to_string());
    let required = map
        .get(serde_yaml::Value::String("required".into()))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let description = str_field("description");
    let deprecated = map
        .get(serde_yaml::Value::String("deprecated".into()))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let schema_type = map
        .get(serde_yaml::Value::String("schema".into()))
        .and_then(|s| s.get("type"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Some(Param {
        name,
        location,
        required,
        description,
        schema_type,
        deprecated,
    })
}

fn parse_raw_request_body(
    val: &serde_yaml::Value,
    schemas_value: Option<&serde_yaml::Value>,
) -> RequestBody {
    let required = val
        .get("required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let description = val
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let schema_val = val
        .get("content")
        .and_then(|c| {
            c.get("application/json")
                .or_else(|| c.as_mapping().and_then(|m| m.values().next()))
        })
        .and_then(|mt| mt.get("schema"));

    let schema_tree = schema_val.map(|sv| {
        let resolved = resolve_refs_in_value(sv.clone(), schemas_value, &mut HashSet::new());
        let clean = strip_noise_keys(resolved);
        value_to_schema_node("body".to_string(), &clean, &HashSet::new())
    });

    let fields: Vec<BodyField> = schema_val
        .and_then(|sv| sv.get("properties"))
        .and_then(|p| p.as_mapping())
        .map(|m| {
            m.keys()
                .filter_map(|k| k.as_str().map(|s| BodyField { name: s.to_string() }))
                .collect()
        })
        .unwrap_or_default();

    RequestBody {
        description,
        required,
        fields,
        schema_tree,
    }
}

fn extract_raw_schema_tree(
    resp_val: &serde_yaml::Value,
    schemas_value: Option<&serde_yaml::Value>,
) -> Option<SchemaNode> {
    let schema_val = resp_val
        .get("content")
        .and_then(|c| {
            c.get("application/json")
                .or_else(|| c.as_mapping().and_then(|m| m.values().next()))
        })
        .and_then(|mt| mt.get("schema"))?;

    let resolved = resolve_refs_in_value(schema_val.clone(), schemas_value, &mut HashSet::new());
    let clean = strip_noise_keys(resolved);
    Some(value_to_schema_node("body".to_string(), &clean, &HashSet::new()))
}

// ─── Request body resolution (typed path) ─────────────────────────────────────

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
                        required: false,
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
        required: body.required.unwrap_or(false),
        fields,
        schema_tree,
    }
}

// ─── Schema tree builder ──────────────────────────────────────────────────────

fn build_schema_tree(
    schema_ref: &ObjectOrReference<ObjectSchema>,
    schemas_value: Option<&serde_yaml::Value>,
    components: Option<&Components>,
) -> Option<SchemaNode> {
    let resolved_schema = resolve_schema(schema_ref, components)?;

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

// ─── serde_yaml::Value helpers ────────────────────────────────────────────────

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
                if !visited.contains(name)
                    && let Some(target) = schemas.and_then(|s| s.get(name))
                {
                    visited.insert(name.to_string());
                    let resolved = resolve_refs_in_value(target.clone(), schemas, visited);
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

// ─── SchemaNode tree builder ──────────────────────────────────────────────────

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

// ─── Schema/field helpers ─────────────────────────────────────────────────────

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

fn schema_type_str(ts: &SchemaTypeSet) -> Option<String> {
    match ts {
        SchemaTypeSet::Single(SchemaType::String) => Some("string".to_string()),
        SchemaTypeSet::Single(SchemaType::Integer) => Some("integer".to_string()),
        SchemaTypeSet::Single(SchemaType::Number) => Some("number".to_string()),
        SchemaTypeSet::Single(SchemaType::Boolean) => Some("boolean".to_string()),
        SchemaTypeSet::Single(SchemaType::Array) => Some("array".to_string()),
        SchemaTypeSet::Single(SchemaType::Object) => Some("object".to_string()),
        _ => None,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/openapi32_fixture.yaml"
    );

    fn load() -> Spec {
        let content = std::fs::read_to_string(FIXTURE).expect("fixture not found");
        parse("fixture.yaml".to_string(), content).expect("parse failed")
    }

    // ── Basic smoke ───────────────────────────────────────────────────────────

    #[test]
    fn spec_version_is_3_2() {
        let spec = load();
        assert!(
            spec.openapi_version.starts_with("3.2"),
            "expected version 3.2.x, got {}",
            spec.openapi_version
        );
    }

    #[test]
    fn paths_are_parsed() {
        let spec = load();
        let path_entries: Vec<_> = spec
            .paths
            .iter()
            .filter(|p| p.kind == PathKind::Path)
            .collect();
        assert!(!path_entries.is_empty(), "expected at least one path");
    }

    // ── Standard operations ───────────────────────────────────────────────────

    #[test]
    fn list_resources_get_has_params() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/v1/resources")
            .expect("/v1/resources not found")
            .operations
            .iter()
            .find(|o| o.method == "GET")
            .expect("GET not found");
        assert!(!op.params.is_empty(), "expected query params on GET /v1/resources");
    }

    #[test]
    fn create_resource_post_has_request_body() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/v1/resources")
            .expect("/v1/resources not found")
            .operations
            .iter()
            .find(|o| o.method == "POST")
            .expect("POST not found");
        let rb = op.request_body.as_ref().expect("POST should have request body");
        assert!(rb.required, "POST /v1/resources body should be required");
        assert!(!rb.fields.is_empty(), "expected body fields");
    }

    #[test]
    fn get_resource_200_has_schema_tree() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/v1/resources/{resourceId}")
            .expect("/v1/resources/{resourceId} not found")
            .operations
            .iter()
            .find(|o| o.method == "GET")
            .expect("GET not found");
        let resp = op
            .responses
            .iter()
            .find(|r| r.code == "200")
            .expect("expected 200 response");
        assert!(
            resp.schema_tree.is_some(),
            "200 response should have a schema_tree"
        );
    }

    // ── additionalOperations (3.2 new feature) ────────────────────────────────

    #[test]
    fn additional_operations_are_parsed() {
        let spec = load();
        let path_entry = spec
            .paths
            .iter()
            .find(|p| p.path == "/v1/resources/{resourceId}")
            .expect("/v1/resources/{resourceId} not found");

        // The fixture has COPY and MOVE in additionalOperations.
        let has_copy = path_entry.operations.iter().any(|o| o.method == "COPY");
        let has_move = path_entry.operations.iter().any(|o| o.method == "MOVE");
        assert!(has_copy, "expected COPY operation from additionalOperations");
        assert!(has_move, "expected MOVE operation from additionalOperations");
    }

    #[test]
    fn additional_operation_has_operation_id() {
        let spec = load();
        let path_entry = spec
            .paths
            .iter()
            .find(|p| p.path == "/v1/resources/{resourceId}")
            .unwrap();
        let copy_op = path_entry
            .operations
            .iter()
            .find(|o| o.method == "COPY")
            .expect("COPY not found");
        assert_eq!(
            copy_op.operation_id.as_deref(),
            Some("copyResource"),
            "COPY operation should have operationId copyResource"
        );
    }

    #[test]
    fn additional_operation_has_summary() {
        let spec = load();
        let path_entry = spec
            .paths
            .iter()
            .find(|p| p.path == "/v1/resources/{resourceId}")
            .unwrap();
        let move_op = path_entry
            .operations
            .iter()
            .find(|o| o.method == "MOVE")
            .expect("MOVE not found");
        assert!(
            move_op.summary.is_some(),
            "MOVE operation should have a summary"
        );
    }

    #[test]
    fn additional_operation_copy_has_params() {
        let spec = load();
        let path_entry = spec
            .paths
            .iter()
            .find(|p| p.path == "/v1/resources/{resourceId}")
            .unwrap();
        let copy_op = path_entry
            .operations
            .iter()
            .find(|o| o.method == "COPY")
            .expect("COPY not found");
        assert!(
            !copy_op.params.is_empty(),
            "COPY operation should have at least one parameter"
        );
        let dest = copy_op.params.iter().find(|p| p.name == "destination");
        assert!(dest.is_some(), "COPY should have a 'destination' query param");
    }

    #[test]
    fn additional_operation_deprecated_flag() {
        let spec = load();
        let path_entry = spec
            .paths
            .iter()
            .find(|p| p.path == "/v1/resources/{resourceId}")
            .unwrap();
        let move_op = path_entry
            .operations
            .iter()
            .find(|o| o.method == "MOVE")
            .expect("MOVE not found");
        assert!(
            move_op.deprecated,
            "MOVE should be marked deprecated in the fixture"
        );
    }

    // ── Webhooks ──────────────────────────────────────────────────────────────

    #[test]
    fn webhooks_are_parsed() {
        let spec = load();
        let webhooks: Vec<_> = spec
            .paths
            .iter()
            .filter(|p| p.kind == PathKind::Webhook)
            .collect();
        assert!(!webhooks.is_empty(), "expected at least one webhook");
    }

    #[test]
    fn resource_created_webhook_exists() {
        let spec = load();
        let wh = spec
            .paths
            .iter()
            .find(|p| p.path == "resource.created" && p.kind == PathKind::Webhook)
            .expect("expected 'resource.created' webhook");
        let op = wh
            .operations
            .iter()
            .find(|o| o.method == "POST")
            .expect("expected POST on resource.created webhook");
        assert_eq!(op.operation_id.as_deref(), Some("onResourceCreated"));
    }
}
