//! ACP schema validation for the frames the agent and the mock client write.
//!
//! Two vendored releases are supported, one per protocol version: the v1 set
//! (`tests/fixtures/acp/schema-v1.26.0.json`, tag `schema-v1.26.0`) and the v2
//! set (`tests/fixtures/acp/schema-v2.0.0-alpha.10.json`, tag
//! `schema-v2.0.0-alpha.10`). Both annotate every method-specific `$defs` entry
//! with `x-method` (the wire name) and `x-side` (which side implements the
//! method). Those two annotations are what let this module pick the schema for
//! one message instead of hand-copying a table from the specification: a request
//! or notification is checked against its params `$def`, a response against the
//! response `$def` of the method it answers.
//!
//! Both sets also name those `$defs` entries `<Name>Request`,
//! `<Name>Notification` or `<Name>Response`, so one index builder serves both.
//! The names themselves differ where the protocol renamed a method: v2 calls the
//! `session/update` notification `UpdateSessionNotification` (v1:
//! `SessionNotification`), `auth/login` and `auth/logout` use `LoginAuthRequest`
//! and `LogoutAuthRequest` (v1: `AuthenticateRequest` and `LogoutRequest`), and
//! `session/cancel` uses `CancelSessionNotification` (v1: `CancelNotification`).
//! No v2 `$defs` entry breaks the suffix rule, so nothing here depends on those
//! names.
//!
//! Two checks run per message, mirroring the reference implementation in
//! `agentclientprotocol/acp-tck` (`src/tck/v1/validation.py` and its v2 twin,
//! pinned at `b7f40fb3c003c45926547be5a1f7e2b374035815`):
//!
//! 1. the JSON-RPC envelope (`jsonrpc`, `id`, params/method, `result` XOR
//!    `error`) and the method-specific payload;
//! 2. the root-level keys of that payload against the union of `properties`
//!    reachable through `allOf`/`anyOf`/`oneOf`/`$ref`. Neither schema sets
//!    `additionalProperties: false` anywhere, so extraneous root keys have to
//!    be found by hand. `_meta` is always allowed on spec objects; a JSON-RPC
//!    `Error` object allows only `code`, `message` and `data`.
//!
//! One envelope difference: a v1 request id may be `null`, and the v1
//! `RequestId` `$def` says so. The v2 `RequestId` `$def` refuses `null`
//! outright, so a v2 validator checks every id against that `$def` as well.
#![allow(dead_code)]

use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Mutex, OnceLock};

pub const SCHEMA_JSON: &str = include_str!("../fixtures/acp/schema-v1.26.0.json");
pub const META_JSON: &str = include_str!("../fixtures/acp/meta-v1.26.0.json");
pub const SCHEMA_FIXTURE_PATH: &str = "tests/fixtures/acp/schema-v1.26.0.json";
pub const META_FIXTURE_PATH: &str = "tests/fixtures/acp/meta-v1.26.0.json";
pub const SCHEMA_V2_JSON: &str = include_str!("../fixtures/acp/schema-v2.0.0-alpha.10.json");
pub const META_V2_JSON: &str = include_str!("../fixtures/acp/meta-v2.0.0-alpha.10.json");
pub const SCHEMA_V2_FIXTURE_PATH: &str = "tests/fixtures/acp/schema-v2.0.0-alpha.10.json";
pub const META_V2_FIXTURE_PATH: &str = "tests/fixtures/acp/meta-v2.0.0-alpha.10.json";

/// Which side wrote the message being validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// The agent under test wrote it (the frames ACP tests assert on).
    AgentToClient,
    /// The mock client under test wrote it.
    ClientToAgent,
}

impl Direction {
    /// The `x-side` of a method the *other* side implements: an agent-authored
    /// request targets a method whose `x-side` is `client`.
    fn target_side(self) -> &'static str {
        match self {
            Self::AgentToClient => "client",
            Self::ClientToAgent => "agent",
        }
    }

    /// The `x-side` of a method this side implements, which is the side a
    /// response carries: the responder is the implementer.
    fn author_side(self) -> &'static str {
        match self {
            Self::AgentToClient => "agent",
            Self::ClientToAgent => "client",
        }
    }
}

/// One schema, envelope or root-key violation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// JSON pointer into the message (`""` is the message root).
    pub path: String,
    pub message: String,
    /// JSON pointer into the schema, or a fixed literal for the hand-written
    /// envelope and root-key checks.
    pub schema_path: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {} ({})", self.path, self.message, self.schema_path)
    }
}

