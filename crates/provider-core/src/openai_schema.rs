use serde_json::Value;
use std::collections::HashSet;

/// Normalize a tool-parameter schema for the OpenAI function-parameters subset.
///
/// One construct OpenAI rejects fails the entire tool catalog rather than the
/// one tool, which is why this class of bug (#446, #495, #543, #687, #711, #713,
/// #754) has recurred: each fix appended a keyword to a deny-list, so the next
/// unlisted keyword from the next MCP server was the next outage.
///
/// The subset is now an allow-list in `horon-agent-schema-dialect`, shared with every
/// other provider, so a construct nobody has seen yet is dropped rather than
/// forwarded. Strict-mode eligibility and normalization stay here: they are
/// OpenAI-specific and have no dialect equivalent.
pub fn openai_compatible_schema(schema: &Value) -> Value {
    horon_agent_schema_dialect::normalize(schema, &horon_agent_schema_dialect::registry::OPENAI)
}

/// [`openai_compatible_schema`], also appending the name of every keyword or
/// string format the OpenAI dialect removed to `drops`.
///
/// A tool whose schema had to be narrowed is still sent, so the only record of
/// what the model never saw is this list.
pub fn openai_compatible_schema_reporting(schema: &Value, drops: &mut Vec<String>) -> Value {
    let mut log = horon_agent_schema_dialect::DropLog::enabled(drops);
    horon_agent_schema_dialect::normalize_reporting(schema, &horon_agent_schema_dialect::registry::OPENAI, &mut log)
}

/// Resolve the schema into the shape OpenAI's strict subset can express,
/// before eligibility is judged.
///
/// Three legal JSON Schema constructs are rewritten rather than abandoned,
/// because each one *can* be said in the strict subset:
///
/// * local `$ref`s are inlined (sibling keys win over the resolved target);
/// * a single-branch `allOf` is merged into its parent;
/// * `type: ["string", "null"]` becomes an `anyOf` whose branches each name
///   one type.
///
/// Anything that cannot be resolved — an external `$ref`, a cycle — is left
/// untouched, which makes the eligibility check fail closed and keeps the tool
/// usable without `strict`.
pub fn prepare_strict_schema(schema: &Value) -> Value {
    let inlined = inline_local_refs(schema);
    let collapsed = collapse_single_branch_allof(&inlined);
    expand_union_types(&collapsed)
}

/// Depth cap for `$ref` inlining. Deep chains are rare; a cap keeps a hostile
/// or accidentally recursive schema from expanding exponentially.
const MAX_REF_INLINE_DEPTH: usize = 32;

fn inline_local_refs(root: &Value) -> Value {
    let mut resolving: Vec<String> = Vec::new();
    inline_value(root, root, &mut resolving, 0)
}

fn inline_value(value: &Value, root: &Value, resolving: &mut Vec<String>, depth: usize) -> Value {
    let Value::Object(map) = value else {
        return match value {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| inline_value(item, root, resolving, depth + 1))
                    .collect(),
            ),
            other => other.clone(),
        };
    };
    if depth > MAX_REF_INLINE_DEPTH {
        return value.clone();
    }

    let mut merged = serde_json::Map::new();
    let mut unresolved = false;
    if let Some(Value::String(reference)) = map.get("$ref") {
        let inlineable = reference.starts_with("#/")
            && !resolving.iter().any(|seen| seen == reference)
            && depth < MAX_REF_INLINE_DEPTH;
        if inlineable {
            if let Some(target) = root
                .pointer(&reference[1..])
                .filter(|target| target.is_object())
                .cloned()
            {
                resolving.push(reference.clone());
                let resolved = inline_value(&target, root, resolving, depth + 1);
                resolving.pop();
                if let Value::Object(resolved_map) = resolved {
                    merged = resolved_map;
                } else {
                    unresolved = true;
                }
            } else {
                // Dangling or non-object target: keep the `$ref` so strict
                // eligibility fails closed.
                unresolved = true;
            }
        } else {
            unresolved = true;
        }
    }
    if unresolved {
        merged.insert("$ref".to_string(), map["$ref"].clone());
    }
    // Sibling keys win over the resolved target.
    for (key, child) in map {
        if key == "$ref" {
            continue;
        }
        merged.insert(key.clone(), inline_value(child, root, resolving, depth + 1));
    }
    Value::Object(merged)
}

