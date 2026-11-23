//! ADR-106: the pinned-snapshot atomic backup.
//!
//! [`backup_database`] copies the source at one committed snapshot, verifies the
//! copy, and publishes it with a `rename`, so the destination path only ever
//! holds a complete, checked file.
//!
//! # Why the source read transaction is the whole concurrency design
//!
//! SQLite restarts an online backup from page 1 whenever the source is modified
//! by a connection **other than** the one running the backup. A writing bot
//! supplies that modification continuously, so an unpinned stepped copy can
//! make no forward progress at all (measured: 1,203 restarts, never finishing).
//! A read transaction open on the backup's own source connection freezes the
//! `WAL` snapshot the copy reads: no restarts, one pass, and writers keep
//! committing throughout (readers never block `WAL` writers).
//!
//! The pin's cost is that the checkpointer cannot retire frames past our
//! snapshot, so the source `WAL` grows at the writer's rate for the duration of
//! the copy — hence the free-space preflight and the reported growth.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use crate::error::DbError;
use crate::ops::{file_ro_uri, remove_sidecar_if_present, sibling_with_suffix, AdvisoryLock};

/// Bounded wait for a source lock, matching the open path's `busy_timeout`.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Pages per backup step. Each step reports progress, so this bounds how stale
/// a progress line and how late an abort can be, not throughput.
const PAGES_PER_STEP: i32 = 4096;

/// Free space required beyond the copy itself, as a multiple of the source
/// size. The copy needs one source image; the pin makes the source `WAL` grow
/// at the writer's rate for as long as the copy runs, which scales with the
/// source. Requiring the same order of headroom turns a mid-copy
/// `disk is full` (which endangers the writing bot, not just the backup) into
/// an immediate refusal that creates nothing.
const HEADROOM_FACTOR: u64 = 2;

/// Progress of one copy step, in pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupProgress {
    /// Source pages not yet copied.
    pub remaining: u32,
    /// Source pages at the pinned snapshot.
    pub pagecount: u32,
}

/// What a completed [`backup_database`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupReport {
    /// Pages in the published backup.
    pub pages: u32,
    /// Bytes in the published backup.
    pub bytes: u64,
    /// Wall time of the whole operation, including verification.
    pub elapsed: Duration,
    /// Backup restarts. Zero by construction while the snapshot pin holds; a
    /// nonzero value means the copy was restarted and is the canary for a
    /// broken pin.
    pub restarts: u32,
    /// Bytes the source `WAL` grew by while the snapshot was pinned.
    pub wal_growth_bytes: u64,
}

/// Knobs the public entry point fixes and the tests vary.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BackupOptions {
    /// Free bytes on the destination filesystem, or `None` where the platform
    /// cannot report it (the preflight is then skipped).
    pub(crate) available_bytes: Option<u64>,
    /// Pages copied per step.
    pub(crate) pages_per_step: i32,
}

/// Back up `src` into `dst`: one point-in-time snapshot, verified, published
/// atomically.
///
/// `dst` is replaced only once the copy is complete and `PRAGMA integrity_check`
/// passes on it; any failure leaves the previous `dst` untouched. `progress`
/// receives every step and returns `false` to abort (the temp copy is removed,
/// `dst` untouched, [`DbError::BackupAborted`] returned).
///
/// # Errors
///
/// [`DbError::InsufficientDisk`] when the preflight refuses (nothing is
/// created); [`DbError::BackupAborted`] on a caller abort;
/// [`DbError::SourceUnopenable`] when the source cannot be opened read-only;
/// [`DbError::IntegrityCheckFailed`] when the copy fails verification;
/// [`DbError::Sqlite`] / [`DbError::Io`] on a copy or filesystem failure.
pub fn backup_database(
    src: &Path,
    dst: &Path,
    progress: &mut dyn FnMut(BackupProgress) -> bool,
) -> Result<BackupReport, DbError> {
    let available = dst
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map_or_else(|| available_bytes(Path::new(".")), available_bytes);
    backup_with(
        src,
        dst,
        BackupOptions {
            available_bytes: available,
            pages_per_step: PAGES_PER_STEP,
        },
        progress,
    )
}

