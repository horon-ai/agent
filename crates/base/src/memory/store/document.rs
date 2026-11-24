//! The on-disk page document: frontmatter plus body, and the path a page is
//! stored at.
//!
//! The document is deliberately explicit. Every field the store writes is
//! checked on the way in, and every field is required to have the type the store
//! wrote, so a hand-edited page either parses completely or is reported as
//! malformed with the field name. Links and reinforcements use the inline
//! map-list form of the frontmatter dialect.

use super::frontmatter::{Document, Value};
use anyhow::{Result, bail};
use horon_agent_memory_types::page::{
    MemoryPage, PAGE_TITLE_MAX_CHARS, PageId, PageKind, PageLink, PageLinkKind, is_iso_date, validate_project_slug,
};
use horon_agent_memory_types::{MemoryCategory, Reinforcement, TrustLevel};
use horon_agent_time::Timestamp;
use std::path::{Path, PathBuf};

/// Longest accepted page body, in bytes.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Most tags a page may carry.
pub const MAX_TAGS: usize = 64;
/// Longest accepted tag, in characters.
pub const MAX_TAG_CHARS: usize = 64;
/// Most entities a page may carry.
pub const MAX_ENTITIES: usize = 128;
/// Longest accepted entity name, in characters.
pub const MAX_ENTITY_CHARS: usize = 128;
/// Most links a page may carry.
pub const MAX_LINKS: usize = 128;
/// Most reinforcement records a page may carry.
pub const MAX_REINFORCEMENTS: usize = 256;

const KNOWN_KEYS: [&str; 21] = [
    "id",
    "title",
    "kind",
    "category",
    "project",
    "tags",
    "entities",
    "links",
    "valid_from",
    "valid_to",
    "confidence",
    "source",
    "created_at",
    "updated_at",
    "access_count",
    "trust",
    "strength",
    "active",
    "superseded_by",
    "reinforcements",
    "schema",
];

/// Frontmatter schema number, so a future field can be detected instead of
/// guessed at.
pub const PAGE_SCHEMA: i64 = 1;

/// Render a page as its file text.
#[cfg(test)]
pub fn encode(page: &MemoryPage) -> Result<String> {
    validate(page)?;
    encode_validated(page)
}

/// Render a page whose invariants were already checked by [`validate`].
pub(super) fn encode_validated(page: &MemoryPage) -> Result<String> {
    use std::fmt::Write;

    let mut out = String::with_capacity(512 + page.body.len());
    out.push_str("---\n");

    writeln!(out, "schema: {PAGE_SCHEMA}")?;
    out.push_str("id: ");
    super::frontmatter::render_string(&mut out, page.id.as_str());
    out.push_str("\ntitle: ");
    super::frontmatter::render_string(&mut out, &page.title);
    out.push_str("\nkind: ");
    super::frontmatter::render_string(&mut out, page.kind.as_str());
    out.push_str("\ncategory: ");
    let category_text = page.category.to_string();
    super::frontmatter::render_string(&mut out, &category_text);
    out.push_str("\nproject: ");
    render_optional_str(&mut out, page.project.as_deref());
    out.push_str("\ntags: ");
    render_str_list(&mut out, &page.tags);
    out.push_str("\nentities: ");
    render_str_list(&mut out, &page.entities);
    out.push_str("\nlinks: ");
    render_links(&mut out, &page.links);
    out.push_str("\nvalid_from: ");
    render_optional_str(&mut out, page.valid_from.as_deref());
    out.push_str("\nvalid_to: ");
    render_optional_str(&mut out, page.valid_to.as_deref());
    out.push_str("\nconfidence: ");
    super::frontmatter::write_float(&mut out, page.confidence);
    out.push_str("\nsource: ");
    render_optional_str(&mut out, page.source.as_deref());
    out.push_str("\ncreated_at: ");
    let created_str = page.created_at.to_string();
    super::frontmatter::render_string(&mut out, &created_str);
    out.push_str("\nupdated_at: ");
    let updated_str = page.updated_at.to_string();
    super::frontmatter::render_string(&mut out, &updated_str);
    writeln!(out, "\naccess_count: {}", page.access_count)?;
    out.push_str("trust: ");
    super::frontmatter::render_string(&mut out, trust_str(page.trust));
    writeln!(out, "\nstrength: {}", page.strength)?;
    out.push_str("active: ");
    out.push_str(if page.active { "true" } else { "false" });
    out.push_str("\nsuperseded_by: ");
    render_optional_str(&mut out, page.superseded_by.as_deref());
    out.push_str("\nreinforcements: ");
    render_reinforcements(&mut out, &page.reinforcements)?;
    out.push_str("\n---\n");
    out.push_str(&page.body);
    Ok(out)
}

