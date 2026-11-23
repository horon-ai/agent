//! Micro-benchmarks for round-3 optimization paths in `horon-tui`.
//!
//! Exercises public surfaces (`App::apply_event`, `App::new`, `App::open_settings`,
//! `App::on_key`, `draw`, `/diff`, `/commit`, `/copy`, draft suggestions) under
//! release `opt-level = 3` with a deterministic counting allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use horon_agent_sdk::api::ApiEvent;
use horon_tui::app::App;
use horon_tui::sdk_bridge::Incoming;
use horon_tui::settings::Sources;
use horon_tui::settings::layers::LayerSet;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

struct CountingAlloc;

impl CountingAlloc {
    fn record(bytes: usize) {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
    }
}

// SAFETY: forwards every call to `System` and updates only atomics/thread-local bool.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

const RUNS: usize = 7;

fn measure<F: FnMut() -> Result<(), Box<dyn Error>>>(name: &str, mut work: F) -> Result<(), Box<dyn Error>> {
    // Warmup run outside timing.
    work()?;
    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _ in 0..RUNS {
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.with(|c| c.set(true));
        let start = Instant::now();
        let res = work();
        let elapsed = start.elapsed();
        COUNTING.with(|c| c.set(false));
        res?;
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
        times_us.push(elapsed.as_micros());
    }
    times_us.sort_unstable();
    let min = times_us[0];
    let med = times_us[RUNS / 2];
    let max = times_us[RUNS - 1];
    println!(
        "BENCH {name}: runs={RUNS} min_us={min} med_us={med} max_us={max} allocs={last_allocs} alloc_bytes={last_bytes}"
    );
    Ok(())
}

fn submit(app: &mut App, line: &str) {
    app.input.set_text(line);
    drop(app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
}

fn type_keys(app: &mut App, text: &str) {
    for ch in text.chars() {
        drop(app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)));
    }
}

fn build_app_with_transcript(turns: usize) -> App {
    let (_tx, rx) = mpsc::channel::<Incoming>();
    let mut app = App::new("socket · bench".to_string(), rx);
    for i in 0..turns {
        app.transcript.push_user(format!("Prompt #{i}: inspect module status"));
        app.transcript.push_assistant(format!(
            "Response #{i}: checked status, verified state invariants, and completed pass {i}."
        ));
    }
    app
}

