//! Fixture generator: mutate one line of a real `.rs` file and write a task
//! directory the runner can execute.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::mutate::{self, ALL as ALL_MUTATIONS, Mutation};
use crate::rng::{SplitMix64, shuffle};

/// Files shorter than this are skipped: a one-line bug in a tiny file is
/// usually found by reading the whole thing.
pub(crate) const MIN_FILE_LINES: usize = 50;

/// Directories never walked while collecting sources.
const SKIPPED_DIRS: [&str; 1] = ["target"];

/// How many shuffled passes over `(file, mutation)` pairs to attempt before
/// giving up on the requested task count.
const MAX_PASSES: usize = 8;

#[derive(Clone, Debug)]
pub struct GenerateOptions {
    pub source: PathBuf,
    pub out: PathBuf,
    pub seed: u64,
    pub count: usize,
}

/// One generated task, already written or ready to write.
#[derive(Clone, Debug)]
pub struct GeneratedTask {
    pub id: String,
    /// Path of the mutated file relative to `--source`.
    pub relative: PathBuf,
    pub prompt: String,
    pub metadata: TaskMetadata,
    pub original_source: String,
    pub mutated_source: String,
}

/// `metadata.json` contents.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskMetadata {
    pub seed: u64,
    pub mutation: String,
    pub file: String,
    pub line: usize,
    pub original: String,
    pub mutated: String,
}

/// Generate and write `count` fixtures under `options.out`.
pub fn generate(options: &GenerateOptions) -> Result<Vec<GeneratedTask>> {
    if options.count == 0 {
        bail!("--count must be at least 1");
    }
    let source_root = fs::canonicalize(&options.source)
        .with_context(|| format!("cannot resolve --source {}", options.source.display()))?;
    let files = collect_source_files(&source_root)?;
    if files.is_empty() {
        bail!(
            "no .rs files with at least {MIN_FILE_LINES} lines under {}",
            source_root.display()
        );
    }

    let mut pairs: Vec<(PathBuf, Mutation)> = Vec::new();
    for file in &files {
        for mutation in ALL_MUTATIONS {
            pairs.push((file.clone(), mutation));
        }
    }

    let seed = options.seed;
    let mut rng = SplitMix64::new(seed);
    shuffle(&mut pairs, &mut rng);

    let mut tasks: Vec<GeneratedTask> = Vec::with_capacity(options.count);
    let mut seen: HashSet<(PathBuf, Mutation, usize)> = HashSet::new();
    for pass in 0..MAX_PASSES {
        if pass > 0 {
            shuffle(&mut pairs, &mut rng);
        }
        for (file, mutation) in &pairs {
            if tasks.len() >= options.count {
                break;
            }
            let source = fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
            // A pass may land on a site already used; retry a few draws before
            // moving on to the next pair.
            for _attempt in 0..4 {
                let Some(applied) = mutate::apply(*mutation, &source, &mut rng) else {
                    break;
                };
                if !seen.insert((file.clone(), *mutation, applied.line)) {
                    continue;
                }
                let relative = file
                    .strip_prefix(&source_root)
                    .with_context(|| format!("{} is outside {}", file.display(), source_root.display()))?
                    .to_path_buf();
                let display = relative.to_string_lossy().replace('\\', "/");
                let index = tasks.len();
                tasks.push(GeneratedTask {
                    id: format!("{seed:016x}-{index:03}-{}", mutation.as_str()),
                    relative,
                    prompt: format!(
                        "The file {display} has a single-line bug introduced near line {line} ({description}). Fix it with the smallest possible edit. Do not change anything else.",
                        line = applied.line,
                        description = mutation.description(),
                    ),
                    metadata: TaskMetadata {
                        seed,
                        mutation: mutation.as_str().to_string(),
                        file: display,
                        line: applied.line,
                        original: applied.original,
                        mutated: applied.mutated,
                    },
                    original_source: source.clone(),
                    mutated_source: applied.mutated_source,
                });
                break;
            }
        }
        if tasks.len() >= options.count {
            break;
        }
    }

    if tasks.len() < options.count {
        bail!(
            "generated only {} of {} requested task(s): the source tree has no more distinct mutation sites",
            tasks.len(),
            options.count
        );
    }

    fs::create_dir_all(&options.out).with_context(|| format!("cannot create {}", options.out.display()))?;
    for task in &tasks {
        write_task(&options.out, task)?;
    }
    Ok(tasks)
}

