//! Worker census registry (ergo PE4FPM; observability surface of the
//! ADR-042 role-switching fleet, section 7).
//!
//! Every execution resource — a tokio runtime, a worker seat pool, a semaphore
//! capacity, a sidecar/drainer thread — SELF-REGISTERS one census entry
//! (id, kind, worker count, OS thread-name pattern, sizing rule) into one
//! process-wide table:
//!
//! - [`snapshot`] is the table (sorted, stable `resource` ids usable as the
//!   `resource` label of the `degenbot_worker_census{resource=...}` gauge,
//!   exported by `degenbot-bot::instruments` via the hook installed at
//!   metrics init);
//! - [`emit_boot_table`] fires the one structured `info!` boot line with
//!   the full table (called by the drivers' boot prelude);
//! - registrations AFTER the boot dump each emit their own structured
//!   `info!` line, so lazily-booted resources are visible in the log too.
//!
//! # GOQWCL lesson: distinct thread names
//!
//! Two concurrent multi-thread tokio runtimes must never share a thread
//! name (the incident: both defaulting to `tokio-runtime-worker` made
//! thread dumps unattributable). Spawn sites name their threads with the
//! census-declared pattern (e.g. `degenbot-io-rt-{n}`) and REGISTER here.
//! The solve-fleet naming (`degenbot-solve-*`) and the hotpath
//! profiler-owned `hp-*` threads are untouched by this registry.
//!
//! # REGISTRATION IS DOCUMENTATION-ENFORCED
//!
//! **If you add a new spawn site — any `thread::Builder::new`,
//! `tokio::runtime::Builder` (a solve executor), or capacity
//! constant that bounds concurrent execution — you MUST register it here**
//! via `worker_census::register`. The census survives unknown-future spawns
//! only by this documentation; an unregistered thread is invisible to the
//! gauge, the boot dump, and the /proc comm cross-check. Registered ids
//! today: `io_runtime_workers`, `inline_sim_runtime_workers`,
//! `solve_probe_executor`, `fleet_solver_slots`, `fleet_simdriver_slots`,
//! `fleet_resolve_slots`, `fleet_merge_slots`,
//! `fleet_pool_state_updater_slots`,
//! `detached_solve_bins`, `metrics_scrape`,
//! `rust_log_drainer`, `gil_probe`.
//!
//! # Future fleet crate
//!
//! The upcoming `degenbot-workers` fleet (ADR-042) registers ITS worker
//! slots with role labels through the same [`register`] entry point — the
//! registration API is label-driven, not enum-driven, so fleet roles need no
//! change here.

use parking_lot::Mutex;
use std::sync::OnceLock;

/// One execution resource: a pool/runtime/capacity owning concurrent work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCensusEntry {
    /// Stable, low-cardinality resource id — the `resource` metric label.
    /// Must be a small closed set (metric cardinality rule); reuse an id
    /// listed in the module docs instead of minting a near-duplicate.
    pub resource: &'static str,
    /// What kind of execution resource this is (fleet `kind` label surface).
    pub kind: &'static str,
    /// Worker / slot count. For burst resources: the sustained cap enforced
    /// by the pacer (the `sizing` text documents the burst behavior).
    pub count: usize,
    /// OS thread-name pattern (`{n}` = pool index, `{pid}` = path id).
    /// `n/a (hoisted capacity)` for capacities that own no thread.
    pub thread_name: &'static str,
    /// Sizing rule in words: what derives the count and which override wins.
    pub sizing: &'static str,
    /// How this row's work binds to host threads (FF-T2, the closed
    /// vocabulary): `pinned` = dedicated thread(s) owned by the row (the
    /// fleet's pinned-binding seats; single-purpose infra threads),
    /// `shared` = threads shared across concerns (the ambient I/O runtime,
    /// runtime pools), `logical` = a lane/capacity with no thread of its
    /// own (hoisted capacities, registry probes; the fleet roles become
    /// logical lanes under the serial binding, FF-T4).
    pub binding: &'static str,
}

static CENSUS: OnceLock<Mutex<Vec<WorkerCensusEntry>>> = OnceLock::new();
static BOOT_DUMPED: OnceLock<()> = OnceLock::new();

/// Export hook: the consumer above the registry (degenbot-bot instruments)
/// installs a gauge exporter; every registration re-fires it so the
/// scrape always reflects the current table. A plain `fn` pointer — no
/// captures, set once at metrics init.
static EXPORT_HOOK: OnceLock<fn(&[WorkerCensusEntry])> = OnceLock::new();

/// Install the metric exporter (called by degenbot-bot at metrics init,
/// before any registration site runs). Idempotent; the first installation
/// wins — a fleet crate must EXPORT through the bot layer, not replace it.
pub fn set_export_hook(hook: fn(&[WorkerCensusEntry])) {
    let _ = EXPORT_HOOK.set(hook);
}

