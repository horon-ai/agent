//! Page model shared by the markdown memory wiki and its callers.
//!
//! The wiki keeps one markdown file per memory page. This module owns the
//! values that file carries: the page id and its reversible file name, the page
//! kind (which decides the directory and how much authority a page has in
//! ranking), the typed links between pages, and the full page record. It holds
//! no I/O and no `rusqlite`: the store that reads and writes the files lives in
//! the base crate.
//!
//! Every non-derived field of [`MemoryEntry`] survives the trip through a page,
//! because the SQLite index is derived and must be rebuildable from the markdown
//! alone. Embeddings are the one exception: they are derived data and are not
//! stored in the wiki.

use crate::graph::EdgeKind;
use crate::{MemoryCategory, MemoryEntry, Reinforcement, TrustLevel};
use horon_agent_time::Timestamp;
use std::fmt;

/// Longest accepted page id, in bytes. The id has to fit a file name with room
/// for the reversible encoding and the `.md` suffix.
pub const PAGE_ID_MAX_BYTES: usize = 256;
/// Longest accepted page title, in characters.
pub const PAGE_TITLE_MAX_CHARS: usize = 400;
/// Longest accepted project slug, in characters.
pub const PROJECT_SLUG_MAX_CHARS: usize = 64;

/// A page id that is safe to use as one file name component.
///
/// Ids come from several producers (`mem_<ms>_<rand>`, `goal:<uuid>`,
/// `skill:<name>`), so the file name is a reversible encoding of the id rather
/// than the id itself. Only the plain form is stored as-is: lowercase letters,
/// digits, `.`, `_` and `-`, starting with a letter or digit. Everything else —
/// uppercase, `:`, spaces, non-ASCII — is encoded as `~` followed by the hex of
/// the id's UTF-8 bytes. `~` is outside the plain alphabet, so the mapping is
/// unambiguous, survives a case-insensitive filesystem without collisions, and
/// can never contain a path separator or a `..` component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageId(String);

impl PageId {
    /// Validate `raw` as a page id.
    ///
    /// Rejected: empty ids, ids over [`PAGE_ID_MAX_BYTES`], control characters,
    /// NUL, `/` and `\` (path separators), and a leading `.` (so `.` and `..`
    /// never reach the filesystem). An id that passes validation still gets the
    /// encoded file name when it is not plain.
    pub fn parse(raw: &str) -> Result<Self, PageIdError> {
        if raw.is_empty() {
            return Err(PageIdError::Empty);
        }
        if raw.len() > PAGE_ID_MAX_BYTES {
            return Err(PageIdError::TooLong { len: raw.len() });
        }
        if raw.starts_with('.') {
            return Err(PageIdError::LeadingDot);
        }
        for ch in raw.chars() {
            if ch == '/' || ch == '\\' {
                return Err(PageIdError::Separator);
            }
            if ch.is_control() {
                return Err(PageIdError::Control);
            }
        }
        Ok(Self(raw.to_string()))
    }

    /// The id as its callers know it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The one-component file name (without `.md`) that stores this id.
    pub fn file_stem(&self) -> String {
        if is_plain_stem(&self.0) {
            self.0.clone()
        } else {
            format!("~{}", hex_encode(self.0.as_bytes()))
        }
    }

    /// Recover the id from a file stem produced by [`PageId::file_stem`].
    ///
    /// The plain form is accepted only when it is canonical (it would have been
    /// written plainly); an encoded stem must decode to an id that is not
    /// plain, so a hand-placed alias never maps two files to one id.
    pub fn from_file_stem(stem: &str) -> Result<Self, PageIdError> {
        let Some(encoded) = stem.strip_prefix('~') else {
            if !is_plain_stem(stem) {
                return Err(PageIdError::NotCanonical);
            }
            return Self::parse(stem);
        };
        let bytes = hex_decode(encoded).ok_or(PageIdError::BadEncoding)?;
        let raw = String::from_utf8(bytes).map_err(|_| PageIdError::BadEncoding)?;
        if is_plain_stem(&raw) {
            return Err(PageIdError::NotCanonical);
        }
        Self::parse(&raw)
    }

