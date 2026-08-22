//! Diagnostic wrapper around the shared `parking_lot::RwLock` that guards
//! `BotState` (ergo Z4Z6VO, incident 2026-08-21).
//!
//! ## Why this exists
//!
//! The 2026-08-21 settlement-bot wedge was caused by ONE `RwLock<BotState>`
//! read guard that was never released (held by a suspended async task across
//! an `.await` — invisible in every thread dump). The engine solver blocked
//! in `write()` waiting for readers; registration-lifecycle reads and the
//! Python `build_v4_pool`/`register_token` FFI queued behind it; the pump
//! stalled and both Python streams went silent. Nothing in the process held
//! the GIL, so the gil-probe reported "busy, not a deadlock" for ~11 minutes.
//!
//! `parking_lot` does not track lock owners, and clippy's `await_holding_lock`
//! does not cover `lock_api` guards — this bug class is otherwise invisible.
//! This wrapper makes the holder visible:
//!
//! - every active READ hold is registered in a process-wide table (thread,
//!   `#[track_caller]` acquire site, monotonic acquire time, optional
//!   acquire-time backtrace when `DEGENBOT_LOCK_TRACE=1`);
//! - acquiring a read flags any hold older than `DEGENBOT_LOCK_WARN_MS`
//!   (default 500) exactly once, WARN-ing with the holder's site/thread;
//! - a `write()` that blocks longer than the threshold WARNs with the count
//!   and locations of the readers it is (or was) waiting for;
//! - `dump_active_holds()` renders the live table for forensics (later wired
//!   into the gil-probe registry dump).
//!
//! Overhead in the uncontended steady state: one mutex-guarded `HashMap`
//! insert + remove per read acquisition. If that ever shows up in pump-loop
//! profiles, gate the tracking behind a runtime flag and keep the wrapper.

use std::backtrace::Backtrace;
use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::ops::{Deref, DerefMut};
use std::panic::Location;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Instant;

use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Monotonic process-start instant for hold ages (immune to wall-clock jumps).
static START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Warn threshold ms for read-hold age AND write-acquisition block, resolved
/// once from `DEGENBOT_LOCK_WARN_MS` (default 500). Stored in an atomic so
/// operators/tests may adjust it at runtime (`set_warn_threshold_ms`).
static WARN_THRESHOLD_MS: AtomicU64 = AtomicU64::new(0);
static THRESHOLD_INIT: LazyLock<()> = LazyLock::new(|| {
    let raw = std::env::var("DEGENBOT_LOCK_WARN_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(500);
    WARN_THRESHOLD_MS.store(raw.max(1), Ordering::Relaxed);
});

/// Full backtraces captured at acquire when `DEGENBOT_LOCK_TRACE=1`.
static TRACE_BACKTRACES: LazyLock<bool> =
    LazyLock::new(|| std::env::var("DEGENBOT_LOCK_TRACE").is_ok_and(|v| v == "1"));

/// Unique id per registered hold — unambiguous removal on Drop.
static HOLD_SEQ: AtomicU64 = AtomicU64::new(1);

/// One active READ hold on some `StateLock`.
struct HoldRecord {
    seq: u64,
    thread: String,
    /// `#[track_caller]` acquire site, pre-formatted.
    location: &'static str,
    /// Monotonic ms since process start at acquisition.
    acquired_ms: u64,
    /// Slow-hold warning already emitted for this hold (warn once).
    warned: bool,
    /// Captured only when `DEGENBOT_LOCK_TRACE=1`.
    backtrace: Option<String>,
}

/// Lock-key -> active read holds. All access under one short-held mutex;
/// never acquire this while holding a `StateLock` guard.
static ACTIVE_READS: LazyLock<Mutex<HashMap<usize, Vec<HoldRecord>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A slow-hold finding produced by [`flag_aged_records`].
#[derive(Debug, PartialEq, Eq)]
pub struct SlowHold {
    pub seq: u64,
    pub thread: String,
    pub location: String,
    pub held_ms: u64,
    pub backtrace: Option<String>,
}

/// Warn threshold in ms (env `DEGENBOT_LOCK_WARN_MS`, default 500).
#[must_use]
pub fn warn_threshold_ms() -> u64 {
    LazyLock::force(&THRESHOLD_INIT);
    WARN_THRESHOLD_MS.load(Ordering::Relaxed)
}

/// Override the warn threshold at runtime (tests / operator tooling).
pub fn set_warn_threshold_ms(ms: u64) {
    LazyLock::force(&THRESHOLD_INIT);
    WARN_THRESHOLD_MS.store(ms.max(1), Ordering::Relaxed);
}

/// Test-only clock advance: the whole test binary can start AND finish a
/// test inside millisecond zero of [`START`], so tests shift the clock
/// forward instead of sleeping.
#[cfg(test)]
static CLOCK_OFFSET_MS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
fn advance_clock_ms(ms: u64) {
    CLOCK_OFFSET_MS.fetch_add(ms, Ordering::Relaxed);
}

fn now_ms() -> u64 {
    let base = u64::try_from(START.elapsed().as_millis()).unwrap_or(u64::MAX);
    #[cfg(test)]
    let base = base.saturating_add(CLOCK_OFFSET_MS.load(Ordering::Relaxed));
    base
}

fn format_location(loc: &'static Location<'_>) -> &'static str {
    Box::leak(Box::<str>::from(format!("{loc}")))
}