/// `.rs` files under `root` with at least [`MIN_FILE_LINES`] lines, in a
/// deterministic depth-first order.
pub(crate) fn collect_source_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk(root, &mut files)?;
    Ok(files)
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("cannot list {}", dir.display()))? {
        let entry = entry?;
        entries.push(entry.path());
    }
    entries.sort();
    for path in entries {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        if metadata.is_dir() {
            if name.starts_with('.') || SKIPPED_DIRS.contains(&name) {
                continue;
            }
            walk(&path, files)?;
        } else if metadata.is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("rs")
            && line_count(&path)? >= MIN_FILE_LINES
        {
            files.push(path);
        }
    }
    Ok(())
}

fn line_count(path: &Path) -> Result<usize> {
    let content = fs::read_to_string(path)?;
    Ok(content.lines().count())
}

/// Write one task directory: `{id}/{prompt.md, input/<file>, expected/<file>, metadata.json}`.
pub(crate) fn write_task(out: &Path, task: &GeneratedTask) -> Result<()> {
    let dir = out.join(&task.id);
    let input_dir = dir.join("input");
    let expected_dir = dir.join("expected");
    create_parent_dirs(&input_dir, &task.relative)?;
    create_parent_dirs(&expected_dir, &task.relative)?;
    fs::create_dir_all(&input_dir)?;
    fs::create_dir_all(&expected_dir)?;

    fs::write(input_dir.join(&task.relative), &task.mutated_source)
        .with_context(|| format!("cannot write input for {}", task.id))?;
    fs::write(expected_dir.join(&task.relative), &task.original_source)
        .with_context(|| format!("cannot write expected for {}", task.id))?;
    fs::write(dir.join("prompt.md"), format!("{}\n", task.prompt))?;
    fs::write(
        dir.join("metadata.json"),
        format!("{}\n", serde_json::to_string_pretty(&task.metadata)?),
    )?;
    Ok(())
}

fn create_parent_dirs(base: &Path, relative: &Path) -> Result<()> {
    if let Some(parent) = relative.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(base.join(parent))?;
    }
    Ok(())
}

