//! Schema-directed repair of model-supplied tool arguments.
//!
//! Models serialize tool calls through the provider's JSON writer, and that
//! writer is not always faithful to the schema it was handed: a number arrives
//! quoted (`"timeout": "5000"`), a boolean arrives as `"true"`, an argument
//! meant for `a[0].b` is sent as a literal key, or an optional property is sent
//! as `null`. Every one of those is a schema violation the tool then has to
//! reject, costing a whole round trip and usually the model's confidence.
//!
//! This module repairs only what the schema can prove: it validates the input,
//! walks the reported errors, and rewrites the argument to the shape the schema
//! asks for. When the repaired input still does not validate, the original is
//! returned untouched so the tool's own deserialization error stays the
//! authority — a half-repaired payload is worse than an honest refusal.

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

/// Bounded passes: each pass fixes what the previous one exposed (for example
/// wrapping a scalar in an array turns its item type into the next error).
const MAX_PASSES: usize = 5;

/// Largest array index accepted when reconstructing a flattened `a[0].b` key.
const MAX_BRACKET_INDEX: usize = 100_000;

/// Distinct schemas whose prepared form is kept; the map is cleared when full.
/// Built-in tools have a handful of fixed schemas, MCP servers add theirs.
const PREPARED_CAP: usize = 256;

/// A tool schema in the shape repair works on, with its compiled validator.
/// `validator` is `None` when the schema is not object-shaped or does not
/// compile, in which case input passes through untouched.
struct Prepared {
    schema: Value,
    validator: Option<jsonschema::Validator>,
}

/// Compiling a validator walks and allocates the whole schema, and a tool's
/// schema does not change between calls, so each one is prepared once.
static PREPARED: LazyLock<Mutex<HashMap<String, Arc<Prepared>>>> = LazyLock::new(Mutex::default);

fn prepared(schema: &Value) -> Arc<Prepared> {
    let key = schema.to_string();
    let cached = PREPARED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .cloned();
    if let Some(hit) = cached {
        return hit;
    }
    let built = Arc::new(prepare(schema));
    let mut map = PREPARED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if map.len() >= PREPARED_CAP {
        map.clear();
    }
    map.insert(key, Arc::clone(&built));
    built
}

fn prepare(schema: &Value) -> Prepared {
    let mut schema = horon_agent_tool_core::ensure_intent_in_schema(schema.clone());
    // `ensure_intent_in_schema` advertises `intent` as required because the
    // prompt wants it. Repair must treat it as optional: an unrelated repair
    // would otherwise be discarded whenever the model omitted the label, since
    // the repaired input would still fail validation.
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|name| name.as_str() != Some("intent"));
    }
    let object_shaped = schema.as_object().is_some_and(|object| {
        object
            .get("type")
            .and_then(Value::as_str)
            .map(|kind| kind == "object")
            .unwrap_or_else(|| object.contains_key("properties"))
    });
    if !object_shaped {
        return Prepared {
            schema,
            validator: None,
        };
    }
    let Ok(validator) = jsonschema::validator_for(&schema) else {
        return Prepared {
            schema,
            validator: None,
        };
    };
    Prepared {
        schema,
        validator: Some(validator),
    }
}

/// What a repair did to one call's arguments.
pub(crate) struct Repaired {
    /// The arguments the tool will run with.
    pub(crate) input: Value,
    /// The arguments as the model sent them, present only when a repair was
    /// applied and the result validated. This is the same clone `repair` already
    /// takes to fall back on, so recording costs no extra copy.
    pub(crate) original: Option<Value>,
    /// One note per change, empty when nothing was repaired.
    pub(crate) changes: Vec<String>,
}