/// Pure verdict: which of `records` exceed `threshold_ms` of hold time at
/// `now_ms`, excluding ones already warned. Marks them warned.
fn flag_aged_records(records: &mut [HoldRecord], now_ms: u64, threshold_ms: u64) -> Vec<SlowHold> {
    let mut out = Vec::new();
    for rec in records.iter_mut() {
        let held = now_ms.saturating_sub(rec.acquired_ms);
        if held >= threshold_ms && !rec.warned {
            rec.warned = true;
            out.push(SlowHold {
                seq: rec.seq,
                thread: rec.thread.clone(),
                location: rec.location.to_owned(),
                held_ms: held,
                backtrace: rec.backtrace.clone(),
            });
        }
    }
    out
}

fn log_slow_holds(key: usize, holds: &[SlowHold]) {
    for hold in holds {
        let mut msg = format!(
            "[state-lock] read guard held {}ms by {} at {} (lock 0x{key:x})",
            hold.held_ms, hold.thread, hold.location,
        );
        if let Some(bt) = &hold.backtrace {
            let _ = write!(msg, "\n{bt}");
        }
        tracing::warn!("{msg}");
    }
}

fn register_read(key: usize, location: &'static str) -> u64 {
    let seq = HOLD_SEQ.fetch_add(1, Ordering::Relaxed);
    let trace = *TRACE_BACKTRACES;
    let mut map = ACTIVE_READS.lock();
    map.entry(key).or_default().push(HoldRecord {
        seq,
        thread: std::thread::current()
            .name()
            .map_or_else(|| "<unnamed>".to_owned(), str::to_owned),
        location,
        acquired_ms: now_ms(),
        warned: false,
        backtrace: trace.then(|| Backtrace::capture().to_string()),
    });
    seq
}

fn remove_read(key: usize, seq: u64) {
    let mut map = ACTIVE_READS.lock();
    if let Some(records) = map.get_mut(&key) {
        records.retain(|rec| rec.seq != seq);
        if records.is_empty() {
            map.remove(&key);
        }
    }
}