fn collapse_single_branch_allof(value: &Value) -> Value {
    let Value::Object(map) = value else {
        return match value {
            Value::Array(items) => Value::Array(items.iter().map(collapse_single_branch_allof).collect()),
            other => other.clone(),
        };
    };

    let single_branch = map.get("allOf").and_then(|value| match value {
        Value::Array(items) if items.len() == 1 => items[0].as_object(),
        _ => None,
    });

    let mut out = serde_json::Map::new();
    for (key, child) in map {
        if key == "allOf" && single_branch.is_some() {
            continue;
        }
        out.insert(key.clone(), collapse_single_branch_allof(child));
    }
    if let Some(branch) = single_branch {
        for (key, child) in branch {
            merge_allof_branch(&mut out, key, collapse_single_branch_allof(child));
        }
    }
    Value::Object(out)
}

/// Fold one `allOf` branch into its parent. Both halves of an `allOf` apply, so
/// a collapsed object keeps the union of its declared properties and required
/// names; the parent's own entries win where the two disagree.
fn merge_allof_branch(out: &mut serde_json::Map<String, Value>, key: &str, collapsed: Value) {
    match key {
        "properties" => match (out.get_mut("properties"), collapsed) {
            (Some(Value::Object(existing)), Value::Object(extra)) => {
                for (name, schema) in extra {
                    existing.entry(name).or_insert(schema);
                }
            }
            (None, Value::Object(extra)) => {
                out.insert("properties".to_string(), Value::Object(extra));
            }
            _ => {}
        },
        "required" => match (out.get_mut("required"), collapsed) {
            (Some(Value::Array(existing)), Value::Array(extra)) => {
                for name in extra {
                    if !existing.contains(&name) {
                        existing.push(name);
                    }
                }
            }
            (None, Value::Array(extra)) => {
                out.insert("required".to_string(), Value::Array(extra));
            }
            _ => {}
        },
        _ => {
            out.entry(key.to_string()).or_insert(collapsed);
        }
    }
}

fn expand_union_types(value: &Value) -> Value {
    let Value::Object(map) = value else {
        return match value {
            Value::Array(items) => Value::Array(items.iter().map(expand_union_types).collect()),
            other => other.clone(),
        };
    };

    let mut out = serde_json::Map::new();
    for (key, child) in map {
        let expanded = match key.as_str() {
            "properties" | "$defs" | "definitions" | "patternProperties" => match child {
                Value::Object(children) => Value::Object(
                    children
                        .iter()
                        .map(|(child_key, child_value)| (child_key.clone(), expand_union_types(child_value)))
                        .collect(),
                ),
                _ => expand_union_types(child),
            },
            _ => expand_union_types(child),
        };
        out.insert(key.clone(), expanded);
    }

    let Some(Value::Array(types)) = out.get("type").cloned() else {
        return Value::Object(out);
    };
    let names: Vec<String> = types
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect();
    if names.is_empty() {
        return Value::Object(out);
    }
    if names.len() == 1 {
        out.insert("type".to_string(), Value::String(names[0].clone()));
        return Value::Object(out);
    }

    // Strict mode wants one type per schema object, so a union type becomes a
    // union of schemas. Type-specific keywords stay only on the branch that can
    // use them: a `properties` map on the string branch is meaningless and
    // makes the strict validator reject the catalog.
    let description = out.remove("description");
    let mut variants = Vec::with_capacity(names.len());
    for name in names {
        let mut variant = out.clone();
        variant.insert("type".to_string(), Value::String(name.clone()));
        if name != "object" {
            variant.remove("properties");
            variant.remove("required");
            variant.remove("additionalProperties");
        }
        if name != "array" {
            variant.remove("items");
            variant.remove("prefixItems");
        }
        variants.push(Value::Object(variant));
    }
    let mut wrapper = serde_json::Map::new();
    wrapper.insert("anyOf".to_string(), Value::Array(variants));
    if let Some(description) = description {
        wrapper.insert("description".to_string(), description);
    }
    Value::Object(wrapper)
}

/// Whether a schema declares enough type information for OpenAI strict mode.
/// An "empty" schema like `{"description": "..."}` accepts any instance in JSON
/// Schema, but OpenAI's strict subset requires a concrete type keyword.
fn schema_has_type_info(schema: &Value) -> bool {
    match schema {
        Value::Bool(_) => false,
        Value::Object(map) => [
            "type",
            "enum",
            "const",
            "$ref",
            "anyOf",
            "oneOf",
            "allOf",
            "properties",
            "items",
        ]
        .iter()
        .any(|key| map.contains_key(*key)),
        _ => true,
    }
}

