use super::*;

struct ResolvedSearchScope {
    root: Option<String>,
    glob: Option<String>,
}

fn resolved_search_scope(
    ctx: &ToolContext,
    path: Option<&str>,
    file: Option<&str>,
    glob: Option<&str>,
) -> ResolvedSearchScope {
    // `file` scopes grep/find to one exact file when `path` is absent.
    let path = path.or(file);
    let Some(path) = path else {
        return ResolvedSearchScope {
            root: None,
            glob: normalized_agentgrep_glob_owned(glob),
        };
    };

    let resolved = resolve_path_arg(ctx, path);
    if resolved.is_file() {
        let root = resolved
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .display()
            .to_string();
        let glob = resolved.file_name().map(|name| name.to_string_lossy().into_owned());
        return ResolvedSearchScope { root: Some(root), glob };
    }

    ResolvedSearchScope {
        root: Some(resolved.display().to_string()),
        glob: normalized_agentgrep_glob_owned(glob),
    }
}

/// [`build_grep_args`] plus the regex fallback ladder.
///
/// The second element is a note for the caller to prepend when the requested
/// regex did not compile and the search ran differently.
pub(super) fn build_grep_args_with_note(
    params: &AgentGrepInput,
    ctx: &ToolContext,
) -> Result<(GrepArgs, Option<String>)> {
    let mut args = build_grep_args(params, ctx)?;
    if !args.regex {
        return Ok((args, None));
    }
    let (query, regex, note) = regex_fallback(&args.query);
    args.query = query;
    args.regex = regex;
    Ok((args, note))
}

/// Makes a requested regex compile, or degrades it to a literal search.
///
/// The engine is Rust `regex`, so look-around and backreferences never compile.
/// Escaping the usual accident (a stray brace, an unbalanced parenthesis) keeps
/// most queries searching something close to what was meant; anything else
/// falls back to a literal search rather than failing the call.
pub(super) fn regex_fallback(query: &str) -> (String, bool, Option<String>) {
    let Err(original) = regex::Regex::new(query) else {
        return (query.to_string(), true, None);
    };
    let reason = one_line(&original.to_string());

    let braced = escape_loose_braces(query);
    if braced != query {
        match regex::Regex::new(&braced) {
            Ok(_) => return (braced, true, Some(note(query, &reason, "with escaped braces"))),
            Err(next) if needs_paren_escape(&next.to_string()) => {
                let repaired = escape_unbalanced_parens(&braced);
                if repaired != braced && regex::Regex::new(&repaired).is_ok() {
                    return (repaired, true, Some(note(query, &reason, "with escaped parentheses")));
                }
            }
            Err(_) => {}
        }
    }

    if needs_paren_escape(&original.to_string()) {
        let repaired = escape_unbalanced_parens(query);
        if repaired != query && regex::Regex::new(&repaired).is_ok() {
            return (repaired, true, Some(note(query, &reason, "with escaped parentheses")));
        }
    }

    (query.to_string(), false, Some(note(query, &reason, "literally")))
}

fn note(query: &str, error: &str, how: &str) -> String {
    format!("[Regex '{query}' did not compile ({error}); searched {how}]")
}

fn one_line(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::util::truncate_str(&collapsed, 300).to_string()
}

fn needs_paren_escape(error: &str) -> bool {
    error.contains("unclosed group") || error.contains("unopened group")
}

/// Escapes `{` and `}` that cannot be a counted repetition.
///
/// A valid `{N}`, `{N,}`, or `{N,M}` is left alone, as is the brace of a
/// `\p{...}`, `\x{...}`, or `\u{...}` class.
fn escape_loose_braces(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len());
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            out.push(ch);
            if index + 1 < chars.len() {
                out.push(chars[index + 1]);
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if ch == '{' {
            if is_class_brace(&chars, index) {
                out.push(ch);
                index += 1;
                continue;
            }
            if let Some(end) = repetition_end(&chars[index + 1..]) {
                for ch in &chars[index..=index + 1 + end] {
                    out.push(*ch);
                }
                index += end + 2;
                continue;
            }
            out.push_str("\\{");
            index += 1;
            continue;
        }
        if ch == '}' {
            out.push_str("\\}");
            index += 1;
            continue;
        }
        out.push(ch);
        index += 1;
    }
    out
}

/// Whether the `{` at `index` opens a `\p{`, `\x{`, or `\u{` class.
fn is_class_brace(chars: &[char], index: usize) -> bool {
    if index < 2 || chars[index - 2] != '\\' || !matches!(chars[index - 1], 'p' | 'x' | 'u') {
        return false;
    }
    // An even number of backslashes before the letter means the backslash itself
    // is escaped, so this brace does not open a class.
    let mut backslashes: usize = 0;
    let mut cursor = index - 2;
    loop {
        backslashes += 1;
        if cursor == 0 || chars[cursor - 1] != '\\' {
            break;
        }
        cursor -= 1;
    }
    !backslashes.is_multiple_of(2)
}