    /// The file name this id is stored under.
    pub fn file_name(&self) -> String {
        format!("{}.md", self.file_stem())
    }
}

impl fmt::Display for PageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::str::FromStr for PageId {
    type Err = PageIdError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

/// Why an id was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageIdError {
    /// The id is empty.
    Empty,
    /// The id is longer than [`PAGE_ID_MAX_BYTES`].
    TooLong { len: usize },
    /// The id starts with `.`, which would name `.` or `..` or a hidden file.
    LeadingDot,
    /// The id contains a path separator.
    Separator,
    /// The id contains a control character.
    Control,
    /// A file stem is not in the canonical form this module writes.
    NotCanonical,
    /// An encoded file stem does not decode to a UTF-8 id.
    BadEncoding,
}

impl fmt::Display for PageIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("page id is empty"),
            Self::TooLong { len } => write!(
                formatter,
                "page id is {len} bytes, over the {} byte limit",
                PAGE_ID_MAX_BYTES
            ),
            Self::LeadingDot => formatter.write_str("page id must not start with `.`"),
            Self::Separator => formatter.write_str("page id must not contain `/` or `\\`"),
            Self::Control => formatter.write_str("page id must not contain control characters"),
            Self::NotCanonical => formatter.write_str("page file stem is not in canonical form"),
            Self::BadEncoding => formatter.write_str("page file stem is not a valid encoded id"),
        }
    }
}

impl std::error::Error for PageIdError {}

/// Whether `id` can be used verbatim as a file stem.
fn is_plain_stem(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_' || ch == '-')
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        out.push(high << 4 | low);
    }
    Some(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// What a page is about. The kind decides the wiki directory and the kind's
/// authority in ranking; the memory category a page came from is kept in its own
/// field so the mapping in both directions stays lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PageKind {
    /// A stable preference of the user.
    Preference,
    /// A person the user works with.
    Person,
    /// A dated event.
    Event,
    /// A statement about the world that can go stale.
    Fact,
    /// A procedure worth repeating.
    Procedure,
    /// A user correction of an earlier statement.
    Correction,
    /// A lesson learned while working.
    Lesson,
    /// A named organization, project or other entity.
    Entity,
    /// Any category the memory model does not name.
    Custom,
}

impl PageKind {
    /// Every kind in the fixed order used by listings and tests.
    pub const ALL: [Self; 9] = [
        Self::Preference,
        Self::Person,
        Self::Event,
        Self::Fact,
        Self::Procedure,
        Self::Correction,
        Self::Lesson,
        Self::Entity,
        Self::Custom,
    ];

    /// The frontmatter and index text of the kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::Person => "person",
            Self::Event => "event",
            Self::Fact => "fact",
            Self::Procedure => "procedure",
            Self::Correction => "correction",
            Self::Lesson => "lesson",
            Self::Entity => "entity",
            Self::Custom => "custom",
        }
    }

    /// Parse the frontmatter and index text of the kind. Exact, lowercase only.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == raw)
    }

    /// The wiki directory this kind is stored under.
    pub fn directory(self) -> &'static str {
        match self {
            Self::Preference => "preferences",
            Self::Person => "people",
            Self::Event => "events",
            Self::Fact => "facts",
            Self::Procedure => "procedures",
            Self::Correction => "corrections",
            Self::Lesson => "lessons",
            Self::Entity => "entities",
            Self::Custom => "custom",
        }
    }

    /// The kind a memory category lands in.
    pub fn from_category(category: &MemoryCategory) -> Self {
        match category {
            MemoryCategory::Fact => Self::Fact,
            MemoryCategory::Preference => Self::Preference,
            MemoryCategory::Entity => Self::Entity,
            MemoryCategory::Correction => Self::Correction,
            MemoryCategory::Lesson => Self::Lesson,
            MemoryCategory::Custom(_) => Self::Custom,
        }
    }

    /// The category a page falls back to when its frontmatter has no `category`
    /// field, e.g. a page written by hand.
    pub fn default_category(self) -> MemoryCategory {
        match self {
            Self::Preference => MemoryCategory::Preference,
            Self::Person | Self::Entity => MemoryCategory::Entity,
            Self::Event | Self::Fact => MemoryCategory::Fact,
            Self::Procedure | Self::Lesson => MemoryCategory::Lesson,
            Self::Correction => MemoryCategory::Correction,
            Self::Custom => MemoryCategory::Custom("custom".to_string()),
        }
    }

    /// Sorting weight for ranking, higher first.
    ///
    /// Preferences, people and corrections outrank facts, procedures and
    /// lessons, which outrank dated events: "what the user likes", "who someone
    /// is" and "what the user corrected" stay useful longest, while an event is
    /// only relevant while it is fresh. The weight feeds the authority stream
    /// that the fusion ranks with RRF, not a hard filter.
    pub fn authority(self) -> u8 {
        match self {
            Self::Preference | Self::Person | Self::Correction => 5,
            Self::Procedure | Self::Lesson | Self::Entity => 4,
            Self::Fact | Self::Custom => 3,
            Self::Event => 1,
        }
    }
}