fn render_optional_str(out: &mut String, value: Option<&str>) {
    match value {
        Some(text) => super::frontmatter::render_string(out, text),
        None => out.push_str("null"),
    }
}

fn render_str_list(out: &mut String, values: &[String]) {
    out.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        super::frontmatter::render_string(out, value);
    }
    out.push(']');
}

fn render_links(out: &mut String, links: &[PageLink]) {
    out.push('[');
    for (index, link) in links.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str("{kind: ");
        super::frontmatter::render_string(out, link.kind.as_str());
        out.push_str(", dst: ");
        super::frontmatter::render_string(out, link.dst.as_str());
        if let Some(weight) = link.weight {
            out.push_str(", weight: ");
            super::frontmatter::write_float(out, weight);
        }
        out.push('}');
    }
    out.push(']');
}

fn render_reinforcements(out: &mut String, reinforcements: &[Reinforcement]) -> std::fmt::Result {
    use std::fmt::Write;
    out.push('[');
    for (index, item) in reinforcements.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str("{session: ");
        super::frontmatter::render_string(out, &item.session_id);
        let msg_idx = i64::try_from(item.message_index).unwrap_or(i64::MAX);
        write!(out, ", message_index: {msg_idx}, timestamp: ")?;
        let ts_str = item.timestamp.to_string();
        super::frontmatter::render_string(out, &ts_str);
        out.push('}');
    }
    out.push(']');
    Ok(())
}

fn trust_str(trust: TrustLevel) -> &'static str {
    match trust {
        TrustLevel::High => "high",
        TrustLevel::Medium => "medium",
        TrustLevel::Low => "low",
    }
}

fn parse_trust(text: &str) -> Result<TrustLevel> {
    match text {
        "high" => Ok(TrustLevel::High),
        "medium" => Ok(TrustLevel::Medium),
        "low" => Ok(TrustLevel::Low),
        other => bail!("frontmatter `trust` must be high, medium or low, not `{other}`"),
    }
}

