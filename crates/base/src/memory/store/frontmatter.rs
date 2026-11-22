//! The strict frontmatter dialect used by memory wiki pages.
//!
//! A page file is a frontmatter block followed by the memory body:
//!
//! ```text
//! ---
//! title: "아침 회의 선호"
//! kind: preference
//! tags: ["schedule", "meeting"]
//! links: [{kind: relates_to, dst: "mem_2_b", weight: 0.5}]
//! confidence: 0.8
//! valid_to: null
//! ---
//! body text
//! ```
//!
//! This is a hand-written reader and writer for that subset, not a YAML
//! implementation. The grammar is fixed so the writer's output is the only
//! accepted form:
//!
//! - The file starts with a `---` line and the block ends at the next `---`
//!   line. Everything after that line is the body, verbatim.
//! - One field per line: `key: value`. Keys are lowercase ASCII identifiers,
//!   unique, at most [`MAX_KEYS`].
//! - Scalars are a double-quoted string, an integer, a float, `true`, `false`,
//!   or `null`. A double-quoted string escapes `"`, `\`, newline, carriage
//!   return, tab, and other control characters as `\uXXXX`.
//! - A list is `[a, b]`; a list of maps is `[{k: v}, {k: v}]`. Items are
//!   scalars or maps, separated by `, `.
//! - Unknown escapes, bare control characters, oversized lines and unparsable
//!   values are refused, so hostile content cannot break out of the block: a
//!   title holding `"` and a newline is escaped, and a body holding a `---`
//!   line is never scanned because parsing stops at the closing delimiter.
//!
//! `render(parse(text)) == text` holds for every text this module writes.
//!
//! Bodies that are not valid UTF-8 never reach this module: files are read as
//! bytes and decoded strictly first.

use anyhow::{Result, anyhow, bail};

/// Longest accepted frontmatter line, in bytes.
pub const MAX_LINE_BYTES: usize = 8 * 1024;
/// Most fields a frontmatter block may carry.
pub const MAX_KEYS: usize = 64;
/// Most items a single list value may carry.
pub const MAX_LIST_ITEMS: usize = 4096;
/// Most keys a single inline map may carry.
pub const MAX_MAP_KEYS: usize = 16;

/// One frontmatter value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A double-quoted string.
    Str(String),
    /// A whole number.
    Int(i64),
    /// A decimal number, stored as `f32` because every page number is one.
    Float(f32),
    /// `true` or `false`.
    Bool(bool),
    /// `null`.
    Null,
    /// A list of scalars.
    List(Vec<Value>),
    /// A list of inline maps, in written key order.
    MapList(Vec<Vec<(String, Value)>>),
}

/// A parsed frontmatter block and the body that follows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// Fields in written order.
    pub fields: Vec<(String, Value)>,
    /// Everything after the closing delimiter line.
    pub body: String,
}

impl Document {
    /// The value of `key`, when the block carries it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }

    /// The string value of `key`, when it is a string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(text)) => Some(text),
            _ => None,
        }
    }

    /// The list value of `key`, when it is a list.
    pub fn get_list(&self, key: &str) -> Option<&[Value]> {
        match self.get(key) {
            Some(Value::List(items)) => Some(items),
            _ => None,
        }
    }

    /// The map-list value of `key`, when it is a list of maps.
    pub fn get_map_list(&self, key: &str) -> Option<&[Vec<(String, Value)>]> {
        match self.get(key) {
            Some(Value::MapList(items)) => Some(items),
            _ => None,
        }
    }

    /// Render the document in canonical form.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(256 + self.body.len());
        out.push_str("---\n");
        for (key, value) in &self.fields {
            out.push_str(key);
            out.push_str(": ");
            render_value(&mut out, value);
            out.push('\n');
        }
        out.push_str("---\n");
        out.push_str(&self.body);
        out
    }

    /// Parse `text` in the dialect described by the module documentation.
    pub fn parse(text: &str) -> Result<Self> {
        let Some(mut rest) = text.strip_prefix("---\n") else {
            bail!("page does not start with a `---` line");
        };
        let mut fields: Vec<(String, Value)> = Vec::new();
        loop {
            let Some((line, tail)) = rest.split_once('\n') else {
                bail!("frontmatter block is not closed by a `---` line");
            };
            rest = tail;
            if line == "---" {
                return Ok(Self {
                    fields,
                    body: rest.to_string(),
                });
            }
            if fields.len() >= MAX_KEYS {
                bail!("frontmatter has more than {MAX_KEYS} fields");
            }
            if line.len() > MAX_LINE_BYTES {
                bail!("frontmatter line is longer than {MAX_LINE_BYTES} bytes");
            }
            let (key, value) = parse_field_line(line)?;
            if fields.iter().any(|(name, _)| name.as_str() == key) {
                bail!("frontmatter repeats the field `{key}`");
            }
            fields.push((key, value));
        }
    }
}