impl fmt::Display for PageKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A typed link from one page to another, stored in the source page's
/// frontmatter.
#[derive(Debug, Clone, PartialEq)]
pub struct PageLink {
    /// Relationship to the destination page.
    pub kind: PageLinkKind,
    /// Destination page id.
    pub dst: PageId,
    /// Strength for [`PageLinkKind::RelatesTo`] in `0.0..=1.0`. Always `None`
    /// for the other kinds.
    pub weight: Option<f32>,
}

impl PageLink {
    /// A `relates_to` link with the given weight.
    pub fn relates_to(dst: PageId, weight: f32) -> Self {
        Self {
            kind: PageLinkKind::RelatesTo,
            dst,
            weight: Some(weight),
        }
    }

    /// A link of `kind` with no weight.
    pub fn new(kind: PageLinkKind, dst: PageId) -> Self {
        Self {
            kind,
            dst,
            weight: None,
        }
    }

    /// The traversal weight of the link, matching the graph edge weights.
    pub fn traversal_weight(&self) -> f32 {
        match self.kind {
            PageLinkKind::RelatesTo => self.weight.unwrap_or(1.0),
            PageLinkKind::Supersedes => 0.9,
            PageLinkKind::Contradicts => 0.3,
            PageLinkKind::DerivedFrom => 0.7,
        }
    }
}

/// The typed links a page can carry. `HasTag` and `InCluster` are graph
/// bookkeeping and are not page links: tags live in the `tags` frontmatter
/// field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PageLinkKind {
    /// Semantic relationship with a weight.
    RelatesTo,
    /// This page replaces the destination.
    Supersedes,
    /// Both pages are kept and the conflict is flagged.
    Contradicts,
    /// This page was derived from the destination.
    DerivedFrom,
}

impl PageLinkKind {
    /// Every link kind in the fixed order used by listings and tests.
    pub const ALL: [Self; 4] = [Self::RelatesTo, Self::Supersedes, Self::Contradicts, Self::DerivedFrom];

    /// The frontmatter and index text of the link kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RelatesTo => "relates_to",
            Self::Supersedes => "supersedes",
            Self::Contradicts => "contradicts",
            Self::DerivedFrom => "derived_from",
        }
    }

    /// Parse the frontmatter and index text of the link kind.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == raw)
    }

    /// The graph edge a page link becomes.
    pub fn to_edge_kind(self, weight: Option<f32>) -> EdgeKind {
        match self {
            Self::RelatesTo => EdgeKind::RelatesTo {
                weight: weight.unwrap_or(1.0),
            },
            Self::Supersedes => EdgeKind::Supersedes,
            Self::Contradicts => EdgeKind::Contradicts,
            Self::DerivedFrom => EdgeKind::DerivedFrom,
        }
    }

    /// The page link a graph edge becomes, or `None` for tag and cluster edges.
    pub fn from_edge_kind(edge: &EdgeKind) -> Option<(Self, Option<f32>)> {
        match edge {
            EdgeKind::HasTag | EdgeKind::InCluster => None,
            EdgeKind::RelatesTo { weight } => Some((Self::RelatesTo, Some(*weight))),
            EdgeKind::Supersedes => Some((Self::Supersedes, None)),
            EdgeKind::Contradicts => Some((Self::Contradicts, None)),
            EdgeKind::DerivedFrom => Some((Self::DerivedFrom, None)),
        }
    }
}

