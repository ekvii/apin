use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use indexmap::IndexMap;
use openapiv3::{Components, OpenAPI, Parameter, ReferenceOr, Schema, SchemaKind, Type};

use crate::universe::{
    BodyField, Operation, Param, PathEntry, RequestBody, SchemaKindHint, SchemaNode, Spec,
};

/// Parse an OpenAPI 3.0.x document from a YAML/JSON string and convert it
/// into our internal [`Spec`] representation.
pub fn parse(file_path: String, content: String) -> Result<Spec> {
    // Parse the raw YAML value tree once — used later for inline $ref resolution.
    let raw_value: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse '{}' as YAML", file_path))?;

    let api: OpenAPI = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse '{}' as OpenAPI 3.0.x", file_path))?;

    let components = api.components.as_ref();

    // Pre-extract the components/schemas subtree from the raw value for $ref resolution.
    let schemas_value: Option<&serde_yaml::Value> =
        raw_value.get("components").and_then(|c| c.get("schemas"));

    let paths = api
        .paths
        .paths
        .iter()
        .map(|(path_str, ref_or_item)| {
            let operations = match ref_or_item {
                ReferenceOr::Item(item) => item
                    .iter()
                    .map(|(method, op)| {
                        let params = op.parameters.iter().filter_map(resolve_param).collect();

                        let responses = op
                            .responses
                            .responses
                            .iter()
                            .map(|(sc, ref_or_resp)| {
                                let desc = match ref_or_resp {
                                    ReferenceOr::Item(r) => {
                                        let d = r.description.trim().to_string();
                                        if d.is_empty() {
                                            None
                                        } else {
                                            Some(d)
                                        }
                                    }
                                    ReferenceOr::Reference { .. } => None,
                                };
                                (sc.to_string(), desc)
                            })
                            .collect();

                        let request_body = op
                            .request_body
                            .as_ref()
                            .map(|rb| resolve_request_body(rb, components, schemas_value));

                        Operation {
                            method: method.to_uppercase(),
                            summary: op.summary.clone(),
                            description: op.description.clone(),
                            operation_id: op.operation_id.clone(),
                            params,
                            request_body,
                            responses,
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

// ─── Request body resolution ─────────────────────────────────────────────────

fn resolve_request_body(
    rb: &ReferenceOr<openapiv3::RequestBody>,
    components: Option<&Components>,
    schemas_value: Option<&serde_yaml::Value>,
) -> RequestBody {
    let body = match rb {
        ReferenceOr::Item(b) => b,
        ReferenceOr::Reference { reference } => {
            // e.g. "#/components/requestBodies/Foo" — rare, skip for now
            return RequestBody {
                description: Some(format!("$ref: {}", reference)),
                fields: vec![],
                schema_tree: None,
            };
        }
    };

    // Pick the application/json media type schema, falling back to the first.
    let schema_ref = body
        .content
        .get("application/json")
        .or_else(|| body.content.values().next())
        .and_then(|mt| mt.schema.as_ref());

    let fields = schema_ref
        .map(|sr| extract_fields(sr, components))
        .unwrap_or_default();

    let schema_tree = schema_ref.and_then(|sr| build_schema_tree(sr, schemas_value));

    RequestBody {
        description: body.description.clone(),
        fields,
        schema_tree,
    }
}

/// Resolve the schema ref to a cleaned `serde_yaml::Value`, then walk it
/// recursively to build a `SchemaNode` tree.
fn build_schema_tree(
    schema_ref: &ReferenceOr<Schema>,
    schemas_value: Option<&serde_yaml::Value>,
) -> Option<SchemaNode> {
    // Determine the root label from a $ref name if available.
    let (root_label, base_value) = match schema_ref {
        ReferenceOr::Reference { reference } => {
            let name = reference.strip_prefix("#/components/schemas/")?;
            let val = schemas_value?.get(name)?.clone();
            (name.to_string(), val)
        }
        ReferenceOr::Item(schema) => {
            let val = serde_yaml::to_value(schema).ok()?;
            ("body".to_string(), val)
        }
    };

    let resolved = resolve_refs_in_value(base_value, schemas_value, &mut HashSet::new());
    let clean = strip_noise_keys(resolved);
    Some(value_to_schema_node(root_label, &clean, &HashSet::new()))
}

// ─── serde_yaml::Value helpers ───────────────────────────────────────────────

/// Recursively walk a `serde_yaml::Value` and replace every mapping that
/// looks like `{"$ref": "#/components/schemas/Foo"}` with the actual schema
/// value found in `schemas`.  A `visited` set prevents infinite recursion on
/// cyclic schemas.
fn resolve_refs_in_value(
    val: serde_yaml::Value,
    schemas: Option<&serde_yaml::Value>,
    visited: &mut HashSet<String>,
) -> serde_yaml::Value {
    match val {
        serde_yaml::Value::Mapping(mut map) => {
            // Check if this mapping is a bare $ref node.
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
                // Cyclic or not found: leave as-is but drop the ugly $ref string
                map.remove(&ref_key);
                let name_key = serde_yaml::Value::String("type".to_string());
                map.insert(name_key, serde_yaml::Value::String(name.to_string()));
                return serde_yaml::Value::Mapping(map);
            }
            // Not a $ref — recurse into all values.
            let resolved_map: serde_yaml::Mapping = map
                .into_iter()
                .map(|(k, v)| (k, resolve_refs_in_value(v, schemas, visited)))
                .collect();
            serde_yaml::Value::Mapping(resolved_map)
        }
        serde_yaml::Value::Sequence(seq) => {
            let resolved_seq = seq
                .into_iter()
                .map(|v| resolve_refs_in_value(v, schemas, visited))
                .collect();
            serde_yaml::Value::Sequence(resolved_seq)
        }
        other => other,
    }
}

/// Remove keys that add visual noise without helping the reader understand
/// the schema structure.
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

/// Walk a cleaned `serde_yaml::Value` and build a `SchemaNode` tree.
///
/// `label` is the display name of this node (property name, "items",
/// "allOf[0]", etc.).  `required_set` contains the names of required
/// properties of the *parent* object (used only for object property nodes).
fn value_to_schema_node(
    label: String,
    val: &serde_yaml::Value,
    required_set: &HashSet<String>,
) -> SchemaNode {
    let required = required_set.contains(&label);

    let mapping = match val {
        serde_yaml::Value::Mapping(m) => m,
        _ => {
            // Scalar or sequence at top level — treat as unknown primitive.
            return SchemaNode {
                label,
                kind: SchemaKindHint::Unknown,
                description: None,
                required,
                children: vec![],
            };
        }
    };

    // Extract description (first line only).
    let description: Option<String> = mapping
        .get(serde_yaml::Value::String("description".into()))
        .and_then(|v| match v {
            serde_yaml::Value::String(s) => Some(s.lines().next().unwrap_or("").trim().to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty());

    // Helper to get a string key from the mapping.
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
        // Collect required set for children.
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

    // ── Primitive (has a `type` key) ──────────────────────────────────────────
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

/// Walk a `ReferenceOr<Schema>`, resolving `$ref`s against components,
/// and return the top-level object properties as `BodyField`s.
///
/// Handles common patterns:
/// - Plain object: `type: object, properties: {...}`
/// - Array wrapper (FastAPI): `type: array, items: $ref` — unwraps to the
///   item schema and shows its fields (prefixed with `[]`)
/// - AnySchema (when `additionalProperties` triggers fallback in openapiv3)
/// - allOf: merges from first sub-schema
fn extract_fields(
    schema_ref: &ReferenceOr<Schema>,
    components: Option<&Components>,
) -> Vec<BodyField> {
    let schema = resolve_schema(schema_ref, components);
    let schema = match schema {
        Some(s) => s,
        None => return vec![],
    };

    match &schema.schema_kind {
        // ── Typed array: drill into items ────────────────────────────────────
        SchemaKind::Type(Type::Array(arr)) => {
            if let Some(items_ref) = &arr.items {
                let unboxed: ReferenceOr<Schema> = unbox_ref(items_ref);
                return extract_fields(&unboxed, components)
                    .into_iter()
                    .map(|mut f| {
                        f.name = format!("[]{}", f.name);
                        f
                    })
                    .collect();
            }
            vec![]
        }

        // ── Typed object: extract properties ─────────────────────────────────
        SchemaKind::Type(Type::Object(o)) => {
            extract_object_fields(&o.properties, &o.required, components)
        }

        // ── allOf: treat first sub-schema as the object ───────────────────────
        SchemaKind::AllOf { all_of } => all_of
            .first()
            .map(|s| extract_fields(s, components))
            .unwrap_or_default(),

        // ── AnySchema fallback (openapiv3 uses this when extra keywords like  ─
        //    additionalProperties prevent clean Type discrimination)           ─
        SchemaKind::Any(any) => {
            match any.typ.as_deref() {
                Some("array") => {
                    if let Some(items_ref) = &any.items {
                        let unboxed: ReferenceOr<Schema> = unbox_ref(items_ref);
                        return extract_fields(&unboxed, components)
                            .into_iter()
                            .map(|mut f| {
                                f.name = format!("[]{}", f.name);
                                f
                            })
                            .collect();
                    }
                    vec![]
                }
                _ => {
                    // Treat as object if it has properties.
                    if !any.properties.is_empty() {
                        extract_object_fields(&any.properties, &any.required, components)
                    } else {
                        vec![]
                    }
                }
            }
        }

        _ => vec![],
    }
}

/// Shared helper: build `BodyField`s from an IndexMap of properties.
fn extract_object_fields(
    properties: &IndexMap<String, ReferenceOr<Box<Schema>>>,
    _required: &[String],
    _components: Option<&Components>,
) -> Vec<BodyField> {
    properties
        .keys()
        .map(|name| BodyField { name: name.clone() })
        .collect()
}

/// Unbox a `ReferenceOr<Box<Schema>>` into a `ReferenceOr<Schema>`.
fn unbox_ref(r: &ReferenceOr<Box<Schema>>) -> ReferenceOr<Schema> {
    match r {
        ReferenceOr::Item(boxed) => ReferenceOr::Item(*boxed.clone()),
        ReferenceOr::Reference { reference } => ReferenceOr::Reference {
            reference: reference.clone(),
        },
    }
}

/// Resolve a `ReferenceOr<Schema>` — follow a single `$ref` into
/// `components/schemas` if needed.
fn resolve_schema<'a>(
    r: &'a ReferenceOr<Schema>,
    components: Option<&'a Components>,
) -> Option<&'a Schema> {
    match r {
        ReferenceOr::Item(s) => Some(s),
        ReferenceOr::Reference { reference } => {
            // "#/components/schemas/Foo"
            let name = reference.strip_prefix("#/components/schemas/")?;
            let schema_ref = components?.schemas.get(name)?;
            match schema_ref {
                ReferenceOr::Item(s) => Some(s),
                ReferenceOr::Reference { .. } => None, // don't recurse further
            }
        }
    }
}

// ─── Parameter resolution ────────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/openapi30_fixture.yaml"
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
