//! Structural code queries over tree-sitter grammars, built on `ast-grep-core`.
//!
//! Three consumers share this crate: the `ast_grep`/`ast_edit` tools, the
//! structural `read` summary, and `hashline_edit`'s block-expansion mode. They
//! all need the same language table and the same tree walk, so it lives here
//! rather than in a tool module.

pub mod summary;

pub use summary::{Segment, summarize};

use anyhow::{Result, anyhow};
use ast_grep_core::AstGrep;
use ast_grep_core::language::Language;
use ast_grep_core::matcher::{Pattern, PatternBuilder, PatternError};
use ast_grep_core::meta_var::MetaVariable;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use std::collections::BTreeMap;
use std::path::Path;

/// Languages this crate can parse. Anything else is reported as unsupported
/// rather than silently parsed as another grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
}

impl Lang {
    /// Resolves a language from a file extension. Unknown extensions return
    /// `None`, so callers can skip a file instead of guessing.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Option<Self> {
        let path = path.as_ref();
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        Self::from_extension(&extension)
    }

    pub fn from_extension(extension: &str) -> Option<Self> {
        Some(match extension {
            "rs" => Self::Rust,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "py" => Self::Python,
            "go" => Self::Go,
            _ => return None,
        })
    }

    /// Resolves a language from the name a caller wrote (`rust`, `tsx`, …).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "rust" | "rs" => Self::Rust,
            "typescript" | "ts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "javascript" | "js" | "jsx" => Self::JavaScript,
            "python" | "py" => Self::Python,
            "go" | "golang" => Self::Go,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Go => "go",
        }
    }

    fn ts_language(self) -> TSLanguage {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }
}

impl Language for Lang {
    fn kind_to_id(&self, kind: &str) -> u16 {
        self.ts_language().id_for_node_kind(kind, /* named */ true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.ts_language().field_id_for_name(field).map(|id| id.get())
    }

    fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
        builder.build(|source| StrDoc::try_new(source, *self))
    }
}

impl LanguageExt for Lang {
    fn get_ts_language(&self) -> TSLanguage {
        self.ts_language()
    }
}

/// One structural match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// 1-based first line of the match.
    pub start_line: usize,
    /// 1-based last line of the match.
    pub end_line: usize,
    pub text: String,
    /// Named metavariables, sorted by name.
    pub captures: BTreeMap<String, String>,
}

fn doc_for(source: &str, lang: Lang) -> Result<AstGrep<StrDoc<Lang>>> {
    let doc = StrDoc::try_new(source, lang).map_err(|error| anyhow!("{error}"))?;
    Ok(AstGrep::doc(doc))
}

fn compile(pattern: &str, lang: Lang) -> Result<Pattern> {
    Pattern::try_new(pattern, lang).map_err(|error| anyhow!("{error}"))
}

fn match_from(node: ast_grep_core::NodeMatch<'_, StrDoc<Lang>>) -> Match {
    let matched = node.get_node();
    let mut captures = BTreeMap::new();
    for variable in node.get_env().get_matched_variables() {
        match variable {
            MetaVariable::Capture(name, _) => {
                if let Some(found) = node.get_env().get_match(&name) {
                    captures.insert(name, found.text().to_string());
                }
            }
            MetaVariable::MultiCapture(name) => {
                let parts: Vec<String> = node
                    .get_env()
                    .get_multiple_matches(&name)
                    .iter()
                    .map(|node| node.text().to_string())
                    .collect();
                if !parts.is_empty() {
                    captures.insert(name, parts.join(", "));
                }
            }
            _ => {}
        }
    }
    Match {
        start_line: matched.start_pos().line() + 1,
        end_line: matched.end_pos().line() + 1,
        text: matched.text().to_string(),
        captures,
    }
}

/// Finds every non-overlapping match of `pattern`, in source order.
pub fn find(source: &str, lang: Lang, pattern: &str) -> Result<Vec<Match>> {
    let compiled = compile(pattern, lang)?;
    let doc = doc_for(source, lang)?;
    Ok(doc.root().find_all(compiled.clone()).map(match_from).collect())
}