/// Parse a page file. `file_stem` is the file's name without `.md`, used as the
/// id when the frontmatter carries no `id`; `fallback_time` stamps pages whose
/// frontmatter carries no timestamps (a page written by hand).
pub fn decode(text: &str, file_stem: &str, fallback_time: Timestamp) -> Result<MemoryPage> {
    let mut document = Document::parse(text)?;
    for (key, _) in &document.fields {
        if !KNOWN_KEYS.contains(&key.as_str()) {
            bail!("frontmatter field `{key}` is not part of the page schema");
        }
    }
    match document.get("schema") {
        None | Some(Value::Int(PAGE_SCHEMA)) => {}
        Some(Value::Int(schema)) => {
            bail!("page schema {schema} is not supported (this build writes {PAGE_SCHEMA})");
        }
        Some(_) => bail!("frontmatter `schema` must be integer {PAGE_SCHEMA}"),
    }
    let id = match document.get_str("id") {
        Some(raw) => PageId::parse(raw)?,
        None => PageId::from_file_stem(file_stem)?,
    };
    let title = take_string_field(&mut document, "title")?.unwrap_or_else(String::new);
    let kind = match document.get_str("kind") {
        Some(raw) => {
            PageKind::parse(raw).ok_or_else(|| anyhow::anyhow!("frontmatter `kind` names no known kind: `{raw}`"))?
        }
        None => match document.get("kind") {
            None | Some(Value::Null) => bail!("page has no `kind` field"),
            Some(_) => bail!("frontmatter `kind` must be a string or null"),
        },
    };
    let category = match document.get("category") {
        None | Some(Value::Null) => kind.default_category(),
        Some(Value::Str(raw)) => {
            if raw.is_empty() {
                kind.default_category()
            } else {
                raw.parse::<MemoryCategory>()?
            }
        }
        Some(_) => bail!("frontmatter `category` must be a string or null"),
    };
    let page = MemoryPage {
        id,
        kind,
        category,
        title,
        tags: take_string_list_field(&mut document, "tags")?,
        entities: take_string_list_field(&mut document, "entities")?,
        links: parse_links(&document)?,
        project: take_string_field(&mut document, "project")?,
        valid_from: take_string_field(&mut document, "valid_from")?,
        valid_to: take_string_field(&mut document, "valid_to")?,
        confidence: float_field(&document, "confidence")?.unwrap_or(1.0),
        source: take_string_field(&mut document, "source")?,
        created_at: timestamp_field(&document, "created_at")?.unwrap_or(fallback_time),
        updated_at: timestamp_field(&document, "updated_at")?.unwrap_or(fallback_time),
        access_count: u32_field(&document, "access_count")?.unwrap_or(0),
        trust: match document.get("trust") {
            None | Some(Value::Null) => TrustLevel::default(),
            Some(Value::Str(raw)) => parse_trust(raw)?,
            Some(_) => bail!("frontmatter `trust` must be a string or null"),
        },
        strength: u32_field(&document, "strength")?.unwrap_or(1),
        active: bool_field(&document, "active")?.unwrap_or(true),
        superseded_by: take_string_field(&mut document, "superseded_by")?,
        reinforcements: take_reinforcements(&mut document)?,
        body: document.body,
    };
    validate(&page)?;
    Ok(page)
}

fn parse_links(document: &Document) -> Result<Vec<PageLink>> {
    let Some(items) = document.get_map_list("links") else {
        return Ok(Vec::new());
    };
    let mut links = Vec::with_capacity(items.len());
    for entries in items {
        let mut kind = None;
        let mut dst = None;
        let mut weight = None;
        for (key, value) in entries {
            match (key.as_str(), value) {
                ("kind", Value::Str(text)) => {
                    kind = Some(
                        PageLinkKind::parse(text)
                            .ok_or_else(|| anyhow::anyhow!("page link kind `{text}` is unknown"))?,
                    );
                }
                ("dst", Value::Str(text)) => dst = Some(PageId::parse(text)?),
                ("weight", Value::Float(number)) => weight = Some(*number),
                ("weight", Value::Int(number)) => weight = Some(*number as f32),
                (other, _) => bail!("page link carries the unknown field `{other}`"),
            }
        }
        let Some(kind) = kind else {
            bail!("page link has no `kind`");
        };
        let Some(dst) = dst else {
            bail!("page link has no `dst`");
        };
        links.push(PageLink { kind, dst, weight });
    }
    Ok(links)
}

fn take_reinforcements(document: &mut Document) -> Result<Vec<Reinforcement>> {
    let Some(Value::MapList(items)) = document.take("reinforcements") else {
        return Ok(Vec::new());
    };
    let mut reinforcements = Vec::with_capacity(items.len());
    for entries in items {
        let mut session = None;
        let mut message_index = None;
        let mut timestamp = None;
        for (key, value) in entries {
            match (key.as_str(), value) {
                ("session", Value::Str(text)) => session = Some(text),
                ("message_index", Value::Int(number)) => {
                    message_index = Some(u64::try_from(number).unwrap_or(0));
                }
                ("timestamp", Value::Str(text)) => timestamp = Some(text.parse::<Timestamp>()?),
                (other, _) => bail!("reinforcement carries the unknown field `{other}`"),
            }
        }
        let (Some(session_id), Some(message_index), Some(timestamp)) = (session, message_index, timestamp) else {
            bail!("reinforcement needs `session`, `message_index` and `timestamp`");
        };
        reinforcements.push(Reinforcement {
            session_id,
            message_index: usize::try_from(message_index).unwrap_or(usize::MAX),
            timestamp,
        });
    }
    Ok(reinforcements)
}