pub fn schema_supports_strict(schema: &Value) -> bool {
    fn check_map(map: &serde_json::Map<String, Value>) -> bool {
        let is_object_typed = match map.get("type") {
            Some(Value::String(t)) => t == "object",
            Some(Value::Array(types)) => types.iter().any(|v| v.as_str() == Some("object")),
            _ => false,
        };
        let has_properties = map
            .get("properties")
            .and_then(|v| v.as_object())
            .map(|props| !props.is_empty())
            .unwrap_or(false);

        if is_object_typed && !has_properties {
            return false;
        }
        if is_object_typed {
            if matches!(map.get("additionalProperties"), Some(Value::Bool(true))) {
                return false;
            }
            if matches!(map.get("additionalProperties"), Some(Value::Object(_))) {
                return false;
            }
        }
        // A declared property that says nothing about its type is legal JSON
        // Schema (an empty schema accepts anything) and Anthropic takes it, but
        // OpenAI's strict validator rejects the whole catalog over it (#713:
        // cua-driver's `set_config.value`, whose type genuinely depends on
        // `key`). Strict eligibility must fail closed here: the schema is still
        // sent, just without `strict: true`, so the tool stays usable.
        if let Some(Value::Object(properties)) = map.get("properties")
            && properties.values().any(|property| !declares_a_type(property))
        {
            return false;
        }

        // The same rule inside a combiner. A branch that only adds a
        // constraint (`{"required": ["memory_id"]}`) or only an enum, with no
        // type of its own, cannot satisfy OpenAI's strict object requirements
        // and fails the whole catalog (#711, observed on a real MCP catalog).
        //
        // Branches are held to a stricter bar than properties: a bare `enum`
        // is enough to describe a property, but a combiner branch must name a
        // `type` (OpenAI reports "schema must have a 'type' key"), which is the
        // same rule the registry-wide sweep already enforces on horon-agent's own
        // tools.
        for combiner in ["anyOf", "oneOf", "allOf"] {
            if let Some(Value::Array(branches)) = map.get(combiner)
                && branches.iter().any(|branch| !branch_names_a_type(branch))
            {
                return false;
            }
        }

        // An array whose `items` are unconstrained: strict mode requires the
        // element shape to be known (#711).
        let is_array_typed = match map.get("type") {
            Some(Value::String(t)) => t == "array",
            Some(Value::Array(types)) => types.iter().any(|v| v.as_str() == Some("array")),
            _ => false,
        };
        if is_array_typed && !map.get("items").is_some_and(declares_a_type) {
            return false;
        }

        // A `$ref` that survived normalization points at a definition the
        // request does not carry (`$defs` is stripped for some paths), so the
        // strict validator cannot resolve it (#711).
        if map.contains_key("$ref") {
            return false;
        }

        // A property carrying no type information at all (e.g. only a
        // `description`) is valid JSON Schema, but strict normalization turns it
        // into an untyped `anyOf` branch that makes OpenAI reject the entire tool
        // catalog. Fall back to non-strict instead. See issue #713.
        if let Some(Value::Object(props)) = map.get("properties")
            && props.values().any(|prop| !schema_has_type_info(prop))
        {
            return false;
        }

        map.values().all(schema_supports_strict)
    }

    match schema {
        Value::Object(map) => check_map(map),
        Value::Array(items) => items.iter().all(schema_supports_strict),
        _ => true,
    }
}

/// Whether a subschema says anything about what it accepts.
///
/// `true` for a boolean schema: `true`/`false` are complete JSON Schemas whose
/// meaning is unambiguous, unlike an object that simply omits `type`.
fn declares_a_type(schema: &Value) -> bool {
    let Some(map) = schema.as_object() else {
        return schema.is_boolean();
    };
    const TYPE_BEARING_KEYWORDS: &[&str] = &[
        "type",
        "enum",
        "const",
        "anyOf",
        "oneOf",
        "allOf",
        "$ref",
        "properties",
        "items",
    ];
    TYPE_BEARING_KEYWORDS.iter().any(|keyword| map.contains_key(*keyword))
}

/// Whether a combiner branch names a concrete `type`.
///
/// Stricter than [`declares_a_type`]: OpenAI accepts a property described only
/// by an `enum`, but rejects a *branch* that does not name a `type`, which is
/// what #711's `mcp__cirqul__create_object` hit with an enum-only `anyOf`
/// branch. A nested combiner is accepted here and validated on recursion.
fn branch_names_a_type(branch: &Value) -> bool {
    let Some(map) = branch.as_object() else {
        return branch.is_boolean();
    };
    map.contains_key("type")
        || map.contains_key("$ref")
        || ["anyOf", "oneOf", "allOf"]
            .iter()
            .any(|combiner| map.contains_key(*combiner))
}

