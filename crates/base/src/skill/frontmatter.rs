//! Frontmatter-only parsing for skill discovery.
//!
//! A catalog entry needs the name, description, and allowed tools, all of
//! which live in the YAML frontmatter. Reading 4 KiB at a time up to a 16 KiB
//! cap finds the closing delimiter without touching a multi-megabyte body; a
//! file whose frontmatter is larger than the cap (or that never closes it) falls
//! back to one full read so the result is exactly what the whole-file parser
//! would have produced.
//!
//! A body that is not valid UTF-8 is not rejected here: discovery never decodes
//! past the closing delimiter, so such a skill still lists, and [`super::Skill::body`]
//! reports the encoding error when the body is actually needed.

use anyhow::Result;
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Size of one incremental read while hunting for the closing delimiter.
pub(crate) const FRONTMATTER_CHUNK: usize = 4 * 1024;

/// Upper bound on the frontmatter hunt before falling back to a full read.
pub(crate) const FRONTMATTER_MAX: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
pub(crate) struct SkillFrontmatter {
    pub(crate) name: String,
    pub(crate) description: String,
    #[serde(rename = "allowed-tools")]
    pub(crate) allowed_tools: Option<AllowedTools>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum AllowedTools {
    CommaDelimited(String),
    Sequence(Vec<String>),
}

impl AllowedTools {
    pub(crate) fn into_tools(self) -> Vec<String> {
        match self {
            Self::CommaDelimited(tools) => tools.split(',').map(|tool| tool.trim().to_string()).collect(),
            Self::Sequence(tools) => tools,
        }
    }
}

/// What one frontmatter read actually cost.
///
/// Discovery accumulates this per directory so a test can assert on its own
/// measurement: the process-wide counters are shared with every other test
/// running in parallel, which would make a delta assertion flaky.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadStats {
    pub(crate) files: u64,
    pub(crate) bytes: u64,
    /// Directories enumerated for this measurement. Discovery counts a scan
    /// here so a caller can prove a request enumerated a folder exactly once.
    pub(crate) dirs: u64,
}

impl ReadStats {
    fn one_file(bytes: usize) -> Self {
        Self {
            files: 1,
            bytes: bytes as u64,
            dirs: 0,
        }
    }

    pub(crate) fn add(&mut self, other: Self) {
        self.files += other.files;
        self.bytes += other.bytes;
        self.dirs += other.dirs;
    }
}

/// One full parse of a SKILL.md: its frontmatter and its trimmed body.
pub(crate) struct ParsedFrontmatter {
    pub(crate) frontmatter: SkillFrontmatter,
    pub(crate) body: String,
}

/// Parse YAML frontmatter from markdown.
pub(crate) fn parse_text(content: &str) -> Result<ParsedFrontmatter> {
    let content = content.trim();

    if !content.starts_with("---") {
        anyhow::bail!("Missing YAML frontmatter");
    }

    let rest = &content[3..];
    let end = rest
        .find("---")
        .ok_or_else(|| anyhow::anyhow!("Unclosed frontmatter"))?;

    let yaml = &rest[..end];
    let body = rest[end + 3..].trim().to_string();

    let frontmatter: SkillFrontmatter = serde_yaml::from_str(yaml)?;

    Ok(ParsedFrontmatter { frontmatter, body })
}

/// Read only the frontmatter of `path`, reporting what was read.
///
/// The process-wide counter is still bumped for every read: the structural gate
/// reads it from a single-threaded measurement. The returned [`ReadStats`] is
/// the caller's own measurement, which stays correct under parallel tests.
pub(crate) fn read_frontmatter(path: &Path) -> Result<(SkillFrontmatter, ReadStats)> {
    let (parsed, stats) = read_frontmatter_measured(path);
    horon_agent_perf_counters::record_skill_file_read(stats.bytes);
    Ok((parsed?, stats))
}

/// The same read, reporting its cost even when the parse fails.
///
/// A file whose YAML does not validate still had its bytes read, so the caller
/// gets the measurement alongside the error. Tests use this to assert their own
/// cost without depending on the process-wide counter.
pub(crate) fn read_frontmatter_measured(path: &Path) -> (Result<SkillFrontmatter>, ReadStats) {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => return (Err(error.into()), ReadStats::default()),
    };
    let mut buf = Vec::with_capacity(FRONTMATTER_MAX);
    let mut chunk = [0u8; FRONTMATTER_CHUNK];
    loop {
        let read = match file.read(&mut chunk) {
            Ok(read) => read,
            Err(error) => return (Err(error.into()), ReadStats::default()),
        };
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        match scan(&buf) {
            Ok(Some(frontmatter)) => return (Ok(frontmatter), ReadStats::one_file(buf.len())),
            Ok(None) => {}
            Err(error) => return (Err(error), ReadStats::one_file(buf.len())),
        }
        if buf.len() >= FRONTMATTER_MAX {
            break;
        }
    }

    if let Err(error) = file.read_to_end(&mut buf) {
        return (Err(error.into()), ReadStats::one_file(buf.len()));
    }
    let stats = ReadStats::one_file(buf.len());
    let parsed = String::from_utf8(buf)
        .map_err(|_| anyhow::anyhow!("stream did not contain valid UTF-8"))
        .and_then(|content| parse_text(&content).map(|parsed| parsed.frontmatter));
    (parsed, stats)
}

/// Try to parse the frontmatter from what has been read so far.
///
/// `Ok(None)` means the closing delimiter is not inside the buffer yet (or the
/// buffer ends mid-character); the caller reads another chunk. `Err` is a real
/// parse failure and is reported as-is, exactly like a full-file parse.
fn scan(buf: &[u8]) -> Result<Option<SkillFrontmatter>> {
    let Some(prefix) = frontmatter_prefix(buf) else {
        return Ok(None);
    };
    let Ok(text) = std::str::from_utf8(prefix) else {
        return Ok(None);
    };
    parse_text(text).map(|parsed| Some(parsed.frontmatter))
}

/// Slice from the opening marker through the closing marker, or `None` while
/// the closing marker has not been read yet.
fn frontmatter_prefix(buf: &[u8]) -> Option<&[u8]> {
    let start = skip_leading_whitespace(buf)?;
    if !buf[start..].starts_with(b"---") {
        return None;
    }
    let rest = start + 3;
    let closing = buf[rest..].windows(3).position(|window| window == b"---")?;
    Some(&buf[start..rest + closing + 3])
}

/// Byte offset of the first non-whitespace character.
///
/// This matches the `str::trim()` that [`parse_text`] applies, so a file whose
/// frontmatter only starts after a non-ASCII space (a no-break space, say) is
/// still found in the first chunk instead of falling back to a full read.
/// `None` means the buffer holds only whitespace so far, or ends mid-character.
fn skip_leading_whitespace(buf: &[u8]) -> Option<usize> {
    let mut start = 0;
    while start < buf.len() {
        let rest = &buf[start..];
        let (ch, width) = if rest[0].is_ascii() {
            (rest[0] as char, 1)
        } else {
            let decoded = (2..=4.min(rest.len())).find_map(|width| {
                std::str::from_utf8(&rest[..width])
                    .ok()
                    .and_then(|text| text.chars().next().map(|ch| (ch, width)))
            });
            decoded?
        };
        if !ch.is_whitespace() {
            return Some(start);
        }
        start += width;
    }
    None
}