fn value_field<'a>(document: &'a Document, key: &str) -> Result<Option<&'a Value>> {
    match document.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(value)),
    }
}

fn take_string_field(document: &mut Document, key: &str) -> Result<Option<String>> {
    match document.take(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Str(text)) => Ok(Some(text)),
        Some(_) => bail!("frontmatter `{key}` must be a string or null"),
    }
}

fn take_string_list_field(document: &mut Document, key: &str) -> Result<Vec<String>> {
    match document.take(key) {
        None => Ok(Vec::new()),
        Some(Value::List(items)) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Str(text) => values.push(text),
                    _ => bail!("frontmatter `{key}` must hold strings"),
                }
            }
            Ok(values)
        }
        Some(_) => bail!("frontmatter `{key}` must be a list"),
    }
}

fn u32_field(document: &Document, key: &str) -> Result<Option<u32>> {
    match value_field(document, key)? {
        None => Ok(None),
        Some(Value::Int(number)) => u32::try_from(*number)
            .map(Some)
            .map_err(|_| anyhow::anyhow!("frontmatter `{key}` is out of range")),
        Some(_) => bail!("frontmatter `{key}` must be an integer"),
    }
}

fn float_field(document: &Document, key: &str) -> Result<Option<f32>> {
    match value_field(document, key)? {
        None => Ok(None),
        Some(Value::Float(number)) => Ok(Some(*number)),
        Some(Value::Int(number)) => Ok(Some(*number as f32)),
        Some(_) => bail!("frontmatter `{key}` must be a number"),
    }
}

fn bool_field(document: &Document, key: &str) -> Result<Option<bool>> {
    match value_field(document, key)? {
        None => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => bail!("frontmatter `{key}` must be true or false"),
    }
}

fn timestamp_field(document: &Document, key: &str) -> Result<Option<Timestamp>> {
    match value_field(document, key)? {
        None => Ok(None),
        Some(Value::Str(text)) => {
            Ok(Some(text.parse::<Timestamp>().map_err(|error| {
                anyhow::anyhow!("frontmatter `{key}` is not a timestamp ({error})")
            })?))
        }
        Some(_) => bail!("frontmatter `{key}` must be a timestamp string"),
    }
}

