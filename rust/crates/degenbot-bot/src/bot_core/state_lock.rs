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
//! Overhead in the uncontended steady state: the tracking costs a
//! mutex-guarded `HashMap` insert + remove per read acquisition (historically
//! plus a per-acquire string leak), so tracking is OFF by default —
//! `DEGENBOT_STATE_LOCK_DIAG=1` enables it for soak/incident forensics. The
//! default read path is the bare `parking_lot` read plus the cheap blocked-wait
//! warning (no registry traffic, no allocation).

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

/// 1 when hold-tracking diagnostics are enabled (`DEGENBOT_STATE_LOCK_DIAG=1`),
/// resolved once at first consultation. Default 0: the pump-path read
/// acquisition stays a bare `parking_lot` read (the rare blocked-wait warning
/// needs no per-hold bookkeeping). Set to 1 for soak/incident forensics.
/// Follows the `WARN_THRESHOLD_MS` resolution pattern so tests can force a mode.
static DIAG: AtomicU64 = AtomicU64::new(0);
static DIAG_INIT: LazyLock<()> = LazyLock::new(|| {
    let enabled = std::env::var("DEGENBOT_STATE_LOCK_DIAG").is_ok_and(|v| v == "1");
    DIAG.store(u64::from(enabled), Ordering::Relaxed);
});

/// True when read-hold tracking is enabled (env `DEGENBOT_STATE_LOCK_DIAG=1`).
fn diag_enabled() -> bool {
    LazyLock::force(&DIAG_INIT);
    DIAG.load(Ordering::Relaxed) == 1
}

/// Test-only override of the diag mode (stateful registry tests force a
/// deterministic mode per test).
#[cfg(test)]
fn set_diag_enabled_for_tests(enabled: bool) {
    LazyLock::force(&DIAG_INIT);
    DIAG.store(u64::from(enabled), Ordering::Relaxed);
}

/// Unique id per registered hold — unambiguous removal on Drop.
static HOLD_SEQ: AtomicU64 = AtomicU64::new(1);

/// One active READ hold on some `StateLock`.
struct HoldRecord {
    seq: u64,
    thread: String,
    /// `#[track_caller]` acquire site — stored unallocated; formatted only
    /// when a slow-hold warning fires.
    location: &'static Location<'static>,
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

/// Lock-key -> active WRITE hold. Writers serialize, so at most one record
/// per key; used to name a slow WRITE holder on drop (XC7SWD).
static ACTIVE_WRITES: LazyLock<Mutex<HashMap<usize, HoldRecord>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A slow-hold finding produced by [`flag_aged_records`].
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Epic K4ETHF T2: classify an acquire site into a small closed set for the
/// `degenbot.state_lock` wait/hold histograms. Pure on the file string so
/// unit tests can pin the taxonomy; the line is logged raw by the warn
/// paths above (location strings are unstable - never a metric label).
pub(crate) fn site_class_for(file: &str) -> &'static str {
    if file.contains("bot/mod.rs") {
        "python"
    } else if file.contains("block_pump.rs") {
        "pump"
    } else if file.contains("registration_lifecycle.rs") || file.contains("cl_orchestration.rs") {
        "reg"
    } else if file.contains("solver_dispatch.rs") || file.contains("engine_handle.rs") {
        // BEFORE the dispatch.rs branch: solver_dispatch.rs contains it.
        "solver"
    } else if file.contains("dispatch.rs") {
        "sim"
    } else if file.contains("state_lock.rs") {
        "core"
    } else {
        "other"
    }
}

fn site_class(loc: &Location) -> &'static str {
    site_class_for(loc.file())
}

/// Emit the wait observation if the metrics pipeline is up (K4ETHF T2;
/// seconds - the instruments' unit). Cheap one-Option-branch per acquire.
fn record_wait(site: &'static str, mode: &'static str, t0: Instant) {
    if let Some(p) = crate::instruments::pipeline() {
        let secs = t0.elapsed().as_secs_f64();
        p.observe_state_lock_wait(site, mode, secs);
    }
}