/// Snapshot the active read holds for `key`: (thread, location, `held_ms`).
fn snapshot_holds(key: usize) -> Vec<(String, String, u64)> {
    let now = now_ms();
    let map = ACTIVE_READS.lock();
    map.get(&key)
        .map(|records| {
            records
                .iter()
                .map(|rec| {
                    (
                        rec.thread.clone(),
                        rec.location.to_owned(),
                        now.saturating_sub(rec.acquired_ms),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Render every active read hold in the process (forensic dump).
#[must_use]
pub fn dump_active_holds() -> String {
    let now = now_ms();
    let map = ACTIVE_READS.lock();
    let mut out = String::new();
    let _ = writeln!(out, "state-lock active read holds:");
    if map.is_empty() {
        let _ = writeln!(out, "  (none)");
    }
    for (key, records) in map.iter() {
        let _ = writeln!(out, "  lock 0x{key:x}: {} active read(s)", records.len());
        for rec in records {
            let _ = writeln!(
                out,
                "    seq={} held={}ms thread={} at {}",
                rec.seq,
                now.saturating_sub(rec.acquired_ms),
                rec.thread,
                rec.location,
            );
            if let Some(bt) = &rec.backtrace {
                let _ = writeln!(out, "{bt}");
            }
        }
    }
    out
}

/// Diagnostic wrapper around [`parking_lot::RwLock`] tracking active read
/// holders. API-compatible for `.read()`/`.write()` call sites; guards deref
/// to `T`.
///
/// Writer waits and long-lived reads above
/// [`warn_threshold_ms`](self::warn_threshold_ms) emit `tracing::warn!`
/// diagnostics naming the acquire sites.
///
/// # NEVER NEST ACQUISITIONS ON THE SAME LOCK
///
/// A second `.read()` acquired while a first read guard is still alive
/// (e.g. inside a closure fed by the first guard's data) self-deadlocks
/// when a writer queues between the two acquisitions: read#2 parks behind
/// the writer, the writer waits on read#1, and read#1 lives until the
/// expression completes — which needs read#2. Both soak-2026-08-22
/// deadlocks (`a3ab1c676` V3, `decf7cd8a` V4) were exactly this shape.
/// Scope the first guard explicitly so it drops before the second
/// acquires; the compiler cannot catch this class.
pub struct StateLock<T> {
    inner: RwLock<T>,
}

impl<T> StateLock<T> {
    fn key_of(&self) -> usize {
        std::ptr::from_ref(self).addr()
    }

    /// Create a tracked lock.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self {
            inner: RwLock::new(value),
        }
    }

    /// Acquire a read guard, registering the hold.
    #[track_caller]
    pub fn read(&self) -> StateReadGuard<'_, T> {
        let location = format_location(Location::caller());
        let t0 = Instant::now();
        let guard = self.inner.read();
        let key = self.key_of();
        let seq = register_read(key, location);
        let waited = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
        if waited >= warn_threshold_ms() {
            let holders = snapshot_holds(key);
            tracing::warn!(
                "[state-lock] read acquisition blocked {waited}ms at {location} \
                 (lock 0x{key:x}); active reads at acquire: {holders:?}"
            );
        }
        // Flag previously-registered holds that have now aged past the
        // threshold (the phantom-reader signature from the incident).
        let threshold = warn_threshold_ms();
        let mut map = ACTIVE_READS.lock();
        let aged = map
            .get_mut(&key)
            .map(|records| flag_aged_records(records, now_ms(), threshold))
            .unwrap_or_default();
        drop(map);
        log_slow_holds(key, &aged);
        StateReadGuard {
            inner: guard,
            key,
            seq,
        }
    }

    /// Acquire a write guard. Long blocks WARN with the reader snapshot taken
    /// immediately after acquisition (the readers we were waiting for).
    #[track_caller]
    pub fn write(&self) -> StateWriteGuard<'_, T> {
        let location = format_location(Location::caller());
        let t0 = Instant::now();
        let guard = self.inner.write();
        let waited = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
        let key = self.key_of();
        if waited >= warn_threshold_ms() {
            let holders = snapshot_holds(key);
            tracing::warn!(
                "[state-lock] WRITE acquisition blocked {waited}ms at {location} \
                 (lock 0x{key:x}); readers still registered after acquire: {holders:?}"
            );
        }
        StateWriteGuard { inner: guard }
    }

    /// Try to acquire a write guard without blocking (`None` when contended).
    /// Mirrors `parking_lot::RwLock::try_write` for callers that only probe.
    #[track_caller]
    pub fn try_write(&self) -> Option<StateWriteGuard<'_, T>> {
        self.inner
            .try_write()
            .map(|guard| StateWriteGuard { inner: guard })
    }
}

impl<T> Default for StateLock<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> fmt::Debug for StateLock<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateLock")
            .field("key", &self.key_of())
            .finish_non_exhaustive()
    }
}

/// Read guard registered in the tracker; removes its record on Drop.
pub struct StateReadGuard<'a, T> {
    /// Declared first so it drops (releasing the `parking_lot` guard) before
    /// the bookkeeping fields — though the registry removal below happens in
    /// `Drop::drop`, i.e. while the underlying lock is still held, which keeps
    /// the tracker consistent with real hold windows.
    inner: RwLockReadGuard<'a, T>,
    key: usize,
    seq: u64,
}

impl<T> Deref for StateReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.inner.deref()
    }
}

impl<T> Drop for StateReadGuard<'_, T> {
    fn drop(&mut self) {
        remove_read(self.key, self.seq);
    }
}

/// Write guard passthrough (writers serialize among themselves; the
/// interesting diagnostic is the acquisition-time wait report).
pub struct StateWriteGuard<'a, T> {
    inner: RwLockWriteGuard<'a, T>,
}

impl<T> Deref for StateWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.inner.deref()
    }
}

