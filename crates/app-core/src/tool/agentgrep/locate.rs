//! `agentgrep` mode `locate`: rank code locations for a natural-language query.
//!
//! The ranking is local and deterministic: the query is split into words, each
//! word becomes a concept (its stem plus a small built-in synonym group), and
//! every window of lines is scored by the rarity of the concepts it contains.
//! Nothing is sent anywhere and nothing is persisted. Only files under the
//! searched root are read, and symbolic links are never followed. A result says
//! how much of the query it found, so a weak match is reported as weak.

use anyhow::{Result, bail};
use ignore::WalkBuilder;
use std::collections::BTreeSet;
use std::path::Path;

const DEFAULT_RESULTS: usize = 5;
const DEFAULT_MAX_FILES: usize = 2_000;
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_TOTAL_LINES: usize = 200_000;
const MAX_CONCEPTS: usize = 16;
const WINDOW: usize = 6;
const EVIDENCE_CHARS: usize = 160;

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "in", "on", "at", "to", "for", "from", "by", "with", "and", "or", "is", "are", "was",
    "were", "be", "it", "its", "this", "that", "these", "those", "when", "where", "which", "who", "what", "how", "why",
    "code", "function", "method", "where", "find", "does", "do", "did", "into", "than", "then", "so", "as", "if",
];

/// Words that usually name the same behavior in code. Each group is one concept.
const SYNONYMS: &[&[&str]] = &[
    &[
        "remove",
        "delete",
        "drop",
        "purge",
        "clear",
        "evict",
        "invalidate",
        "expire",
        "cleanup",
        "prune",
    ],
    &["old", "stale", "expired", "outdated", "obsolete", "timeout"],
    &[
        "login",
        "session",
        "auth",
        "authenticate",
        "signin",
        "credential",
        "token",
    ],
    &["error", "fail", "failure", "exception", "panic", "fault"],
    &["create", "make", "build", "new", "init", "construct", "generate"],
    &["send", "post", "transmit", "dispatch", "emit", "publish"],
    &["read", "load", "fetch", "get", "parse", "open"],
    &["save", "store", "persist", "write", "flush"],
    &["check", "validate", "verify", "assert", "ensure"],
    &["config", "setting", "option", "preference", "flag"],
    &["user", "account", "member", "profile"],
    &["file", "path", "document", "directory", "folder"],
    &["start", "begin", "launch", "run", "spawn", "boot"],
    &["stop", "end", "halt", "terminate", "shutdown", "kill", "cancel"],
    &["retry", "again", "backoff", "reattempt"],
    &["limit", "cap", "max", "quota", "threshold", "budget"],
    &["cache", "memoize", "memo"],
    &["secret", "password", "key", "apikey", "credential"],
    &["log", "trace", "record", "audit"],
    &["connect", "attach", "link", "bind"],
];

fn stem(word: &str) -> String {
    for suffix in ["ing", "ed", "es", "s"] {
        if word.len() > suffix.len() + 2
            && let Some(base) = word.strip_suffix(suffix)
        {
            return base.to_string();
        }
    }
    word.to_string()
}

/// Lowercase words of `text`, split on non-alphanumerics and camelCase humps.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for ch in text.chars() {
        if !ch.is_alphanumeric() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if ch.is_uppercase() && previous_lower && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        previous_lower = ch.is_lowercase() || ch.is_ascii_digit();
        current.extend(ch.to_lowercase());
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn stems(text: &str) -> BTreeSet<String> {
    words(text).iter().map(|word| stem(word)).collect()
}

struct Concept {
    label: String,
    forms: BTreeSet<String>,
}

/// One concept per distinct idea in the query; words of one synonym group merge.
fn concepts(query: &str) -> Vec<Concept> {
    let mut out: Vec<Concept> = Vec::new();
    for word in words(query) {
        if STOPWORDS.contains(&word.as_str()) || word.len() < 2 {
            continue;
        }
        let own = stem(&word);
        let mut forms = BTreeSet::from([own.clone()]);
        if let Some(group) = SYNONYMS
            .iter()
            .find(|group| group.iter().any(|member| *member == word || stem(member) == own))
        {
            forms.extend(group.iter().map(|member| stem(member)));
        }
        match out.iter_mut().find(|concept| !concept.forms.is_disjoint(&forms)) {
            Some(existing) => existing.forms.extend(forms),
            None => out.push(Concept { label: word, forms }),
        }
    }
    out
}

struct FileLines {
    path: String,
    path_mask: u32,
    masks: Vec<u32>,
}

struct Hit {
    path: String,
    start: usize,
    end: usize,
    score: f64,
    matched: Vec<String>,
}