/// One vendored release: the JSON Schema document plus the meta document that
/// lists the wire methods per side.
#[derive(Debug)]
pub struct SchemaSet {
    version: u64,
    label: &'static str,
    schema: Value,
    meta: Value,
    /// `"<kind>:<method>:<side>"` -> `$defs` entry name, built once from the
    /// schema's own `x-method`/`x-side` annotations.
    index: HashMap<String, String>,
    /// The `sessionUpdate` discriminator values this set defines, in schema
    /// order.
    session_update_variants: Vec<String>,
}

impl SchemaSet {
    /// The v1 set (`schema-v1.26.0`).
    pub fn v1() -> Self {
        Self::new(1, "schema-v1.26.0", SCHEMA_JSON, META_JSON)
    }

    /// The v2 set (`schema-v2.0.0-alpha.10`, the stable v2 document).
    pub fn v2() -> Self {
        Self::new(2, "schema-v2.0.0-alpha.10", SCHEMA_V2_JSON, META_V2_JSON)
    }

    /// The set for one negotiated protocol version, or `None` when no vendored
    /// schema covers it.
    pub fn for_version(version: u64) -> Option<Self> {
        match version {
            1 => Some(Self::v1()),
            2 => Some(Self::v2()),
            _ => None,
        }
    }

    fn new(version: u64, label: &'static str, schema: &str, meta: &str) -> Self {
        let schema: Value = serde_json::from_str(schema).expect("the vendored ACP schema is valid JSON");
        let meta: Value = serde_json::from_str(meta).expect("the vendored ACP meta is valid JSON");
        let mut index = HashMap::new();
        for (name, def) in schema["$defs"]
            .as_object()
            .expect("the vendored ACP schema has a $defs object")
        {
            let (Some(method), Some(side)) = (
                def.get("x-method").and_then(Value::as_str),
                def.get("x-side").and_then(Value::as_str),
            ) else {
                continue;
            };
            let kind = if name.ends_with("Request") {
                "request"
            } else if name.ends_with("Notification") {
                "notification"
            } else if name.ends_with("Response") {
                "response"
            } else {
                continue;
            };
            index.insert(format!("{kind}:{method}:{side}"), name.clone());
        }
        Self {
            version,
            label,
            schema,
            meta,
            index,
            session_update_variants: Vec::new(),
        }
        .with_session_update_variants()
    }

    fn with_session_update_variants(mut self) -> Self {
        self.session_update_variants = self.collect_session_update_variants();
        self
    }

    /// The `sessionUpdate` values every `session/update` variant declares.
    ///
    /// The schema's open "other" branch accepts any string, so the list of known
    /// variants has to be read out of the union the `update` property points at.
    fn collect_session_update_variants(&self) -> Vec<String> {
        let Some(def) = self
            .index
            .iter()
            .find(|(key, _)| key.starts_with("notification:session/update:"))
            .map(|(_, name)| name.clone())
        else {
            return Vec::new();
        };
        let Some(update) = self
            .defs()
            .get(&def)
            .and_then(|notification| notification.get("properties"))
            .and_then(|properties| properties.get("update"))
        else {
            return Vec::new();
        };
        self.union_branches(update, 0)
            .into_iter()
            .filter_map(|branch| {
                branch
                    .get("properties")?
                    .get("sessionUpdate")?
                    .get("const")?
                    .as_str()
                    .map(str::to_string)
            })
            .collect()
    }

    /// The `oneOf`/`anyOf` branches a node points at, following the `$ref` and
    /// `allOf` wrappers the schema puts in front of a union.
    fn union_branches<'a>(&'a self, node: &'a Value, depth: usize) -> Vec<&'a Value> {
        if depth > 4 {
            return Vec::new();
        }
        if let Some(name) = node
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
            && let Some(target) = self.defs().get(name)
        {
            return self.union_branches(target, depth + 1);
        }
        if let Some(branches) = node
            .get("oneOf")
            .or_else(|| node.get("anyOf"))
            .and_then(Value::as_array)
        {
            return branches.iter().collect();
        }
        for part in node.get("allOf").and_then(Value::as_array).into_iter().flatten() {
            let branches = self.union_branches(part, depth + 1);
            if !branches.is_empty() {
                return branches;
            }
        }
        Vec::new()
    }