impl<T> DerefMut for StateWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.inner.deref_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure verdict logic -------------------------------------------------

    #[test]
    fn aged_records_flagged_once_with_holder_info() {
        let mut records = vec![
            HoldRecord {
                seq: 1,
                thread: "t-old".into(),
                location: "src/x.rs:10",
                acquired_ms: 1_000,
                warned: false,
                backtrace: None,
            },
            HoldRecord {
                seq: 2,
                thread: "t-young".into(),
                location: "src/y.rs:20",
                acquired_ms: 5_900, // held 100ms at now=6_000: below threshold
                warned: false,
                backtrace: None,
            },
            HoldRecord {
                seq: 3,
                thread: "t-already-warned".into(),
                location: "src/z.rs:30",
                acquired_ms: 100,
                warned: true,
                backtrace: None,
            },
        ];
        let flagged = flag_aged_records(&mut records, 6_000, 500);
        assert_eq!(flagged.len(), 1, "only the aged, never-warned hold fires");
        assert_eq!(flagged[0].seq, 1);
        assert_eq!(flagged[0].held_ms, 5_000);
        assert_eq!(flagged[0].location, "src/x.rs:10");
        // Second pass must be silent (warn-once).
        assert!(flag_aged_records(&mut records, 6_000, 500).is_empty());
    }

    #[test]
    fn zero_threshold_flags_every_unwarned_hold() {
        let mut records = vec![HoldRecord {
            seq: 7,
            thread: "t".into(),
            location: "l",
            acquired_ms: 3_999,
            warned: false,
            backtrace: None,
        }];
        assert_eq!(flag_aged_records(&mut records, 4_000, 1).len(), 1);
    }

    // ---- registry lifecycle --------------------------------------------------

    #[test]
    #[expect(clippy::expect_used)]
    fn read_guard_registers_and_deregisters() {
        let lock: StateLock<u8> = StateLock::new(0);
        let key = lock.key_of();
        {
            let guard = lock.read();
            assert_eq!(*guard, 0);
            let map = ACTIVE_READS.lock();
            let records = map.get(&key).expect("hold registered");
            assert_eq!(records.len(), 1);
            assert!(
                records[0].location.contains("state_lock.rs"),
                "track_caller site recorded, got {}",
                records[0].location
            );
        }
        let map = ACTIVE_READS.lock();
        assert!(map.get(&key).is_none(), "drop removed the hold");
    }

    #[test]
    fn multiple_concurrent_reads_all_tracked_and_removed() {
        let lock: StateLock<u8> = StateLock::new(0);
        let key = lock.key_of();
        let g1 = lock.read();
        let g2 = lock.read();
        {
            let map = ACTIVE_READS.lock();
            assert_eq!(map.get(&key).map_or(0, Vec::len), 2);
        }
        drop(g1);
        {
            let map = ACTIVE_READS.lock();
            assert_eq!(map.get(&key).map_or(0, Vec::len), 1);
        }
        drop(g2);
        let map = ACTIVE_READS.lock();
        assert!(map.get(&key).is_none());
    }

    #[test]
    fn write_access_works_through_wrapper() {
        let lock: StateLock<String> = StateLock::new("a".into());
        {
            let mut guard = lock.write();
            guard.push('b');
        }
        assert_eq!(*lock.read(), "ab");
    }

    #[test]
    fn dump_lists_active_holds_and_clears() {
        let lock: StateLock<u8> = StateLock::new(0);
        let guard = lock.read();
        let dump = dump_active_holds();
        assert!(dump.contains("active read holds"));
        assert!(dump.contains("state_lock.rs"), "dump names the site");
        drop(guard);
        assert!(dump_active_holds().contains("(none)"));
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn slow_reader_is_flagged_on_next_acquisition() {
        set_warn_threshold_ms(1);
        let lock: StateLock<u8> = StateLock::new(0);
        let key = lock.key_of();
        let holder = lock.read();
        // Age the existing hold synthetically (no sleeping in tests).
        {
            let mut map = ACTIVE_READS.lock();
            let records = map.get_mut(&key).expect("holder registered");
            records[0].acquired_ms = now_ms().saturating_sub(10_000);
        }
        // Shift the clock forward instead of sleeping: the holder's acquire
        // timestamp stays where it was, so its hold age now exceeds the 1ms
        // threshold.
        advance_clock_ms(60_000);
        let second = lock.read(); // must flag-and-warn the aged first hold
        let map = ACTIVE_READS.lock();
        let records = map.get(&key).expect("both holds registered");
        assert_eq!(records.len(), 2);
        assert_eq!(records.iter().filter(|r| r.warned).count(), 1);
        drop(map);
        drop(second);
        drop(holder);
        set_warn_threshold_ms(500);
    }

    #[test]
    fn fast_write_quiet_path_returns_working_guard() {
        // An uncontended write waits < 1ms virtually always, so the quiet path
        // runs (log-content assertions belong to an integration harness).
        let lock: StateLock<u16> = StateLock::new(3);
        assert_eq!(*lock.write(), 3);
    }
}
