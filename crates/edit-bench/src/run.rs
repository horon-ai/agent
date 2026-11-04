//! Runner: executes an arm against generated fixtures through the real
//! `horon-agent run` CLI and records one JSONL row per `(task, repeat)`.
//!
//! CLI mapping (verified against `src/cli/args.rs` and
//! `src/cli/commands.rs`):
//!
//! - the working directory is a temp copy of `input/`, passed with the global
//!   `-C <dir>` flag;
//! - the prompt is the positional message of the `run` subcommand and events
//!   come from `run --ndjson` on stdout;
//! - `tool_start` events carry `name`, `tokens` events carry `input`/`output`;
//! - `--model` is a global flag, so it is placed before `run`;
//! - `HORON_AGENT_HOME` points at a temp dir holding the arm's `config.toml`,
//!   and `HORON_AGENT_RUN_MCP=0` / `HORON_AGENT_RUN_AUTO_POKE=0` keep one
//!   invocation to one turn with no MCP servers.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::generate::copy_tree;
use crate::results::ResultRow;

/// Per-task wall-clock budget for one agent run.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(300);

/// How often the timeout loop re-checks the child.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub fixtures: PathBuf,
    pub arm: String,
    pub config: PathBuf,
    pub model: Option<String>,
    pub repeat: usize,
    pub out: PathBuf,
}

