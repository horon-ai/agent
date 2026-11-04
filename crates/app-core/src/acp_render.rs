//! ACP v2 `git_patch` rendering.
//!
//! A v2 `patch` payload is one or more `diff --git` sections in git's `-p`
//! format with one difference: every path is absolute, including `rename from`
//! and `rename to`, and no `a/` or `b/` prefix is used. Everything else follows
//! git: `/dev/null` on the missing side of an added or deleted file, `new file
//! mode` and `deleted file mode`, `rename from`/`rename to` for a move, and
//! quoting for paths that would otherwise break the header lines.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use similar::TextDiff;

/// One file entry of a `git_patch` payload.
pub struct PatchFile<'a> {
    /// Destination path, absolute.
    pub path: &'a str,
    /// Source path of a move, absolute. `None` for an add, delete or modify.
    pub old_path: Option<&'a str>,
    /// Contents before the change. `None` means the file did not exist.
    pub old_text: Option<&'a str>,
    /// Contents after the change. `None` means the file no longer exists.
    pub new_text: Option<&'a str>,
}

/// Render `files` as consecutive `diff --git` sections.
///
/// A file whose text is unchanged and that is not a move contributes no section:
/// an empty section would claim a change the entry does not describe.
///
/// Paths must be absolute. A relative path fails a debug assertion, because the
/// v2 contract says "absolute" and silently emitting a relative header would
/// produce a patch a client cannot place.
pub fn git_patch(files: &[PatchFile<'_>]) -> String {
    let mut patch = String::new();
    for file in files {
        assert_absolute(file.path);
        if let Some(old_path) = file.old_path {
            assert_absolute(old_path);
        }
        patch.push_str(&render(file));
    }
    patch
}

/// Encode `bytes` as standard base64 (RFC 4648, padded), the encoding the v2
/// `content` blocks use.
pub fn base64_encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

fn assert_absolute(path: &str) {
    debug_assert!(
        std::path::Path::new(path).is_absolute(),
        "path must be absolute: {path:?}"
    );
}

fn render(file: &PatchFile<'_>) -> String {
    let path = quote_path(file.path);
    let new_header = file_header_path(file.path);
    match (file.old_path, file.old_text, file.new_text) {
        (Some(old_path), old_text, new_text) => {
            let old = quote_path(old_path);
            let mut section = format!("diff --git {old} {path}\nrename from {old}\nrename to {path}\n");
            let old_text = old_text.unwrap_or_default();
            let new_text = new_text.unwrap_or_default();
            if old_text != new_text {
                section.push_str(&hunks(&file_header_path(old_path), &new_header, old_text, new_text));
            }
            section
        }
        (None, None, Some(new_text)) => {
            let mut section = format!("diff --git {path} {path}\nnew file mode 100644\n");
            section.push_str(&hunks("/dev/null", &new_header, "", new_text));
            section
        }
        (None, Some(old_text), None) => {
            let mut section = format!("diff --git {path} {path}\ndeleted file mode 100644\n");
            section.push_str(&hunks(&new_header, "/dev/null", old_text, ""));
            section
        }
        (None, old_text, new_text) => {
            let old_text = old_text.unwrap_or_default();
            let new_text = new_text.unwrap_or_default();
            if old_text == new_text {
                return String::new();
            }
            let mut section = format!("diff --git {path} {path}\n");
            section.push_str(&hunks(&new_header, &new_header, old_text, new_text));
            section
        }
    }
}

/// Unified hunks at git's default context radius, behind the given headers.
///
/// `similar` writes the `---`/`+++` pair only when at least one hunk exists,
/// which matches git: a file with no content change has no hunk and no headers.
fn hunks(old_header: &str, new_header: &str, old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(old_header, new_header)
        .to_string()
}

/// True when git would quote `path` with its default `core.quotePath=true`:
/// any byte below 0x20, DEL, a byte above ASCII, a quote, or a backslash.
fn needs_quote(path: &str) -> bool {
    path.bytes()
        .any(|byte| byte < 0x20 || byte == 0x7f || byte >= 0x80 || byte == b'"' || byte == b'\\')
}

/// Quote `path` the way git quotes an unusual path.
///
/// An ordinary path stays bare. Anything `needs_quote` reports is wrapped in
/// double quotes with git's C escapes, which is the form git's own patch reader
/// unquotes back to the original path.
fn quote_path(path: &str) -> String {
    if !needs_quote(path) {
        return path.to_owned();
    }
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for byte in path.bytes() {
        match byte {
            b'\n' => quoted.push_str("\\n"),
            b'\r' => quoted.push_str("\\r"),
            b'\t' => quoted.push_str("\\t"),
            0x07 => quoted.push_str("\\a"),
            0x08 => quoted.push_str("\\b"),
            0x0b => quoted.push_str("\\v"),
            0x0c => quoted.push_str("\\f"),
            b'\\' => quoted.push_str("\\\\"),
            b'"' => quoted.push_str("\\\""),
            0x20..=0x7e => quoted.push(char::from(byte)),
            other => quoted.push_str(&format!("\\{other:03o}")),
        }
    }
    quoted.push('"');
    quoted
}

/// The path field after `--- ` or `+++ `.
///
/// git ends that field with a tab when the path contains a space, quoted or not,
/// because the unified header lets a tab separate the path from a timestamp.
fn file_header_path(path: &str) -> String {
    let mut header = quote_path(path);
    if path.contains(' ') {
        header.push('\t');
    }
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_uses_dev_null_old_side() {
        let patch = git_patch(&[PatchFile {
            path: "/tmp/project/added.txt",
            old_path: None,
            old_text: None,
            new_text: Some("hello\nworld\n"),
        }]);
        assert_eq!(
            patch,
            "diff --git /tmp/project/added.txt /tmp/project/added.txt\n\
             new file mode 100644\n\
             --- /dev/null\n\
             +++ /tmp/project/added.txt\n\
             @@ -0,0 +1,2 @@\n\
             +hello\n\
             +world\n"
        );
    }

    #[test]
    fn delete_uses_dev_null_new_side() {
        let patch = git_patch(&[PatchFile {
            path: "/tmp/project/gone.txt",
            old_path: None,
            old_text: Some("bye\nnow\n"),
            new_text: None,
        }]);
        assert_eq!(
            patch,
            "diff --git /tmp/project/gone.txt /tmp/project/gone.txt\n\
             deleted file mode 100644\n\
             --- /tmp/project/gone.txt\n\
             +++ /dev/null\n\
             @@ -1,2 +0,0 @@\n\
             -bye\n\
             -now\n"
        );
    }

    #[test]
    fn move_emits_rename_lines() {
        let pure = git_patch(&[PatchFile {
            path: "/tmp/project/b.txt",
            old_path: Some("/tmp/project/a.txt"),
            old_text: Some("same\n"),
            new_text: Some("same\n"),
        }]);
        assert_eq!(
            pure,
            "diff --git /tmp/project/a.txt /tmp/project/b.txt\n\
             rename from /tmp/project/a.txt\n\
             rename to /tmp/project/b.txt\n"
        );

        let edited = git_patch(&[PatchFile {
            path: "/tmp/project/b.txt",
            old_path: Some("/tmp/project/a.txt"),
            old_text: Some("before\n"),
            new_text: Some("after\n"),
        }]);
        assert_eq!(
            edited,
            "diff --git /tmp/project/a.txt /tmp/project/b.txt\n\
             rename from /tmp/project/a.txt\n\
             rename to /tmp/project/b.txt\n\
             --- /tmp/project/a.txt\n\
             +++ /tmp/project/b.txt\n\
             @@ -1 +1 @@\n\
             -before\n\
             +after\n"
        );
    }

    /// git is the authority for hunk text, so this compares the rendered hunks
    /// with `git diff --no-index --no-prefix -U3` line for line.
    #[test]
    fn modify_hunks_match_git_diff() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old_path = dir.path().join("old.txt");
        let new_path = dir.path().join("new.txt");
        let old_text = "alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\ngolf\nhotel\nindia\njuliet\nkilo\nlima\n";
        let new_text = "alpha\nbravo\ncharlie\nDELTA\necho\nfoxtrot\ngolf\nhotel\nindia\njuliet\nkilo\nlima\nmike\n";
        std::fs::write(&old_path, old_text).expect("write old");
        std::fs::write(&new_path, new_text).expect("write new");

        let git = std::process::Command::new("git")
            .args(["--no-pager", "diff", "--no-index", "--no-prefix", "-U3"])
            .arg(&old_path)
            .arg(&new_path)
            .output()
            .expect("run git diff");
        assert_eq!(
            git.status.code(),
            Some(1),
            "git diff --no-index reports differences with exit status 1: {}",
            String::from_utf8_lossy(&git.stderr)
        );
        let git_text = String::from_utf8(git.stdout).expect("git output is utf-8");

        let patch = git_patch(&[PatchFile {
            path: new_path.to_str().expect("utf-8 path"),
            old_path: None,
            old_text: Some(old_text),
            new_text: Some(new_text),
        }]);
        let git_hunks = strip_hunk_hints(from_first_hunk(&git_text));
        let rendered_hunks = strip_hunk_hints(from_first_hunk(&patch));
        assert!(git_hunks.starts_with("@@"), "git printed hunks: {git_text}");
        assert_eq!(
            rendered_hunks, git_hunks,
            "git_patch hunks must match git's own -U3 output"
        );

        let unchanged = git_patch(&[PatchFile {
            path: "/tmp/project/same.txt",
            old_path: None,
            old_text: Some("same\n"),
            new_text: Some("same\n"),
        }]);
        assert_eq!(unchanged, "");
    }

    #[test]
    fn missing_trailing_newline_marker() {
        let patch = git_patch(&[PatchFile {
            path: "/tmp/project/no-newline.txt",
            old_path: None,
            old_text: Some("first\nlast"),
            new_text: Some("first\nchanged"),
        }]);
        assert!(patch.contains("-last\n\\ No newline at end of file\n"), "{patch}");
        assert!(patch.contains("+changed\n\\ No newline at end of file\n"), "{patch}");
    }

    #[test]
    fn quotes_unusual_paths() {
        let patch = git_patch(&[PatchFile {
            path: "/tmp/project/we\"ird\\name.txt",
            old_path: None,
            old_text: None,
            new_text: Some("x\n"),
        }]);
        assert!(
            patch.starts_with(
                "diff --git \"/tmp/project/we\\\"ird\\\\name.txt\" \"/tmp/project/we\\\"ird\\\\name.txt\"\n"
            ),
            "{patch}"
        );
        assert!(
            patch.contains("+++ \"/tmp/project/we\\\"ird\\\\name.txt\"\n"),
            "{patch}"
        );

        let patch = git_patch(&[PatchFile {
            path: "/tmp/project/two\nlines.txt",
            old_path: None,
            old_text: None,
            new_text: Some("x\n"),
        }]);
        assert!(patch.contains("+++ \"/tmp/project/two\\nlines.txt\"\n"), "{patch}");
        assert!(!patch.contains("/tmp/project/two\nlines.txt"), "{patch}");

        let patch = git_patch(&[PatchFile {
            path: "/tmp/project/with space.txt",
            old_path: None,
            old_text: None,
            new_text: Some("x\n"),
        }]);
        assert!(
            patch.starts_with("diff --git /tmp/project/with space.txt /tmp/project/with space.txt\n"),
            "{patch}"
        );
        assert!(
            patch.contains("+++ /tmp/project/with space.txt\t\n"),
            "the header field ends with a tab when the path has a space: {patch}"
        );
        assert!(patch.contains("--- /dev/null\n"), "no tab on /dev/null: {patch}");

        let patch = git_patch(&[PatchFile {
            path: "/tmp/project/caf\u{e9}.txt",
            old_path: None,
            old_text: None,
            new_text: Some("x\n"),
        }]);
        assert!(
            patch.contains("+++ \"/tmp/project/caf\\303\\251.txt\"\n"),
            "non-ASCII bytes are octal-escaped and the path is quoted: {patch}"
        );

        let patch = git_patch(&[PatchFile {
            path: "/tmp/project/b\u{7}ell.txt",
            old_path: None,
            old_text: None,
            new_text: Some("x\n"),
        }]);
        assert!(
            patch.contains("+++ \"/tmp/project/b\\aell.txt\"\n"),
            "BEL is git's \\a, not \\007: {patch}"
        );

        let patch = git_patch(&[PatchFile {
            path: "/tmp/project/plain.txt",
            old_path: None,
            old_text: None,
            new_text: Some("x\n"),
        }]);
        assert!(patch.contains("+++ /tmp/project/plain.txt\n"), "{patch}");
        assert!(!patch.contains("\"+++"), "{patch}");
    }

    /// git is the authority for header text too. This stages a path with a
    /// space, a quote, a backslash, a tab, a non-ASCII byte and a BEL, plus one
    /// rename, then compares every `diff --git`, `---`, `+++`, `rename from` and
    /// `rename to` line with the renderer's.
    #[test]
    fn headers_match_git_for_unusual_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).expect("create repo");
        let root = repo.to_str().expect("utf-8 path").to_owned();
        let git = |args: &[&str]| git_output(&repo, args);

        git(&["init", "-q", "."]);
        std::fs::write(repo.join("ren ame.txt"), "same\n").expect("write rename source");
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "base",
        ]);

        let added = [
            "sp ace.txt",
            "we\"ird.txt",
            "back\\slash.txt",
            "ta\tb.txt",
            "caf\u{e9}.txt",
            "b\u{7}ell.txt",
        ];
        for name in added {
            std::fs::write(repo.join(name), "x\n").expect("write added file");
        }
        std::fs::rename(repo.join("ren ame.txt"), repo.join("ren amed.txt")).expect("rename");
        git(&["add", "-A"]);

        let prefix = format!("{root}/");
        let git_text = git(&[
            "-c",
            "core.quotePath=true",
            "diff",
            "--cached",
            "-M",
            &format!("--src-prefix={prefix}"),
            &format!("--dst-prefix={prefix}"),
        ]);

        let mut rendered = String::new();
        for name in added {
            let path = format!("{root}/{name}");
            rendered.push_str(&git_patch(&[PatchFile {
                path: &path,
                old_path: None,
                old_text: None,
                new_text: Some("x\n"),
            }]));
        }
        let rename_from = format!("{root}/ren ame.txt");
        let rename_to = format!("{root}/ren amed.txt");
        rendered.push_str(&git_patch(&[PatchFile {
            path: &rename_to,
            old_path: Some(&rename_from),
            old_text: Some("same\n"),
            new_text: Some("same\n"),
        }]));

        // git prints `rename from`/`rename to` with repository-relative paths,
        // as `/tmp/acpv2/full.txt:14945` describes; v2 wants them absolute, so
        // prepending the root is the one rewrite this comparison applies.
        let mut git_sections = header_sections(&git_text);
        for lines in git_sections.values_mut() {
            for line in lines.iter_mut() {
                for kind in ["rename from ", "rename to "] {
                    if let Some(relative) = line.strip_prefix(kind) {
                        *line = format!("{kind}{root}/{relative}");
                    }
                }
            }
        }

        assert_eq!(
            header_sections(&rendered),
            git_sections,
            "rendered:\n{rendered}\ngit:\n{git_text}"
        );
    }

    #[test]
    fn base64_matches_standard_alphabet() {
        // RFC 4648 section 10 test vectors.
        for (plain, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(
                base64_encode(plain.as_bytes()),
                expected,
                "RFC 4648 vector for {plain:?}"
            );
        }

        let all: Vec<u8> = (0..=255u8).collect();
        let encoded = base64_encode(&all);
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='),
            "outside the standard alphabet: {encoded}"
        );
        assert_eq!(encoded.len(), 344, "256 bytes encode to 344 characters");
        assert!(encoded.ends_with("=="), "one leftover byte pads twice");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "path must be absolute")]
    fn relative_path_is_rejected() {
        git_patch(&[PatchFile {
            path: "src/main.rs",
            old_path: None,
            old_text: None,
            new_text: Some("fn main() {}\n"),
        }]);
    }

    fn git_output(repo: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git output is utf-8")
    }

    /// Maps each `diff --git` line to the header lines that follow it, so the
    /// comparison checks which section a line belongs to, not just its text.
    fn header_sections(text: &str) -> std::collections::BTreeMap<String, Vec<String>> {
        let mut sections: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        let mut current: Option<String> = None;
        for line in text.lines() {
            if let Some(key) = line.strip_prefix("diff --git ") {
                sections.entry(key.to_owned()).or_default();
                current = Some(key.to_owned());
                continue;
            }
            let Some(key) = current.as_ref() else {
                continue;
            };
            if line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with("rename from ")
                || line.starts_with("rename to ")
            {
                sections
                    .get_mut(key)
                    .expect("a section was opened")
                    .push(line.to_owned());
            }
        }
        sections
    }

    fn from_first_hunk(text: &str) -> &str {
        let start = text.find("@@").expect("a hunk header");
        &text[start..]
    }

    /// Removes git's optional function-context hint from hunk headers, as in
    /// `@@ -10,3 +10,4 @@ india`. The v2 `git_patch` example carries the bare
    /// header, so the hint is the only cosmetic difference the comparison skips;
    /// hunk ranges and body lines still have to match byte for byte.
    fn strip_hunk_hints(text: &str) -> String {
        let mut stripped = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let numbers = line
                .strip_prefix("@@")
                .and_then(|rest| rest.find("@@").map(|end| &rest[..end]));
            match numbers {
                Some(numbers) => {
                    stripped.push_str("@@");
                    stripped.push_str(numbers);
                    stripped.push_str("@@\n");
                }
                None => stripped.push_str(line),
            }
        }
        stripped
    }
}