    /// The `sessionUpdate` values this set defines.
    pub fn session_update_variants(&self) -> &[String] {
        &self.session_update_variants
    }

    /// The protocol version this set validates.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The release tag of this set, for failure messages.
    pub fn label(&self) -> &'static str {
        self.label
    }

    pub fn schema(&self) -> &Value {
        &self.schema
    }

    pub fn meta(&self) -> &Value {
        &self.meta
    }

    /// `defs["$defs"]` of this set's schema.
    fn defs(&self) -> &Map<String, Value> {
        self.schema["$defs"]
            .as_object()
            .expect("the vendored ACP schema has a $defs object")
    }

    fn def_name(&self, kind: &str, method: &str, side: &str) -> Option<&str> {
        self.index.get(&format!("{kind}:{method}:{side}")).map(String::as_str)
    }

    fn validator_for(&self, name: &str) -> Result<jsonschema::Validator, String> {
        static CACHE: OnceLock<Mutex<HashMap<(u64, String), jsonschema::Validator>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let key = (self.version, name.to_string());
        if let Some(validator) = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(&key) {
            return Ok(validator.clone());
        }
        // The wrapper carries the whole `$defs` map beside the `$ref`, so every
        // internal reference resolves without a separate registry.
        let wrapper = serde_json::json!({
            "$schema": self.schema["$schema"],
            "$ref": format!("#/$defs/{name}"),
            "$defs": self.defs(),
        });
        let validator = jsonschema::validator_for(&wrapper).map_err(|error| {
            format!(
                "the wrapper schema for #/$defs/{name} in {} does not compile: {error}",
                self.label
            )
        })?;
        cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, validator.clone());
        Ok(validator)
    }

    fn schema_violations(&self, def: &str, value: &Value) -> Result<Vec<Violation>, String> {
        let validator = self.validator_for(def)?;
        Ok(validator
            .iter_errors(value)
            .map(|error| Violation {
                path: error.instance_path().to_string(),
                message: error.to_string(),
                schema_path: error.schema_path().to_string(),
            })
            .collect())
    }

    fn schema_violations_for(&self, def: &str, value: &Value) -> Vec<Violation> {
        match self.schema_violations(def, value) {
            Ok(violations) => violations,
            Err(message) => vec![Violation {
                path: String::new(),
                message,
                schema_path: "validator:compilation".to_string(),
            }],
        }
    }

    /// Every property name reachable at the root of `#/$defs/{def}`, or `None`
    /// when no branch declares `properties` (a bare scalar such as `RequestId`).
    fn allowed_root_properties(&self, def: &str) -> Option<HashSet<String>> {
        let mut allowed = HashSet::new();
        let mut seen = HashSet::new();
        let mut found = false;
        self.walk_properties(
            self.defs().get(def).unwrap_or(&Value::Null),
            &mut allowed,
            &mut seen,
            &mut found,
        );
        if found {
            allowed.insert("_meta".to_string());
            Some(allowed)
        } else {
            None
        }
    }

    fn walk_properties(
        &self,
        node: &Value,
        allowed: &mut HashSet<String>,
        seen: &mut HashSet<String>,
        found: &mut bool,
    ) {
        let Some(object) = node.as_object() else {
            return;
        };
        if let Some(name) = object
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| r.strip_prefix("#/$defs/"))
            && seen.insert(name.to_string())
            && let Some(target) = self.defs().get(name)
        {
            self.walk_properties(target, allowed, seen, found);
        }
        if let Some(properties) = object.get("properties").and_then(Value::as_object)
            && !properties.is_empty()
        {
            *found = true;
            allowed.extend(properties.keys().cloned());
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            if let Some(branches) = object.get(key).and_then(Value::as_array) {
                for branch in branches {
                    self.walk_properties(branch, allowed, seen, found);
                }
            }
        }
    }

    /// Root keys of a spec object that `#/$defs/{def}` does not declare.
    pub fn unknown_root_keys(&self, def: &str, object: &Value) -> Vec<String> {
        let Some(object) = object.as_object() else {
            return Vec::new();
        };
        let Some(allowed) = self.allowed_root_properties(def) else {
            return Vec::new();
        };
        let mut unknown: Vec<String> = object.keys().filter(|key| !allowed.contains(*key)).cloned().collect();
        unknown.sort();
        unknown
    }

    /// The method names this set's meta advertises for one side.
    pub fn meta_methods(&self, side: &str) -> Vec<String> {
        let key = match side {
            "agent" => "agentMethods",
            "client" => "clientMethods",
            _ => "protocolMethods",
        };
        self.meta
            .get(key)
            .and_then(Value::as_object)
            .map(|methods| methods.values().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn root_key_violations(&self, def: &str, value: &Value) -> Vec<Violation> {
        self.unknown_root_keys(def, value)
            .into_iter()
            .map(|key| Violation {
                path: format!("/{key}"),
                message: format!("{key:?} is not a root property of #/$defs/{def}"),
                schema_path: "acp-schema-002:root-keys".to_string(),
            })
            .collect()
    }

    fn error_violations(&self, error: &Value) -> Vec<Violation> {
        let mut violations = self.schema_violations_for("Error", error);
        for key in unknown_error_keys(error) {
            violations.push(Violation {
                path: format!("/error/{key}"),
                message: format!("{key:?} is not a property of #/$defs/Error"),
                schema_path: "acp-schema-002:error-keys".to_string(),
            });
        }
        violations
    }

    /// The `RequestId` check a v2 validator adds on top of the hand-written one:
    /// the v2 `$def` refuses a `null` id, which v1 allows.
    fn v2_id_violations(&self, id: &Value, path: &str) -> Vec<Violation> {
        if self.version < 2 {
            return Vec::new();
        }
        self.schema_violations_for("RequestId", id)
            .into_iter()
            .map(|violation| Violation {
                path: path.to_string(),
                ..violation
            })
            .collect()
    }

    /// A v2 `session/update` whose `sessionUpdate` names no variant of this set.
    ///
    /// The schema's open "other" branch accepts any string, so the schema alone
    /// accepts a v1 `tool_call` on a v2 connection. The schema also says unknown
    /// values that do not start with `_` are reserved for future ACP variants, so
    /// an agent writing one violates the version it negotiated. The v1 set needs
    /// no such check (its union is closed), so this is v2-only and a v1 verdict
    /// carries exactly the violations it carried before v2 existed.
    fn session_update_violations(&self, params: &Value) -> Vec<Violation> {
        if self.version < 2 {
            return Vec::new();
        }
        let Some(session_update) = params
            .get("update")
            .and_then(|update| update.get("sessionUpdate"))
            .and_then(Value::as_str)
        else {
            return Vec::new();
        };
        if session_update.starts_with('_') || self.session_update_variants.iter().any(|known| known == session_update) {
            return Vec::new();
        }
        vec![Violation {
            path: "/update/sessionUpdate".to_string(),
            message: format!(
                "{session_update:?} is not a session update variant of {} (an unknown value must start with `_`)",
                self.label
            ),
            schema_path: "acp-schema-002:update-variants".to_string(),
        }]
    }
}

/// The v1 set, parsed once per test binary.
pub fn v1_set() -> &'static SchemaSet {
    static SET: OnceLock<SchemaSet> = OnceLock::new();
    SET.get_or_init(SchemaSet::v1)
}