fn mask_of(text: &str, concepts: &[Concept]) -> u32 {
    let present = stems(text);
    concepts
        .iter()
        .enumerate()
        .filter(|(_, concept)| !concept.forms.is_disjoint(&present))
        .fold(0, |mask, (index, _)| mask | (1 << index))
}

fn scan(root: &Path, concepts: &[Concept], max_files: usize) -> (Vec<FileLines>, usize) {
    let mut files = Vec::new();
    let mut total_lines = 0usize;
    let walker = WalkBuilder::new(root)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .build();
    for entry in walker.flatten() {
        if files.len() >= max_files || total_lines >= MAX_TOTAL_LINES {
            break;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() <= MAX_FILE_BYTES => {}
            _ => continue,
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if bytes[..bytes.len().min(8192)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let relative = path.strip_prefix(root).unwrap_or(path).display().to_string();
        let masks: Vec<u32> = text.lines().map(|line| mask_of(line, concepts)).collect();
        total_lines += masks.len();
        files.push(FileLines {
            path_mask: mask_of(&relative, concepts),
            path: relative,
            masks,
        });
    }
    let scanned = files.len();
    (files, scanned)
}

fn weight(mask: u32, idf: &[f64]) -> f64 {
    idf.iter()
        .enumerate()
        .filter(|(index, _)| mask & (1 << index) != 0)
        .map(|(_, value)| value)
        .sum()
}

fn best_window(file: &FileLines, idf: &[f64]) -> Option<(usize, usize, f64, u32)> {
    let mut best: Option<(usize, usize, f64, u32)> = None;
    for start in 0..file.masks.len() {
        let end = (start + WINDOW).min(file.masks.len());
        let union = file.masks[start..end].iter().fold(0, |all, mask| all | mask);
        if union == 0 {
            continue;
        }
        let hit_lines = file.masks[start..end].iter().filter(|mask| **mask != 0).count();
        let score = weight(union, idf) + 0.1 * hit_lines as f64 + 0.5 * weight(file.path_mask & !union, idf);
        if best.is_none_or(|(_, _, top, _)| score > top) {
            best = Some((start, end, score, union));
        }
    }
    best
}

fn rank(files: &[FileLines], concepts: &[Concept], limit: usize) -> Vec<Hit> {
    let total = files.len().max(1) as f64;
    let idf: Vec<f64> = (0..concepts.len())
        .map(|index| {
            let df = files
                .iter()
                .filter(|file| file.masks.iter().any(|mask| mask & (1 << index) != 0))
                .count() as f64;
            (1.0 + total / (1.0 + df)).ln()
        })
        .collect();
    let mut hits: Vec<Hit> = files
        .iter()
        .filter_map(|file| {
            let (start, end, score, union) = best_window(file, &idf)?;
            let hit_lines = || (start..end).filter(|line| file.masks[*line] != 0);
            let first = hit_lines().next()?;
            let last = hit_lines().next_back()?;
            Some(Hit {
                path: file.path.clone(),
                start: first + 1,
                end: last + 1,
                score,
                matched: concepts
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| union & (1 << index) != 0)
                    .map(|(_, concept)| concept.label.clone())
                    .collect(),
            })
        })
        .collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    hits.truncate(limit);
    hits
}

fn evidence(root: &Path, hit: &Hit, concepts: &[Concept]) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join(&hit.path)) else {
        return Vec::new();
    };
    let mut lines: Vec<(usize, u32, String)> = text
        .lines()
        .enumerate()
        .skip(hit.start - 1)
        .take(hit.end - hit.start + 1)
        .map(|(index, line)| (index + 1, mask_of(line, concepts).count_ones(), line.trim().to_string()))
        .filter(|(_, count, _)| *count > 0)
        .collect();
    lines.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    lines.truncate(2);
    lines.sort_by_key(|line| line.0);
    lines
        .into_iter()
        .map(|(number, _, line)| format!("{number}: {}", crate::util::truncate_str(&line, EVIDENCE_CHARS)))
        .collect()
}

