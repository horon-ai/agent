//! Refuses edits to files that are produced by a generator.
//!
//! An edit to a generated file is lost the next time its source is regenerated,
//! so the correct move is always to change the generator input. The guard is
//! name-based first (cheap, unambiguous), then reads a small head of the file
//! and looks for a "generated" marker in its comment lines only, so ordinary
//! prose mentioning generation does not trip it. `[tools] block_auto_generated
//! = false` turns it off.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

const PROBE_BYTES: usize = 1024;
const PROBE_LINES: usize = 40;

static NAME_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^zz_generated\.",
        r"\.pb\.(go|cc|h|c|js|ts)$",
        r"_pb2\.py$",
        r"_pb2_grpc\.py$",
        r"\.gen\.(go|ts|js|py)$",
        r"\.swagger\.json$",
        r"\.mocks?\.(go|ts|js)$",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("static file-name pattern compiles"))
    .collect()
});

static CONTENT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)@generated\b",
        r"(?i)\bcode\s+generated\s+by\s+[a-z0-9_.-]+",
        r"(?i)\bcode\s+generated\b.*\bdo\s+not\s+edit\b",
        r"(?i)\bthis\s+file\s+was\s+automatically\s+generated\b",
        r"(?i)\bgenerated\s+by\s+(?:protoc(?:-gen-[\w-]+)?|sqlc|buf|swagger(?:-codegen)?|openapi(?:-generator)?|grpc-gateway|mockery|stringer|easyjson|deepcopy-gen|kysely-codegen|napi-rs)\b",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("static content pattern compiles"))
    .collect()
});

/// `[tools] block_auto_generated`, on by default.
pub(crate) fn enabled() -> bool {
    crate::config::config().tools.block_auto_generated
}

/// The refusal text for `path`, or `None` when the file is safe to edit.
///
/// `display` is the path as the caller spelled it, which is what the message
/// quotes back.
pub(crate) fn refusal(path: &Path, display: &str) -> Option<String> {
    if !enabled() {
        return None;
    }
    let marker = detect(path)?;
    Some(format!(
        "Cannot modify auto-generated file: {display}\n\n\
         This file appears to be automatically generated (detected marker: \"{marker}\").\n\
         Auto-generated files should not be edited directly. Instead:\n\
         1. Find the source file or generator configuration\n\
         2. Make changes to the source\n\
         3. Regenerate the file\n\n\
         Set [tools] block_auto_generated = false to allow edits."
    ))
}

/// The marker that made `path` look generated.
fn detect(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    if NAME_PATTERNS.iter().any(|pattern| pattern.is_match(&name)) {
        return Some(name);
    }

    let extension = path.extension().map(|ext| ext.to_string_lossy().to_lowercase());
    let comment = comment_prefixes(extension.as_deref(), &name)?;

    let bytes = read_probe(path);
    let text = String::from_utf8_lossy(&bytes);
    for line in text.lines().take(PROBE_LINES) {
        let trimmed = line.trim_start();
        if !comment.iter().any(|prefix| trimmed.starts_with(prefix)) {
            continue;
        }
        for pattern in CONTENT_PATTERNS.iter() {
            if let Some(found) = pattern.find(line) {
                return Some(found.as_str().trim().to_string());
            }
        }
    }
    None
}

fn read_probe(path: &Path) -> Vec<u8> {
    use std::io::Read;

    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut buffer = vec![0u8; PROBE_BYTES];
    let mut filled = 0usize;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => break,
        }
    }
    buffer.truncate(filled);
    buffer
}

/// Comment prefixes that can carry a marker for this file, or `None` when the
/// extension is one the guard does not inspect (name check only).
fn comment_prefixes(extension: Option<&str>, name: &str) -> Option<&'static [&'static str]> {
    const C_LIKE: &[&str] = &["//", "/*", "*"];
    const HASH: &[&str] = &["#"];
    const SQL: &[&str] = &["--"];
    const MARKUP: &[&str] = &["<!--"];

    if matches!(name, "Dockerfile" | "Makefile" | "justfile") {
        return Some(HASH);
    }
    let extension = extension?;
    Some(match extension {
        "c" | "cc" | "cpp" | "cs" | "dart" | "go" | "h" | "hpp" | "java" | "js" | "jsx" | "kt" | "kts" | "mjs"
        | "cjs" | "php" | "rs" | "scala" | "swift" | "ts" | "tsx" => C_LIKE,
        "py" | "rb" | "sh" | "bash" | "zsh" | "yml" | "yaml" | "toml" | "ini" | "cfg" | "conf" | "env" | "pl" | "r" => {
            HASH
        }
        "sql" => SQL,
        "html" | "htm" | "xml" | "svg" | "xhtml" => MARKUP,
        _ => return None,
    })
}

#[cfg(test)]
#[path = "generated_guard_tests.rs"]
mod tests;