/// Repair `input` against `schema`, keeping what the model originally sent.
///
/// The stored call record and the SDK event both want the original arguments and
/// the change list; the plain [`repair`] entry point exists for callers that only
/// need the value that runs.
pub(crate) fn repair_recorded(schema: &Value, input: Value) -> Repaired {
    let prepared = prepared(schema);
    let Some(validator) = prepared.validator.as_ref() else {
        return Repaired {
            input,
            original: None,
            changes: Vec::new(),
        };
    };
    // The common case: valid input needs no copy and no error walk.
    if validator.is_valid(&input) {
        return Repaired {
            input,
            original: None,
            changes: Vec::new(),
        };
    }
    let schema = &prepared.schema;

    let original = input.clone();
    let mut current = input;
    let mut changes = Vec::new();
    for _ in 0..MAX_PASSES {
        let problems = collect_problems(validator, &current);
        if problems.is_empty() {
            break;
        }
        let mut changed = false;
        if unquote_root_keys(schema, &mut current, &mut changes) {
            changed = true;
        }
        if rebuild_bracketed_keys(schema, &mut current, &mut changes) {
            changed = true;
        }
        for problem in &problems {
            if let Some(note) = repair_problem(schema, &mut current, problem) {
                changes.push(note);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    if validator.is_valid(&current) {
        Repaired {
            input: current,
            original: Some(original),
            changes,
        }
    } else {
        Repaired {
            input: original,
            original: None,
            changes: Vec::new(),
        }
    }
}

/// Repair `input` against `schema`, returning the repaired value and one note
/// per change. No notes means the input was already valid or could not be
/// improved.
pub(crate) fn repair(schema: &Value, input: Value) -> (Value, Vec<String>) {
    let repaired = repair_recorded(schema, input);
    (repaired.input, repaired.changes)
}

/// One validation failure reduced to owned data, so the input can be mutated
/// while the errors are still being applied.
struct Problem {
    tokens: Vec<String>,
    expected: Vec<String>,
    enum_options: Option<Vec<Value>>,
    constant: Option<Value>,
}

fn collect_problems(validator: &jsonschema::Validator, input: &Value) -> Vec<Problem> {
    use jsonschema::error::{TypeKind, ValidationErrorKind};

    validator
        .iter_errors(input)
        .map(|error| {
            let (expected, enum_options, constant) = match error.kind() {
                ValidationErrorKind::Type { kind } => {
                    let names = match kind {
                        TypeKind::Single(one) => vec![one.as_str().to_string()],
                        TypeKind::Multiple(many) => many.iter().map(|one| one.as_str().to_string()).collect(),
                    };
                    (names, None, None)
                }
                ValidationErrorKind::Enum { options } => (Vec::new(), options.as_array().cloned(), None),
                ValidationErrorKind::Constant { expected_value } => (Vec::new(), None, Some(expected_value.clone())),
                _ => (Vec::new(), None, None),
            };
            Problem {
                tokens: pointer_tokens(&error.instance_path().to_string()),
                expected,
                enum_options,
                constant,
            }
        })
        .collect()
}

/// Split a JSON pointer into unescaped reference tokens.
fn pointer_tokens(pointer: &str) -> Vec<String> {
    pointer
        .split('/')
        .skip(1)
        .map(|token| token.replace("~1", "/").replace("~0", "~"))
        .collect()
}

fn value_at<'a>(root: &'a Value, tokens: &[String]) -> Option<&'a Value> {
    let mut current = root;
    for token in tokens {
        current = match current {
            Value::Object(map) => map.get(token)?,
            Value::Array(items) => items.get(token.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn value_at_mut<'a>(root: &'a mut Value, tokens: &[String]) -> Option<&'a mut Value> {
    let mut current = root;
    for token in tokens {
        current = match current {
            Value::Object(map) => map.get_mut(token)?,
            Value::Array(items) => items.get_mut(token.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Best-effort schema lookup for an instance path. Combiners are searched in
/// order, which is enough to find the expected type of a reparsed value.
fn schema_at<'a>(schema: &'a Value, tokens: &[String]) -> Option<&'a Value> {
    let mut current = schema;
    for token in tokens {
        current = child_schema(current, token)?;
    }
    Some(current)
}

fn child_schema<'a>(schema: &'a Value, token: &str) -> Option<&'a Value> {
    let map = schema.as_object()?;
    if let Ok(index) = token.parse::<usize>()
        && let Some(Value::Array(prefix)) = map.get("prefixItems")
        && let Some(item) = prefix.get(index)
    {
        return Some(item);
    }
    if map.get("type").and_then(Value::as_str) != Some("object")
        && token.parse::<usize>().is_ok()
        && let Some(items) = map.get("items")
    {
        return Some(items);
    }
    if let Some(Value::Object(properties)) = map.get("properties")
        && let Some(property) = properties.get(token)
    {
        return Some(property);
    }
    for combiner in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(branches)) = map.get(combiner) {
            for branch in branches {
                if let Some(found) = child_schema(branch, token) {
                    return Some(found);
                }
            }
        }
    }
    match map.get("additionalProperties") {
        Some(Value::Object(_)) => map.get("additionalProperties"),
        Some(Value::Bool(true)) => Some(schema),
        _ => None,
    }
}

/// Root keys that arrived wrapped in quotes (`"\"op\""`).
fn unquote_root_keys(schema: &Value, input: &mut Value, changes: &mut Vec<String>) -> bool {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return false;
    };
    let Some(object) = input.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    // Models occasionally double-encode the quotes; peel up to three layers.
    for _ in 0..3 {
        let rewrites: Vec<(String, String)> = object
            .keys()
            .filter_map(|key| {
                let unquoted = peel_quotes(key);
                (unquoted != *key && properties.contains_key(&unquoted) && !object.contains_key(&unquoted))
                    .then_some((key.clone(), unquoted))
            })
            .collect();
        if rewrites.is_empty() {
            return changed;
        }
        for (quoted, unquoted) in rewrites {
            if let Some(value) = object.remove(&quoted) {
                object.insert(unquoted.clone(), value);
                changes.push(format!("unquoted argument key `{quoted}` to `{unquoted}`"));
                changed = true;
            }
        }
    }
    changed
}

