//! Wall-time attribution for the recorded-read benchmarks: the six buckets a
//! leg's wall is charged to, the spans the drivers record, and the arithmetic
//! that partitions and reconciles them.
//!
//! The benchmark drivers spend modeled costs (the recorded execution and the
//! priced round trips) and measure real phases (list fetch/decode, snapshot
//! clone/update). Every one of those becomes ONE span charged to exactly one
//! bucket on one timeline shared by the leg's lanes, so the three cache
//! policies are compared without charging one policy's work to another's
//! bucket.
//!
//! # What the partition guarantees
//!
//! Several spans can cover the same wall moment — a mixed round's two batch
//! prices start together, and two lanes are busy at once. The partition
//! ([`reconcile`]) charges every wall moment of the leg's window to exactly one
//! bucket by [`WallBucket`] precedence, so the six buckets sum to the measured
//! wall by construction and a shared moment is never counted twice. The
//! double occupancy the partition resolved is not hidden:
//! [`WallReport::overlap`] is its measured size, and [`WallReport::gross`]
//! keeps the per-bucket span sums the eliminable-wait ceiling reads.
//!
//! # What the recorded durations are
//!
//! A modeled charge records its MODELED duration (the price point's number,
//! plus the negative probe's planted delay), never the sleep's measured
//! overshoot: the buckets stay deterministic given the schedule, and the
//! overshoot lands in the scheduler/other residual like any other unattributed
//! wall. A measured phase records its measured duration. Timing is reported,
//! never asserted.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use alloy::primitives::B256;

/// The wall-time buckets the comparison reports. The first five carry spans;
/// `SchedulerOther` is the residual wall no span covers. Declaration order is
/// the partition's precedence: a moment several spans cover is charged to the
/// earliest declared bucket among them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallBucket {
    /// The recorded candidate execution (`sim_ns`) spent as wall.
    RecordedExec,
    /// Modeled storage round-trip wait.
    StorageWait,
    /// Modeled multi-account round-trip wait.
    AccountWait,
    /// The block access list fetch and full decode.
    BalFetchDecode,
    /// Snapshot clone and final-write update.
    SnapshotCloneUpdate,
    /// Wall no span covers: polling, scheduling, and sleep overshoot.
    SchedulerOther,
}

impl WallBucket {
    /// Every bucket, in precedence order.
    pub const ALL: [Self; 6] = [
        Self::RecordedExec,
        Self::StorageWait,
        Self::AccountWait,
        Self::BalFetchDecode,
        Self::SnapshotCloneUpdate,
        Self::SchedulerOther,
    ];

    /// The bucket's index into a [`WallReport`] array.
    pub const fn index(self) -> usize {
        match self {
            Self::RecordedExec => 0,
            Self::StorageWait => 1,
            Self::AccountWait => 2,
            Self::BalFetchDecode => 3,
            Self::SnapshotCloneUpdate => 4,
            Self::SchedulerOther => 5,
        }
    }

    /// The short label the report table prints.
    pub const fn label(self) -> &'static str {
        match self {
            Self::RecordedExec => "exec",
            Self::StorageWait => "storage",
            Self::AccountWait => "account",
            Self::BalFetchDecode => "bal",
            Self::SnapshotCloneUpdate => "snapshot",
            Self::SchedulerOther => "other",
        }
    }
}

/// One recorded wall span: `[start_ns, end_ns)` offsets from the leg's start,
/// charged to one bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// The bucket the span is charged to.
    pub bucket: WallBucket,
    /// Start offset from the leg's start.
    pub start_ns: u64,
    /// End offset from the leg's start.
    pub end_ns: u64,
}

/// The leg's wall-time ledger: the spans its drivers recorded and the offsets
/// at which candidates completed, all on one timeline shared by the lanes.
pub struct WallLedger {
    start: Instant,
    spans: Mutex<Vec<Span>>,
    completions: Mutex<Vec<u64>>,
}

