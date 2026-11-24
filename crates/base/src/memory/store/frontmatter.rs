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

    /// The map-list value of `key`, when it is a list of maps.
    pub fn get_map_list(&self, key: &str) -> Option<&[Vec<(String, Value)>]> {
        match self.get(key) {
            Some(Value::MapList(items)) => Some(items),
            _ => None,
        }
    }

    /// Take and remove the value of `key` by replacing it with `Value::Null`.
    pub fn take(&mut self, key: &str) -> Option<Value> {
        self.fields
            .iter_mut()
            .find(|(name, _)| name == key)
            .map(|(_, value)| std::mem::replace(value, Value::Null))
    }

    /// Parse `text` in the dialect described by the module documentation.
    pub fn parse(text: &str) -> Result<Self> {
        let Some(mut rest) = text.strip_prefix("---\n") else {
            bail!("page does not start with a `---` line");
        };
        let mut fields: Vec<(String, Value)> = Vec::with_capacity(21);
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
    text: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        let b = *self.text.as_bytes().get(self.pos)?;
        if b < 0x80 {
            Some(b as char)
        } else {
            self.rest().chars().next()
        }
    }

    fn next(&mut self) -> Option<char> {
        let b = *self.text.as_bytes().get(self.pos)?;
        if b < 0x80 {
            self.pos += 1;
            Some(b as char)
        } else {
            let ch = self.rest().chars().next()?;
            self.pos += ch.len_utf8();
            Some(ch)
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn take(&mut self, expected: char) -> Result<()> {
        match self.next() {
            Some(ch) if ch == expected => Ok(()),
            _ => bail!("expected `{expected}` in a frontmatter value"),
        }
    }

    /// Skip the optional single space a writer puts after a comma or a colon.
    fn skip_optional_space(&mut self) {
        if self.text.as_bytes().get(self.pos) == Some(&b' ') {
            self.pos += 1;
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
    if !cursor.rest().starts_with(text) {
        bail!("frontmatter literal `{text}` is misspelled");
    }
    cursor.pos += text.len();
    Ok(value)
}

fn parse_number(cursor: &mut Cursor<'_>) -> Result<Value> {
    let bytes = cursor.text.as_bytes();
    let start = cursor.pos;
    if bytes.get(cursor.pos) == Some(&b'-') {
        cursor.pos += 1;
    }
    let mut digits = 0usize;
    while let Some(&b) = bytes.get(cursor.pos) {
        if b.is_ascii_digit() {
            digits += 1;
            cursor.pos += 1;
        } else {
            break;
        }
    }
    if digits == 0 {
        bail!("frontmatter number has no digits");
    }
    let mut is_float = false;
    if bytes.get(cursor.pos) == Some(&b'.') {
        is_float = true;
        cursor.pos += 1;
        let mut fraction = 0usize;
        while let Some(&b) = bytes.get(cursor.pos) {
            if b.is_ascii_digit() {
                fraction += 1;
                cursor.pos += 1;
            } else {
                break;
            }
        }
        if fraction == 0 {
            bail!("frontmatter number has no fraction digits");
        }
    }
    if matches!(bytes.get(cursor.pos), Some(b'e' | b'E')) {
        is_float = true;
        cursor.pos += 1;
        if matches!(bytes.get(cursor.pos), Some(b'-' | b'+')) {
            cursor.pos += 1;
        }
        let mut exponent = 0usize;
        while let Some(&b) = bytes.get(cursor.pos) {
            if b.is_ascii_digit() {
                exponent += 1;
                cursor.pos += 1;
            } else {
                break;
            }
        }
        if exponent == 0 {
            bail!("frontmatter number has no exponent digits");
        }
    }
    let slice = &cursor.text[start..cursor.pos];
    if is_float {
        let value: f32 = slice
            .parse()
            .map_err(|_| anyhow!("frontmatter number `{slice}` is not a finite decimal"))?;
        if !value.is_finite() {
            bail!("frontmatter number `{slice}` is not finite");
        }
        Ok(Value::Float(value))
    } else {
        let value: i64 = slice
            .parse()
            .map_err(|_| anyhow!("frontmatter integer `{slice}` is out of range"))?;
        Ok(Value::Int(value))
    }
}

fn parse_string(cursor: &mut Cursor<'_>) -> Result<Value> {
    cursor.take('"')?;
    let bytes = cursor.text.as_bytes();
    let start = cursor.pos;
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            match b {
                b'"' => {
                    cursor.pos = i + 1;
                    return Ok(Value::Str(cursor.text[start..i].to_string()));
                }
                b'\\' => break,
                _ if b < 0x20 || b == 0x7f => {
                    bail!("frontmatter string holds a bare control character");
                }
                _ => i += 1,
            }
        } else {
            let Some(ch) = cursor.text[i..].chars().next() else {
                break;
            };
            if is_forbidden_raw(ch) {
                bail!("frontmatter string holds a bare control character");
            }
            i += ch.len_utf8();
        }
    }
    let mut text = String::with_capacity(i.saturating_sub(start) + 16);
    text.push_str(&cursor.text[start..i]);
    cursor.pos = i;
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
    if cursor.peek() == Some(']') {
        cursor.next();
        return Ok(Value::List(Vec::new()));
    }
    if cursor.peek() == Some('{') {
        let mut maps: Vec<Vec<(String, Value)>> = Vec::new();
        loop {
            if maps.len() >= MAX_LIST_ITEMS {
                bail!("frontmatter list has more than {MAX_LIST_ITEMS} items");
            }
            if cursor.peek() == Some('{') {
                maps.push(parse_map_entries(cursor, depth + 1)?);
                cursor.skip_optional_space();
                match cursor.next() {
                    Some(',') => cursor.skip_optional_space(),
                    Some(']') => return Ok(Value::MapList(maps)),
                    _ => bail!("frontmatter list is not closed with `]`"),
                }
            } else {
                let mut items: Vec<Value> = maps.into_iter().map(|entries| Value::MapList(vec![entries])).collect();
                return parse_scalar_list_rest(cursor, depth, &mut items);
            }
        }
    }
    let mut items = Vec::new();
    parse_scalar_list_rest(cursor, depth, &mut items)
}

fn parse_scalar_list_rest(cursor: &mut Cursor<'_>, depth: usize, items: &mut Vec<Value>) -> Result<Value> {
    loop {
        if items.len() >= MAX_LIST_ITEMS {
            bail!("frontmatter list has more than {MAX_LIST_ITEMS} items");
        }
        items.push(parse_value(cursor, depth + 1)?);
        cursor.skip_optional_space();
        match cursor.next() {
            Some(',') => cursor.skip_optional_space(),
            Some(']') => return Ok(Value::List(std::mem::take(items))),
            _ => bail!("frontmatter list is not closed with `]`"),
        }
    }
}

fn parse_map(cursor: &mut Cursor<'_>, depth: usize) -> Result<Value> {
    Ok(Value::MapList(vec![parse_map_entries(cursor, depth)?]))
}

fn parse_map_entries(cursor: &mut Cursor<'_>, depth: usize) -> Result<Vec<(String, Value)>> {
    cursor.take('{')?;
    let mut entries: Vec<(String, Value)> = Vec::new();
    if cursor.peek() == Some('}') {
        cursor.next();
        return Ok(entries);
    }
    loop {
        if entries.len() >= MAX_MAP_KEYS {
            bail!("frontmatter map has more than {MAX_MAP_KEYS} keys");
        }
        let key_start = cursor.pos;
        let bytes = cursor.text.as_bytes();
        while let Some(&b) = bytes.get(cursor.pos) {
            if b.is_ascii_alphanumeric() || b == b'_' {
                cursor.pos += 1;
            } else {
                break;
            }
        }
        let key_slice = &cursor.text[key_start..cursor.pos];
        if !is_key(key_slice) {
            bail!("frontmatter map key `{key_slice}` is not a lowercase identifier");
        }
        let key = key_slice.to_string();
        cursor.take(':')?;
        cursor.skip_optional_space();
        let value = parse_value(cursor, depth + 1)?;
        entries.push((key, value));
        cursor.skip_optional_space();
        match cursor.next() {
            Some(',') => cursor.skip_optional_space(),
            Some('}') => return Ok(entries),
            _ => bail!("frontmatter map is not closed with `}}`"),
        }
    }
}

pub(super) fn render_string(out: &mut String, text: &str) {
    use std::fmt::Write;
    if !text
        .bytes()
        .any(|b| b == b'"' || b == b'\\' || b < 0x20 || b == 0x7f || b == 0xc2)
    {
        out.push('"');
        out.push_str(text);
        out.push('"');
        return;
    }
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if is_forbidden_raw(ch) => {
                let _written = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// Render a page float in its shortest exact form (`0.8`, `0.25`, `1.0`).
pub(super) fn write_float(out: &mut String, number: f32) {
    use std::fmt::Write;
    let start = out.len();
    let _written = write!(out, "{number}");
    let written = &out[start..];
    if !written.contains('.') && !written.contains('e') {
        out.push_str(".0");
    }
}

/// Parse just the `id` field value when `line` is `id: "..."`.
pub(super) fn parse_id_field_value(line: &str) -> Option<String> {
    let raw = line.strip_prefix("id:")?;
    let raw = raw.strip_prefix(' ').unwrap_or(raw);
    if raw.is_empty() {
        return None;
    }
    let mut cursor = Cursor::new(raw);
    let Ok(Value::Str(id)) = parse_string(&mut cursor) else {
        return None;
    };
    if !cursor.at_end() {
        return None;
    }
    Some(id)
}

fn is_forbidden_raw(ch: char) -> bool {
    ch.is_control() || ch == '\u{7f}'
}