fn parse_field_line(line: &str) -> Result<(String, Value)> {
    let Some((key, raw_value)) = line.split_once(':') else {
        bail!("frontmatter line is not `key: value`");
    };
    if !is_key(key) {
        bail!("frontmatter key `{key}` is not a lowercase identifier");
    }
    let raw_value = raw_value.strip_prefix(' ').unwrap_or(raw_value);
    if raw_value.is_empty() {
        bail!("frontmatter field `{key}` has no value");
    }
    let mut cursor = Cursor::new(raw_value);
    let value = parse_value(&mut cursor, 0)?;
    if !cursor.at_end() {
        bail!("frontmatter field `{key}` has trailing text after its value");
    }
    Ok((key.to_string(), value))
}

fn is_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

struct Cursor<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            chars: text.chars().peekable(),
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn next(&mut self) -> Option<char> {
        self.chars.next()
    }

    fn at_end(&mut self) -> bool {
        self.peek().is_none()
    }

    fn take(&mut self, expected: char) -> Result<()> {
        match self.next() {
            Some(ch) if ch == expected => Ok(()),
            _ => bail!("expected `{expected}` in a frontmatter value"),
        }
    }

    /// Skip the optional single space a writer puts after a comma or a colon.
    fn skip_optional_space(&mut self) {
        if self.peek() == Some(' ') {
            self.next();
        }
    }
}

fn parse_value(cursor: &mut Cursor<'_>, depth: usize) -> Result<Value> {
    if depth > 2 {
        bail!("frontmatter value is nested too deeply");
    }
    match cursor.peek() {
        Some('"') => parse_string(cursor),
        Some('[') => parse_list(cursor, depth),
        Some('{') => parse_map(cursor, depth),
        Some('t') => parse_literal(cursor, "true", Value::Bool(true)),
        Some('f') => parse_literal(cursor, "false", Value::Bool(false)),
        Some('n') => parse_literal(cursor, "null", Value::Null),
        Some(ch) if ch == '-' || ch.is_ascii_digit() => parse_number(cursor),
        Some(ch) => bail!("frontmatter value starts with an unsupported character `{ch}`"),
        None => bail!("frontmatter value is missing"),
    }
}

fn parse_literal(cursor: &mut Cursor<'_>, text: &str, value: Value) -> Result<Value> {
    for expected in text.chars() {
        if cursor.next() != Some(expected) {
            bail!("frontmatter literal `{text}` is misspelled");
        }
    }
    Ok(value)
}

fn parse_number(cursor: &mut Cursor<'_>) -> Result<Value> {
    let mut text = String::new();
    if cursor.peek() == Some('-') {
        text.push('-');
        cursor.next();
    }
    let mut digits = 0usize;
    while let Some(ch) = cursor.peek() {
        if ch.is_ascii_digit() {
            text.push(ch);
            digits += 1;
            cursor.next();
        } else {
            break;
        }
    }
    if digits == 0 {
        bail!("frontmatter number has no digits");
    }
    let mut is_float = false;
    if cursor.peek() == Some('.') {
        is_float = true;
        text.push('.');
        cursor.next();
        let mut fraction = 0usize;
        while let Some(ch) = cursor.peek() {
            if ch.is_ascii_digit() {
                text.push(ch);
                fraction += 1;
                cursor.next();
            } else {
                break;
            }
        }
        if fraction == 0 {
            bail!("frontmatter number has no fraction digits");
        }
    }
    if cursor.peek() == Some('e') || cursor.peek() == Some('E') {
        is_float = true;
        text.push('e');
        cursor.next();
        if cursor.peek() == Some('-') || cursor.peek() == Some('+') {
            let sign = cursor.next().unwrap_or('+');
            text.push(sign);
        }
        let mut exponent = 0usize;
        while let Some(ch) = cursor.peek() {
            if ch.is_ascii_digit() {
                text.push(ch);
                exponent += 1;
                cursor.next();
            } else {
                break;
            }
        }
        if exponent == 0 {
            bail!("frontmatter number has no exponent digits");
        }
    }
    if is_float {
        let value: f32 = text
            .parse()
            .map_err(|_| anyhow!("frontmatter number `{text}` is not a finite decimal"))?;
        if !value.is_finite() {
            bail!("frontmatter number `{text}` is not finite");
        }
        Ok(Value::Float(value))
    } else {
        let value: i64 = text
            .parse()
            .map_err(|_| anyhow!("frontmatter integer `{text}` is out of range"))?;
        Ok(Value::Int(value))
    }
}