fn schema_is_object_typed(map: &serde_json::Map<String, Value>) -> bool {
    match map.get("type") {
        Some(Value::String(t)) => t == "object",
        Some(Value::Array(types)) => types.iter().any(|v| v.as_str() == Some("object")),
        _ => false,
    }
}

fn schema_contains_null_type(schema: &Value) -> bool {
    schema
        .get("type")
        .and_then(Value::as_str)
        .map(|ty| ty == "null")
        .unwrap_or(false)
}

pub fn make_schema_nullable(schema: Value) -> Value {
    match schema {
        Value::Object(mut map) => {
            if let Some(Value::String(t)) = map.get("type").cloned() {
                if t != "null" {
                    map.insert(
                        "type".to_string(),
                        Value::Array(vec![Value::String(t), Value::String("null".to_string())]),
                    );
                }
                return Value::Object(map);
            }

            if let Some(Value::Array(mut types)) = map.get("type").cloned() {
                if !types.iter().any(|v| v.as_str() == Some("null")) {
                    types.push(Value::String("null".to_string()));
                }
                map.insert("type".to_string(), Value::Array(types));
                return Value::Object(map);
            }

            if let Some(Value::Array(mut any_of)) = map.get("anyOf").cloned() {
                if !any_of.iter().any(schema_contains_null_type) {
                    any_of.push(serde_json::json!({ "type": "null" }));
                }
                map.insert("anyOf".to_string(), Value::Array(any_of));
                return Value::Object(map);
            }

            serde_json::json!({
                "anyOf": [
                    Value::Object(map),
                    { "type": "null" }
                ]
            })
        }
        other => serde_json::json!({
            "anyOf": [
                other,
                { "type": "null" }
            ]
        }),
    }
}

fn normalize_strict_schema_keyword(key: &str, value: &Value) -> Value {
    match key {
        "properties" | "$defs" | "definitions" | "patternProperties" => match value {
            Value::Object(children) => Value::Object(
                children
                    .iter()
                    .map(|(child_key, child_value)| (child_key.clone(), strict_normalize_schema(child_value)))
                    .collect(),
            ),
            _ => strict_normalize_schema(value),
        },
        "allOf" | "anyOf" | "oneOf" | "prefixItems" => match value {
            Value::Array(items) => Value::Array(items.iter().map(strict_normalize_schema).collect()),
            _ => strict_normalize_schema(value),
        },
        _ => strict_normalize_schema(value),
    }
}