/// The v2 set, parsed once per test binary.
pub fn v2_set() -> &'static SchemaSet {
    static SET: OnceLock<SchemaSet> = OnceLock::new();
    SET.get_or_init(SchemaSet::v2)
}

/// The cached set for one protocol version. Only the versions with a vendored
/// schema exist, so anything else is a test bug rather than a protocol error.
pub fn set_for_version(version: u64) -> &'static SchemaSet {
    match version {
        1 => v1_set(),
        2 => v2_set(),
        other => panic!("no vendored ACP schema covers protocol version {other}; only 1 and 2 are supported"),
    }
}

pub fn schema() -> &'static Value {
    v1_set().schema()
}

pub fn meta() -> &'static Value {
    v1_set().meta()
}

/// Root keys of a spec object that the v1 schema does not declare.
pub fn unknown_root_keys(def: &str, object: &Value) -> Vec<String> {
    v1_set().unknown_root_keys(def, object)
}

/// The v1 method names the vendored meta advertises for one side.
pub fn meta_methods(side: &str) -> Vec<String> {
    v1_set().meta_methods(side)
}

/// Keys of a JSON-RPC `Error` object outside `code`, `message`, `data`.
pub fn unknown_error_keys(error: &Value) -> Vec<String> {
    let allowed = ["code", "message", "data"];
    let Some(object) = error.as_object() else {
        return Vec::new();
    };
    let mut unknown: Vec<String> = object
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect();
    unknown.sort();
    unknown
}

