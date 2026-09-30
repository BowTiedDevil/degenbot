//! Discovery-phase progress diagnostics for `BundledSearch`.
//!
//! A silently-stalled DFS grinds with the GIL released; on the async path
//! that grind runs on a tokio worker, so the Python event loop keeps
//! turning and a Python-side progress log cannot reflect the DFS's
//! internal progress. The heartbeat emits to stderr (GIL-free, zero deps)
//! so a future zero-yield hang is visible at a glance, not just "78% CPU,
//! no logs".
//!
//! # Diagnostic-only contract
//!
//! [`ProgressReporter`] is PURELY DIAGNOSTIC — it never alters
//! `advance()`'s return values or enumeration order, and it is NEVER a
//! cancel-check owner. Cooperative cancellation stays on `BundledSearch`'s
//! own checks (the entry gate before the emission dispatcher and the
//! inner walk-loop break); those suppress different things, and the
//! pinned cancel tests depend on both.

use std::time::{Duration, Instant};

/// Minimum elapsed wall-clock between discovery heartbeat emissions.
///
/// ~10s keeps a long search quiet but surfaces a hang within the ~5-min
/// bounded-time target. Tuned so small synthetic test fixtures
/// (which complete in µs) never emit.
const DISCOVERY_HEARTBEAT: Duration = Duration::from_secs(10);

/// Check the heartbeat clock every this many stack-frame iterations (amortizes
/// `Instant::now` out of the hot per-edge DFS loop). Power-of-two so the modulo
/// is a bitmask.
const HEARTBEAT_CHECK_EVERY: u64 = 4096;

/// A long-running walk's live progress, delivered to a caller-installed
/// observation hook ([`BundledSearch::with_progress`]) on the heartbeat
/// checkpoint. A pure snapshot: reading it never perturbs the enumeration.
#[derive(Clone, Copy, Debug)]
pub struct WalkerTally {
    /// Wall clock since the search's first advance.
    pub elapsed: Duration,
    /// Completed paths yielded so far.
    pub paths_yielded: u64,
    /// Stack-frame advances since the most recent yield: a large value with a
    /// small `paths_yielded` is the no-yield straggler shape.
    pub advances_since_yield: u64,
    /// Peak DFS stack depth observed — distinguishes "stuck shallow" from
    /// "grinding deep" on a real run.
    pub max_stack_depth: usize,
}

/// Caller-installed long-run observation hook ([`WalkerTally`]); when
/// present it replaces the default stderr heartbeat as the emit channel.
pub(crate) type ProgressHook = dyn FnMut(&WalkerTally) + Send + Sync;

/// Diagnostic-only discovery-progress reporter: the discovery heartbeat
/// counters, the caller's observation hook, and the search-end summary
/// line.
///
/// Purely diagnostic — never alters `advance()`'s return values or
/// enumeration order, and never owns a cancel check (see the module doc).
pub(crate) struct ProgressReporter {
    search_started: Instant,
    paths_yielded: u64,
    advances_since_yield: u64,
    last_heartbeat: Instant,
    /// Peak DFS stack depth observed — distinguishes "stuck shallow" (ordering
    /// gap) from "grinding deep" (graph-size variance) on a real run.
    max_stack_depth: usize,
    /// Caller-installed long-run observation hook; when present it
    /// replaces the default stderr heartbeat as the emit channel.
    progress: Option<Box<ProgressHook>>,
    /// Wall clock between progress reports. The hook's interval when a hook
    /// is installed; [`DISCOVERY_HEARTBEAT`] for the default stderr line.
    progress_every: Duration,
}

impl ProgressReporter {
    /// Start the report clock at `now` (the search's construction instant).
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            search_started: now,
            paths_yielded: 0,
            advances_since_yield: 0,
            last_heartbeat: now,
            max_stack_depth: 0,
            progress: None,
            progress_every: DISCOVERY_HEARTBEAT,
        }
    }

    /// Install a caller hook, replacing the default stderr heartbeat as
    /// the emit channel from the next heartbeat checkpoint on.
    pub(crate) fn install_hook(&mut self, every: Duration, sink: Box<ProgressHook>) {
        self.progress = Some(sink);
        self.progress_every = every;
    }

    /// Record one yielded path.
    pub(crate) fn on_yield(&mut self) {
        self.paths_yielded += 1;
        self.advances_since_yield = 0;
    }

    /// Record one stack-frame advance at `stack_depth` and, on the throttled
    /// heartbeat checkpoint (every `HEARTBEAT_CHECK_EVERY` advances), emit
    /// the [`WalkerTally`] snapshot through the caller's hook or the
    /// default stderr line. Diagnostic-only: observation, never control
    /// flow.
    pub(crate) fn on_advance(&mut self, stack_depth: usize) {
        self.advances_since_yield = self.advances_since_yield.wrapping_add(1);
        if stack_depth > self.max_stack_depth {
            self.max_stack_depth = stack_depth;
        }
        if self
            .advances_since_yield
            .is_multiple_of(HEARTBEAT_CHECK_EVERY)
        {
            let now = Instant::now();
            if now.duration_since(self.last_heartbeat) >= self.progress_every {
                self.last_heartbeat = now;
                let tally = WalkerTally {
                    elapsed: now.duration_since(self.search_started),
                    paths_yielded: self.paths_yielded,
                    advances_since_yield: self.advances_since_yield,
                    max_stack_depth: self.max_stack_depth,
                };
                match self.progress.as_mut() {
                    Some(sink) => sink(&tally),
                    // Default diagnostic on a zero-dependency leaf; no
                    // logging crate is available and this runs off the
                    // hot path.
                    None => {
                        #[expect(clippy::print_stderr)]
                        {
                            eprintln!(
                                "discovery heartbeat: elapsed={:?} \
                                 paths_yielded={} advances_since_yield={} max_stack_depth={}",
                                tally.elapsed,
                                tally.paths_yielded,
                                tally.advances_since_yield,
                                tally.max_stack_depth
                            );
                        }
                    }
                }
            }
        }
    }

    /// Emit a final discovery-complete line so the operator sees the total at
    /// search end (cheap; covers the common fast-search case that never
    /// tripped the throttled heartbeat).
    pub(crate) fn finish(&self) {
        let elapsed = self.search_started.elapsed();
        // Low-frequency stderr diagnostic on a zero-dependency leaf (no
        // logging crate available); one line at discovery completion.
        #[expect(clippy::print_stderr)]
        {
            eprintln!(
                "discovery complete: elapsed={elapsed:?} paths_yielded={} max_stack_depth={}",
                self.paths_yielded, self.max_stack_depth
            );
        }
    }
}