/// Check every constraint the store relies on.
///
/// Called on encode and after decode, so a page that passed decode cannot fail
/// the same checks again and a hand-built page cannot be written malformed.
pub fn validate(page: &MemoryPage) -> Result<()> {
    if page.title.trim().is_empty() {
        bail!("page title is empty");
    }
    if page.title.chars().count() > PAGE_TITLE_MAX_CHARS {
        bail!("page title is over {PAGE_TITLE_MAX_CHARS} characters");
    }
    if page.title.contains('\0') {
        bail!("page title holds a NUL byte");
    }
    if page.body.len() > MAX_BODY_BYTES {
        bail!("page body is over {MAX_BODY_BYTES} bytes");
    }
    if page.body.contains('\0') {
        bail!("page body holds a NUL byte");
    }
    if page.tags.len() > MAX_TAGS {
        bail!("page carries more than {MAX_TAGS} tags");
    }
    for tag in &page.tags {
        if tag.trim().is_empty() || tag.chars().count() > MAX_TAG_CHARS || tag.chars().any(char::is_control) {
            bail!("page tag `{tag}` is empty, too long, or holds a control character");
        }
    }
    if page.entities.len() > MAX_ENTITIES {
        bail!("page carries more than {MAX_ENTITIES} entities");
    }
    for entity in &page.entities {
        if entity.trim().is_empty() || entity.chars().count() > MAX_ENTITY_CHARS || entity.chars().any(char::is_control)
        {
            bail!("page entity `{entity}` is empty, too long, or holds a control character");
        }
    }
    if let Some(project) = &page.project {
        validate_project_slug(project).map_err(|error| anyhow::anyhow!("page project: {error}"))?;
    }
    for (key, value) in [("valid_from", &page.valid_from), ("valid_to", &page.valid_to)] {
        if let Some(date) = value
            && !is_iso_date(date)
        {
            bail!("page `{key}` must be a YYYY-MM-DD date, not `{date}`");
        }
    }
    if let (Some(from), Some(to)) = (&page.valid_from, &page.valid_to)
        && from > to
    {
        bail!("page `valid_from` {from} is after `valid_to` {to}");
    }
    if !page.confidence.is_finite() || !(0.0..=1.0).contains(&page.confidence) {
        bail!("page confidence must be a finite number in 0..=1");
    }
    if let Some(source) = &page.source
        && source.chars().any(char::is_control)
    {
        bail!("page source holds a control character");
    }
    if page.links.len() > MAX_LINKS {
        bail!("page carries more than {MAX_LINKS} links");
    }
    let mut seen: Vec<(PageLinkKind, &str)> = Vec::with_capacity(page.links.len());
    for link in &page.links {
        if link.dst == page.id {
            bail!("page links to itself");
        }
        if seen.contains(&(link.kind, link.dst.as_str())) {
            bail!("page repeats the link {} -> {}", link.kind, link.dst);
        }
        seen.push((link.kind, link.dst.as_str()));
        match link.kind {
            PageLinkKind::RelatesTo => match link.weight {
                Some(weight) if weight.is_finite() && (0.0..=1.0).contains(&weight) => {}
                _ => bail!("page `relates_to` link needs a weight in 0..=1"),
            },
            _ => {
                if link.weight.is_some() {
                    bail!("page `{}` link must not carry a weight", link.kind);
                }
            }
        }
    }
    if let Some(superseded_by) = &page.superseded_by {
        PageId::parse(superseded_by)?;
        if page.active {
            bail!("page is active but names a `superseded_by` page");
        }
    }
    if page.reinforcements.len() > MAX_REINFORCEMENTS {
        bail!("page carries more than {MAX_REINFORCEMENTS} reinforcements");
    }
    for reinforcement in &page.reinforcements {
        if reinforcement.session_id.chars().any(char::is_control) {
            bail!("reinforcement session id holds a control character");
        }
    }
    if let MemoryCategory::Custom(name) = &page.category {
        let builtin = matches!(
            name.as_str(),
            "fact" | "preference" | "entity" | "correction" | "lesson"
        );
        if name.is_empty()
            || builtin
            || !name
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
        {
            bail!("custom category `{name}` must be lowercase letters, digits, `_` or `-`, and not a built-in name");
        }
    }
    Ok(())
}

/// The path of `page` relative to the wiki root.
///
/// Kind decides the first directory; a dated event nests by the creation year
/// and month; a project-scoped page nests by its project slug. Every component
/// is validated above, so the result is always inside the wiki root.
pub fn relative_path(page: &MemoryPage) -> Result<PathBuf> {
    let directory = page.kind.directory();
    let file = page.id.file_name();
    if page.kind == PageKind::Event {
        let calendar = page.created_at.calendar_utc();
        let month_key = calendar.month_key();
        let Some((year, month)) = month_key.rsplit_once('-') else {
            bail!("event page has no usable creation month");
        };
        if year.is_empty() || month.len() != 2 {
            bail!("event page has no usable creation month");
        }
        return Ok(Path::new(directory).join(year).join(month).join(file));
    }
    match &page.project {
        Some(project) => Ok(Path::new(directory).join(project).join(file)),
        None => Ok(Path::new(directory).join(file)),
    }
}