/// Offset of the `}` closing a `{N}`, `{N,}`, or `{N,M}` repetition.
fn repetition_end(rest: &[char]) -> Option<usize> {
    let mut index = 0;
    while index < rest.len() && rest[index].is_ascii_digit() {
        index += 1;
    }
    if index == 0 {
        return None;
    }
    if rest.get(index) == Some(&'}') {
        return Some(index);
    }
    if rest.get(index) == Some(&',') {
        let mut end = index + 1;
        while end < rest.len() && rest[end].is_ascii_digit() {
            end += 1;
        }
        if rest.get(end) == Some(&'}') {
            return Some(end);
        }
    }
    None
}

/// Escapes parentheses that no group can pair with.
fn escape_unbalanced_parens(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut open: Vec<usize> = Vec::new();
    let mut unmatched_close: Vec<usize> = Vec::new();
    let mut in_class = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            index += 2;
            continue;
        }
        if in_class {
            if ch == ']' {
                in_class = false;
            }
            index += 1;
            continue;
        }
        match ch {
            '[' => in_class = true,
            '(' => open.push(index),
            ')' => {
                if open.pop().is_none() {
                    unmatched_close.push(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    if open.is_empty() && unmatched_close.is_empty() {
        return pattern.to_string();
    }
    let mut out = String::with_capacity(pattern.len() + open.len() + unmatched_close.len());
    for (index, ch) in chars.iter().enumerate() {
        if open.contains(&index) || unmatched_close.contains(&index) {
            out.push('\\');
        }
        out.push(*ch);
    }
    out
}

pub(super) fn build_grep_args(params: &AgentGrepInput, ctx: &ToolContext) -> Result<GrepArgs> {
    let query = params
        .query
        .clone()
        .ok_or_else(|| anyhow::anyhow!("agentgrep grep requires 'query'"))?;
    let scope = resolved_search_scope(
        ctx,
        params.path.as_deref(),
        params.file.as_deref(),
        params.glob.as_deref(),
    );
    Ok(GrepArgs {
        query,
        regex: params.regex.unwrap_or(false),
        file_type: params.file_type.clone(),
        json: false,
        paths_only: params.paths_only.unwrap_or(false),
        hidden: params.hidden.unwrap_or(false),
        no_ignore: params.no_ignore.unwrap_or(false),
        // Keep following symlinks (pre-v0.1.7 behavior); the guardrail is opt-in.
        no_follow: false,
        path: scope.root,
        glob: scope.glob,
    })
}

pub(super) fn build_find_args(params: &AgentGrepInput, ctx: &ToolContext) -> Result<FindArgs> {
    let query = params.query.as_deref().unwrap_or_default();
    if query.trim().is_empty()
        && params.path.as_deref().is_none_or(str::is_empty)
        && params.file.as_deref().is_none_or(str::is_empty)
        && normalized_agentgrep_glob(params.glob.as_deref()).is_none()
        && params.file_type.as_deref().is_none_or(str::is_empty)
    {
        return Err(anyhow::anyhow!(
            "agentgrep find requires 'query' unless path, glob, or type narrows the search"
        ));
    }
    let scope = resolved_search_scope(
        ctx,
        params.path.as_deref(),
        params.file.as_deref(),
        params.glob.as_deref(),
    );
    Ok(FindArgs {
        query_parts: query.split_whitespace().map(ToOwned::to_owned).collect(),
        file_type: params.file_type.clone(),
        json: false,
        paths_only: params.paths_only.unwrap_or(false),
        debug_score: params.debug_score.unwrap_or(false),
        max_files: params.max_files.unwrap_or(10),
        hidden: params.hidden.unwrap_or(false),
        no_ignore: params.no_ignore.unwrap_or(false),
        // Keep following symlinks (pre-v0.1.7 behavior); the guardrail is opt-in.
        no_follow: false,
        path: scope.root,
        glob: scope.glob,
    })
}

pub(super) fn build_outline_args(
    params: &AgentGrepInput,
    ctx: &ToolContext,
    context_json_path: Option<&Path>,
) -> Result<OutlineArgs> {
    // Agents sometimes point `path` at the file itself, either instead of or
    // in addition to `file`. Treat a file-valued `path` as the outline target
    // so the file argument is not joined onto it (for example,
    // ".../todo.rs/.../todo.rs").
    if let Some(path) = params.path.as_deref() {
        let resolved = resolve_path_arg(ctx, path);
        if resolved.is_file() {
            return Ok(OutlineArgs {
                file: resolved.display().to_string(),
                json: false,
                max_items: None,
                path: None,
                context_json: context_json_path.map(|path| path.display().to_string()),
            });
        }
    }

    let file = outline_file_arg(params)?;
    Ok(OutlineArgs {
        file,
        json: false,
        max_items: None,
        path: resolved_root_string(ctx, params.path.as_deref()),
        context_json: context_json_path.map(|path| path.display().to_string()),
    })
}

pub(super) fn build_smart_args_and_query(
    params: &AgentGrepInput,
    ctx: &ToolContext,
    context_json_path: Option<&Path>,
) -> Result<(SmartArgs, SmartQuery)> {
    let terms = trace_or_smart_terms_owned(params)?;
    let query = parse_smart_query(&terms).map_err(|err| {
        anyhow::anyhow!(
            "{}\n\ntrace queries use a small DSL. Example:\n  agentgrep trace subject:auth_status relation:rendered support:ui",
            err
        )
    })?;
    let scope = resolved_search_scope(
        ctx,
        params.path.as_deref(),
        params.file.as_deref(),
        params.glob.as_deref(),
    );

    let args = SmartArgs {
        terms,
        json: false,
        max_files: params.max_files.unwrap_or(5),
        max_regions: params.max_regions.unwrap_or(6),
        full_region: parse_full_region_mode(params.full_region.as_deref())?,
        debug_plan: params.debug_plan.unwrap_or(false),
        debug_score: params.debug_score.unwrap_or(false),
        paths_only: params.paths_only.unwrap_or(false),
        path: scope.root,
        file_type: params.file_type.clone(),
        glob: scope.glob,
        hidden: params.hidden.unwrap_or(false),
        no_ignore: params.no_ignore.unwrap_or(false),
        context_json: context_json_path.map(|path| path.display().to_string()),
    };

    Ok((args, query))
}

pub(super) fn trace_or_smart_terms_owned(params: &AgentGrepInput) -> Result<Vec<String>> {
    if let Some(terms) = params.terms.as_ref().filter(|terms| !terms.is_empty()) {
        return Ok(terms.clone());
    }

    if params.mode == "smart"
        && let Some(query) = params.query.as_deref()
    {
        let split_terms: Vec<String> = query
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        if !split_terms.is_empty() {
            return Ok(split_terms);
        }
    }

    let field_hint = if params.mode == "smart" {
        "non-empty 'terms' or 'query'"
    } else {
        "non-empty 'terms'"
    };

    Err(anyhow::anyhow!("agentgrep {} requires {}", params.mode, field_hint))
}

fn outline_file_arg(params: &AgentGrepInput) -> Result<String> {
    params
        .file
        .clone()
        .or_else(|| params.query.clone())
        .or_else(|| params.terms.as_ref().and_then(|terms| terms.first().cloned()))
        .ok_or_else(|| anyhow::anyhow!("agentgrep outline requires 'file' (or legacy 'query' / first term)"))
}

fn parse_full_region_mode(value: Option<&str>) -> Result<FullRegionMode> {
    match value.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(FullRegionMode::Auto),
        "always" => Ok(FullRegionMode::Always),
        "never" => Ok(FullRegionMode::Never),
        other => Err(anyhow::anyhow!(
            "agentgrep trace full_region must be one of: auto, always, never; got {other}"
        )),
    }
}

fn resolved_root_string(ctx: &ToolContext, path: Option<&str>) -> Option<String> {
    path.map(|path| resolve_path_arg(ctx, path).display().to_string())
}

pub(super) fn resolve_search_root(ctx: &ToolContext, path: Option<&str>) -> Result<PathBuf> {
    path.map(PathBuf::from)
        .or_else(|| ctx.working_dir.clone())
        .ok_or_else(|| anyhow::anyhow!("agentgrep requires a session working directory"))
}

pub(super) fn summarize_agentgrep_request(
    params: &AgentGrepInput,
    ctx: &ToolContext,
    context_json_path: Option<&Path>,
) -> String {
    let mut parts = vec![format!("mode={}", params.mode)];
    if let Some(query) = params.query.as_deref() {
        parts.push(format!("query={}", util::truncate_str(query, 80)));
    }
    if let Some(file) = params.file.as_deref() {
        parts.push(format!("file={file}"));
    }
    if let Some(terms) = params.terms.as_ref() {
        parts.push(format!("terms={}", util::truncate_str(&terms.join(" "), 80)));
    }
    if let Some(path) = resolved_root_string(ctx, params.path.as_deref()) {
        parts.push(format!("root={path}"));
    }
    if let Some(glob) = normalized_agentgrep_glob(params.glob.as_deref()) {
        parts.push(format!("glob={glob}"));
    }
    if let Some(file_type) = params.file_type.as_deref() {
        parts.push(format!("type={file_type}"));
    }
    if params.paths_only.unwrap_or(false) {
        parts.push("paths_only=true".to_string());
    }
    if context_json_path.is_some() {
        parts.push("context_json=true".to_string());
    }
    parts.join(" ")
}