/// Publish the full table as the ONE structured `info!` boot line and arm
/// the late-registration notice. Called once by the drivers' boot prelude
/// (degenbot-python pyinit) after the eager registrations.
pub fn emit_boot_table() {
    let table = snapshot();
    BOOT_DUMPED.get_or_init(|| {
        crate::op_info!(domain = pump, entries = ?table,
            "boot table full",
        );
    });
}

fn table_cell() -> &'static Mutex<Vec<WorkerCensusEntry>> {
    CENSUS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register (or update) this process's census entry for `resource`.
/// Cheap: one short, unpoisoned mutex section over a handful of rows,
/// called once per resource init (idempotent upsert, so
/// lazily-initialized sites may call it unconditionally). Fires the
/// export hook after the upsert; a registration arriving AFTER the boot
/// dump emits its own structured `info!` line (the census survives
/// unknown-future spawns only by registration documentation — see the
/// module docs).
pub fn register(entry: WorkerCensusEntry) {
    let late = BOOT_DUMPED.get().is_some();
    // Copy the logging fields out before the move into the table.
    let announced = (entry.resource, entry.count, entry.thread_name, entry.sizing);
    {
        let mut table = table_cell().lock();
        match table.iter_mut().find(|e| e.resource == entry.resource) {
            Some(slot) => *slot = entry,
            None => table.push(entry),
        }
        table.sort_by(|a, b| a.resource.cmp(b.resource));
    }
    if late {
        crate::op_info!(
            domain = pump,
            resource = announced.0,
            count = announced.1,
            thread_name = announced.2,
            sizing = announced.3,
            "registered after boot dump — new spawn site must register",
        );
    }
    if let Some(hook) = EXPORT_HOOK.get() {
        hook(&snapshot());
    }
}