fn parse_string(cursor: &mut Cursor<'_>) -> Result<Value> {
    cursor.take('"')?;
    let mut text = String::new();
    loop {
        let Some(ch) = cursor.next() else {
            bail!("frontmatter string is not closed");
        };
        match ch {
            '"' => return Ok(Value::Str(text)),
            '\\' => {
                let Some(escape) = cursor.next() else {
                    bail!("frontmatter string ends inside an escape");
                };
                match escape {
                    '"' => text.push('"'),
                    '\\' => text.push('\\'),
                    'n' => text.push('\n'),
                    'r' => text.push('\r'),
                    't' => text.push('\t'),
                    'u' => text.push(parse_unicode_escape(cursor)?),
                    other => bail!("frontmatter string uses the unsupported escape `\\{other}`"),
                }
            }
            ch if is_forbidden_raw(ch) => bail!("frontmatter string holds a bare control character"),
            ch => text.push(ch),
        }
    }
}

fn parse_unicode_escape(cursor: &mut Cursor<'_>) -> Result<char> {
    let mut code = 0u32;
    for _ in 0..4 {
        let Some(ch) = cursor.next() else {
            bail!("frontmatter `\\u` escape is shorter than four digits");
        };
        let Some(digit) = ch.to_digit(16) else {
            bail!("frontmatter `\\u` escape holds a non-hex digit");
        };
        code = code * 16 + digit;
    }
    char::from_u32(code).ok_or_else(|| anyhow!("frontmatter `\\u` escape is not a character"))
}

fn parse_list(cursor: &mut Cursor<'_>, depth: usize) -> Result<Value> {
    cursor.take('[')?;
    let mut items = Vec::new();
    if cursor.peek() == Some(']') {
        cursor.next();
        return Ok(Value::List(items));
    }
    loop {
        if items.len() >= MAX_LIST_ITEMS {
            bail!("frontmatter list has more than {MAX_LIST_ITEMS} items");
        }
        items.push(parse_value(cursor, depth + 1)?);
        cursor.skip_optional_space();
        match cursor.next() {
            Some(',') => cursor.skip_optional_space(),
            Some(']') => return Ok(flatten_map_list(items)),
            _ => bail!("frontmatter list is not closed with `]`"),
        }
    }
}

/// A list whose items are all single maps is a map list. This keeps
/// `render(parse(text)) == text` for `links: [{...}, {...}]`, which would
/// otherwise parse as a list of one-map lists.
fn flatten_map_list(items: Vec<Value>) -> Value {
    if items.is_empty()
        || !items
            .iter()
            .all(|item| matches!(item, Value::MapList(entries) if entries.len() == 1))
    {
        return Value::List(items);
    }
    let mut flattened = Vec::with_capacity(items.len());
    for item in items {
        if let Value::MapList(mut entries) = item
            && let Some(first) = entries.pop()
        {
            flattened.push(first);
        }
    }
    Value::MapList(flattened)
}

fn parse_map(cursor: &mut Cursor<'_>, depth: usize) -> Result<Value> {
    cursor.take('{')?;
    let mut entries: Vec<(String, Value)> = Vec::new();
    if cursor.peek() == Some('}') {
        cursor.next();
        return Ok(Value::MapList(vec![entries]));
    }
    loop {
        if entries.len() >= MAX_MAP_KEYS {
            bail!("frontmatter map has more than {MAX_MAP_KEYS} keys");
        }
        let mut key = String::new();
        while let Some(ch) = cursor.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                key.push(ch);
                cursor.next();
            } else {
                break;
            }
        }
        if !is_key(&key) {
            bail!("frontmatter map key `{key}` is not a lowercase identifier");
        }
        cursor.take(':')?;
        cursor.skip_optional_space();
        let value = parse_value(cursor, depth + 1)?;
        entries.push((key, value));
        cursor.skip_optional_space();
        match cursor.next() {
            Some(',') => cursor.skip_optional_space(),
            Some('}') => return Ok(Value::MapList(vec![entries])),
            _ => bail!("frontmatter map is not closed with `}}`"),
        }
    }
}

fn render_value(out: &mut String, value: &Value) {
    match value {
        Value::Str(text) => render_string(out, text),
        Value::Int(number) => out.push_str(&number.to_string()),
        Value::Float(number) => out.push_str(&format_float(*number)),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Null => out.push_str("null"),
        Value::List(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                render_value(out, item);
            }
            out.push(']');
        }
        Value::MapList(entries_list) => {
            out.push('[');
            for (index, entries) in entries_list.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push('{');
                for (entry_index, (key, entry_value)) in entries.iter().enumerate() {
                    if entry_index > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(key);
                    out.push_str(": ");
                    render_value(out, entry_value);
                }
                out.push('}');
            }
            out.push(']');
        }
    }
}

fn render_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if is_forbidden_raw(ch) => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// Render a page float in its shortest exact form (`0.8`, `0.25`, `1`).
///
/// A whole value is written as `1`, which parses back as an integer, so the
/// caller that expects a float reads through [`Value::Int`] as well.
fn format_float(number: f32) -> String {
    let mut text = format!("{number}");
    if !text.contains('.') && !text.contains('e') {
        text.push_str(".0");
    }
    text
}

fn is_forbidden_raw(ch: char) -> bool {
    ch.is_control() || ch == '\u{7f}'
}
