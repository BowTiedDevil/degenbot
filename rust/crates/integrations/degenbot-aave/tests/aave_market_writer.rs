//! The shared Aave market writer across a real process boundary.
//!
//! The exclusivity contract is one advisory `flock` per (canonical database
//! path, market id), taken by `AaveMarketWriter`. Two writers in one process
//! collide through two open file descriptions; two PROCESSES are the case the
//! lock actually exists for, so the holder here is a re-executed copy of this
//! test binary that dies on SIGKILL.
//!
//! The lock file is created if absent and never unlinked: an unlink would let a
//! waiter blocked on the old inode and a newcomer holding the re-created path
//! both believe they own the market. The recreation tests below pin that: the
//! file outlives its holder, a deleted file is recreated as ONE owner, and a
//! stale pid from a dead holder never blocks a takeover.

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use degenbot_aave::updater::{NoProgress, RunError, RunSpec, VerifyPolicy};
use degenbot_aave::{market_lock_path, AaveMarketWriter};
use degenbot_db::DegenbotDb;
use degenbot_rpc::offline::OfflineProvider;
use degenbot_rpc::provider::AlloyProvider;
use tempfile::TempDir;

const MARKET_ID: i64 = 7;

/// A provider over an empty recorded corpus: the mismatch regressions refuse
/// before any read reaches it, so it must never dial anything.
fn inert_provider() -> AlloyProvider {
    OfflineProvider::from_json_str(r#"{"chain_id":1,"block_number":1}"#)
        .expect("the empty recorded corpus parses")
        .as_alloy_provider()
}

fn spec(market_id: i64, to_block: u64) -> RunSpec {
    RunSpec {
        chain_id: 1,
        market_id,
        to_block: Some(to_block),
        chunk_size: 1,
        max_block_range: 1,
        max_chunks: None,
        verify: VerifyPolicy::default(),
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(NoProgress),
        provider: inert_provider(),
    }
}

fn database(dir: &Path) -> PathBuf {
    let path = dir.join("degenbot.db");
    degenbot_db::create_new_database(&path).unwrap();
    path
}

/// Spawn the `#[ignore]` holder test as a competing PROCESS and wait until it
/// reports the lock armed.
fn spawn_holder(db: &Path, dir: &Path) -> std::process::Child {
    let ready = dir.join("holder-ready");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "holder_child", "--ignored", "--nocapture"])
        .env("AAVE_WRITER_HOLD_DB", db)
        .env("AAVE_WRITER_HOLD_READY", &ready)
        .env("AAVE_WRITER_HOLD_MARKET", MARKET_ID.to_string())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "the holder process never armed the market writer"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

#[test]
fn one_process_cannot_take_a_market_another_process_holds() {
    let dir = TempDir::new().unwrap();
    let db = database(dir.path());
    let mut holder = spawn_holder(&db, dir.path());

    let refused = AaveMarketWriter::acquire(&db, MARKET_ID);
    assert!(
        matches!(
            refused,
            Err(RunError::MarketHeld {
                market_id: MARKET_ID,
                ..
            })
        ),
        "a live holder refuses the second writer: {:?}",
        refused.err()
    );

    // SIGKILL: the kernel drops the flock with the process, the file survives,
    // and the next writer takes over without any cleanup step.
    holder.kill().unwrap();
    holder.wait().unwrap();
    let writer = AaveMarketWriter::acquire(&db, MARKET_ID).unwrap();
    assert!(
        writer.lock_path().exists(),
        "the lock file outlives the holder it was created for"
    );
}

#[test]
fn the_held_market_is_refused_by_name_and_path() {
    let dir = TempDir::new().unwrap();
    let db = database(dir.path());
    let holder = AaveMarketWriter::acquire(&db, MARKET_ID).unwrap();
    let lock_path = holder.lock_path();

    match AaveMarketWriter::acquire(&db, MARKET_ID).unwrap_err() {
        RunError::MarketHeld {
            market_id,
            lock_path: reported,
        } => {
            assert_eq!(market_id, MARKET_ID);
            assert_eq!(PathBuf::from(reported), lock_path);
        }
        other => panic!("expected MarketHeld, got {other:?}"),
    }
}

