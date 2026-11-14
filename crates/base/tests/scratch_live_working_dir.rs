//! A live process's working directory is never deleted, whatever its age.
//!
//! The unit tests in `crates/base/src/scratch_maintenance.rs` pass an explicit
//! `busy` list, so they prove the comparison and not the platform answer. This
//! check goes through the real adapter: a child process is started with its
//! working directory inside a scratch entry whose whole tree is older than the
//! retention window, and the prune must keep that entry. On a platform whose
//! `process_working_dirs` returns nothing (Windows, before this change macOS)
//! the entry is deleted and the assertion fails, which is the whole defect.

use horon_agent_base::scratch_maintenance::{DEFAULT_SCRATCH_RETENTION_DAYS, prune_stale_scratch_in};
use std::fs::{self, File};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Age every file and directory under `path`, deepest last so a parent is
/// touched after its children.
fn age_tree(path: &Path, by: Duration) {
    let when = SystemTime::now() - by;
    let mut stack = vec![path.to_path_buf()];
    let mut all = Vec::new();
    while let Some(current) = stack.pop() {
        all.push(current.clone());
        if current.is_dir()
            && let Ok(children) = fs::read_dir(&current)
        {
            stack.extend(children.flatten().map(|child| child.path()));
        }
    }
    // Children first, then parents, so ageing a directory does not look fresh
    // merely because its children were just written.
    all.sort();
    for entry in all {
        let file = File::options().read(true).open(&entry).expect("open for mtime");
        file.set_modified(when).expect("set mtime");
    }
}

/// A child that sits in `dir` until this guard drops it.
struct ChildInDir {
    child: Child,
}

impl ChildInDir {
    fn start(dir: &Path) -> Self {
        // `sleep` holds the working directory without writing anything, which is
        // exactly the case the mtime check cannot save: an idle shell in a
        // scratch tree looks untouched. This check runs on Unix only; the
        // compiler gate lives here so no OS condition reaches the test body.
        let mut command = Command::new("sleep");
        command.arg("60");
        let child = command
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a child in the scratch entry");
        // The kernel records the working directory at exec, so wait until the
        // snapshot really names this directory before the prune runs. A process
        // still starting would otherwise make the test's premise unproven.
        let expected = std::fs::canonicalize(dir).expect("canonical scratch entry");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let busy = horon_agent_platform::process::process_working_dirs();
            if busy
                .iter()
                .any(|cwd| cwd == &expected || std::fs::canonicalize(cwd).is_ok_and(|resolved| resolved == expected))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the child never appeared in the working-directory snapshot under {expected:?}; \
                 the platform answer cannot guard scratch cleanup"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Self { child }
    }
}

impl Drop for ChildInDir {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_live_working_directory_survives_the_prune() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scratch = temp.path().join("scratch");
    fs::create_dir_all(&scratch).unwrap();
    // The kernel reports canonical paths (`/private/var` for `/var`), and
    // `prune_stale_scratch` canonicalizes its base for that reason. Do the same
    // here so the comparison sees one spelling.
    let scratch = fs::canonicalize(&scratch).expect("canonical scratch");

    // One entry a child sits in, and one control entry nothing uses.
    let live = scratch.join("in-progress-build");
    fs::create_dir_all(&live).unwrap();
    fs::write(live.join("Cargo.lock"), b"stale").unwrap();
    let idle = scratch.join("finished-build");
    fs::create_dir_all(&idle).unwrap();
    fs::write(idle.join("artifact.rlib"), b"stale").unwrap();

    let retention = Duration::from_secs(DEFAULT_SCRATCH_RETENTION_DAYS * 24 * 60 * 60);
    age_tree(&scratch, retention + 30 * DAY);

    let _child = ChildInDir::start(&live);

    // The real platform answer, not a hand-built list.
    let busy = horon_agent_platform::process::process_working_dirs();
    assert!(
        !busy.is_empty(),
        "process_working_dirs returned nothing, so the in-use guard is inert on this platform"
    );

    let report = prune_stale_scratch_in(&scratch, SystemTime::now(), retention, &busy);

    assert!(
        live.join("Cargo.lock").exists(),
        "a live process's working directory was deleted; kept {} entries, removed {:?}",
        report.kept,
        report.removed
    );
    assert!(
        !idle.exists(),
        "the control entry, which nothing uses, must still be removed"
    );
}

#[test]
fn the_platform_answer_names_a_directory_it_can_be_asked_about() {
    // Guards the adapter independently of the prune: the answer must contain a
    // path that really exists, so a snapshot full of garbage cannot satisfy it.
    let dirs = horon_agent_platform::process::process_working_dirs();
    assert!(!dirs.is_empty(), "the snapshot must not be empty");
    assert!(
        dirs.iter().any(|dir| dir.is_dir()),
        "at least one reported working directory must exist: {dirs:?}"
    );
}