/// Recursively copy the *contents* of `from` into `to`.
pub(crate) fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(from)? {
        entries.push(entry?.path());
    }
    entries.sort();
    for path in entries {
        let name = path.file_name().expect("entry has a file name").to_owned();
        let target = to.join(&name);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            copy_tree(&path, &target)?;
        } else if metadata.is_file() {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_sample_source(dir: &Path) -> PathBuf {
        let mut body = String::from("pub fn compare(a: i32, b: i32, flag: bool) -> i32 {\n");
        for index in 0..14 {
            body.push_str(&format!(
                "    let first_{index} = a + {index};\n    let second_{index} = b - {index};\n"
            ));
            body.push_str(&format!(
                "    let ordered_{index} = a <= b && b >= a;\n    let equal_{index} = a == b || b != a;\n"
            ));
            body.push_str(&format!("    // rows {index}: a < b and true\n"));
        }
        body.push_str("    if !flag {\n        return 1;\n    }\n");
        body.push_str("    let on = true;\n    let off = false;\n    a + b\n}\n");
        let path = dir.join("sample.rs");
        fs::write(&path, body).expect("write sample source");
        path
    }

    fn differing_lines(expected: &str, actual: &str) -> Vec<usize> {
        let expected: Vec<&str> = expected.split('\n').collect();
        let actual: Vec<&str> = actual.split('\n').collect();
        assert_eq!(expected.len(), actual.len(), "line count must not change");
        expected
            .iter()
            .zip(actual.iter())
            .enumerate()
            .filter(|(_, (left, right))| left != right)
            .map(|(index, _)| index + 1)
            .collect()
    }

    #[test]
    fn generate_writes_requested_tasks_with_one_changed_line_each() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_root = temp.path().join("src");
        fs::create_dir_all(&source_root).expect("create source dir");
        write_sample_source(&source_root);
        let out = temp.path().join("fixtures");

        let tasks = generate(&GenerateOptions {
            source: source_root.clone(),
            out: out.clone(),
            seed: 1,
            count: 5,
        })
        .expect("generate");
        assert_eq!(tasks.len(), 5);

        let mut ids: Vec<&str> = tasks.iter().map(|task| task.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 5, "task ids must be unique");

        for task in &tasks {
            let dir = out.join(&task.id);
            assert!(dir.join("prompt.md").is_file(), "{}: prompt.md", task.id);
            assert!(dir.join("metadata.json").is_file(), "{}: metadata.json", task.id);
            let input = fs::read_to_string(dir.join("input").join(&task.relative)).expect("input");
            let expected = fs::read_to_string(dir.join("expected").join(&task.relative)).expect("expected");
            assert_eq!(input, task.mutated_source);
            assert_eq!(expected, task.original_source);

            let changed = differing_lines(&expected, &input);
            if task.metadata.mutation == Mutation::SwapAdjacentLines.as_str() {
                assert_eq!(changed.len(), 2, "{} swapped lines", task.id);
                assert_eq!(changed[0] + 1, changed[1]);
            } else {
                assert_eq!(changed.len(), 1, "{} changed {changed:?}", task.id);
            }
            assert_eq!(changed[0], task.metadata.line, "{} line", task.id);

            let prompt = fs::read_to_string(dir.join("prompt.md")).expect("prompt");
            assert!(prompt.contains(&format!("near line {}", task.metadata.line)));
            assert!(prompt.contains(&task.metadata.file));
            assert!(prompt.contains("Fix it with the smallest possible edit."));

            let metadata: TaskMetadata =
                serde_json::from_str(&fs::read_to_string(dir.join("metadata.json")).unwrap()).expect("metadata json");
            assert_eq!(metadata.seed, 1);
            assert_eq!(metadata.mutation, task.metadata.mutation);
            let expected_lines: Vec<&str> = expected.split('\n').collect();
            let span = metadata.original.split('\n').count();
            let original_span = expected_lines[metadata.line - 1..metadata.line - 1 + span].join("\n");
            assert_eq!(metadata.original, original_span);
            assert_ne!(metadata.original, metadata.mutated);
        }
    }

    #[test]
    fn generate_is_deterministic_for_a_seed() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_root = temp.path().join("src");
        fs::create_dir_all(&source_root).expect("create source dir");
        write_sample_source(&source_root);

        let first = generate(&GenerateOptions {
            source: source_root.clone(),
            out: temp.path().join("a"),
            seed: 42,
            count: 4,
        })
        .expect("generate a");
        let second = generate(&GenerateOptions {
            source: source_root.clone(),
            out: temp.path().join("b"),
            seed: 42,
            count: 4,
        })
        .expect("generate b");

        let ids = |tasks: &[GeneratedTask]| {
            tasks
                .iter()
                .map(|task| (task.id.clone(), task.metadata.line, task.metadata.mutation.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&first), ids(&second));
    }

    #[test]
    fn generate_rejects_a_source_tree_without_rust_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let error = generate(&GenerateOptions {
            source: temp.path().to_path_buf(),
            out: temp.path().join("out"),
            seed: 1,
            count: 1,
        })
        .expect_err("empty tree must fail");
        assert!(error.to_string().contains("no .rs files with at least"), "{error}");
    }

    #[test]
    fn collect_source_files_skips_short_files_and_target_dirs() {
        let temp = tempfile::tempdir().expect("temp dir");
        let short = temp.path().join("short.rs");
        fs::write(&short, "fn tiny() {}\n").unwrap();
        let target = temp.path().join("target").join("deep.rs");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "fn big() {}\n".repeat(MIN_FILE_LINES)).unwrap();
        write_sample_source(temp.path());

        let files = collect_source_files(temp.path()).expect("collect");
        assert_eq!(files, vec![temp.path().join("sample.rs")]);
    }
}