impl Default for WallLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl WallLedger {
    /// A ledger whose timeline starts now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            spans: Mutex::new(Vec::new()),
            completions: Mutex::new(Vec::new()),
        }
    }

    /// The offset of now from the leg's start.
    #[must_use]
    pub fn offset(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Record a span of a MODELED duration starting now (the driver sleeps it
    /// right after), so the bucket holds the model's number, not the sleep's
    /// overshoot.
    pub fn record_fixed(&self, bucket: WallBucket, duration: Duration) {
        let start_ns = self.offset();
        self.record_at(bucket, start_ns, duration);
    }

    /// Record a modeled span from a start offset taken earlier — one round's
    /// concurrent prices share the round's start, never two nearby instants.
    fn record_at(&self, bucket: WallBucket, start_ns: u64, duration: Duration) {
        let end_ns =
            start_ns.saturating_add(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX));
        self.spans
            .lock()
            .expect("a ledger's span list is never poisoned")
            .push(Span {
                bucket,
                start_ns,
                end_ns,
            });
    }

    /// Record a measured phase: the span from `since` to now.
    pub fn record_since(&self, bucket: WallBucket, since: Instant) {
        let end_ns = self.offset();
        let start_ns = u64::try_from(since.duration_since(self.start).as_nanos()).unwrap_or(0);
        self.spans
            .lock()
            .expect("a ledger's span list is never poisoned")
            .push(Span {
                bucket,
                start_ns,
                end_ns: end_ns.max(start_ns),
            });
    }

    /// Spend one candidate's recorded execution cost as wall and record it.
    pub fn spend_exec(&self, sim_ns: u64) {
        self.record_fixed(WallBucket::RecordedExec, Duration::from_nanos(sim_ns));
        std::thread::sleep(Duration::from_nanos(sim_ns));
    }

    /// Record that a candidate completed now.
    pub fn complete(&self) {
        let offset = self.offset();
        self.completions
            .lock()
            .expect("a ledger's completion list is never poisoned")
            .push(offset);
    }

    /// The recorded spans, in recording order.
    #[must_use]
    pub fn spans(&self) -> Vec<Span> {
        self.spans
            .lock()
            .expect("a ledger's span list is never poisoned")
            .clone()
    }

    /// The candidate completion offsets, in completion order.
    #[must_use]
    pub fn completions(&self) -> Vec<u64> {
        self.completions
            .lock()
            .expect("a ledger's completion list is never poisoned")
            .clone()
    }
}

/// One leg's reconciled wall attribution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WallReport {
    /// The leg's measured wall.
    pub wall_ns: u64,
    /// The partitioned buckets: every wall moment charged once.
    pub buckets: [u64; 6],
    /// The raw per-bucket span sums (lane-time; the sixth is always zero).
    pub gross: [u64; 6],
    /// The double occupancy the partition resolved: `sum(gross) - covered`.
    pub overlap_ns: u64,
    /// Wall the five attributed buckets cover (the rest is scheduler/other).
    pub covered_ns: u64,
}

impl WallReport {
    /// The attributed bucket's partitioned wall.
    #[must_use]
    pub const fn bucket(&self, bucket: WallBucket) -> u64 {
        self.buckets[bucket.index()]
    }

    /// The attributed bucket's gross span sum.
    #[must_use]
    pub const fn gross_bucket(&self, bucket: WallBucket) -> u64 {
        self.gross[bucket.index()]
    }
}

/// Partition `spans` over the measured `wall_ns` window and reconcile the sum.
///
/// # Errors
///
/// A span that escapes the measured wall (or runs backwards) is a broken
/// instrumentation record, never silently clipped.
pub fn reconcile(spans: &[Span], wall_ns: u64) -> Result<WallReport, String> {
    let mut gross = [0u64; 6];
    for span in spans {
        if span.start_ns > span.end_ns {
            return Err(format!("span {span:?} runs backwards"));
        }
        if span.end_ns > wall_ns {
            return Err(format!(
                "span {span:?} escapes the measured wall of {wall_ns}ns"
            ));
        }
        let index = span.bucket.index();
        gross[index] = gross[index].saturating_add(span.end_ns - span.start_ns);
    }
    // Event sweep: at every boundary the active counts change, and the open
    // segment between two boundaries is charged to the highest-precedence
    // bucket covering it (or to scheduler/other when nothing covers it).
    let mut events: Vec<(u64, usize, i64)> = Vec::with_capacity(spans.len() * 2);
    for span in spans {
        if span.start_ns == span.end_ns {
            continue;
        }
        events.push((span.start_ns, span.bucket.index(), 1));
        events.push((span.end_ns, span.bucket.index(), -1));
    }
    events.sort_unstable_by_key(|(at, _, _)| *at);
    let mut active = [0i64; 6];
    let mut buckets = [0u64; 6];
    let mut cursor = 0u64;
    let mut event = 0usize;
    while event < events.len() {
        let at = events[event].0;
        if at > cursor {
            let chosen = active
                .iter()
                .take(5)
                .position(|count| *count > 0)
                .unwrap_or(5);
            buckets[chosen] = buckets[chosen].saturating_add(at - cursor);
            cursor = at;
        }
        while event < events.len() && events[event].0 == at {
            let (_, index, delta) = events[event];
            active[index] += delta;
            event += 1;
        }
    }
    if cursor < wall_ns {
        buckets[5] = buckets[5].saturating_add(wall_ns - cursor);
        cursor = wall_ns;
    }
    let covered_ns = buckets[..5].iter().sum();
    let gross_sum: u64 = gross.iter().sum();
    Ok(WallReport {
        wall_ns: cursor,
        buckets,
        gross,
        overlap_ns: gross_sum.saturating_sub(covered_ns),
        covered_ns,
    })
}