impl fmt::Display for PageLinkKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One memory page: the unit the markdown wiki stores.
///
/// `body` is the memory content, `title` a human label derived from it (or set
/// by hand), and the remaining fields carry the provenance of the
/// [`MemoryEntry`] the page stands for. `project` scopes the page to a project
/// key; `None` means user-global.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryPage {
    /// Stable id, unique across kinds and projects.
    pub id: PageId,
    /// What the page is about.
    pub kind: PageKind,
    /// The memory category this page stands for. Kept apart from `kind` so a
    /// `Custom` category name survives a round trip.
    pub category: MemoryCategory,
    /// Human label; defaults to the first line of the body.
    pub title: String,
    /// Memory content.
    pub body: String,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Named entities this page is about.
    pub entities: Vec<String>,
    /// Typed links to other pages.
    pub links: Vec<PageLink>,
    /// Project key this page belongs to; `None` for user-global pages.
    pub project: Option<String>,
    /// First date the page is valid, `YYYY-MM-DD`.
    pub valid_from: Option<String>,
    /// Last date the page is valid, `YYYY-MM-DD`.
    pub valid_to: Option<String>,
    /// Confidence in `0.0..=1.0`.
    pub confidence: f32,
    /// Where the page came from, e.g. `conversation/2026-11-21`.
    pub source: Option<String>,
    /// When the memory was first stored.
    pub created_at: Timestamp,
    /// When the memory was last written.
    pub updated_at: Timestamp,
    /// How many times the memory was retrieved.
    pub access_count: u32,
    /// Trust level for this memory.
    pub trust: TrustLevel,
    /// Consolidation strength.
    pub strength: u32,
    /// Whether the page is current or was superseded.
    pub active: bool,
    /// Id of the page that superseded this one.
    pub superseded_by: Option<String>,
    /// Reinforcement breadcrumbs.
    pub reinforcements: Vec<Reinforcement>,
}

impl MemoryPage {
    /// A page with the defaults a freshly captured memory gets.
    pub fn draft(
        id: PageId,
        kind: PageKind,
        category: MemoryCategory,
        title: impl Into<String>,
        body: impl Into<String>,
        now: Timestamp,
    ) -> Self {
        Self {
            id,
            kind,
            category,
            title: title.into(),
            body: body.into(),
            tags: Vec::new(),
            entities: Vec::new(),
            links: Vec::new(),
            project: None,
            valid_from: None,
            valid_to: None,
            confidence: 1.0,
            source: None,
            created_at: now,
            updated_at: now,
            access_count: 0,
            trust: TrustLevel::default(),
            strength: 1,
            active: true,
            superseded_by: None,
            reinforcements: Vec::new(),
        }
    }

    /// Build a page from an existing memory entry, dropping the embedding.
    ///
    /// The body is the full entry content, so [`MemoryPage::to_entry`] returns
    /// the same content; the title is derived from the first line only as a
    /// label. Links come from the caller, because graph edges are not part of
    /// the entry.
    pub fn from_entry(entry: &MemoryEntry, project: Option<String>) -> Result<Self, PageIdError> {
        Ok(Self {
            id: PageId::parse(&entry.id)?,
            kind: PageKind::from_category(&entry.category),
            category: entry.category.clone(),
            title: derive_title(&entry.content),
            body: entry.content.clone(),
            tags: entry.tags.clone(),
            entities: Vec::new(),
            links: Vec::new(),
            project,
            valid_from: None,
            valid_to: None,
            confidence: entry.confidence,
            source: entry.source.clone(),
            created_at: entry.created_at,
            updated_at: entry.updated_at,
            access_count: entry.access_count,
            trust: entry.trust,
            strength: entry.strength,
            active: entry.active,
            superseded_by: entry.superseded_by.clone(),
            reinforcements: entry.reinforcements.clone(),
        })
    }