/// Emit the hold observation at guard drop (see `record_wait`).
fn record_hold(site: &'static str, mode: &'static str, t0: Instant) {
    if let Some(p) = crate::instruments::pipeline() {
        let secs = t0.elapsed().as_secs_f64();
        p.observe_state_lock_hold(site, mode, secs);
    }
}

fn now_ms() -> u64 {
    let base = u64::try_from(START.elapsed().as_millis()).unwrap_or(u64::MAX);
    #[cfg(test)]
    let base = base.saturating_add(CLOCK_OFFSET_MS.load(Ordering::Relaxed));
    base
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
                location: rec.location.to_string(),
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

fn register_read(key: usize, location: &'static Location<'static>) -> u64 {
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

fn register_write(key: usize, location: &'static Location<'static>) -> u64 {
    let seq = HOLD_SEQ.fetch_add(1, Ordering::Relaxed);
    let trace = *TRACE_BACKTRACES;
    let mut map = ACTIVE_WRITES.lock();
    map.insert(
        key,
        HoldRecord {
            seq,
            thread: std::thread::current()
                .name()
                .map_or_else(|| "<unnamed>".to_owned(), str::to_owned),
            location,
            acquired_ms: now_ms(),
            warned: false,
            backtrace: trace.then(|| Backtrace::capture().to_string()),
        },
    );
    seq
}

/// Remove a read hold's registry entry, returning the removed record so the
/// guard's `Drop` can report a slow hold that no later acquire flagged
/// (epic K4ETHF T1: holders that release before the waiter stampede were
/// invisible — the aged-check only runs on subsequent acquisitions).
fn remove_read(key: usize, seq: u64) -> Option<HoldRecord> {
    let mut map = ACTIVE_READS.lock();
    let records = map.get_mut(&key)?;
    let idx = records.iter().position(|rec| rec.seq == seq)?;
    let removed = records.remove(idx);
    if records.is_empty() {
        map.remove(&key);
    }
    Some(removed)
}

/// Released read holds that exceeded the threshold (ring, oldest evicted).
/// Complements [`flag_aged_records`]: a hold released before ANY later
/// acquire (the waiter-stampede shape) is only visible here.
static SLOW_READ_DROPS: LazyLock<Mutex<Vec<SlowHold>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Ring capacity for released slow-hold reports.
const SLOW_READ_DROPS_CAP: usize = 128;

/// Record a released read hold that exceeded the threshold: ring entry +
/// warn (mirrors [`log_slow_holds`]'s message shape).
fn record_slow_read_drop(hold: SlowHold) {
    let mut msg = format!(
        "[state-lock] read guard (drop-report) held {}ms by {} at {}",
        hold.held_ms, hold.thread, hold.location,
    );
    if let Some(bt) = &hold.backtrace {
        let _ = write!(msg, "\n{bt}");
    }
    tracing::warn!("{msg}");
    let mut ring = SLOW_READ_DROPS.lock();
    while ring.len() >= SLOW_READ_DROPS_CAP {
        ring.remove(0);
    }
    ring.push(hold);
}

/// Recent released slow read holds (forensic surface: query + dump).
#[must_use]
pub fn recent_slow_read_drops() -> Vec<SlowHold> {
    SLOW_READ_DROPS.lock().clone()
}

#[cfg(test)]
fn clear_recent_slow_read_drops() {
    SLOW_READ_DROPS.lock().clear();
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
                        rec.location.to_string(),
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
    if !diag_enabled() {
        return "(state-lock diagnostics disabled; set DEGENBOT_STATE_LOCK_DIAG=1 to track read holds)".to_owned();
    }
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
    // Released slow holds (K4ETHF T1): a hold that dropped before any later
    // acquire fired the aged-check is invisible in the active table — the
    // drop-time ring is the only record of it.
    let drops = SLOW_READ_DROPS.lock();
    if !drops.is_empty() {
        let _ = writeln!(
            out,
            "  recent slow read drops (released before a later acquire could flag them):"
        );
        for hold in drops.iter().rev().take(16) {
            let _ = writeln!(
                out,
                "    seq={} held={}ms thread={} at {}",
                hold.seq, hold.held_ms, hold.thread, hold.location,
            );
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

    /// Acquire a read guard.
    ///
    /// With diagnostics gated OFF (default) this registers nothing: just the
    /// `parking_lot` read + the cheap blocked-wait warning. With diagnostics ON
    /// (`DEGENBOT_STATE_LOCK_DIAG=1`) the hold is registered for slow-hold
    /// forensics.
    #[track_caller]
    pub fn read(&self) -> StateReadGuard<'_, T> {
        let t0 = Instant::now();
        let guard = self.inner.read();
        let site = site_class(Location::caller());
        record_wait(site, "read", t0);
        if !diag_enabled() {
            // Gated-off steady state: no registry traffic, no allocation.
            // Keep the blocked-wait warning (rare; no per-hold bookkeeping
            // required for it).
            let waited = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
            if waited >= warn_threshold_ms() {
                tracing::warn!(
                    "[state-lock] read acquisition blocked {waited}ms at {} \
                     (hold tracking disabled - set DEGENBOT_STATE_LOCK_DIAG=1 to name holders)",
                    Location::caller()
                );
            }
            return StateReadGuard {
                inner: guard,
                key: 0, // sentinel: Drop skips removal when seq == 0
                seq: 0,
                site,
                acquired: t0,
            };
        }
        let location = Location::caller();
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
            site,
            acquired: t0,
        }
    }

    /// Acquire a write guard. Long blocks WARN with the reader snapshot taken
    /// immediately after acquisition (the readers we were waiting for); long
    /// HOLDS warn on drop naming the hold site (XC7SWD).
    #[track_caller]
    pub fn write(&self) -> StateWriteGuard<'_, T> {
        let location = Location::caller();
        let t0 = Instant::now();
        let guard = self.inner.write();
        let waited = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
        let key = self.key_of();
        let site = site_class(location);
        record_wait(site, "write", t0);
        if waited >= warn_threshold_ms() {
            let holders = snapshot_holds(key);
            tracing::warn!(
                "[state-lock] WRITE acquisition blocked {waited}ms at {location} \
                 (lock 0x{key:x}); readers still registered after acquire: {holders:?}"
            );
        }
        let mut seq = 0;
        if diag_enabled() {
            seq = register_write(key, location);
        }
        StateWriteGuard {
            inner: guard,
            key,
            seq,
            site,
            acquired: t0,
        }
    }

    /// Try to acquire a write guard without blocking (`None` when contended).
    /// Mirrors `parking_lot::RwLock::try_write` for callers that only probe.
    #[track_caller]
    pub fn try_write(&self) -> Option<StateWriteGuard<'_, T>> {
        self.inner.try_write().map(|guard| {
            let key = self.key_of();
            let mut seq = 0;
            if diag_enabled() {
                seq = register_write(key, Location::caller());
            }
            StateWriteGuard {
                inner: guard,
                key,
                seq,
                site: site_class(Location::caller()),
                acquired: Instant::now(),
            }
        })
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
    /// K4ETHF T2: closed-set acquire-site class + acquire instant for the
    /// hold histogram (recorded at drop regardless of the diag gate).
    site: &'static str,
    acquired: Instant,
}

impl<T> Deref for StateReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.inner.deref()
    }
}