pub(crate) fn backup_with(
    src: &Path,
    dst: &Path,
    options: BackupOptions,
    progress: &mut dyn FnMut(BackupProgress) -> bool,
) -> Result<BackupReport, DbError> {
    if let Some(available) = options.available_bytes {
        let source_bytes = std::fs::metadata(src).map_err(DbError::Io)?.len();
        let required = source_bytes
            .saturating_mul(HEADROOM_FACTOR)
            .saturating_add(sidecar_len(src, "-wal"));
        if available < required {
            return Err(DbError::InsufficientDisk {
                available_bytes: available,
                required_bytes: required,
            });
        }
    }

    let _lock = AdvisoryLock::acquire(&sibling_with_suffix(src, ".backup.lock"))?;
    let started = Instant::now();
    let tmp = TempCopy::new(dst);

    let snapshot = PinnedSnapshot::open(src)?;
    let wal_before = sidecar_len(src, "-wal");
    let copy = copy_snapshot(&snapshot, &tmp.path, options.pages_per_step, progress);
    let wal_after = sidecar_len(src, "-wal");
    drop(snapshot);
    let copied = copy?;

    verify_integrity(&tmp.path)?;
    remove_sidecar_if_present(&tmp.path, "-wal");
    remove_sidecar_if_present(&tmp.path, "-shm");
    let bytes = std::fs::metadata(&tmp.path).map_err(DbError::Io)?.len();
    std::fs::File::open(&tmp.path)
        .map_err(DbError::Io)?
        .sync_all()
        .map_err(DbError::Io)?;

    remove_sidecar_if_present(dst, "-wal");
    remove_sidecar_if_present(dst, "-shm");
    tmp.publish(dst)?;

    Ok(BackupReport {
        pages: copied.pagecount,
        bytes,
        elapsed: started.elapsed(),
        restarts: copied.restarts,
        wal_growth_bytes: wal_after.saturating_sub(wal_before),
    })
}

/// The source connection that owns the snapshot pin. The pin only holds while
/// every backup step runs on **this** connection, so the wrapper hands out
/// nothing but a shared reference to it.
struct PinnedSnapshot {
    conn: Connection,
}

impl PinnedSnapshot {
    fn open(src: &Path) -> Result<Self, DbError> {
        let conn = Connection::open_with_flags(
            file_ro_uri(src),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|error| DbError::SourceUnopenable {
            path: src.display().to_string(),
            reason: error.to_string(),
        })?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;")?;
        Ok(Self { conn })
    }

    fn connection(&self) -> &Connection {
        &self.conn
    }
}

struct CopyOutcome {
    pagecount: u32,
    restarts: u32,
}

fn copy_snapshot(
    snapshot: &PinnedSnapshot,
    tmp: &Path,
    pages_per_step: i32,
    progress: &mut dyn FnMut(BackupProgress) -> bool,
) -> Result<CopyOutcome, DbError> {
    let mut dst_conn = Connection::open_with_flags(
        tmp,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let backup = Backup::new(snapshot.connection(), &mut dst_conn)?;

    let mut previous: Option<u32> = None;
    let mut restarts: u32 = 0;
    loop {
        let step = backup.step(pages_per_step)?;
        if matches!(step, StepResult::Done) {
            break;
        }
        if matches!(step, StepResult::Busy | StepResult::Locked) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let step_progress = backup.progress();
        let remaining = clamp_page_count(step_progress.remaining);
        if previous.is_some_and(|last| remaining > last) {
            restarts = restarts.saturating_add(1);
        }
        previous = Some(remaining);
        if !progress(BackupProgress {
            remaining,
            pagecount: clamp_page_count(step_progress.pagecount),
        }) {
            return Err(DbError::BackupAborted);
        }
    }

    Ok(CopyOutcome {
        pagecount: clamp_page_count(backup.progress().pagecount),
        restarts,
    })
}

/// The copy target: a sibling temp file that removes itself unless published.
struct TempCopy {
    path: PathBuf,
    armed: bool,
}

impl TempCopy {
    fn new(dst: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let token = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = sibling_with_suffix(dst, &format!(".tmp-{}-{token}", std::process::id()));
        remove_sidecar_if_present(&path, "-wal");
        remove_sidecar_if_present(&path, "-shm");
        let _ = std::fs::remove_file(&path);
        Self { path, armed: true }
    }

    fn publish(mut self, dst: &Path) -> Result<(), DbError> {
        std::fs::rename(&self.path, dst)?;
        self.armed = false;
        sync_dir(dst)
    }
}

impl Drop for TempCopy {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = std::fs::remove_file(&self.path);
        remove_sidecar_if_present(&self.path, "-wal");
        remove_sidecar_if_present(&self.path, "-shm");
    }
}