#[test]
fn a_lock_file_left_by_a_crashed_holder_is_reused_not_recreated() {
    let dir = TempDir::new().unwrap();
    let db = database(dir.path());
    let canonical = db.canonicalize().unwrap();
    let lock_path = market_lock_path(&canonical, MARKET_ID);
    fs::write(&lock_path, "999999999 dead holder\n").unwrap();
    let inode = fs::metadata(&lock_path).unwrap().ino();

    let writer = AaveMarketWriter::acquire(&db, MARKET_ID).unwrap();
    assert_eq!(
        fs::metadata(&lock_path).unwrap().ino(),
        inode,
        "a surviving file is the file the next writer locks"
    );
    assert_eq!(
        fs::read_to_string(&lock_path).unwrap().trim(),
        std::process::id().to_string(),
        "the takeover rewrites the holder record"
    );
    drop(writer);
}

#[test]
fn a_deleted_lock_file_is_recreated_without_creating_two_owners() {
    let dir = TempDir::new().unwrap();
    let db = database(dir.path());
    let first = AaveMarketWriter::acquire(&db, MARKET_ID).unwrap();
    let lock_path = first.lock_path();
    drop(first);

    // An operator (or a botched cleanup) removes the file between holders. The
    // next acquire re-creates it and is still the ONLY owner: exclusion is the
    // lock, never the file's existence.
    fs::remove_file(&lock_path).unwrap();
    let second = AaveMarketWriter::acquire(&db, MARKET_ID).unwrap();
    assert!(lock_path.exists(), "acquire creates the absent lock file");
    assert!(
        matches!(
            AaveMarketWriter::acquire(&db, MARKET_ID),
            Err(RunError::MarketHeld { .. })
        ),
        "the re-created file carries the same exclusion"
    );
    drop(second);
}

#[test]
fn a_handle_onto_another_database_is_refused_before_the_run() {
    let dir = TempDir::new().unwrap();
    let guarded = database(dir.path());
    let other_path = dir.path().join("other.db");
    degenbot_db::create_new_database(&other_path).unwrap();
    let writer = AaveMarketWriter::acquire(&guarded, MARKET_ID).unwrap();
    let (other, _state) = DegenbotDb::open_for_writes(&other_path).unwrap();

    let err = writer
        .update_through(&other, spec(MARKET_ID, 10))
        .unwrap_err();
    match err {
        RunError::MarketWriterHandleMismatch {
            writer_database,
            handle_database,
        } => {
            assert_eq!(
                PathBuf::from(writer_database),
                guarded.canonicalize().unwrap()
            );
            assert_eq!(
                PathBuf::from(handle_database),
                other_path.canonicalize().unwrap()
            );
        }
        other => panic!("expected MarketWriterHandleMismatch, got {other:?}"),
    }
}

#[test]
fn a_handle_onto_the_writers_own_database_passes_the_identity_check() {
    let dir = TempDir::new().unwrap();
    let real_dir = dir.path().join("real");
    fs::create_dir(&real_dir).unwrap();
    let db = database(&real_dir);
    let alias = dir.path().join("alias.db");
    std::os::unix::fs::symlink(&db, &alias).unwrap();

    // The writer is acquired through the alias, the handle is opened on the real
    // path: one identity, so the seam accepts it and the run proceeds (and fails
    // for its own reason — the empty corpus cannot answer the fetch).
    let writer = AaveMarketWriter::acquire(&alias, MARKET_ID).unwrap();
    let (handle, _state) = DegenbotDb::open_for_writes(&db).unwrap();
    let result = writer.update_through(&handle, spec(MARKET_ID, 10));
    assert!(
        !matches!(result, Err(RunError::MarketWriterHandleMismatch { .. })),
        "the alias and the real path are one database: {result:?}"
    );
}

/// The competing holder: re-executed as a child process by
/// `one_process_cannot_take_a_market_another_process_holds`, killed mid-hold.
#[test]
#[ignore = "spawned as the competing holder by one_process_cannot_take_a_market_another_process_holds"]
fn holder_child() {
    let db = PathBuf::from(std::env::var("AAVE_WRITER_HOLD_DB").unwrap());
    let ready = PathBuf::from(std::env::var("AAVE_WRITER_HOLD_READY").unwrap());
    let market_id = std::env::var("AAVE_WRITER_HOLD_MARKET")
        .unwrap()
        .parse::<i64>()
        .unwrap();
    let _writer = AaveMarketWriter::acquire(&db, market_id).unwrap();
    fs::write(&ready, b"armed").unwrap();
    // Hold until the parent kills us; the deadline only bounds a leak if the
    // parent dies first.
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}