impl<T> Drop for StateReadGuard<'_, T> {
    fn drop(&mut self) {
        // Hold telemetry rides the guard in both diag modes (K4ETHF T2).
        record_hold(self.site, "read", self.acquired);
        // seq == 0 is the gated-off sentinel (never registered; no removal).
        if self.seq != 0 {
            let removed = remove_read(self.key, self.seq);
            // Drop-time slow-hold report (epic K4ETHF T1): complements the
            // aged-check. The observed stall shape is holder-releases-first,
            // waiter-stampede-second — the aged-check (which only runs on a
            // later acquire) never saw the long holder. Warn-once: a hold the
            // aged-check already flagged is not re-reported here.
            if let Some(rec) = removed {
                let held = now_ms().saturating_sub(rec.acquired_ms);
                if !rec.warned && held >= warn_threshold_ms() {
                    record_slow_read_drop(SlowHold {
                        seq: rec.seq,
                        thread: rec.thread,
                        location: rec.location.to_string(),
                        held_ms: held,
                        backtrace: rec.backtrace,
                    });
                }
            }
        }
    }
}

/// Write guard: with diagnostics ON, holds are registered so a slow WRITE
/// hold is named at guard drop (XC7SWD: a long WRITE hold was invisible —
/// only waits and read holds had forensics).
pub struct StateWriteGuard<'a, T> {
    inner: RwLockWriteGuard<'a, T>,
    key: usize,
    seq: u64,
    /// K4ETHF T2: closed-set acquire-site class + acquire instant for the
    /// hold histogram (recorded at drop).
    site: &'static str,
    acquired: Instant,
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