fn envelope_violations(message: &Value) -> Vec<Violation> {
    let Some(object) = message.as_object() else {
        return vec![Violation {
            path: String::new(),
            message: "a frame must be a JSON object".to_string(),
            schema_path: "envelope:object".to_string(),
        }];
    };
    let mut violations = Vec::new();
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        violations.push(Violation {
            path: "/jsonrpc".to_string(),
            message: "jsonrpc must be the literal string \"2.0\"".to_string(),
            schema_path: "envelope:jsonrpc".to_string(),
        });
    }
    violations
}

fn request_id_is_valid(id: &Value) -> bool {
    match id {
        Value::Null | Value::String(_) => true,
        Value::Number(number) => number.is_i64() || number.is_u64(),
        _ => false,
    }
}

/// Validates the agent's streams while remembering which request each response
/// answers (the schema needs the method to pick the response `$def`).
#[derive(Debug)]
pub struct FrameValidator {
    set: &'static SchemaSet,
    direction: Direction,
    answered: HashMap<String, String>,
    extension_root_keys: HashSet<String>,
}

impl Default for FrameValidator {
    fn default() -> Self {
        Self::new(Direction::AgentToClient)
    }
}

impl FrameValidator {
    /// A v1 validator, which is what every v1 test target uses.
    pub fn new(direction: Direction) -> Self {
        Self::for_set(v1_set(), direction)
    }

    /// A validator for one negotiated protocol version: 1 or 2.
    pub fn for_version(version: u64, direction: Direction) -> Self {
        Self::for_set(set_for_version(version), direction)
    }

    fn for_set(set: &'static SchemaSet, direction: Direction) -> Self {
        Self {
            set,
            direction,
            answered: HashMap::new(),
            extension_root_keys: HashSet::new(),
        }
    }

    /// The protocol version this validator checks.
    pub fn version(&self) -> u64 {
        self.set.version()
    }

    /// Root keys this run accepts although the spec object does not declare them.
    ///
    /// The default is empty, so a frame the agent invents is still refused; a test that
    /// knows the adapter's documented extension declares it here.
    pub fn allow_extension_root_keys(&mut self, keys: &[&str]) {
        self.extension_root_keys
            .extend(keys.iter().map(|key| (*key).to_string()));
    }

    fn root_key_violations(&self, def: &str, value: &Value) -> Vec<Violation> {
        self.set
            .root_key_violations(def, value)
            .into_iter()
            .filter(|violation| {
                !self
                    .extension_root_keys
                    .contains(violation.path.trim_start_matches('/'))
            })
            .collect()
    }

    /// The object without the extension root keys this run declared.
    ///
    /// The spec objects are validated strictly, so an extension the run accepts is removed
    /// before validation instead of being validated as if the spec had declared it.
    fn without_extension_keys(&self, value: &Value) -> Value {
        let Some(object) = value.as_object() else {
            return value.clone();
        };
        if !object.keys().any(|key| self.extension_root_keys.contains(key)) {
            return value.clone();
        }
        let mut filtered = object.clone();
        filtered.retain(|key, _| !self.extension_root_keys.contains(key));
        Value::Object(filtered)
    }

    /// Remember the method of an outbound request so its response can be
    /// validated. `424242` and `"424242"` are different keys.
    pub fn note_request(&mut self, id: &Value, method: &str) {
        self.answered.insert(request_key(id), method.to_string());
    }

    pub fn validate(&self, message: &Value) -> Vec<Violation> {
        let mut violations = envelope_violations(message);
        let Some(object) = message.as_object() else {
            return violations;
        };
        match object.get("method").and_then(Value::as_str) {
            Some(method) => {
                let is_request = object.contains_key("id");
                if is_request {
                    match object.get("id") {
                        Some(id) if request_id_is_valid(id) => {
                            violations.extend(self.set.v2_id_violations(id, "/id"));
                        }
                        Some(id) => {
                            violations.push(Violation {
                                path: "/id".to_string(),
                                message: "a request id must be a string, an integer, or null".to_string(),
                                schema_path: "#/$defs/RequestId".to_string(),
                            });
                            violations.extend(self.set.v2_id_violations(id, "/id"));
                        }
                        None => {}
                    }
                }
                if method.starts_with('_') {
                    return violations;
                }
                let kind = if is_request { "request" } else { "notification" };
                match self
                    .set
                    .def_name(kind, method, self.direction.target_side())
                    .or_else(|| self.set.def_name(kind, method, "protocol"))
                {
                    Some(def) => {
                        let params = object.get("params").cloned().unwrap_or(Value::Null);
                        let params = if params.is_null() {
                            Value::Object(Map::new())
                        } else {
                            params
                        };
                        let params = self.without_extension_keys(&params);
                        violations.extend(self.set.schema_violations_for(def, &params));
                        violations.extend(self.root_key_violations(def, &params));
                        if method == "session/update" {
                            violations.extend(self.set.session_update_violations(&params));
                        }
                    }
                    None => violations.push(Violation {
                        path: "/method".to_string(),
                        message: format!(
                            "{method:?} is not a known {kind} method for the {} side (vendor: {})",
                            self.direction.target_side(),
                            self.set.label()
                        ),
                        schema_path: "method:index".to_string(),
                    }),
                }
                violations
            }
            None => {
                violations.extend(self.response_violations(object));
                violations
            }
        }
    }