fn git_cmd(dir: &Path, args: &[&str]) -> Result<(), Box<dyn Error>> {
    let out = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(format!("git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(())
}

fn setup_git_repo(hooks_dir: &Path, staged_only: bool) -> Result<tempfile::TempDir, Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    git_cmd(root, &["init", "-q", "-b", "main"])?;
    let hooks_str = hooks_dir.to_str().ok_or_else(|| "non-utf8 hooks dir".to_string())?;
    for (k, v) in [
        ("user.name", "bench"),
        ("user.email", "bench@example.invalid"),
        ("commit.gpgsign", "false"),
        ("tag.gpgsign", "false"),
        ("core.autocrlf", "false"),
        ("core.fsmonitor", "false"),
        ("core.hooksPath", hooks_str),
        ("gc.auto", "0"),
    ] {
        git_cmd(root, &["config", k, v])?;
    }
    for i in 0..6 {
        fs::write(
            root.join(format!("file_{i}.txt")),
            format!("line 1 of {i}\nline 2 of {i}\n"),
        )?;
    }
    git_cmd(root, &["add", "."])?;
    git_cmd(root, &["commit", "-q", "-m", "base"])?;
    fs::write(root.join("file_0.txt"), "line 1 of 0\nchanged staged line\n")?;
    git_cmd(root, &["add", "--", "file_0.txt"])?;
    if !staged_only {
        fs::write(root.join("file_1.txt"), "line 1 of 1\nchanged unstaged line\n")?;
        fs::write(root.join("untracked.txt"), "new untracked content\n")?;
    }
    Ok(dir)
}

fn large_code_answer(index: usize) -> String {
    let mut out = format!("Explanation for step {index}:\n\n");
    for block in 0..2 {
        out.push_str("```rust\n");
        for line in 0..150 {
            out.push_str(&format!("pub fn item_{index}_{block}_{line}() -> usize {{ {line} }}\n"));
        }
        out.push_str("```\n\n");
    }
    for line in 0..80 {
        out.push_str(&format!("Detail line {line} for answer {index}.\n"));
    }
    out
}

fn main() -> Result<(), Box<dyn Error>> {
    // 1a. record_new_assistants when recording is OFF (delta flood).
    let mut off_app = build_app_with_transcript(150);
    let deltas: Vec<ApiEvent> = (0..400)
        .map(|i| ApiEvent::TextDelta {
            session_id: "bench".to_string(),
            text: format!("chunk-{i} "),
            message_id: Some("msg-live".to_string()),
        })
        .collect();
    let done = ApiEvent::TextDone {
        session_id: "bench".to_string(),
        message_id: Some("msg-live".to_string()),
    };
    measure("tui_record_off_400_deltas", || {
        for ev in &deltas {
            off_app.apply_event(ev);
        }
        off_app.apply_event(&done);
        black_box(off_app.transcript.items().len());
        Ok(())
    })?;

    // 1b. record_new_assistants when recording is ON.
    let rec_dir = tempfile::tempdir()?;
    let mut rec_apps: Vec<App> = (0..=RUNS).map(|_| build_app_with_transcript(100)).collect();
    let mut rec_run = 0usize;
    measure("tui_record_on_400_deltas_20_turns", || {
        let app = &mut rec_apps[rec_run];
        rec_run += 1;
        let rec_path = rec_dir.path().join(format!("rec-{rec_run}.jsonl"));
        submit(app, &format!("/record {}", rec_path.display()));
        for t in 0..20 {
            let msg_id = format!("m-{t}");
            for d in 0..20 {
                app.apply_event(&ApiEvent::TextDelta {
                    session_id: "bench".to_string(),
                    text: format!("part-{d} "),
                    message_id: Some(msg_id.clone()),
                });
            }
            app.apply_event(&ApiEvent::TextDone {
                session_id: "bench".to_string(),
                message_id: Some(msg_id),
            });
        }
        submit(app, "/record stop");
        black_box(app.transcript.items().len());
        Ok(())
    })?;

    // 2. App::new startup cost (Voice::new PATH + models probe).
    measure("tui_app_new_startup_100", || {
        let mut count = 0usize;
        for _ in 0..100 {
            let (_tx, rx) = mpsc::channel::<Incoming>();
            let app = App::new("socket · bench".to_string(), rx);
            count = count.wrapping_add(app.transcript.items().len());
            black_box(app);
        }
        black_box(count);
        Ok(())
    })?;

    // 3. SettingsOverlay: open_settings, search typing, draw.
    let settings_dir = tempfile::tempdir()?;
    let themes_dir = settings_dir.path().join("themes");
    fs::create_dir_all(&themes_dir)?;
    for name in ["alpha", "beta", "gamma", "delta", "epsilon"] {
        fs::write(themes_dir.join(format!("{name}.json")), "{}")?;
    }
    let global_toml = settings_dir.path().join("tui.toml");
    fs::write(
        &global_toml,
        "[editor]\nsuggestions = true\n[transcript]\nturn_usage = true\n",
    )?;
    let sources = Sources {
        global: global_toml,
        project: None,
        configs: Vec::new(),
    };
    let mut warnings = Vec::new();
    let mut layers = LayerSet::new(&sources);
    layers.refresh(&mut warnings);
    let (resolved_settings, _resolve_warnings) = layers.resolve(|_: &str| None);

    let (_tx, rx) = mpsc::channel::<Incoming>();
    let mut settings_app = App::new("socket · bench".to_string(), rx);
    settings_app.themes_dir = themes_dir;
    settings_app.refresh_settings(&resolved_settings);

    measure("tui_settings_open_200", || {
        let mut opened = 0usize;
        for _ in 0..200 {
            drop(settings_app.open_settings());
            if let Some(ov) = settings_app.overlay.take() {
                opened += ov.tabs().len();
                black_box(ov);
            }
        }
        black_box(opened);
        Ok(())
    })?;

    drop(settings_app.open_settings());
    measure("tui_settings_search_typing_50", || {
        let mut hits = 0usize;
        for _ in 0..50 {
            type_keys(&mut settings_app, "/statusline");
            for _ in 0..5 {
                drop(settings_app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
            }
            type_keys(&mut settings_app, "max");
            if let Some(ov) = settings_app.overlay.as_ref() {
                hits += ov.rows().len();
            }
            // Single Esc leaves Search mode and clears query while keeping the overlay open.
            drop(settings_app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        }
        assert!(settings_app.overlay.is_some());
        black_box(hits);
        Ok(())
    })?;

    // Ensure search filter is active during draw to stress row/group rendering, and also normal tab.
    let mut settings_term = Terminal::new(TestBackend::new(100, 32))?;
    measure("tui_settings_draw_browse_200f", || {
        for _ in 0..200 {
            horon_tui::draw(&mut settings_term, &mut settings_app)?;
        }
        black_box(settings_term.backend().buffer().content().len());
        Ok(())
    })?;

    type_keys(&mut settings_app, "/e");
    measure("tui_settings_draw_search_200f", || {
        for _ in 0..200 {
            horon_tui::draw(&mut settings_term, &mut settings_app)?;
        }
        black_box(settings_term.backend().buffer().content().len());
        Ok(())
    })?;
    drop(settings_app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    drop(settings_app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));

    // 4. git_review (/diff + /commit review & execute).
    let hooks_dir = tempfile::tempdir()?;
    let diff_repo = setup_git_repo(hooks_dir.path(), false)?;
    let staged_only_repo = setup_git_repo(hooks_dir.path(), true)?;
    let mut commit_repos = Vec::with_capacity(RUNS + 1);
    for _ in 0..=RUNS {
        commit_repos.push(setup_git_repo(hooks_dir.path(), false)?);
    }

    let (_tx, rx) = mpsc::channel::<Incoming>();
    let mut git_app = App::new("socket · bench".to_string(), rx);
    git_app.workspace_known = true;
    git_app.workspace_is_local = true;

    git_app.cwd = Some(diff_repo.path().display().to_string());
    measure("tui_git_diff_both", || {
        submit(&mut git_app, "/diff");
        black_box(git_app.transcript.items().len());
        Ok(())
    })?;

    git_app.cwd = Some(staged_only_repo.path().display().to_string());
    measure("tui_git_diff_staged_only_repo", || {
        submit(&mut git_app, "/diff");
        black_box(git_app.transcript.items().len());
        Ok(())
    })?;

    git_app.cwd = Some(diff_repo.path().display().to_string());
    measure("tui_git_commit_review_cancel", || {
        submit(&mut git_app, "/commit review bench");
        submit(&mut git_app, "/commit --cancel");
        black_box(git_app.transcript.items().len());
        Ok(())
    })?;

    let mut commit_idx = 0usize;
    measure("tui_git_commit_review_and_yes", || {
        let repo = &commit_repos[commit_idx];
        commit_idx += 1;
        git_app.cwd = Some(repo.path().display().to_string());
        submit(&mut git_app, "/commit commit bench");
        submit(&mut git_app, "/commit --yes");
        black_box(git_app.transcript.items().len());
        Ok(())
    })?;

    // 5. Draft suggestions steady draw & /copy picker open.
    let (_tx, rx) = mpsc::channel::<Incoming>();
    let mut sugg_app = App::new("socket · bench".to_string(), rx);
    sugg_app.refresh_settings(&resolved_settings);
    sugg_app
        .history
        .record("explain how the extension manifest verification pipeline works in detail");
    sugg_app.input.set_text("explain how the ");
    let mut sugg_term = Terminal::new(TestBackend::new(100, 30))?;
    horon_tui::draw(&mut sugg_term, &mut sugg_app)?;
    measure("tui_draw_suggestions_steady_200f", || {
        for _ in 0..200 {
            horon_tui::draw(&mut sugg_term, &mut sugg_app)?;
        }
        black_box(sugg_term.backend().buffer().content().len());
        Ok(())
    })?;

    let (_tx, rx) = mpsc::channel::<Incoming>();
    let mut copy_app = App::new("socket · bench".to_string(), rx);
    for i in 0..4 {
        copy_app.transcript.push_assistant(large_code_answer(i));
    }
    measure("tui_copy_picker_open_50", || {
        let mut rows = 0usize;
        for _ in 0..50 {
            submit(&mut copy_app, "/copy");
            if let Some(picker) = copy_app.picker.take() {
                rows += picker.len();
                black_box(picker);
            }
        }
        black_box(rows);
        Ok(())
    })?;

    Ok(())
}