fn peel_quotes(key: &str) -> String {
    let mut key = key.trim();
    while key.len() >= 2 && key.starts_with('"') && key.ends_with('"') {
        key = &key[1..key.len() - 1];
    }
    key.to_string()
}

/// Object keys that encode a path (`a[0].b`) rebuilt into the nested shape the
/// schema describes.
fn rebuild_bracketed_keys(schema: &Value, input: &mut Value, changes: &mut Vec<String>) -> bool {
    let mut changed = false;
    rebuild_bracketed_walk(schema, input, &mut changed, changes);
    changed
}

fn rebuild_bracketed_walk(schema: &Value, input: &mut Value, changed: &mut bool, changes: &mut Vec<String>) {
    match input {
        Value::Object(map) => {
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                let candidates: Vec<(String, String, Vec<Segment>)> = map
                    .iter()
                    .filter_map(|(key, _)| {
                        let (head, rest) = split_bracketed_key(key)?;
                        let head_schema = properties.get(&head)?;
                        if !is_array_typed(head_schema) || map.contains_key(&head) {
                            return None;
                        }
                        let path = format!("{head}{rest}");
                        let segments = parse_path(&path)?;
                        Some((key.clone(), path, segments))
                    })
                    .collect();
                for (key, path, segments) in candidates {
                    let Some(value) = map.remove(&key) else { continue };
                    if assign_path(map, &segments, value) {
                        changes.push(format!("rebuilt flattened argument key `{key}` as `{path}`"));
                        *changed = true;
                    }
                }
            }
            // Recurse through the declared properties so nested objects under a
            // schema reached by `items` are repaired too.
            for (key, child) in map.iter_mut() {
                if let Some(child_schema) = child_schema_for_key(schema, key) {
                    rebuild_bracketed_walk(child_schema, child, changed, changes);
                }
            }
        }
        Value::Array(items) => {
            let item_schema = schema.get("items");
            for (index, child) in items.iter_mut().enumerate() {
                let child_schema = schema
                    .get("prefixItems")
                    .and_then(Value::as_array)
                    .and_then(|prefix| prefix.get(index))
                    .or(item_schema);
                if let Some(child_schema) = child_schema {
                    rebuild_bracketed_walk(child_schema, child, changed, changes);
                }
            }
        }
        _ => {}
    }
}