/// Rewrites every match of `pattern` with `template`.
///
/// Overlapping matches are refused: applying both would need an arbitrary
/// ordering rule, and a half-rewritten file is worse than an error.
pub fn rewrite(source: &str, lang: Lang, pattern: &str, template: &str) -> Result<(String, usize)> {
    let compiled = compile(pattern, lang)?;
    let doc = doc_for(source, lang)?;
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for found in doc.root().find_all(compiled.clone()) {
        let range = found.range();
        let text = ast_grep_core::replacer::Replacer::generate_replacement(template, &found);
        let text = String::from_utf8(text).map_err(|error| anyhow!("replacement is not UTF-8: {error}"))?;
        replacements.push((range.start, range.end, text));
    }
    if replacements.is_empty() {
        return Ok((source.to_string(), 0));
    }
    replacements.sort_by_key(|(start, _, _)| *start);
    for pair in replacements.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(anyhow!("Overlapping replacements detected; refine the pattern."));
        }
    }
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for (start, end, text) in &replacements {
        output.push_str(&source[cursor..*start]);
        output.push_str(text);
        cursor = *end;
    }
    output.push_str(&source[cursor..]);
    Ok((output, replacements.len()))
}

/// Kinds that should win over a smaller inner block for `block` expansion.
const STRONG_KINDS: &[&str] = &[
    "function_item",
    "function_definition",
    "function_declaration",
    "method_definition",
    "method_declaration",
    "constructor_declaration",
    "impl_item",
    "class_definition",
    "class_declaration",
    "interface_declaration",
    "struct_item",
    "enum_item",
];

const WEAK_KINDS: &[&str] = &[
    "block",
    "statement_block",
    "declaration_list",
    "body",
    "module",
    "source_file",
];

fn kind_rank(kind: &str) -> u8 {
    if STRONG_KINDS.contains(&kind) {
        0
    } else if WEAK_KINDS.contains(&kind) {
        1
    } else {
        2
    }
}

/// Finds the enclosing multi-line block for a 1-based line: the smallest node
/// that starts on or contains the line, preferring function/impl/class kinds
/// over their bodies.
pub fn enclosing_block(source: &str, lang: Lang, line: usize) -> Option<(usize, usize)> {
    if line == 0 {
        return None;
    }
    let doc = doc_for(source, lang).ok()?;
    let root = doc.root();
    let target = line - 1;
    let mut best: Option<(u8, usize, usize, usize)> = None;
    for node in root.dfs() {
        if !node.is_named() {
            continue;
        }
        let start = node.start_pos().line();
        let end = node.end_pos().line();
        if start > target || end < target || end <= start {
            continue;
        }
        let rank = kind_rank(node.kind().as_ref());
        let candidate = (rank, end - start, start, end);
        best = match best {
            None => Some(candidate),
            Some(current) => {
                let better = rank < current.0 || (rank == current.0 && candidate.1 < current.1);
                Some(if better { candidate } else { current })
            }
        };
    }
    best.map(|(_, _, start, end)| (start + 1, end + 1))
}

/// Language names accepted by [`Lang::from_name`], for error messages.
pub fn supported_names() -> &'static [&'static str] {
    &["rust", "typescript", "tsx", "javascript", "python", "go"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_reports_captures_and_lines() {
        let source = "fn a() {\n    foo(1, 2);\n}\n";
        let found = find(source, Lang::Rust, "foo($A, $B)").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start_line, 2);
        assert_eq!(found[0].end_line, 2);
        assert_eq!(found[0].captures.get("A").map(String::as_str), Some("1"));
        assert_eq!(found[0].captures.get("B").map(String::as_str), Some("2"));
        assert_eq!(found[0].text, "foo(1, 2)");
    }

    #[test]
    fn find_rejects_an_empty_pattern() {
        assert!(find("fn a() {}", Lang::Rust, "").is_err());
    }

    #[test]
    fn rewrite_replaces_every_match() {
        let source = "fn a() {\n    foo(1);\n    foo(2);\n}\n";
        let (rewritten, count) = rewrite(source, Lang::Rust, "foo($A)", "bar($A)").unwrap();
        assert_eq!(count, 2);
        assert_eq!(rewritten, "fn a() {\n    bar(1);\n    bar(2);\n}\n");
    }

    #[test]
    fn enclosing_block_prefers_the_function_over_its_body() {
        let source = "fn a() {\n    let x = 1;\n    let y = 2;\n}\n";
        assert_eq!(enclosing_block(source, Lang::Rust, 1), Some((1, 4)));
    }

    #[test]
    fn summarize_elides_a_long_body() {
        let source = "fn a() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    let d = 4;\n    let e = 5;\n}\n";
        let segments = summarize(source, Lang::Rust, 4).expect("long body should be elided");
        assert!(segments.iter().any(|segment| !segment.kept));
        let elided = segments.iter().find(|segment| !segment.kept).unwrap();
        assert!(elided.start_line > 1);
        assert!(elided.end_line < 7);
    }

    #[test]
    fn summarize_returns_none_for_a_small_file() {
        assert!(summarize("fn a() {\n    let x = 1;\n}\n", Lang::Rust, 4).is_none());
    }
}
