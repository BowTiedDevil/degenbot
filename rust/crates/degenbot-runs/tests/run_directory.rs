//! Run-directory acceptance: layout, artifact seeding, the `latest` symlink,
//! and typed IO errors. Uses explicit roots so no test touches the process
//! environment or the real config.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use degenbot_runs::RunDirectory;

static SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("degenbot-runs-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn create_in_seeds_the_session_layout() {
    let root = scratch("layout");
    let run = RunDirectory::create_in(&root, "backrun-sidecar").expect("create");

    let engine_dir = root.join("backrun-sidecar");
    assert_eq!(run.engine_dir(), engine_dir);
    assert!(
        run.session_dir().starts_with(&engine_dir),
        "session nested under the engine dir"
    );
    let name = run
        .session_dir()
        .file_name()
        .and_then(|n| n.to_str())
        .expect("session dir name");
    assert!(
        name.ends_with(&format!("-{}", std::process::id())),
        "session name carries the pid: {name}"
    );
    assert_eq!(
        name.len(),
        "20231114T221320Z".len() + 1 + std::process::id().to_string().len(),
        "yyyymmddTHHMMSSZ-pid shape: {name}"
    );
    assert!(run.stdout_path().is_file(), "stdout.log seeded");
    assert!(run.trace_jsonl_path().is_file(), "trace.jsonl seeded");
    assert_eq!(
        std::fs::metadata(run.stdout_path())
            .expect("metadata")
            .len(),
        0,
        "stdout.log starts empty"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn latest_symlink_replaces_atomically_to_newest_session() {
    let root = scratch("latest");
    let first = RunDirectory::create_in(&root, "engine").expect("first");
    // A monotonic stamp distinguishes sessions within one second.
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    let second = RunDirectory::create_in(&root, "engine").expect("second");

    let latest = root.join("engine").join("latest");
    let target = std::fs::read_link(&latest).expect("latest symlink");
    assert_eq!(
        target,
        PathBuf::from(second.session_dir().file_name().expect("name")),
        "latest points at the newest session"
    );
    assert_ne!(first.session_dir(), second.session_dir());

    // A pre-existing non-symlink `latest` must not abort the run: replace is
    // best-effort and leaves the session dir intact.
    let _ = std::fs::remove_file(&latest);
    std::fs::write(&latest, b"stale").expect("write stale file");
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    let third = RunDirectory::create_in(&root, "engine").expect("third");
    assert!(third.session_dir().is_dir());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn engine_name_must_be_a_single_component() {
    let root = scratch("engine");
    for bad in ["", "a/b", ".."] {
        let err = RunDirectory::create_in(&root, bad).expect_err("must reject");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{bad:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn root_path_blocked_by_a_file_is_a_typed_io_error() {
    let root = scratch("blocked");
    std::fs::create_dir_all(&root).expect("mkdir");
    let file = root.join("blocker");
    std::fs::write(&file, b"x").expect("write file");

    let err = RunDirectory::create_in(&file, "engine").expect_err("file root must fail");
    assert!(
        matches!(
            err.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotADirectory
        ),
        "filesystem error keeps its kind: {:?}",
        err.kind()
    );

    let _ = std::fs::remove_dir_all(&root);
}