fn child_schema_for_key<'a>(schema: &'a Value, key: &str) -> Option<&'a Value> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get(key))
}

/// Split `a[0].b` into the head property name and the remaining path suffix.
fn split_bracketed_key(key: &str) -> Option<(String, String)> {
    let bracket = key.find('[')?;
    if bracket == 0 || key.as_bytes().get(bracket + 1) == Some(&b'[') {
        return None;
    }
    let head = &key[..bracket];
    if !head
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '$')
    {
        return None;
    }
    let rest = &key[bracket..];
    let digits: String = rest.chars().take_while(|c| *c != ']').skip(1).collect();
    let index: usize = digits.parse().ok()?;
    if index > MAX_BRACKET_INDEX {
        return None;
    }
    Some((head.to_string(), rest.to_string()))
}

fn is_array_typed(schema: &Value) -> bool {
    match schema.get("type") {
        Some(Value::String(kind)) => kind == "array",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind.as_str() == Some("array")),
        _ => schema.get("items").is_some(),
    }
}

#[derive(Clone, Debug)]
enum Segment {
    Key(String),
    Index(usize),
}

fn parse_path(path: &str) -> Option<Vec<Segment>> {
    let chars: Vec<char> = path.chars().collect();
    let mut segments = Vec::new();
    let mut cursor = 0;
    let mut expect_key = true;
    while cursor < chars.len() {
        match chars[cursor] {
            '[' => {
                let end = cursor + chars[cursor..].iter().position(|c| *c == ']')?;
                let digits: String = chars[cursor + 1..end].iter().collect();
                let index: usize = digits.parse().ok()?;
                if index > MAX_BRACKET_INDEX {
                    return None;
                }
                segments.push(Segment::Index(index));
                cursor = end + 1;
                expect_key = false;
            }
            '.' => {
                cursor += 1;
                expect_key = true;
            }
            _ if expect_key => {
                let start = cursor;
                while cursor < chars.len() && chars[cursor] != '.' && chars[cursor] != '[' {
                    cursor += 1;
                }
                let key: String = chars[start..cursor].iter().collect();
                if key.is_empty() {
                    return None;
                }
                segments.push(Segment::Key(key));
                expect_key = false;
            }
            _ => return None,
        }
    }
    (!segments.is_empty()).then_some(segments)
}

/// Write `value` at `segments` inside `map`, creating intermediate containers.
fn assign_path(map: &mut Map<String, Value>, segments: &[Segment], value: Value) -> bool {
    let Some((Segment::Key(head), rest)) = segments.split_first() else {
        return false;
    };
    let mut target = Value::Object(std::mem::take(map));
    let assigned = assign_segments(&mut target, head, rest, value);
    if let Value::Object(restored) = target {
        *map = restored;
    }
    assigned
}

fn assign_segments(target: &mut Value, key: &str, rest: &[Segment], value: Value) -> bool {
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Some(map) = target.as_object_mut() else {
        return false;
    };
    let slot = map.entry(key.to_string()).or_insert(Value::Null);
    assign_tail(slot, rest, value)
}