fn existing_required_keys(map: &serde_json::Map<String, Value>) -> HashSet<String> {
    map.get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

fn normalize_required_properties(map: &mut serde_json::Map<String, Value>) {
    let Some(property_names) = map.get("properties").and_then(Value::as_object).map(|properties| {
        let mut names: Vec<String> = properties.keys().cloned().collect();
        names.sort();
        names
    }) else {
        return;
    };

    let existing_required = existing_required_keys(map);

    if let Some(Value::Object(properties)) = map.get_mut("properties") {
        for (prop_name, prop_schema) in properties.iter_mut() {
            if !existing_required.contains(prop_name) {
                *prop_schema = make_schema_nullable(prop_schema.clone());
            }
        }
    }

    map.insert(
        "required".to_string(),
        Value::Array(property_names.into_iter().map(Value::String).collect()),
    );
}

/// String `format` values accepted by OpenAI strict structured-output schemas.
/// Unknown formats (e.g. "uri") cause the API to reject the request, so they
/// are stripped during normalization.
const OPENAI_SUPPORTED_STRING_FORMATS: &[&str] = &[
    "date-time",
    "time",
    "date",
    "duration",
    "email",
    "hostname",
    "ipv4",
    "ipv6",
    "uuid",
];

fn is_supported_string_format(value: &Value) -> bool {
    value
        .as_str()
        .map(|s| OPENAI_SUPPORTED_STRING_FORMATS.contains(&s))
        .unwrap_or(false)
}

/// Keywords OpenAI's strict structured-output subset rejects. Every one of them
/// is legal JSON Schema — often the *useful* part of an MCP tool's parameter
/// contract — so dropping them silently would hand the model a weaker schema
/// with no trace of what was lost. Each removal is appended to the subschema's
/// `description` instead, where it still informs the model.
const STRICT_UNSUPPORTED_KEYWORDS: &[&str] = &[
    "pattern",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minItems",
    "maxItems",
    "uniqueItems",
    "multipleOf",
    "$schema",
    "examples",
    "default",
    "title",
    "$comment",
    "if",
    "then",
    "else",
    "not",
    "unevaluatedProperties",
    "unevaluatedItems",
    "patternProperties",
    "propertyNames",
    "contains",
    "minContains",
    "maxContains",
    "dependentRequired",
    "dependentSchemas",
    "contentEncoding",
    "contentMediaType",
    "contentSchema",
    "deprecated",
    "readOnly",
    "writeOnly",
    "minProperties",
    "maxProperties",
];

/// Render a removed constraint compactly for the description note.
fn compact_constraint(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

pub fn strict_normalize_schema(schema: &Value) -> Value {
    fn normalize_map(map: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
        let mut out = serde_json::Map::new();
        for (key, value) in map {
            if key == "format" && !is_supported_string_format(value) {
                continue;
            }
            if STRICT_UNSUPPORTED_KEYWORDS.contains(&key.as_str()) {
                continue;
            }
            let normalized = normalize_strict_schema_keyword(key, value);
            out.insert(key.clone(), normalized);
        }

        // Fixed keyword order, not map order: the note is part of the prompt
        // cache, so it must be byte-identical between requests that carry the
        // same schema.
        let mut notes = String::new();
        for keyword in STRICT_UNSUPPORTED_KEYWORDS {
            if let Some(value) = map.get(*keyword) {
                notes.push_str(&format!(" ({keyword}: {})", compact_constraint(value)));
            }
        }
        if !notes.is_empty() {
            match out.get_mut("description") {
                Some(Value::String(description)) => description.push_str(&notes),
                Some(other) => {
                    let mut description = compact_constraint(other);
                    description.push_str(&notes);
                    out.insert("description".to_string(), Value::String(description));
                }
                None => {
                    out.insert("description".to_string(), Value::String(notes.trim_start().to_string()));
                }
            }
        }

        let is_object_typed = schema_is_object_typed(&out);
        normalize_required_properties(&mut out);

        if is_object_typed || out.contains_key("properties") {
            out.insert("additionalProperties".to_string(), Value::Bool(false));
        }

        out
    }

    match schema {
        Value::Object(map) => Value::Object(normalize_map(map)),
        Value::Array(items) => Value::Array(items.iter().map(strict_normalize_schema).collect()),
        _ => schema.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        make_schema_nullable, openai_compatible_schema, prepare_strict_schema, schema_supports_strict,
        strict_normalize_schema,
    };
    use serde_json::json;

    #[test]
    fn strict_normalize_schema_marks_optional_properties_nullable_and_required() {
        let schema = json!({
            "type": "object",
            "properties": {
                "required_name": { "type": "string" },
                "optional_age": { "type": "integer" }
            },
            "required": ["required_name"]
        });

        let normalized = strict_normalize_schema(&schema);

        assert_eq!(
            normalized,
            json!({
                "type": "object",
                "properties": {
                    "required_name": { "type": "string" },
                    "optional_age": { "type": ["integer", "null"] }
                },
                "required": ["optional_age", "required_name"],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn strict_normalize_schema_strips_unsupported_formats() {
        let schema = json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "format": "uri" },
                "when": { "type": "string", "format": "date-time" },
                "format": { "type": "string" }
            },
            "required": ["url", "when", "format"]
        });

        let normalized = strict_normalize_schema(&schema);

        // Unsupported format "uri" is dropped.
        assert_eq!(normalized["properties"]["url"], json!({ "type": "string" }));
        // Supported format is preserved.
        assert_eq!(
            normalized["properties"]["when"],
            json!({ "type": "string", "format": "date-time" })
        );
        // A property literally named "format" is untouched.
        assert_eq!(normalized["properties"]["format"], json!({ "type": "string" }));
    }

    #[test]
    fn strict_normalize_schema_strips_nested_unsupported_formats() {
        let schema = json!({
            "type": "object",
            "properties": {
                "links": {
                    "type": "array",
                    "items": { "type": "string", "format": "uri" }
                }
            },
            "required": ["links"]
        });

        let normalized = strict_normalize_schema(&schema);
        assert_eq!(normalized["properties"]["links"]["items"], json!({ "type": "string" }));
    }

    #[test]
    fn strict_normalize_schema_preserves_existing_nullability() {
        let schema = json!({
            "anyOf": [
                { "type": "string" },
                { "type": "null" }
            ]
        });

        assert_eq!(
            make_schema_nullable(schema.clone()),
            json!({
                "anyOf": [
                    { "type": "string" },
                    { "type": "null" }
                ]
            })
        );
    }

    #[test]
    fn strict_normalize_schema_recurses_through_nested_object_keywords() {
        let schema = json!({
            "type": "object",
            "properties": {
                "child": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" }
                    }
                }
            }
        });

        let normalized = strict_normalize_schema(&schema);

        assert_eq!(
            normalized,
            json!({
                "type": "object",
                "properties": {
                    "child": {
                        "type": ["object", "null"],
                        "properties": {
                            "name": { "type": ["string", "null"] }
                        },
                        "required": ["name"],
                        "additionalProperties": false
                    }
                },
                "required": ["child"],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn schema_supports_strict_rejects_open_or_empty_objects() {
        assert!(!schema_supports_strict(&json!({ "type": "object" })));
        assert!(!schema_supports_strict(&json!({
            "type": "object",
            "properties": { "x": { "type": "string" } },
            "additionalProperties": true
        })));
        assert!(schema_supports_strict(&json!({
            "type": "object",
            "properties": { "x": { "type": "string" } },
            "additionalProperties": false
        })));
    }

    /// Regression test for issue #713: an MCP tool property with only a
    /// `description` (no type keyword) made OpenAI reject the whole tool catalog.
    #[test]
    fn schema_supports_strict_rejects_untyped_properties() {
        assert!(!schema_supports_strict(&json!({
            "type": "object",
            "properties": {
                "key": { "type": "string" },
                "value": { "description": "JSON type depends on the key." }
            },
            "additionalProperties": false
        })));
        assert!(!schema_supports_strict(&json!({
            "type": "object",
            "properties": { "value": true },
            "additionalProperties": false
        })));
        assert!(schema_supports_strict(&json!({
            "type": "object",
            "properties": {
                "key": { "type": "string" },
                "value": { "enum": ["a", "b"] }
            },
            "additionalProperties": false
        })));
    }

    #[test]
    fn openai_compatible_schema_flattens_allof_object_branches() {
        let schema = json!({
            "description": "Read params",
            "allOf": [
                {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string" }
                    },
                    "required": ["file_path"]
                },
                {
                    "type": "object",
                    "properties": {
                        "start_line": { "type": "integer" }
                    }
                }
            ]
        });

        let normalized = openai_compatible_schema(&schema);
        assert!(normalized.get("allOf").is_none());
        assert_eq!(normalized["type"], json!("object"));
        assert_eq!(normalized["description"], json!("Read params"));
        assert_eq!(normalized["properties"]["file_path"]["type"], json!("string"));
        assert_eq!(normalized["properties"]["start_line"]["type"], json!("integer"));
        assert_eq!(normalized["required"], json!(["file_path"]));
    }

    /// Regression test for issue #687: a valid MCP schema using `uniqueItems`
    /// made OpenAI reject the whole tool catalog, blocking every turn.
    #[test]
    fn openai_compatible_schema_strips_unsupported_keywords() {
        let schema = json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "maxItems": 50,
                    "uniqueItems": true
                },
                "fields": { "type": ["string", "null"] }
            },
            "required": ["ids"],
            "minProperties": 1
        });

        let normalized = openai_compatible_schema(&schema);

        assert!(normalized["properties"]["ids"].get("uniqueItems").is_none());
        assert!(normalized.get("minProperties").is_none());
        // Supported array constraints survive.
        assert_eq!(normalized["properties"]["ids"]["minItems"], json!(1));
        assert_eq!(normalized["properties"]["ids"]["maxItems"], json!(50));
        assert_eq!(normalized["properties"]["ids"]["items"]["type"], json!("string"));
        assert_eq!(normalized["required"], json!(["ids"]));

        // Strict normalization keeps it clean too.
        let strict = strict_normalize_schema(&normalized);
        assert!(strict["properties"]["ids"].get("uniqueItems").is_none());
    }

    /// Stripping is keyword-aware: a *property* named like an unsupported
    /// keyword must be preserved.
    #[test]
    fn openai_compatible_schema_keeps_properties_named_like_keywords() {
        let schema = json!({
            "type": "object",
            "properties": {
                "uniqueItems": { "type": "boolean" },
                "not": { "type": "string" }
            },
            "required": ["uniqueItems"]
        });

        let normalized = openai_compatible_schema(&schema);

        assert_eq!(normalized["properties"]["uniqueItems"]["type"], json!("boolean"));
        assert_eq!(normalized["properties"]["not"]["type"], json!("string"));
    }

    /// Issue #713: `cua-driver`'s `set_config.value` declares a description and
    /// no type, because its type genuinely depends on the sibling `key`. That
    /// is legal JSON Schema and Anthropic accepts it, but OpenAI's strict
    /// validator rejects the entire tool catalog over it, so every
    /// OpenAI-route agent died on its first turn.
    ///
    /// The fix is to fail strict eligibility closed rather than to rewrite the
    /// schema: the tool is still advertised with its real shape, just without
    /// `strict: true`.
    #[test]
    fn issue_713_a_property_without_a_type_disqualifies_strict_mode() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "key": { "type": "string" },
                "value": { "description": "JSON type depends on the key." }
            }
        });
        assert!(
            !schema_supports_strict(&openai_compatible_schema(&schema)),
            "a typeless property must not be sent as a strict schema"
        );
    }

    /// The counterpart: failing closed must not become failing always, or every
    /// well-formed tool silently loses strict mode and the structured-output
    /// guarantees that come with it.
    #[test]
    fn a_fully_typed_schema_still_qualifies_for_strict_mode() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "where" },
                "count": { "type": "integer" },
                "mode": { "type": "string", "enum": ["fast", "slow"] },
                "nested": {
                    "type": "object",
                    "properties": { "inner": { "type": "boolean" } }
                },
                "either": { "anyOf": [{ "type": "string" }, { "type": "integer" }] }
            },
            "required": ["path"]
        });
        assert!(
            schema_supports_strict(&openai_compatible_schema(&schema)),
            "every property declares its shape, so strict must stay available"
        );
    }

    /// Issue #711, reproduced independently against master before fixing: four
    /// constructs from a real MCP catalog that horon-agent marked `strict: true` and
    /// OpenAI then rejected, failing the entire tool catalog.
    ///
    /// Each is legal JSON Schema that Anthropic accepts, so the fix is to fail
    /// strict eligibility closed rather than to rewrite the schema.
    #[test]
    fn issue_711_constructs_openai_rejects_do_not_claim_strict() {
        let cases: &[(&str, serde_json::Value)] = &[
            (
                // mcp__cirqul__correct_memory: a branch that only adds a
                // constraint cannot satisfy strict object requirements.
                "constraint-only anyOf branch",
                serde_json::json!({
                    "type": "object",
                    "properties": { "memory_id": { "type": "string" } },
                    "anyOf": [ { "required": ["memory_id"] } ]
                }),
            ),
            (
                // mcp__cirqul__create_object: OpenAI wants a `type` key on a
                // branch even when an enum already pins the values.
                "enum-only anyOf branch without a type",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "kind": { "anyOf": [ { "enum": ["a", "b"] }, { "type": "string" } ] }
                    }
                }),
            ),
            (
                "array with unconstrained items",
                serde_json::json!({
                    "type": "object",
                    "properties": { "tags": { "type": "array" } }
                }),
            ),
            (
                // `$defs` is stripped on some paths, so a surviving `$ref`
                // cannot be resolved by the validator.
                "unresolvable $ref",
                serde_json::json!({
                    "type": "object",
                    "properties": { "node": { "$ref": "#/$defs/missing" } }
                }),
            ),
        ];

        for (label, schema) in cases {
            assert!(
                !schema_supports_strict(&openai_compatible_schema(schema)),
                "{label} must not be sent as a strict schema"
            );
        }
    }

    /// Failing closed must not become failing always: every construct below is
    /// well formed, and losing strict mode for them would quietly drop the
    /// structured-output guarantees on every OpenAI-route tool call.
    #[test]
    fn well_formed_schemas_keep_strict_mode() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "where" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "mode": { "enum": ["fast", "slow"] },
                "either": { "anyOf": [ { "type": "string" }, { "type": "integer" } ] },
                "nested": { "type": "object", "properties": { "x": { "type": "boolean" } } }
            },
            "required": ["path"]
        });
        assert!(
            schema_supports_strict(&openai_compatible_schema(&schema)),
            "a well-formed schema must keep strict mode"
        );
    }

    /// A local `$ref` is expressible in strict mode once inlined, and the
    /// constraint that strict mode cannot carry moves into the description
    /// instead of vanishing.
    #[test]
    fn strict_schema_inlines_local_refs_and_keeps_constraints_in_the_description() {
        let schema = json!({
            "type": "object",
            "$defs": {
                "name": { "type": "string", "minLength": 1 }
            },
            "properties": {
                "name": { "$ref": "#/$defs/name", "description": "who" }
            },
            "required": ["name"]
        });

        let prepared = prepare_strict_schema(&schema);
        assert!(prepared["properties"]["name"].get("$ref").is_none(), "{prepared}");
        assert_eq!(prepared["properties"]["name"]["type"], json!("string"));
        assert_eq!(prepared["properties"]["name"]["description"], json!("who"));
        assert!(
            schema_supports_strict(&prepared),
            "an inlined local ref must be strict-eligible: {prepared}"
        );

        let wire = strict_normalize_schema(&prepared);
        assert!(wire["properties"]["name"].get("minLength").is_none(), "{wire}");
        assert!(
            wire["properties"]["name"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("(minLength: 1)")),
            "the removed constraint must survive as a description note: {wire}"
        );
    }

    /// Sibling keys win over the `$ref` target, as JSON Schema specifies.
    #[test]
    fn strict_schema_ref_siblings_override_the_target() {
        let schema = json!({
            "type": "object",
            "$defs": { "id": { "type": "integer", "description": "target" } },
            "properties": {
                "id": { "$ref": "#/$defs/id", "description": "sibling" }
            }
        });
        let prepared = prepare_strict_schema(&schema);
        assert_eq!(prepared["properties"]["id"]["description"], json!("sibling"));
        assert_eq!(prepared["properties"]["id"]["type"], json!("integer"));
    }

    /// A cycle must not inline forever; leaving the `$ref` in place makes
    /// eligibility fail closed, which is the safe outcome.
    #[test]
    fn strict_schema_leaves_cyclic_refs_in_place() {
        let schema = json!({
            "type": "object",
            "properties": { "node": { "$ref": "#/$defs/node" } },
            "$defs": {
                "node": {
                    "type": "object",
                    "properties": { "next": { "$ref": "#/$defs/node" } }
                }
            }
        });
        let prepared = prepare_strict_schema(&schema);
        assert!(
            serde_json::to_string(&prepared).unwrap().contains("$ref"),
            "a cycle must keep its $ref: {prepared}"
        );
    }

    #[test]
    fn strict_schema_collapses_single_branch_allof_and_expands_union_types() {
        let schema = json!({
            "allOf": [
                { "type": "object", "properties": { "a": { "type": "string" } } }
            ],
            "properties": {
                "maybe": { "type": ["string", "null"], "description": "nullable" }
            }
        });
        let prepared = prepare_strict_schema(&schema);
        assert!(prepared.get("allOf").is_none(), "{prepared}");
        assert_eq!(prepared["type"], json!("object"));
        assert_eq!(prepared["properties"]["a"]["type"], json!("string"));

        let maybe = &prepared["properties"]["maybe"];
        assert_eq!(maybe["description"], json!("nullable"));
        let branches = maybe["anyOf"].as_array().expect("union type becomes anyOf");
        assert_eq!(branches.len(), 2, "{maybe}");
        assert_eq!(branches[0]["type"], json!("string"));
        assert_eq!(branches[1]["type"], json!("null"));
    }

    /// Type-specific keywords must not leak into the branches that cannot use
    /// them, or the strict validator rejects the whole catalog again.
    #[test]
    fn strict_schema_union_branches_drop_foreign_keywords() {
        let schema = json!({
            "type": ["object", "null"],
            "properties": { "a": { "type": "string" } },
            "required": ["a"],
            "additionalProperties": false
        });
        let prepared = prepare_strict_schema(&schema);
        let branches = prepared["anyOf"].as_array().expect("union type becomes anyOf");
        let object_branch = branches
            .iter()
            .find(|branch| branch["type"] == json!("object"))
            .expect("object branch");
        let null_branch = branches
            .iter()
            .find(|branch| branch["type"] == json!("null"))
            .expect("null branch");
        assert_eq!(object_branch["properties"]["a"]["type"], json!("string"));
        assert!(null_branch.get("properties").is_none(), "{null_branch}");
        assert!(null_branch.get("required").is_none(), "{null_branch}");
        assert!(null_branch.get("additionalProperties").is_none(), "{null_branch}");
    }

    /// The note order is fixed by the keyword list, not by map order, because
    /// the schema rides in the cached prompt prefix.
    #[test]
    fn strict_schema_description_notes_are_deterministic() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": { "type": "string", "default": "x", "minLength": 2 },
                "b": { "type": "integer", "maximum": 9, "default": 1 }
            }
        });
        let first = strict_normalize_schema(&schema);
        let second = strict_normalize_schema(&schema);
        assert_eq!(first, second);
        assert_eq!(
            first["properties"]["a"]["description"],
            json!("(minLength: 2) (default: \"x\")")
        );
        assert_eq!(
            first["properties"]["b"]["description"],
            json!("(maximum: 9) (default: 1)")
        );
    }
}