    /// The memory entry this page stands for.
    ///
    /// The embedding is `None`: embeddings are derived and are not kept in the
    /// wiki. `search_text` is refreshed from the content and tags.
    pub fn to_entry(&self) -> MemoryEntry {
        let mut entry = MemoryEntry::new(self.category.clone(), self.body.clone());
        entry.id = self.id.as_str().to_string();
        entry.tags = self.tags.clone();
        entry.refresh_search_text();
        entry.created_at = self.created_at;
        entry.updated_at = self.updated_at;
        entry.access_count = self.access_count;
        entry.source = self.source.clone();
        entry.trust = self.trust;
        entry.strength = self.strength;
        entry.active = self.active;
        entry.superseded_by = self.superseded_by.clone();
        entry.reinforcements = self.reinforcements.clone();
        entry.embedding = None;
        entry.embedding_model = None;
        entry.confidence = self.confidence;
        entry
    }

    /// Set the title to the label derived from the body.
    pub fn retitle_from_body(&mut self) {
        self.title = derive_title(&self.body);
    }

    /// Whether the page is inside its validity window at `at`.
    ///
    /// An absent bound is open: `valid_from: None` means "always started",
    /// `valid_to: None` means "still valid". Dates compare as `YYYY-MM-DD`
    /// text.
    pub fn is_valid_at(&self, at: Option<&str>) -> bool {
        let Some(at) = at else {
            return true;
        };
        if let Some(from) = &self.valid_from
            && at < from.as_str()
        {
            return false;
        }
        if let Some(to) = &self.valid_to
            && at > to.as_str()
        {
            return false;
        }
        true
    }
}

/// The label derived from a memory body: its first non-blank line, trimmed and
/// cut to [`PAGE_TITLE_MAX_CHARS`] characters. A body with no text becomes
/// `(untitled memory)` so a title always exists.
pub fn derive_title(body: &str) -> String {
    let line = body.lines().map(str::trim).find(|line| !line.is_empty());
    let Some(line) = line else {
        return "(untitled memory)".to_string();
    };
    truncate_chars(line, PAGE_TITLE_MAX_CHARS)
}

/// Cut `text` to at most `max` characters, without splitting a character.
pub fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((index, _)) => text[..index].to_string(),
        None => text.to_string(),
    }
}

/// Why a project slug was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectSlugError {
    /// The slug is empty.
    Empty,
    /// The slug is longer than [`PROJECT_SLUG_MAX_CHARS`].
    TooLong,
    /// The slug does not start with a lowercase letter or digit.
    BadStart,
    /// The slug holds a character outside lowercase letters, digits, `_` and `-`.
    BadCharacter,
}

impl fmt::Display for ProjectSlugError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("project slug is empty"),
            Self::TooLong => write!(formatter, "project slug is over {PROJECT_SLUG_MAX_CHARS} characters"),
            Self::BadStart => formatter.write_str("project slug must start with a lowercase letter or digit"),
            Self::BadCharacter => {
                formatter.write_str("project slug allows only lowercase letters, digits, `_` and `-`")
            }
        }
    }
}

impl std::error::Error for ProjectSlugError {}

/// Validate a project slug: lowercase letters, digits, `_` and `-`, starting
/// with a letter or digit, at most [`PROJECT_SLUG_MAX_CHARS`] characters. The
/// slug is one directory component, so this is also the path-traversal check for
/// project-scoped pages.
pub fn validate_project_slug(slug: &str) -> Result<(), ProjectSlugError> {
    if slug.is_empty() {
        return Err(ProjectSlugError::Empty);
    }
    if slug.chars().count() > PROJECT_SLUG_MAX_CHARS {
        return Err(ProjectSlugError::TooLong);
    }
    let mut chars = slug.chars();
    let Some(first) = chars.next() else {
        return Err(ProjectSlugError::Empty);
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(ProjectSlugError::BadStart);
    }
    if !chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-') {
        return Err(ProjectSlugError::BadCharacter);
    }
    Ok(())
}

/// Whether `text` is a `YYYY-MM-DD` date.
pub fn is_iso_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}