fn assign_tail(target: &mut Value, rest: &[Segment], value: Value) -> bool {
    match rest.split_first() {
        None => {
            *target = value;
            true
        }
        Some((Segment::Key(key), tail)) => assign_segments(target, key, tail, value),
        Some((Segment::Index(index), tail)) => {
            if !target.is_array() {
                *target = Value::Array(Vec::new());
            }
            let Some(items) = target.as_array_mut() else {
                return false;
            };
            let filler = match tail.first() {
                Some(Segment::Key(_)) => Value::Object(Map::new()),
                _ => Value::Array(Vec::new()),
            };
            while items.len() <= *index {
                items.push(filler.clone());
            }
            assign_tail(&mut items[*index], tail, value)
        }
    }
}

fn repair_problem(schema: &Value, input: &mut Value, problem: &Problem) -> Option<String> {
    let schema_node = schema_at(schema, &problem.tokens);
    let expected: Vec<String> = match schema_node.and_then(|node| node.get("type")) {
        Some(Value::String(kind)) => vec![kind.clone()],
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(|kind| kind.as_str().map(str::to_string))
            .collect(),
        _ => problem.expected.clone(),
    };

    let current = value_at(input, &problem.tokens)?.clone();

    if let Some(note) = remove_null_optional(schema, input, problem) {
        return Some(note);
    }

    if let (Some(options), Some(text)) = (&problem.enum_options, current.as_str()) {
        let trimmed = text.trim();
        if trimmed != text && options.iter().any(|option| option.as_str() == Some(trimmed)) {
            let replacement = Value::String(trimmed.to_string());
            *value_at_mut(input, &problem.tokens)? = replacement;
            return Some(format!(
                "trimmed argument `{}` to match an allowed value",
                path_label(&problem.tokens)
            ));
        }
    }
    if let (Some(expected_value), Some(text)) = (&problem.constant, current.as_str()) {
        let trimmed = text.trim();
        if trimmed != text && expected_value.as_str() == Some(trimmed) {
            *value_at_mut(input, &problem.tokens)? = Value::String(trimmed.to_string());
            return Some(format!(
                "trimmed argument `{}` to match the constant",
                path_label(&problem.tokens)
            ));
        }
    }

    let wants_string = expected.iter().any(|kind| kind == "string");
    let wants_bool = expected.iter().any(|kind| kind == "boolean");
    let wants_number = expected.iter().any(|kind| kind == "number");
    let wants_integer = expected.iter().any(|kind| kind == "integer");
    let wants_object = expected.iter().any(|kind| kind == "object");
    let wants_array = expected.iter().any(|kind| kind == "array");

    if (wants_number || wants_integer)
        && !wants_string
        && let Some(text) = current.as_str()
        && let Some(number) = number_literal(text)
    {
        let integral = match &number {
            Value::Number(number) => is_integral(number),
            _ => false,
        };
        let replacement = if wants_integer && !wants_number && !integral {
            return None;
        } else {
            number
        };
        *value_at_mut(input, &problem.tokens)? = replacement;
        return Some(format!("coerced `{}` to a number", path_label(&problem.tokens)));
    }

    if wants_bool {
        let replacement = match &current {
            Value::String(text) => boolean_literal(text),
            Value::Number(number) => match number.as_i64() {
                Some(0) => Some(false),
                Some(1) => Some(true),
                _ => None,
            },
            _ => None,
        };
        if let Some(boolean) = replacement {
            *value_at_mut(input, &problem.tokens)? = Value::Bool(boolean);
            return Some(format!("coerced `{}` to a boolean", path_label(&problem.tokens)));
        }
    }

    if (wants_object || wants_array)
        && !wants_string
        && let Some(text) = current.as_str()
        && let Some(parsed) = parse_json_container(text, wants_object, wants_array)
    {
        *value_at_mut(input, &problem.tokens)? = parsed;
        return Some(format!("parsed `{}` from a JSON string", path_label(&problem.tokens)));
    }

    if wants_array
        && !wants_object
        && !wants_string
        && !current.is_array()
        && let Some(item_schema) = schema_node.and_then(|node| node.get("items"))
        && scalar_fits(item_schema, &current)
    {
        let scalar = current;
        *value_at_mut(input, &problem.tokens)? = Value::Array(vec![scalar]);
        return Some(format!("wrapped `{}` in an array", path_label(&problem.tokens)));
    }

    None
}

