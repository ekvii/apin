/// Parse a Swagger 2.0 document from a YAML/JSON string.
///
/// Swagger 2.0 differs from OpenAPI 3.x in several key ways that require
/// special handling:
///
/// - The version discriminator is `swagger: "2.0"`, not `openapi:`.
/// - Shared schemas live under `definitions:`, not `components/schemas:`.
/// - Shared parameters/responses live at the top level, not under `components`.
/// - Parameters with `in: body` carry the request body schema directly —
///   there is no separate `requestBody` object.
/// - `in: formData` parameters are also body-style and collected into the
///   request body.
/// - Responses have a `schema:` key directly on the response object, not
///   inside a `content` map.
/// - `$ref` paths use `#/definitions/Foo` instead of `#/components/schemas/Foo`.
/// - No webhooks.
///
/// Because no typed Swagger 2.0 crate is in the dependency tree, the entire
/// parser works against the raw `serde_yaml::Value` tree.
use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::spec::{
    BodyField, Operation, Param, PathEntry, PathKind, RequestBody, Response, SchemaKindHint,
    SchemaNode, Spec,
};

/// Parse a Swagger 2.0 document.
pub fn parse(file_path: String, content: String) -> Result<Spec> {
    let root: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse '{}' as YAML/JSON", file_path))?;

    let str_field = |key: &str| -> String {
        root.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    let swagger_version = str_field("swagger");
    let title = root
        .get("info")
        .and_then(|i| i.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let version = root
        .get("info")
        .and_then(|i| i.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = root
        .get("info")
        .and_then(|i| i.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Swagger 2.0 shared schemas are under `definitions:`.
    let definitions: Option<&serde_yaml::Value> = root.get("definitions");

    // Top-level shared parameters (resolved by name when a $ref appears).
    let global_params: Option<&serde_yaml::Value> = root.get("parameters");

    let paths = root
        .get("paths")
        .and_then(|p| p.as_mapping())
        .map(|paths_map| {
            paths_map
                .iter()
                .map(|(path_key, path_item)| {
                    convert_path_entry(path_key, path_item, definitions, global_params)
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Spec {
        file_path: PathBuf::from(file_path),
        openapi_version: swagger_version,
        title,
        version,
        description,
        paths,
    })
}

// ─── Path entry conversion ────────────────────────────────────────────────────

const METHODS: &[&str] = &["get", "put", "post", "delete", "options", "head", "patch"];

fn convert_path_entry(
    path_key: &serde_yaml::Value,
    path_item: &serde_yaml::Value,
    definitions: Option<&serde_yaml::Value>,
    global_params: Option<&serde_yaml::Value>,
) -> PathEntry {
    let path = match path_key {
        serde_yaml::Value::String(s) => s.clone(),
        _ => "?".to_string(),
    };

    // Path-level parameters are inherited by all operations unless overridden.
    let path_level_params: Vec<&serde_yaml::Value> = path_item
        .get("parameters")
        .and_then(|v| v.as_sequence())
        .map(|s| s.iter().collect())
        .unwrap_or_default();

    let operations = METHODS
        .iter()
        .filter_map(|method| {
            let op_val = path_item.get(*method)?;
            // Merge path-level params with op-level params (op wins on name+location clash).
            let op_params: Vec<&serde_yaml::Value> = op_val
                .get("parameters")
                .and_then(|v| v.as_sequence())
                .map(|s| s.iter().collect())
                .unwrap_or_default();

            let merged_params = merge_params(&path_level_params, &op_params, global_params);

            Some(convert_operation(
                method.to_uppercase(),
                op_val,
                &merged_params,
                definitions,
            ))
        })
        .collect();

    PathEntry {
        path,
        kind: PathKind::Path,
        operations,
    }
}

/// Merge path-level and operation-level parameter lists.
/// Operation params take precedence (by name + location) over path-level ones.
/// Both lists may contain `$ref` entries pointing to `#/parameters/{name}`.
fn merge_params<'a>(
    path_params: &[&'a serde_yaml::Value],
    op_params: &[&'a serde_yaml::Value],
    global_params: Option<&'a serde_yaml::Value>,
) -> Vec<serde_yaml::Value> {
    // Resolve a parameter (possibly a $ref) to an owned Value.
    let resolve = |v: &&serde_yaml::Value| -> Option<serde_yaml::Value> {
        if let Some(ref_str) = v.get("$ref").and_then(|r| r.as_str()) {
            // #/parameters/ParamName
            let name = ref_str.strip_prefix("#/parameters/")?;
            global_params?.get(name).cloned()
        } else {
            Some((*v).clone())
        }
    };

    // Start with path-level params.
    let mut result: Vec<serde_yaml::Value> = path_params.iter().filter_map(resolve).collect();

    // Op-level params: override any path-level param with same name+in.
    for p in op_params {
        let Some(resolved) = resolve(p) else { continue };
        let name = resolved.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let loc = resolved.get("in").and_then(|v| v.as_str()).unwrap_or("");
        // Remove any existing entry with same name+in.
        result.retain(|existing| {
            let en = existing.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let el = existing.get("in").and_then(|v| v.as_str()).unwrap_or("");
            !(en == name && el == loc)
        });
        result.push(resolved);
    }

    result
}

// ─── Operation conversion ─────────────────────────────────────────────────────

fn convert_operation(
    method: String,
    op: &serde_yaml::Value,
    params: &[serde_yaml::Value],
    definitions: Option<&serde_yaml::Value>,
) -> Operation {
    let str_field =
        |key: &str| -> Option<String> { op.get(key).and_then(|v| v.as_str()).map(str::to_string) };

    let summary = str_field("summary");
    let description = str_field("description");
    let operation_id = str_field("operationId");
    let deprecated = op
        .get("deprecated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let tags: Vec<String> = op
        .get("tags")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // Separate non-body params from body/formData params.
    let mut non_body: Vec<Param> = vec![];
    let mut body_params: Vec<&serde_yaml::Value> = vec![];
    let mut form_params: Vec<&serde_yaml::Value> = vec![];

    for p in params {
        let location = p.get("in").and_then(|v| v.as_str()).unwrap_or("");
        match location {
            "body" => body_params.push(p),
            "formData" => form_params.push(p),
            _ => {
                if let Some(param) = convert_non_body_param(p) {
                    non_body.push(param);
                }
            }
        }
    }

    // Build request body from the `body` parameter (at most one in Swagger 2.0)
    // or from formData parameters.
    let request_body = if let Some(body_param) = body_params.first() {
        Some(convert_body_param(body_param, definitions))
    } else if !form_params.is_empty() {
        Some(convert_form_params(form_params))
    } else {
        None
    };

    // Build responses.
    let responses: Vec<Response> = op
        .get("responses")
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

                    // In Swagger 2.0 the response schema is a direct `schema:` key.
                    let schema_val = resp_val.get("schema");
                    let schema_tree = schema_val.map(|sv| {
                        let resolved =
                            resolve_refs_in_value(sv.clone(), definitions, &mut HashSet::new());
                        let clean = strip_noise_keys(resolved);
                        value_to_schema_node("body".to_string(), &clean, &HashSet::new())
                    });

                    Response {
                        code,
                        description: desc,
                        schema_tree,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Operation {
        method,
        summary,
        description,
        operation_id,
        tags,
        deprecated,
        params: non_body,
        request_body,
        responses,
    }
}

// ─── Parameter conversion ─────────────────────────────────────────────────────

fn convert_non_body_param(p: &serde_yaml::Value) -> Option<Param> {
    let name = p.get("name").and_then(|v| v.as_str())?.to_string();
    let location = p.get("in").and_then(|v| v.as_str())?.to_string();
    let required = p.get("required").and_then(|v| v.as_bool()).unwrap_or(false);
    let description = p
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let deprecated = p
        .get("deprecated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // In Swagger 2.0, primitive params have `type:` directly on the parameter
    // (no nested `schema:` object for non-body params).
    // However, some tools emit a `schema:` block anyway — handle both.
    let schema_type = p
        .get("type")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            p.get("schema")
                .and_then(|s| s.get("type"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });

    Some(Param {
        name,
        location,
        required,
        description,
        schema_type,
        deprecated,
    })
}

// ─── Request body helpers ─────────────────────────────────────────────────────

/// Convert a Swagger 2.0 `in: body` parameter into a `RequestBody`.
fn convert_body_param(
    param: &serde_yaml::Value,
    definitions: Option<&serde_yaml::Value>,
) -> RequestBody {
    let description = param
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let required = param
        .get("required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let schema_val = param.get("schema");

    let fields: Vec<BodyField> = schema_val
        .map(|sv| extract_fields_from_value(sv, definitions))
        .unwrap_or_default();

    let schema_tree = schema_val.map(|sv| {
        let resolved = resolve_refs_in_value(sv.clone(), definitions, &mut HashSet::new());
        let clean = strip_noise_keys(resolved);
        value_to_schema_node("body".to_string(), &clean, &HashSet::new())
    });

    RequestBody {
        description,
        required,
        fields,
        schema_tree,
    }
}

/// Convert a set of `in: formData` parameters into a synthetic `RequestBody`.
/// Each formData parameter becomes a top-level field; no deep schema tree.
fn convert_form_params(params: Vec<&serde_yaml::Value>) -> RequestBody {
    let fields: Vec<BodyField> = params
        .iter()
        .filter_map(|p| {
            p.get("name")
                .and_then(|v| v.as_str())
                .map(|name| BodyField {
                    name: name.to_string(),
                })
        })
        .collect();

    RequestBody {
        description: None,
        required: params
            .iter()
            .any(|p| p.get("required").and_then(|v| v.as_bool()).unwrap_or(false)),
        fields,
        schema_tree: None,
    }
}

/// Extract top-level property names from a Swagger 2.0 schema value.
fn extract_fields_from_value(
    schema: &serde_yaml::Value,
    definitions: Option<&serde_yaml::Value>,
) -> Vec<BodyField> {
    // Resolve $ref first.
    let resolved_owned;
    let effective = if let Some(ref_str) = schema.get("$ref").and_then(|v| v.as_str()) {
        if let Some(name) = ref_str.strip_prefix("#/definitions/") {
            if let Some(target) = definitions.and_then(|d| d.get(name)) {
                resolved_owned = target.clone();
                &resolved_owned
            } else {
                return vec![];
            }
        } else {
            return vec![];
        }
    } else {
        schema
    };

    effective
        .get("properties")
        .and_then(|p| p.as_mapping())
        .map(|m| {
            m.keys()
                .filter_map(|k| {
                    k.as_str().map(|s| BodyField {
                        name: s.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ─── serde_yaml::Value helpers ────────────────────────────────────────────────

/// Recursively replace `{"$ref": "#/definitions/X"}` with the target schema.
/// Uses `definitions` (Swagger 2.0 terminology) as the schema store.
fn resolve_refs_in_value(
    val: serde_yaml::Value,
    definitions: Option<&serde_yaml::Value>,
    visited: &mut HashSet<String>,
) -> serde_yaml::Value {
    match val {
        serde_yaml::Value::Mapping(mut map) => {
            let ref_key = serde_yaml::Value::String("$ref".to_string());
            if let Some(serde_yaml::Value::String(ref ref_str)) = map.get(&ref_key).cloned()
                && let Some(name) = ref_str.strip_prefix("#/definitions/")
            {
                if !visited.contains(name)
                    && let Some(target) = definitions.and_then(|d| d.get(name))
                {
                    visited.insert(name.to_string());
                    let resolved =
                        resolve_refs_in_value(target.clone(), definitions, visited);
                    visited.remove(name);
                    return resolved;
                }
                // Cycle or missing — leave a type stub.
                map.remove(&ref_key);
                map.insert(
                    serde_yaml::Value::String("type".to_string()),
                    serde_yaml::Value::String(name.to_string()),
                );
                return serde_yaml::Value::Mapping(map);
            }
            serde_yaml::Value::Mapping(
                map.into_iter()
                    .map(|(k, v)| (k, resolve_refs_in_value(v, definitions, visited)))
                    .collect(),
            )
        }
        serde_yaml::Value::Sequence(seq) => serde_yaml::Value::Sequence(
            seq.into_iter()
                .map(|v| resolve_refs_in_value(v, definitions, visited))
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
        // Swagger 2.0-specific noise
        "collectionFormat",
        "discriminator",
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/swagger20_fixture.yaml"
    );

    fn load() -> Spec {
        let content = std::fs::read_to_string(FIXTURE).expect("fixture not found");
        parse("fixture.yaml".to_string(), content).expect("parse failed")
    }

    // ── Basic smoke ───────────────────────────────────────────────────────────

    #[test]
    fn spec_version_is_2_0() {
        let spec = load();
        assert_eq!(spec.openapi_version, "2.0");
    }

    #[test]
    fn title_and_version_parsed() {
        let spec = load();
        assert!(!spec.title.is_empty(), "title should not be empty");
        assert!(!spec.version.is_empty(), "version should not be empty");
    }

    #[test]
    fn paths_are_parsed() {
        let spec = load();
        assert!(!spec.paths.is_empty(), "expected at least one path");
    }

    #[test]
    fn all_paths_are_path_kind() {
        let spec = load();
        assert!(
            spec.paths.iter().all(|p| p.kind == PathKind::Path),
            "Swagger 2.0 has no webhooks — all entries should be PathKind::Path"
        );
    }

    // ── Operations ────────────────────────────────────────────────────────────

    #[test]
    fn list_pets_get_exists() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets")
            .expect("/pets not found")
            .operations
            .iter()
            .find(|o| o.method == "GET")
            .expect("GET /pets not found");
        assert_eq!(op.operation_id.as_deref(), Some("listPets"));
    }

    #[test]
    fn create_pet_post_has_body() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets")
            .expect("/pets not found")
            .operations
            .iter()
            .find(|o| o.method == "POST")
            .expect("POST /pets not found");
        let rb = op
            .request_body
            .as_ref()
            .expect("POST /pets should have request body");
        assert!(rb.required, "POST /pets body should be required");
        assert!(!rb.fields.is_empty(), "expected body fields on POST /pets");
    }

    #[test]
    fn create_pet_body_has_name_field() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets")
            .unwrap()
            .operations
            .iter()
            .find(|o| o.method == "POST")
            .unwrap();
        let rb = op.request_body.as_ref().unwrap();
        assert!(
            rb.fields.iter().any(|f| f.name == "name"),
            "expected 'name' field in POST /pets body, got: {:?}",
            rb.fields.iter().map(|f| &f.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn get_pet_by_id_has_path_param() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets/{petId}")
            .expect("/pets/{petId} not found")
            .operations
            .iter()
            .find(|o| o.method == "GET")
            .expect("GET /pets/{petId} not found");
        let param = op
            .params
            .iter()
            .find(|p| p.name == "petId" && p.location == "path")
            .expect("expected path param 'petId'");
        assert!(param.required, "path params should be required");
    }

    // ── Parameter types ───────────────────────────────────────────────────────

    #[test]
    fn list_pets_limit_param_is_integer() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets")
            .unwrap()
            .operations
            .iter()
            .find(|o| o.method == "GET")
            .unwrap();
        let limit = op
            .params
            .iter()
            .find(|p| p.name == "limit")
            .expect("expected 'limit' query param");
        assert_eq!(
            limit.schema_type.as_deref(),
            Some("integer"),
            "limit should have type integer"
        );
        assert!(!limit.required, "limit should be optional");
    }

    #[test]
    fn list_pets_has_tags_tag() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets")
            .unwrap()
            .operations
            .iter()
            .find(|o| o.method == "GET")
            .unwrap();
        assert!(
            op.tags.iter().any(|t| t == "pets"),
            "GET /pets should have tag 'pets', got: {:?}",
            op.tags
        );
    }

    // ── Response schema tree ──────────────────────────────────────────────────

    #[test]
    fn list_pets_200_has_schema_tree() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets")
            .unwrap()
            .operations
            .iter()
            .find(|o| o.method == "GET")
            .unwrap();
        let resp = op
            .responses
            .iter()
            .find(|r| r.code == "200")
            .expect("expected 200 response");
        assert!(
            resp.schema_tree.is_some(),
            "GET /pets 200 response should have schema_tree"
        );
    }

    #[test]
    fn get_pet_200_schema_tree_has_children() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets/{petId}")
            .unwrap()
            .operations
            .iter()
            .find(|o| o.method == "GET")
            .unwrap();
        let resp = op.responses.iter().find(|r| r.code == "200").unwrap();
        let tree = resp.schema_tree.as_ref().expect("should have schema_tree");
        assert!(
            !tree.children.is_empty(),
            "Pet schema_tree should have children"
        );
        assert!(
            tree.children.iter().any(|c| c.label == "id"),
            "expected 'id' in Pet schema_tree"
        );
        assert!(
            tree.children.iter().any(|c| c.label == "name"),
            "expected 'name' in Pet schema_tree"
        );
    }

    #[test]
    fn delete_pet_204_has_no_schema_tree() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets/{petId}")
            .expect("/pets/{petId} not found")
            .operations
            .iter()
            .find(|o| o.method == "DELETE")
            .expect("DELETE not found");
        let resp = op
            .responses
            .iter()
            .find(|r| r.code == "204")
            .expect("expected 204 response");
        assert!(
            resp.schema_tree.is_none(),
            "204 No Content should have no schema_tree"
        );
    }

    // ── Request body schema tree ──────────────────────────────────────────────

    #[test]
    fn create_pet_body_schema_tree_populated() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets")
            .unwrap()
            .operations
            .iter()
            .find(|o| o.method == "POST")
            .unwrap();
        let rb = op.request_body.as_ref().unwrap();
        let tree = rb
            .schema_tree
            .as_ref()
            .expect("body should have schema_tree");
        assert!(
            !tree.children.is_empty(),
            "NewPet schema_tree should have children"
        );
    }

    // ── Path-level parameter inheritance ─────────────────────────────────────

    #[test]
    fn path_level_param_inherited_by_operations() {
        let spec = load();
        // /stores/{storeId}/items has a path-level `storeId` param.
        let path_entry = spec
            .paths
            .iter()
            .find(|p| p.path == "/stores/{storeId}/items")
            .expect("/stores/{storeId}/items not found");
        for op in &path_entry.operations {
            assert!(
                op.params.iter().any(|p| p.name == "storeId"),
                "{} /stores/{{storeId}}/items should inherit storeId path param",
                op.method
            );
        }
    }

    // ── formData request body ─────────────────────────────────────────────────

    #[test]
    fn upload_file_has_form_body() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets/{petId}/photo")
            .expect("/pets/{petId}/photo not found")
            .operations
            .iter()
            .find(|o| o.method == "POST")
            .expect("POST not found");
        let rb = op.request_body.as_ref().expect("should have request body");
        assert!(
            rb.fields.iter().any(|f| f.name == "file"),
            "formData body should include 'file' field"
        );
    }

    // ── Deprecated ────────────────────────────────────────────────────────────

    #[test]
    fn deprecated_operation_is_flagged() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets/{petId}")
            .unwrap()
            .operations
            .iter()
            .find(|o| o.method == "PUT")
            .expect("PUT /pets/{petId} not found");
        assert!(op.deprecated, "PUT /pets/{{petId}} should be deprecated");
    }

    #[test]
    fn non_deprecated_operation_not_flagged() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets")
            .unwrap()
            .operations
            .iter()
            .find(|o| o.method == "GET")
            .unwrap();
        assert!(!op.deprecated, "GET /pets should not be deprecated");
    }

    // ── allOf composition ─────────────────────────────────────────────────────

    #[test]
    fn error_response_schema_tree_is_object() {
        let spec = load();
        let op = spec
            .paths
            .iter()
            .find(|p| p.path == "/pets")
            .unwrap()
            .operations
            .iter()
            .find(|o| o.method == "GET")
            .unwrap();
        let resp = op
            .responses
            .iter()
            .find(|r| r.code == "default")
            .expect("expected default error response");
        let tree = resp
            .schema_tree
            .as_ref()
            .expect("error response should have schema_tree");
        assert!(
            !tree.children.is_empty(),
            "Error schema_tree should have children"
        );
    }
}