/// Sorted table snapshot (metric export + boot dump + tests).
#[must_use]
pub fn snapshot() -> Vec<WorkerCensusEntry> {
    table_cell().lock().clone()
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn probe(n: usize, name: &str) -> WorkerCensusEntry {
        WorkerCensusEntry {
            resource: Box::leak(format!("census-probe-{n}").into_boxed_str()),
            kind: "test resource",
            count: n + 1,
            thread_name: Box::leak(name.to_string().into_boxed_str()),
            sizing: "test sizing rule",
            binding: "logical",
        }
    }

    #[test]
    fn register_upserts_by_resource_and_snapshot_is_sorted() {
        let a = probe(1, "census-probe-a-{n}");
        let b = probe(2, "census-probe-b-{n}");
        register(a.clone());
        // Upsert: same id, new count.
        let mut updated = a.clone();
        updated.count = 7;
        register(updated.clone());
        register(b);

        let snap = snapshot();
        let ra = snap
            .iter()
            .find(|e| e.resource == "census-probe-1")
            .expect("probe-1 registered");
        assert_eq!(
            ra.count, 7,
            "re-registering an id must UPSERT, not duplicate"
        );
        assert_eq!(
            snap.iter()
                .filter(|e| e.resource == "census-probe-1")
                .count(),
            1,
            "duplicate ids must not double-count in the census"
        );
        let rb = snap
            .iter()
            .find(|e| e.resource == "census-probe-2")
            .expect("probe-2 registered");

        // Sorted, stable: ids order before later-added resources.
        let idx = |id: &str| snap.iter().position(|e| e.resource == id).unwrap();
        assert!(
            idx("census-probe-1") < idx("census-probe-2"),
            "snapshot must be sorted by resource id"
        );
        assert_eq!(ra.thread_name, "census-probe-a-{n}");
        assert_eq!(rb.count, 3);
    }

    #[test]
    fn export_hook_fires_on_registration_with_full_snapshot() {
        static FIRES: AtomicUsize = AtomicUsize::new(0);
        static LAST_LEN: AtomicUsize = AtomicUsize::new(0);
        fn counting_hook(entries: &[WorkerCensusEntry]) {
            FIRES.fetch_add(1, Ordering::SeqCst);
            LAST_LEN.store(entries.len(), Ordering::SeqCst);
        }
        // First installation wins (single set per process by design).
        set_export_hook(counting_hook);
        let before = FIRES.load(Ordering::SeqCst);
        register(probe(3, "census-probe-hook"));
        assert!(
            FIRES.load(Ordering::SeqCst) > before,
            "each registration must re-fire the export hook"
        );
        let len = LAST_LEN.load(Ordering::SeqCst);
        assert!(
            len >= 1,
            "hook must receive the FULL snapshot, not the entry"
        );
        assert!(
            snapshot().iter().any(|e| e.resource == "census-probe-3"),
            "hook-fired snapshot must contain the new resource"
        );
    }

    // ---- boot dump (the one structured info! line) ----

    /// Mirror of the [cpu-budget] recording-subscriber probe: asserts the
    /// boot line without a tracing-subscriber dependency in this leaf crate.
    struct RecordingSubscriber(std::sync::Mutex<String>);

    struct LogLineVisitor<'a>(&'a mut String);

    impl tracing_core::field::Visit for LogLineVisitor<'_> {
        fn record_debug(
            &mut self,
            field: &tracing_core::field::Field,
            value: &dyn std::fmt::Debug,
        ) {
            std::fmt::write(self.0, format_args!("{}={:?} ", field.name(), value)).ok();
        }
    }

    impl tracing_core::Subscriber for RecordingSubscriber {
        fn enabled(&self, _: &tracing_core::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing_core::span::Attributes<'_>) -> tracing_core::span::Id {
            tracing_core::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing_core::span::Id, _: &tracing_core::span::Record<'_>) {}
        fn enter(&self, _: &tracing_core::span::Id) {}
        fn exit(&self, _: &tracing_core::span::Id) {}
        fn record_follows_from(&self, _: &tracing_core::span::Id, _: &tracing_core::span::Id) {}
        fn event(&self, event: &tracing_core::event::Event<'_>) {
            let mut buf = String::new();
            event.record(&mut LogLineVisitor(&mut buf));
            if let Ok(mut s) = self.0.lock() {
                s.push_str(&buf);
            }
        }
    }

    #[test]
    fn boot_table_emits_one_info_line_with_the_full_table() {
        register(probe(4, "census-probe-boot"));
        let rec = std::sync::Arc::new(RecordingSubscriber(std::sync::Mutex::new(String::new())));
        let sunk = std::sync::Arc::clone(&rec);
        tracing::subscriber::with_default(sunk, emit_boot_table);
        let logged = rec.0.lock().unwrap().clone();
        // The area is derived from the target, not carried in the message
        // (ADR-043 section 7) - the table itself must stay intact.
        assert!(
            !logged.contains("[worker-census]"),
            "tag left in message: {logged}"
        );
        assert!(
            logged.contains("census-probe-boot"),
            "boot table missing the registered resource: {logged}"
        );
    }

    // ---- /proc comm histogram cross-check (the live dry-run acceptance) ----

    /// The dry-run acceptance ("the /proc/<pid>/task comm histogram
    /// census-matches the metric") as an executable process-level check:
    /// spawn REAL threads carrying the census-declared thread-name pattern,
    /// read `/proc/self/task/*/comm`, build the histogram, and match it
    /// against the registered census. Documented manual cross-check on a
    /// live bot: contrast the `degenbot_worker_census` rows on the /metrics
    /// scrape against `for t in /proc/<pid>/task/*; do cat $t/comm; done |
    /// sort | uniq -c` (15-char comm truncation: compare on the census
    /// `thread_name` prefix).
    #[test]
    fn comm_histogram_census_matches_registry() {
        const THREAD_NAME: &str = "census-comm-probe";
        // Kernel `comm` holds at most 15 bytes (TASK_COMM_LEN - 1): the
        // cross-check compares the census thread_name on the same form.
        const COMM_MAX: usize = 15;
        register(WorkerCensusEntry {
            resource: Box::leak("census-probe-comm".to_string().into_boxed_str()),
            kind: "comm-probe threads",
            count: 3,
            thread_name: THREAD_NAME,
            sizing: "test: exactly 3 threads",
            binding: "pinned",
        });

        let deadline = std::time::Duration::from_secs(5);
        let poll = std::time::Duration::from_millis(50);
        let start = std::time::Instant::now();
        std::thread::scope(|s| {
            let parked = std::sync::Arc::new(std::sync::Barrier::new(4));
            for _ in 0..3 {
                let parked = std::sync::Arc::clone(&parked);
                std::thread::Builder::new()
                    .name(THREAD_NAME.to_string())
                    .spawn_scoped(s, move || {
                        parked.wait();
                        // Held: the parent reads /proc while every probe lives.
                        parked.wait();
                    })
                    .expect("comm-probe thread spawn");
            }
            parked.wait();

            let comm = &THREAD_NAME[..THREAD_NAME.len().min(COMM_MAX)];
            // Settle-retry: under heavy parallel test load the kernel can
            // surface newly spawned tasks in /proc readdir with a delay
            // (observed as 0-2 of 3 barrier-held probes visible on one
            // pass). The probes cannot exit (second barrier below), so
            // re-read until the comm histogram exposes the census count;
            // a genuinely wrong count still fails at the deadline.
            let histogram = loop {
                let mut h = std::collections::BTreeMap::new();
                for task in std::fs::read_dir("/proc/self/task")
                    .expect("task dir is present on linux")
                    .flatten()
                {
                    if let Ok(comm) = std::fs::read_to_string(task.path().join("comm")) {
                        *h.entry(comm.trim().to_string()).or_insert(0_u64) += 1;
                    }
                }
                if h.get(comm) == Some(&3) {
                    break h;
                }
                assert!(
                    start.elapsed() <= deadline,
                    "live /proc comm histogram must census-match the registry: {h:?}"
                );
                std::thread::sleep(poll);
            };
            parked.wait();
            assert_eq!(
                histogram.get(comm),
                Some(&3),
                "live /proc comm histogram must census-match the registry: {histogram:?}"
            );
        });
    }
}