/// Rank locations under `root` for `query`, as the text the tool returns.
pub(super) fn run_locate(
    root: &Path,
    query: &str,
    max_results: Option<usize>,
    max_files: Option<usize>,
) -> Result<String> {
    let mut concepts = concepts(query);
    if concepts.is_empty() {
        bail!("locate needs a description with at least one searchable word");
    }
    let dropped = concepts.len().saturating_sub(MAX_CONCEPTS);
    concepts.truncate(MAX_CONCEPTS);
    let (files, scanned) = scan(root, &concepts, max_files.unwrap_or(DEFAULT_MAX_FILES));
    let hits = rank(&files, &concepts, max_results.unwrap_or(DEFAULT_RESULTS).max(1));

    let mut out = format!("locate: {}\n", crate::util::truncate_str(query, 200));
    let Some(best) = hits.first() else {
        let labels: Vec<&str> = concepts.iter().map(|concept| concept.label.as_str()).collect();
        out.push_str(&format!(
            "confidence: none. No line in {scanned} scanned files contains any of: {}.\n",
            labels.join(", ")
        ));
        return Ok(out);
    };
    let found = best.matched.len();
    let confidence = if found * 2 < concepts.len() { "low" } else { "ok" };
    out.push_str(&format!(
        "confidence: {confidence}. The best location matches {found} of {} query terms ({}).",
        concepts.len(),
        best.matched.join(", ")
    ));
    if confidence == "low" {
        out.push_str(" Treat these as weak leads and try a more specific description.");
    }
    if dropped > 0 {
        out.push_str(&format!(" {dropped} further terms were ignored."));
    }
    out.push_str(&format!("\nscanned: {scanned} files\n"));
    for (index, hit) in hits.iter().enumerate() {
        out.push_str(&format!(
            "{}. {}:{}-{}  score {:.2}  matched: {}\n",
            index + 1,
            hit.path,
            hit.start,
            hit.end,
            hit.score,
            hit.matched.join(", ")
        ));
        for line in evidence(root, hit, &concepts) {
            out.push_str(&format!("     {line}\n"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, body: &str| {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, body).unwrap();
        };
        write(
            "src/sessions.rs",
            "use std::time::Duration;\n\nstruct Store;\n\nimpl Store {\n    /// Drop every login whose lifetime ran out.\n    fn purge_expired_sessions(&mut self, now: u64) {\n        self.entries.retain(|entry| entry.expires_at > now);\n    }\n}\n",
        );
        write(
            "src/render.rs",
            "fn draw_border(width: usize) -> String {\n    \"-\".repeat(width)\n}\n\nfn pad_title(title: &str) -> String {\n    format!(\" {title} \")\n}\n",
        );
        write(
            "src/net.rs",
            "fn open_socket(addr: &str) -> Socket {\n    Socket::connect(addr)\n}\n",
        );
        dir
    }

    #[test]
    fn a_paraphrase_finds_the_implementing_line_and_ranks_it_first() {
        let dir = fixture();
        let out = run_locate(dir.path(), "remove old logins that timed out", None, None).unwrap();
        let first = out.lines().find(|line| line.starts_with("1. ")).unwrap();
        assert!(first.starts_with("1. src/sessions.rs:6-8"), "{out}");
        assert!(out.contains("7: fn purge_expired_sessions"), "{out}");
        assert!(out.contains("confidence: ok"), "{out}");
    }

    #[test]
    fn an_unrelated_query_is_reported_as_no_evidence() {
        let dir = fixture();
        let out = run_locate(dir.path(), "quantum teleportation handshake", None, None).unwrap();
        assert!(out.contains("confidence: none"), "{out}");
        assert!(!out.contains("1. "), "{out}");
    }

    #[test]
    fn a_query_that_matches_few_terms_is_called_weak() {
        let dir = fixture();
        let out = run_locate(dir.path(), "purge quantum teleportation handshake protocol", None, None).unwrap();
        assert!(out.contains("confidence: low"), "{out}");
    }

    #[test]
    fn a_query_of_only_filler_words_is_refused() {
        let dir = fixture();
        let error = run_locate(dir.path(), "the code of a function", None, None).unwrap_err();
        assert!(error.to_string().contains("at least one searchable word"));
    }

    #[cfg(unix)]
    #[test]
    fn files_outside_the_root_are_never_read_through_links() {
        let dir = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(
            outside.path().join("leak.rs"),
            "fn purge_expired_sessions_canary() {}\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
        let out = run_locate(dir.path(), "purge expired sessions", None, None).unwrap();
        assert!(!out.contains("canary"), "the linked file must not be read: {out}");
        assert!(out.contains("scanned: 3 files"), "{out}");
        assert!(!out.contains("leak.rs"), "{out}");
    }

    #[test]
    fn the_same_tree_always_ranks_the_same_way() {
        let dir = fixture();
        let first = run_locate(dir.path(), "draw a border and pad the title", None, None).unwrap();
        assert_eq!(
            first,
            run_locate(dir.path(), "draw a border and pad the title", None, None).unwrap()
        );
        assert!(
            first.lines().any(|line| line.starts_with("1. src/render.rs")),
            "{first}"
        );
    }
}