    fn response_violations(&self, object: &Map<String, Value>) -> Vec<Violation> {
        let mut violations = Vec::new();
        match object.get("id") {
            Some(id) if request_id_is_valid(id) => violations.extend(self.set.v2_id_violations(id, "/id")),
            Some(id) => {
                violations.push(Violation {
                    path: "/id".to_string(),
                    message: "a response id must be a string, an integer, or null".to_string(),
                    schema_path: "#/$defs/RequestId".to_string(),
                });
                violations.extend(self.set.v2_id_violations(id, "/id"));
            }
            None => violations.push(Violation {
                path: "/id".to_string(),
                message: "a response must carry the id of the request it answers".to_string(),
                schema_path: "envelope:id".to_string(),
            }),
        }
        let has_result = object.contains_key("result");
        let has_error = object.contains_key("error");
        if has_result && has_error {
            violations.push(Violation {
                path: String::new(),
                message: "a response must not carry both result and error".to_string(),
                schema_path: "envelope:result-xor-error".to_string(),
            });
            return violations;
        }
        if !has_result && !has_error {
            violations.push(Violation {
                path: String::new(),
                message: "a response must carry exactly one of result or error".to_string(),
                schema_path: "envelope:result-xor-error".to_string(),
            });
            return violations;
        }
        if let Some(error) = object.get("error") {
            violations.extend(self.set.error_violations(error));
            return violations;
        }
        let Some(id) = object.get("id") else {
            return violations;
        };
        let Some(method) = self.answered.get(&request_key(id)) else {
            return violations;
        };
        let Some(def) = self.set.def_name("response", method, self.direction.author_side()) else {
            violations.push(Violation {
                path: "/result".to_string(),
                message: format!(
                    "no response $def for {method:?} on the {} side",
                    self.direction.author_side()
                ),
                schema_path: "method:index".to_string(),
            });
            return violations;
        };
        let result = self.without_extension_keys(&object.get("result").cloned().unwrap_or(Value::Null));
        violations.extend(self.set.schema_violations_for(def, &result));
        violations.extend(self.root_key_violations(def, &result));
        violations
    }
}

fn request_key(id: &Value) -> String {
    match id {
        Value::String(text) => format!("s:{text}"),
        Value::Number(number) => format!("n:{number}"),
        Value::Null => "null".to_string(),
        other => format!("j:{other}"),
    }
}

/// Stateless form: requests and notifications are fully validated; a response
/// is validated as far as its envelope goes (use [`FrameValidator`] to also
/// validate a response body against the method it answers).
pub fn validate_outgoing(message: &Value, direction: Direction) -> Vec<Violation> {
    FrameValidator::new(direction).validate(message)
}

/// [`validate_outgoing`] against one protocol version's set.
pub fn validate_outgoing_version(version: u64, message: &Value, direction: Direction) -> Vec<Violation> {
    FrameValidator::for_version(version, direction).validate(message)
}

/// Convenience for the common "agent response to a client request" case.
pub fn validate_response_to(method: &str, message: &Value, direction: Direction) -> Vec<Violation> {
    validate_response_to_version(1, method, message, direction)
}

/// [`validate_response_to`] against one protocol version's set.
pub fn validate_response_to_version(
    version: u64,
    method: &str,
    message: &Value,
    direction: Direction,
) -> Vec<Violation> {
    let mut validator = FrameValidator::for_version(version, direction);
    if let Some(id) = message.get("id") {
        validator.note_request(id, method);
    }
    validator.validate(message)
}