impl<T> Drop for StateWriteGuard<'_, T> {
    fn drop(&mut self) {
        // Hold telemetry rides the guard in both diag modes (K4ETHF T2).
        record_hold(self.site, "write", self.acquired);
        if self.seq == 0 {
            return; // gated-off sentinel: never registered
        }
        let warned = {
            let mut map = ACTIVE_WRITES.lock();
            map.remove(&self.key).and_then(|record| {
                let held = now_ms().saturating_sub(record.acquired_ms);
                if write_hold_slow(held) {
                    Some((record.location.to_string(), held))
                } else {
                    None
                }
            })
        };
        if let Some((loc, held)) = warned {
            tracing::warn!(
                "[state-lock] WRITE guard held {held}ms at {loc} (lock 0x{:x})",
                self.key
            );
        }
    }
}

/// Pure verdict for a completed WRITE hold: slow when diagnostics are on
/// and the hold reached the warn threshold.
fn write_hold_slow(held_ms: u64) -> bool {
    diag_enabled() && held_ms >= warn_threshold_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that flip the process-wide diag flag / assert the
    /// `ACTIVE_READS` table (parallel test threads would otherwise race it).
    static DIAG_TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn test_serial() -> std::sync::MutexGuard<'static, ()> {
        DIAG_TEST_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // ---- pure verdict logic -------------------------------------------------

    #[test]
    fn aged_records_flagged_once_with_holder_info() {
        let loc1 = Location::caller();
        let loc2 = Location::caller();
        let mut records = vec![
            HoldRecord {
                seq: 1,
                thread: "t-old".into(),
                location: loc1,
                acquired_ms: 1_000,
                warned: false,
                backtrace: None,
            },
            HoldRecord {
                seq: 2,
                thread: "t-young".into(),
                location: loc2,
                acquired_ms: 5_900, // held 100ms at now=6_000: below threshold
                warned: false,
                backtrace: None,
            },
            HoldRecord {
                seq: 3,
                thread: "t-already-warned".into(),
                location: loc2, // any site: value irrelevant to the verdict
                acquired_ms: 100,
                warned: true,
                backtrace: None,
            },
        ];
        let flagged = flag_aged_records(&mut records, 6_000, 500);
        assert_eq!(flagged.len(), 1, "only the aged, never-warned hold fires");
        assert_eq!(flagged[0].seq, 1);
        assert_eq!(flagged[0].held_ms, 5_000);
        assert_eq!(flagged[0].location, loc1.to_string());
        // Second pass must be silent (warn-once).
        assert!(flag_aged_records(&mut records, 6_000, 500).is_empty());
    }

    // ---- K4ETHF T2 telemetry taxonomy --------------------------------------

    #[test]
    fn site_class_taxonomy_is_pinned() {
        assert_eq!(
            site_class_for("x/src/degenbot-python/src/bot/mod.rs"),
            "python"
        );
        assert_eq!(site_class_for("x/bot_core/block_pump.rs"), "pump");
        assert_eq!(
            site_class_for("x/bot_core/registration_lifecycle.rs"),
            "reg"
        );
        assert_eq!(site_class_for("x/bot_core/cl_orchestration.rs"), "reg");
        assert_eq!(
            site_class_for("x/degenbot-arbitrage/src/dispatch.rs"),
            "sim"
        );
        assert_eq!(site_class_for("x/arb_engine/solver_dispatch.rs"), "solver");
        assert_eq!(site_class_for("x/arb_engine/engine_handle.rs"), "solver");
        assert_eq!(site_class_for("x/anything/else.rs"), "other");
    }

    #[test]
    fn zero_threshold_flags_every_unwarned_hold() {
        let mut records = vec![HoldRecord {
            seq: 7,
            thread: "t".into(),
            location: Location::caller(),
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
        let _serial = test_serial();
        set_diag_enabled_for_tests(true);
        let lock: StateLock<u8> = StateLock::new(0);
        let key = lock.key_of();
        {
            let guard = lock.read();
            assert_eq!(*guard, 0);
            let map = ACTIVE_READS.lock();
            let records = map.get(&key).expect("hold registered");
            assert_eq!(records.len(), 1);
            assert!(
                records[0].location.to_string().contains("state_lock.rs"),
                "track_caller site recorded, got {}",
                records[0].location
            );
        }
        let map = ACTIVE_READS.lock();
        assert!(map.get(&key).is_none(), "drop removed the hold");
        set_diag_enabled_for_tests(false);
    }

    // ---- write-hold forensics (XC7SWD) ----------------------------------

    #[test]
    #[expect(clippy::expect_used)]
    fn write_guard_registers_and_deregisters() {
        let _serial = test_serial();
        set_diag_enabled_for_tests(true);
        let lock: StateLock<u8> = StateLock::new(0);
        let key = lock.key_of();
        {
            let guard = lock.write();
            assert_eq!(*guard, 0);
            let map = ACTIVE_WRITES.lock();
            let record = map.get(&key).expect("write hold registered");
            assert!(
                record.location.to_string().contains("state_lock.rs"),
                "track_caller site recorded, got {}",
                record.location
            );
        }
        let map = ACTIVE_WRITES.lock();
        assert!(
            map.get(&key).is_none(),
            "drop removed the write hold record"
        );
        set_diag_enabled_for_tests(false);
    }

    #[test]
    fn write_hold_drop_reports_slow_hold() {
        let _serial = test_serial();
        set_diag_enabled_for_tests(true);
        advance_clock_ms(0);
        let lock: StateLock<u8> = StateLock::new(0);
        let key = lock.key_of();
        {
            let guard = lock.write();
            let _ = *guard;
            advance_clock_ms(600);
            // Drop happens here with 600ms held - over the 500ms default.
        }
        let map = ACTIVE_WRITES.lock();
        assert!(map.get(&key).is_none(), "write hold removed on drop");
        set_diag_enabled_for_tests(false);
    }

    #[test]
    fn write_hold_slow_verdict_is_diag_and_threshold_gated() {
        set_warn_threshold_ms(500);
        set_diag_enabled_for_tests(false);
        assert!(!write_hold_slow(600_000));
        set_diag_enabled_for_tests(true);
        assert!(write_hold_slow(600_000));
        assert!(!write_hold_slow(100));
        set_diag_enabled_for_tests(false);
    }

    #[test]
    fn multiple_concurrent_reads_all_tracked_and_removed() {
        let _serial = test_serial();
        set_diag_enabled_for_tests(true);
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
        set_diag_enabled_for_tests(false);
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
        let _serial = test_serial();
        set_diag_enabled_for_tests(true);
        let lock: StateLock<u8> = StateLock::new(0);
        let guard = lock.read();
        let dump = dump_active_holds();
        assert!(dump.contains("active read holds"));
        assert!(dump.contains("state_lock.rs"), "dump names the site");
        drop(guard);
        assert!(dump_active_holds().contains("(none)"));
        set_diag_enabled_for_tests(false);
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn slow_reader_is_flagged_on_next_acquisition() {
        let _serial = test_serial();
        set_diag_enabled_for_tests(true);
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
        set_diag_enabled_for_tests(false);
    }

    // ---- read-hold drop-time forensics (epic K4ETHF T1: the ~3.1s stall) ----
    //
    // The soak-observed hole: a read guard that exceeds the threshold and
    // releases BEFORE any later acquire fires the aged-check leaves the
    // registry unreported (the holder drops, THEN the waiter stampede
    // acquires — flag_aged_records never sees it). The drop must report it.

    #[test]
    #[expect(clippy::expect_used)]
    fn slow_read_hold_reports_at_drop() {
        let _serial = test_serial();
        set_diag_enabled_for_tests(true);
        set_warn_threshold_ms(1);
        clear_recent_slow_read_drops();
        let lock: StateLock<u8> = StateLock::new(0);
        let key = lock.key_of();
        let holder = lock.read();
        // Age the hold synthetically (no sleeping in tests). Matches the
        // observed stall shape: holder releases first, waiters arrive after.
        {
            let mut map = ACTIVE_READS.lock();
            let records = map.get_mut(&key).expect("holder registered");
            records[0].acquired_ms = now_ms().saturating_sub(3_100);
        }
        let dump_before = dump_active_holds();
        assert!(dump_before.contains("state_lock.rs"), "holder still active");
        drop(holder);
        {
            let map = ACTIVE_READS.lock();
            assert!(map.get(&key).is_none(), "drop removed the hold");
        }
        let drops = recent_slow_read_drops();
        assert_eq!(
            drops.len(),
            1,
            "a hold released slow with no intervening acquire must be reported at drop"
        );
        assert!(
            drops[0].location.contains("state_lock.rs"),
            "drop report names the acquire site, got {}",
            drops[0].location
        );
        assert!(drops[0].held_ms >= 3_099, "drop report carries the hold ms");
        // The forensic dump must surface released slow holds too.
        let dump = dump_active_holds();
        assert!(
            dump.contains("recent slow read drops"),
            "dump lists released slow holds, got {dump}"
        );
        set_warn_threshold_ms(500);
        set_diag_enabled_for_tests(false);
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn fast_read_drop_is_silent_and_warned_hold_not_double_reported() {
        let _serial = test_serial();
        set_diag_enabled_for_tests(true);
        set_warn_threshold_ms(500);
        clear_recent_slow_read_drops();
        let lock: StateLock<u8> = StateLock::new(0);
        let key = lock.key_of();
        {
            let guard = lock.read();
            let _ = *guard;
            // held ~0ms — under threshold: silent drop
        }
        assert!(
            recent_slow_read_drops().is_empty(),
            "fast drop must not be reported"
        );
        // A hold already flagged by the aged-check must NOT be re-reported
        // at drop (warn once, whichever path fires first).
        set_warn_threshold_ms(1);
        let holder = lock.read();
        {
            let mut map = ACTIVE_READS.lock();
            let records = map.get_mut(&key).expect("holder registered");
            records[0].acquired_ms = now_ms().saturating_sub(10_000);
        }
        advance_clock_ms(60_000);
        let second = lock.read(); // aged-check flags + warns the first hold
        let flagged = recent_slow_read_drops().len();
        drop(second);
        drop(holder);
        assert_eq!(
            recent_slow_read_drops().len(),
            flagged,
            "an already-warned hold must not be re-reported at drop"
        );
        set_warn_threshold_ms(500);
        set_diag_enabled_for_tests(false);
    }

    #[test]
    fn read_guard_with_diag_disabled_registers_nothing() {
        let _serial = test_serial();
        set_diag_enabled_for_tests(false);
        let lock: StateLock<u8> = StateLock::new(0);
        let key = lock.key_of();
        let guard = lock.read();
        assert_eq!(*guard, 0);
        {
            let map = ACTIVE_READS.lock();
            assert!(
                map.get(&key).is_none(),
                "default (gated-off) mode must not register holds"
            );
        }
        drop(guard);
        set_diag_enabled_for_tests(true);
    }

    #[test]
    fn fast_write_quiet_path_returns_working_guard() {
        // An uncontended write waits < 1ms virtually always, so the quiet path
        // runs (log-content assertions belong to an integration harness).
        let lock: StateLock<u16> = StateLock::new(3);
        assert_eq!(*lock.write(), 3);
    }
}