fn verify_integrity(path: &Path) -> Result<(), DbError> {
    let conn = Connection::open_with_flags(
        file_ro_uri(path),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let result: String = conn.query_row("PRAGMA integrity_check;", [], |row| row.get(0))?;
    if result == "ok" {
        Ok(())
    } else {
        Err(DbError::IntegrityCheckFailed(result))
    }
}

fn clamp_page_count(pages: i32) -> u32 {
    u32::try_from(pages).unwrap_or(0)
}

fn sidecar_len(path: &Path, suffix: &str) -> u64 {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(suffix);
    std::fs::metadata(Path::new(&sidecar)).map_or(0, |meta| meta.len())
}

#[cfg(unix)]
fn available_bytes(dir: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut vfs = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `statvfs` fills the struct it is given; the value is read only
    // when the call reports success.
    let rc = unsafe { libc::statvfs(path.as_ptr(), vfs.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: a zero return means `statvfs` initialized the whole struct.
    let vfs = unsafe { vfs.assume_init() };
    let free_bytes = u128::from(vfs.f_bavail) * u128::from(vfs.f_frsize);
    u64::try_from(free_bytes).ok()
}

#[cfg(not(unix))]
fn available_bytes(_dir: &Path) -> Option<u64> {
    None
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<(), DbError> {
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::File::open(dir)
        .map_err(DbError::Io)?
        .sync_all()
        .map_err(DbError::Io)
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<(), DbError> {
    Ok(())
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::migrate::SchemaState;
    use crate::ops::{create_new_database, inspect_schema_state};
    use rusqlite::Connection as RawConnection;
    use std::sync::atomic::AtomicBool;

    const BASE_ROWS: i64 = 40;

    fn options(pages_per_step: i32) -> BackupOptions {
        BackupOptions {
            available_bytes: None,
            pages_per_step,
        }
    }

    fn temp_leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp-"))
            .collect()
    }

    fn seeded_source(dir: &Path) -> PathBuf {
        let src = dir.join("src.db");
        create_new_database(&src).unwrap();
        let conn = RawConnection::open(&src).unwrap();
        conn.execute_batch(
            "CREATE TABLE blog(x BLOB);\n\
             CREATE TABLE counter(v INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute("INSERT INTO counter(v) VALUES (0)", [])
            .unwrap();
        for _ in 0..BASE_ROWS {
            conn.execute("INSERT INTO blog(x) VALUES (randomblob(4000))", [])
                .unwrap();
        }
        drop(conn);
        src
    }

    #[test]
    fn backup_of_an_idle_db_is_byte_stable_and_rust_owned() {
        let dir = tempfile::tempdir().unwrap();
        let src = seeded_source(dir.path());

        let first = dir.path().join("first.db.bak");
        let second = dir.path().join("second.db.bak");
        let report = backup_database(&src, &first, &mut |_| true).unwrap();
        backup_database(&src, &second, &mut |_| true).unwrap();

        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap(),
            "a second backup of an idle db reproduces identical bytes"
        );
        assert_eq!(report.bytes, std::fs::metadata(&first).unwrap().len());
        assert!(report.pages > 0);
        assert_eq!(report.restarts, 0);
        assert!(matches!(
            inspect_schema_state(&first).unwrap(),
            SchemaState::RustOwned { .. }
        ));
    }

    #[test]
    fn backup_under_an_active_writer_is_one_snapshot_and_never_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let src = seeded_source(dir.path());

        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let writer_stop = std::sync::Arc::clone(&stop);
        let writer_path = src.clone();
        let writer = std::thread::spawn(move || {
            let conn = RawConnection::open(&writer_path).unwrap();
            conn.busy_timeout(BUSY_TIMEOUT).unwrap();
            while !writer_stop.load(Ordering::SeqCst) {
                conn.execute_batch("BEGIN IMMEDIATE").unwrap();
                conn.execute("INSERT INTO blog(x) VALUES (randomblob(4000))", [])
                    .unwrap();
                conn.execute("UPDATE counter SET v = v + 1", []).unwrap();
                conn.execute_batch("COMMIT").unwrap();
            }
        });

        let dst = dir.path().join("src.db.bak");
        let report = backup_with(&src, &dst, options(1), &mut |_| true).unwrap();
        stop.store(true, Ordering::SeqCst);
        writer.join().unwrap();

        assert_eq!(
            report.restarts, 0,
            "the pinned snapshot must never restart the copy"
        );

        let check = RawConnection::open(&dst).unwrap();
        let counter: i64 = check
            .query_row("SELECT v FROM counter", [], |row| row.get(0))
            .unwrap();
        let rows: i64 = check
            .query_row("SELECT count(*) FROM blog", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            rows,
            BASE_ROWS + counter,
            "the backup mixed two write phases"
        );
    }

    #[test]
    fn an_interrupted_backup_keeps_the_previous_artifact_and_leaves_no_partial() {
        let dir = tempfile::tempdir().unwrap();
        let src = seeded_source(dir.path());
        let dst = dir.path().join("src.db.bak");
        std::fs::write(&dst, b"previous backup").unwrap();

        let err = backup_with(&src, &dst, options(1), &mut |_| false).unwrap_err();

        assert!(matches!(err, DbError::BackupAborted));
        assert_eq!(std::fs::read(&dst).unwrap(), b"previous backup");
        assert!(temp_leftovers(dir.path()).is_empty());
    }

    #[test]
    fn concurrent_backups_of_one_destination_serialize() {
        let dir = tempfile::tempdir().unwrap();
        let src = seeded_source(dir.path());
        let dst = dir.path().join("src.db.bak");

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let src = src.clone();
                let dst = dst.clone();
                std::thread::spawn(move || backup_database(&src, &dst, &mut |_| true))
            })
            .collect();

        for handle in handles {
            assert!(
                handle.join().unwrap().is_ok(),
                "a queued backup must succeed"
            );
        }
        assert!(matches!(
            inspect_schema_state(&dst).unwrap(),
            SchemaState::RustOwned { .. }
        ));
        assert!(temp_leftovers(dir.path()).is_empty());
    }

    #[test]
    fn the_disk_preflight_refuses_before_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let src = seeded_source(dir.path());
        let dst = dir.path().join("src.db.bak");

        let err = backup_with(
            &src,
            &dst,
            BackupOptions {
                available_bytes: Some(0),
                pages_per_step: PAGES_PER_STEP,
            },
            &mut |_| true,
        )
        .unwrap_err();

        assert!(matches!(
            err,
            DbError::InsufficientDisk {
                available_bytes: 0,
                ..
            }
        ));
        assert!(!dst.exists());
        assert!(temp_leftovers(dir.path()).is_empty());
    }

    #[test]
    fn a_corrupt_source_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("garbage.db");
        std::fs::write(&src, b"not a database").unwrap();
        let dst = dir.path().join("garbage.db.bak");

        let err = backup_database(&src, &dst, &mut |_| true).unwrap_err();

        assert!(matches!(
            err,
            DbError::Sqlite(_)
                | DbError::IntegrityCheckFailed(_)
                | DbError::SourceUnopenable { .. }
        ));
        assert!(!dst.exists());
        assert!(temp_leftovers(dir.path()).is_empty());
    }
}