/// Run every fixture in `options.fixtures` `options.repeat` times.
pub fn run(options: &RunOptions) -> Result<Vec<ResultRow>> {
    if options.repeat == 0 {
        bail!("--repeat must be at least 1");
    }
    if !options.config.is_file() {
        bail!("config file not found: {}", options.config.display());
    }
    let tasks = discover_tasks(&options.fixtures)?;
    if tasks.is_empty() {
        bail!("no task directories under {}", options.fixtures.display());
    }
    let binary = agent_binary();

    let results_dir = options.out.join(&options.arm);
    fs::create_dir_all(&results_dir).with_context(|| format!("cannot create {}", results_dir.display()))?;
    let results_path = results_dir.join("results.jsonl");
    let mut results_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&results_path)
        .with_context(|| format!("cannot open {}", results_path.display()))?;

    let total = tasks.len() * options.repeat;
    let mut rows = Vec::with_capacity(total);
    let mut index = 0usize;
    for task in &tasks {
        let name = task
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        for rep in 0..options.repeat {
            index += 1;
            let rep = rep as u32;
            let row = run_one(task, rep, options, &binary).unwrap_or_else(|error| {
                eprintln!("{name} rep {rep}: {error:#}");
                ResultRow {
                    task: name.clone(),
                    rep,
                    arm: options.arm.clone(),
                    ..ResultRow::default()
                }
            });
            println!(
                "[{index}/{total}] {} rep {}: pass={} tools={} out_tokens={} ({}ms)",
                row.task, row.rep, row.pass, row.tool_calls, row.output_tokens, row.duration_ms
            );
            writeln!(results_file, "{}", serde_json::to_string(&row)?)?;
            results_file.flush()?;
            rows.push(row);
        }
    }

    let passed = rows.iter().filter(|row| row.pass).count();
    let mean_tools = mean(rows.iter().map(|row| row.tool_calls as f64));
    let mean_tokens = mean(rows.iter().map(|row| row.output_tokens as f64));
    println!(
        "{}: {passed}/{} passed, mean tool calls {mean_tools:.1}, mean output tokens {mean_tokens:.0}",
        options.arm,
        rows.len()
    );
    println!("results: {}", results_path.display());
    Ok(rows)
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for value in values {
        sum += value;
        count += 1;
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

/// Task directories (sorted), each holding `prompt.md`.
pub(crate) fn discover_tasks(fixtures: &Path) -> Result<Vec<PathBuf>> {
    let mut tasks = Vec::new();
    for entry in fs::read_dir(fixtures).with_context(|| format!("cannot list {}", fixtures.display()))? {
        let path = entry?.path();
        if path.is_dir() && path.join("prompt.md").is_file() {
            tasks.push(path);
        }
    }
    tasks.sort();
    Ok(tasks)
}

/// `HORON_AGENT_BIN`, else `horon-agent` from `PATH`.
pub(crate) fn agent_binary() -> PathBuf {
    std::env::var_os("HORON_AGENT_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("horon-agent"))
}

fn run_one(task_dir: &Path, rep: u32, options: &RunOptions, binary: &Path) -> Result<ResultRow> {
    let name = task_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let prompt = fs::read_to_string(task_dir.join("prompt.md"))?.trim().to_string();
    let input_root = task_dir.join("input");
    let expected_root = task_dir.join("expected");

    let work = tempfile::TempDir::new().context("cannot create a working directory")?;
    copy_tree(&input_root, work.path())
        .with_context(|| format!("cannot copy {} into a temp dir", input_root.display()))?;
    let home = tempfile::TempDir::new().context("cannot create a HORON_AGENT_HOME")?;
    fs::copy(&options.config, home.path().join("config.toml"))
        .with_context(|| format!("cannot stage {}", options.config.display()))?;

    let mut command = Command::new(binary);
    command
        .arg("-C")
        .arg(work.path())
        .arg("run")
        .arg("--ndjson")
        .arg(&prompt)
        .env("HORON_AGENT_HOME", home.path())
        .env("HORON_AGENT_RUN_MCP", "0")
        .env("HORON_AGENT_RUN_AUTO_POKE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(model) = &options.model {
        command.arg("--model").arg(model);
    }

    let started = Instant::now();
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot spawn {}", binary.display()))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let stdout_reader = std::thread::spawn(move || drain(stdout));
    let stderr_reader = std::thread::spawn(move || drain(stderr));

    let deadline = started + RUN_TIMEOUT;
    let mut timed_out = false;
    let mut exit_ok = false;
    loop {
        if let Some(status) = child.try_wait()? {
            exit_ok = status.success();
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    let duration_ms = started.elapsed().as_millis() as u64;
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    let mut tool_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut run_error = false;
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match event.get("type").and_then(|value| value.as_str()) {
            Some("tool_start") => {
                if let Some(name) = event.get("name").and_then(|value| value.as_str()) {
                    *tool_counts.entry(name.to_string()).or_insert(0) += 1;
                }
            }
            Some("tokens") => {
                input_tokens += event.get("input").and_then(|value| value.as_u64()).unwrap_or(0);
                output_tokens += event.get("output").and_then(|value| value.as_u64()).unwrap_or(0);
            }
            Some("error") => {
                run_error = true;
                if let Some(message) = event.get("message").and_then(|value| value.as_str()) {
                    eprintln!("{name} rep {rep}: agent error: {message}");
                }
            }
            _ => {}
        }
    }
    if timed_out {
        eprintln!("{name} rep {rep}: timed out after {}s", RUN_TIMEOUT.as_secs());
    } else if !exit_ok {
        let lines: Vec<&str> = stderr.lines().collect();
        let start = lines.len().saturating_sub(20);
        eprintln!("{name} rep {rep}: exit failure\n{}", lines[start..].join("\n"));
    }

    let files_ok = files_match(&expected_root, work.path())?;
    let tool_calls = tool_counts.values().sum();
    Ok(ResultRow {
        task: name,
        rep,
        arm: options.arm.clone(),
        pass: files_ok && exit_ok && !timed_out && !run_error,
        tool_calls,
        tool_counts,
        input_tokens,
        output_tokens,
        duration_ms,
    })
}

fn drain(mut reader: impl Read) -> String {
    let mut buffer = Vec::new();
    let _ = reader.read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).into_owned()
}

/// Every file under `expected_root` must exist under `actual_root` and match
/// after [`normalize`].
pub(crate) fn files_match(expected_root: &Path, actual_root: &Path) -> Result<bool> {
    let mut compared = 0usize;
    let mut all_match = true;
    for expected in walk_files(expected_root)? {
        let relative = expected.strip_prefix(expected_root)?.to_path_buf();
        let actual = actual_root.join(&relative);
        let expected_text =
            fs::read_to_string(&expected).with_context(|| format!("cannot read {}", expected.display()))?;
        let matched = match fs::read_to_string(&actual) {
            Ok(actual_text) => normalize(&actual_text) == normalize(&expected_text),
            Err(_) => false,
        };
        compared += 1;
        all_match &= matched;
    }
    Ok(compared > 0 && all_match)
}

fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) => bail!("cannot list {}: {error}", dir.display()),
        };
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Compare text ignoring trailing whitespace per line, trailing blank lines,
/// and the file's final newline count.
pub(crate) fn normalize(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().map(|line| line.trim_end()).collect();
    while lines.last() == Some(&"") {
        lines.pop();
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ignores_trailing_whitespace_and_final_newline() {
        assert_eq!(normalize("a  \nb\t\n"), normalize("a\nb"));
        assert_eq!(normalize("a\nb\n"), "a\nb\n");
        assert_eq!(normalize(""), "");
        assert_ne!(normalize("a\nb"), normalize("a\nc"));
    }

    #[test]
    fn files_match_compares_expected_files() {
        let temp = tempfile::tempdir().unwrap();
        let expected = temp.path().join("expected");
        let actual = temp.path().join("actual");
        fs::create_dir_all(expected.join("src")).unwrap();
        fs::create_dir_all(actual.join("src")).unwrap();
        fs::write(expected.join("src/x.rs"), "fn a() {}\n").unwrap();
        fs::write(actual.join("src/x.rs"), "fn a() {}   \n\n").unwrap();
        assert!(files_match(&expected, &actual).unwrap());

        fs::write(actual.join("src/x.rs"), "fn b() {}\n").unwrap();
        assert!(!files_match(&expected, &actual).unwrap());

        fs::remove_file(actual.join("src/x.rs")).unwrap();
        assert!(!files_match(&expected, &actual).unwrap());
    }

    #[test]
    fn discover_tasks_requires_a_prompt() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("b-task")).unwrap();
        fs::write(temp.path().join("b-task/prompt.md"), "fix it\n").unwrap();
        fs::create_dir(temp.path().join("a-task")).unwrap();
        fs::write(temp.path().join("a-task/prompt.md"), "fix it\n").unwrap();
        fs::create_dir(temp.path().join("ignored")).unwrap();
        let tasks = discover_tasks(temp.path()).unwrap();
        assert_eq!(tasks, vec![temp.path().join("a-task"), temp.path().join("b-task")]);
    }

    #[test]
    fn events_are_counted_from_ndjson() {
        // The event shapes come from `emit_ndjson_event` in
        // `src/cli/commands.rs`; keep this fixture in sync with it.
        let stdout = concat!(
            r#"{"type":"start","session_id":"s","provider":"p","model":"m"}"#,
            "\n",
            r#"{"type":"tool_start","id":"1","name":"read"}"#,
            "\n",
            r#"{"type":"tool_start","id":"2","name":"edit"}"#,
            "\n",
            r#"{"type":"tool_start","id":"3","name":"read"}"#,
            "\n",
            r#"{"type":"tokens","input":100,"output":20,"cache_read_input":0,"cache_creation_input":0}"#,
            "\n",
            r#"{"type":"tokens","input":50,"output":5,"cache_read_input":0,"cache_creation_input":0}"#,
            "\n",
            r#"{"type":"done","text":"ok"}"#,
        );
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        let mut input_tokens = 0u64;
        let mut output_tokens = 0u64;
        for line in stdout.lines() {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            match event.get("type").and_then(|value| value.as_str()) {
                Some("tool_start") => {
                    let name = event.get("name").and_then(|v| v.as_str()).unwrap();
                    *counts.entry(name.to_string()).or_insert(0) += 1;
                }
                Some("tokens") => {
                    input_tokens += event.get("input").and_then(|v| v.as_u64()).unwrap_or(0);
                    output_tokens += event.get("output").and_then(|v| v.as_u64()).unwrap_or(0);
                }
                _ => {}
            }
        }
        assert_eq!(counts.get("read"), Some(&2));
        assert_eq!(counts.get("edit"), Some(&1));
        assert_eq!(counts.values().sum::<u64>(), 3);
        assert_eq!((input_tokens, output_tokens), (150, 25));
    }
}