/// Drop an optional property the model sent as `null` (or the string `"null"`).
fn remove_null_optional(schema: &Value, input: &mut Value, problem: &Problem) -> Option<String> {
    let (property, parent_path) = problem.tokens.split_last()?;
    let parent_schema = schema_at(schema, parent_path)?;
    let properties = parent_schema.get("properties").and_then(Value::as_object)?;
    if !properties.contains_key(property) {
        return None;
    }
    let required = parent_schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|names| names.iter().any(|name| name.as_str() == Some(property)));
    if required {
        return None;
    }
    let parent_value = value_at_mut(input, parent_path)?;
    let map = parent_value.as_object_mut()?;
    let nullish = map
        .get(property)
        .is_some_and(|value| value.is_null() || value.as_str() == Some("null"));
    if !nullish {
        return None;
    }
    map.remove(property);
    Some(format!(
        "dropped null optional argument `{}`",
        path_label(&problem.tokens)
    ))
}

fn path_label(tokens: &[String]) -> String {
    if tokens.is_empty() {
        "<root>".to_string()
    } else {
        tokens.join(".")
    }
}

fn number_literal(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);
    if !is_number_literal(normalized) {
        return None;
    }
    serde_json::from_str::<Value>(normalized).ok().filter(Value::is_number)
}

fn is_number_literal(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    if matches!(chars.peek(), Some('-')) {
        chars.next();
    }
    let mut digits = 0;
    while chars.peek().is_some_and(char::is_ascii_digit) {
        chars.next();
        digits += 1;
    }
    if digits == 0 {
        return false;
    }
    if matches!(chars.peek(), Some('.')) {
        chars.next();
        let mut fraction = 0;
        while chars.peek().is_some_and(char::is_ascii_digit) {
            chars.next();
            fraction += 1;
        }
        if fraction == 0 {
            return false;
        }
    }
    if matches!(chars.peek(), Some('e' | 'E')) {
        chars.next();
        if matches!(chars.peek(), Some('+' | '-')) {
            chars.next();
        }
        let mut exponent = 0;
        while chars.peek().is_some_and(char::is_ascii_digit) {
            chars.next();
            exponent += 1;
        }
        if exponent == 0 {
            return false;
        }
    }
    chars.next().is_none()
}

fn is_integral(number: &serde_json::Number) -> bool {
    number.is_i64() || number.is_u64() || number.as_f64().is_some_and(|float| float.fract() == 0.0)
}

fn boolean_literal(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Parse a JSON string that encodes an object or array, unwrapping up to three
/// levels of string encoding.
fn parse_json_container(text: &str, want_object: bool, want_array: bool) -> Option<Value> {
    let mut candidate = text.trim().to_string();
    for _ in 0..=3 {
        let parsed: Value = serde_json::from_str(&candidate).ok()?;
        match parsed {
            Value::Object(_) if want_object => return Some(parsed),
            Value::Array(_) if want_array => return Some(parsed),
            Value::String(inner) => candidate = inner,
            _ => return None,
        }
    }
    None
}

fn scalar_fits(item_schema: &Value, value: &Value) -> bool {
    let Some(kind) = item_schema.get("type") else {
        return false;
    };
    let actual = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) => {
            if is_integral(number) {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    match kind {
        Value::String(kind) => kind == actual || (kind == "number" && actual == "integer"),
        Value::Array(kinds) => kinds
            .iter()
            .any(|kind| kind.as_str() == Some(actual) || (kind.as_str() == Some("number") && actual == "integer")),
        _ => false,
    }
}

#[cfg(test)]
#[path = "arg_repair_tests.rs"]
mod tests;