/// One candidate-completion series summarized for the report (nearest-rank
/// percentiles over the completion order).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompletionStats {
    /// How many candidates completed.
    pub count: usize,
    /// Completion of the first candidate.
    pub first_ns: u64,
    /// The median (nearest-rank 50th) completion.
    pub p50_ns: u64,
    /// The nearest-rank 95th percentile completion.
    pub p95_ns: u64,
}

/// Summarize one leg's candidate completion offsets.
///
/// # Errors
///
/// A leg that completed no candidate has no completion series to report.
pub fn completion_stats(mut completions: Vec<u64>) -> Result<CompletionStats, String> {
    if completions.is_empty() {
        return Err("the leg completed no candidate".to_string());
    }
    completions.sort_unstable();
    let rank = |percent: usize| -> usize {
        (completions.len() * percent)
            .div_ceil(100)
            .max(1)
            .min(completions.len())
    };
    Ok(CompletionStats {
        count: completions.len(),
        first_ns: completions[0],
        p50_ns: completions[rank(50) - 1],
        p95_ns: completions[rank(95) - 1],
    })
}

/// Which round trip a read costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadKind {
    /// One storage word.
    Storage,
    /// One account's balance/nonce/code triple.
    Account,
}

impl ReadKind {
    /// The wall bucket this read kind's wait is charged to.
    pub const fn bucket(self) -> WallBucket {
        match self {
            Self::Storage => WallBucket::StorageWait,
            Self::Account => WallBucket::AccountWait,
        }
    }
}

/// The latency model every leg pays: one round trip per dispatched packet, at
/// the point's prices, with the negative probe's planted storage delay folded
/// into the storage wait it inflates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    /// One storage batch's modeled round trip.
    pub storage: Duration,
    /// One multi-account batch's modeled round trip.
    pub account: Duration,
    /// Extra storage delay the negative probe plants into every storage wait.
    pub plant_storage: Duration,
}

impl Node {
    /// The price point's two prices, unplanted.
    #[must_use]
    pub const fn new(storage: Duration, account: Duration) -> Self {
        Self {
            storage,
            account,
            plant_storage: Duration::ZERO,
        }
    }

    /// The same point with the negative probe's planted storage delay.
    #[must_use]
    pub const fn with_plant(self, plant_storage: Duration) -> Self {
        Self {
            plant_storage,
            ..self
        }
    }

    /// The modeled block access list fetch's price: one packet at the model's
    /// slower kind, so a list fetch costs no less than the heaviest batch.
    #[must_use]
    pub const fn bal(self) -> Duration {
        self.account
    }

    /// The wait one read kind's round trip costs, plant included.
    #[must_use]
    pub const fn latency(self, kind: ReadKind) -> Duration {
        match kind {
            ReadKind::Storage => self.storage.saturating_add(self.plant_storage),
            ReadKind::Account => self.account,
        }
    }

    /// One serial read's round trip, charged to its kind's bucket.
    pub fn charge_read(self, kind: ReadKind, ledger: &WallLedger) {
        let wait = self.latency(kind);
        ledger.record_fixed(kind.bucket(), wait);
        std::thread::sleep(wait);
    }

    /// One drained round's round trip: the hub issues its storage and account
    /// batches concurrently, so the round wall is the slower kind it carried,
    /// while each kind is charged its OWN price as a span starting with the
    /// round. The shared part of the two waits is the overlap the partition
    /// resolves, never a second count.
    pub fn charge_round(self, storage: bool, account: bool, ledger: &WallLedger) {
        let start_ns = ledger.offset();
        let mut wall = Duration::ZERO;
        if storage {
            let wait = self.latency(ReadKind::Storage);
            ledger.record_at(ReadKind::Storage.bucket(), start_ns, wait);
            wall = wall.max(wait);
        }
        if account {
            let wait = self.latency(ReadKind::Account);
            ledger.record_at(ReadKind::Account.bucket(), start_ns, wait);
            wall = wall.max(wait);
        }
        std::thread::sleep(wall);
    }
}

/// Per-candidate read digests, keyed by `(anchor, index within the anchor)`.
pub type Digests = BTreeMap<(u64, usize), B256>;
