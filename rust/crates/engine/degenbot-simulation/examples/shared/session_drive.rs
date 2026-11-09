//! The session scheduler's replay driver: the recorded candidates driven
//! through the committed `ReadHub` under the committed W/F slot bound, with
//! every modeled wait and every spent execution recorded to the leg's wall
//! ledger. Shared by the recorded-read benchmarks so every comparison leg runs
//! the SAME schedule — one candidate order, one lane shape, one cost model —
//! and differs only in the hub it is handed.
//!
//! A recorded candidate's read walk registers against the hub and parks until a
//! round settles it, then the recorded execution is spent in the slot. The
//! lanes poll their fibers and settle a round whenever nothing they own can
//! progress; a lane with no round to settle hands the task a fresh cooperative
//! poll budget before its nothing counts as idle.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use alloy::primitives::{keccak256, B256};

use degenbot_rpc::provider::{AccountRead, AlloyProvider};
use degenbot_simulation::sim::evm2::async_fanout::{
    DrainOutcome, DrainPolicy, FanoutLimits, HubDrain, MAX_IDLE_ROUNDS,
};
use degenbot_simulation::sim::evm2::read_hub::{ReadHub, ReadHubStats};
use degenbot_simulation::sim::ReadAnchor;

use crate::recorded_corpus::Recorded;
use crate::wall_ledger::{Digests, Node, WallLedger};

/// The digest material of one account read.
pub fn extend_account(material: &mut Vec<u8>, read: &AccountRead) {
    material.extend_from_slice(&read.balance.to_be_bytes::<32>());
    material.extend_from_slice(&read.nonce.to_be_bytes());
    material.extend_from_slice(&read.code);
}

/// One leg's measured shape over one or more fan-outs.
pub struct LegFacts {
    pub label: &'static str,
    pub wall: Duration,
    /// Round trips paid: one per drained hub round, one per serial read.
    pub rounds: u64,
    pub storage_fetches: u64,
    pub account_fetches: u64,
    pub memo_hits: u64,
    pub coalesced: u64,
    pub peak_active: usize,
    pub lanes: usize,
    pub slots: Option<usize>,
    pub digests: Digests,
}

/// The session-wide in-flight gauge: the peak is a real temporal peak across
/// lanes, never a sum of per-lane maxima.
#[derive(Default)]
pub struct Gauge {
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl Gauge {
    pub fn admitted(&self) {
        let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak.fetch_max(active, Ordering::Relaxed);
    }

    pub fn released(&self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }
}

pub type WalkFuture<'h> = Pin<Box<dyn Future<Output = Result<B256, String>> + 'h>>;

/// One recorded candidate's read walk on one fiber: every read registers
/// against the hub and parks until a round settles it, then the recorded
/// execution is spent in the slot and recorded to the ledger.
pub fn walk_fiber<'h>(
    hub: &'h ReadHub<'_>,
    row: &'h Recorded,
    ledger: &'h WallLedger,
) -> WalkFuture<'h> {
    Box::pin(async move {
        let mut material: Vec<u8> = Vec::new();
        for (address, slots) in &row.reads {
            let read = hub
                .read_account(*address)
                .await
                .map_err(|error| format!("hub account read of {address}: {error}"))?;
            extend_account(&mut material, &read);
            for slot in slots {
                let value = hub
                    .read_storage(*address, *slot)
                    .await
                    .map_err(|error| format!("hub storage read of {address}/{slot}: {error}"))?;
                material.extend_from_slice(&value.to_be_bytes::<32>());
            }
        }
        ledger.spend_exec(row.sim_ns);
        ledger.complete();
        Ok(keccak256(&material))
    })
}

/// Drain one round of a leg-owned hub and charge its modeled round trip.
/// `false` when nothing was queued.
async fn settle(hub: &ReadHub<'_>, node: Node, ledger: &WallLedger) -> bool {
    let report = hub.drain_round().await;
    if report.is_empty() {
        return false;
    }
    node.charge_round(
        report.storage_members > 0,
        report.account_members > 0,
        ledger,
    );
    true
}

/// How a leg schedules its candidates.
#[derive(Clone, Copy, Debug)]
pub struct LanePlan {
    pub limits: FanoutLimits,
    /// One shared hub for the whole leg, or one hub per lane.
    pub shared_hub: bool,
    /// Bound each lane to `FanoutLimits::lane_capacity`, or leave it open.
    pub bounded: bool,
    /// The drain ownership the lanes take over a shared hub. The per-worker-hub
    /// legs own their rounds outright and ignore it.
    pub policy: DrainPolicy,
    /// The lane progress trace.
    pub debug: bool,
}

/// One lane's inputs: its plan, its rows, and the read lane it shares.
struct LaneRun<'r, 'p> {
    plan: LanePlan,
    base: usize,
    rows: &'r [Recorded],
    node: Node,
    provider: &'p AlloyProvider,
    shared: Option<&'r ReadHub<'p>>,
    drain: Option<&'r HubDrain<'r, 'p>>,
    gauge: &'r Gauge,
    ledger: &'r WallLedger,
}

struct LaneOutcome {
    digests: Digests,
    stats: Option<ReadHubStats>,
}

/// Settle one round on the leg's read lane and report what it did — through the
/// scheduler's own drain coordinator for a shared hub, or the leg-owned hub's
/// own round for a per-worker-hub leg.
async fn settle_lane_round(run: &LaneRun<'_, '_>, hub: &ReadHub<'_>) -> DrainOutcome {
    match run.drain {
        Some(drain) => {
            let node = run.node;
            let ledger = run.ledger;
            drain
                .settle_with(move |report| {
                    node.charge_round(
                        report.storage_members > 0,
                        report.account_members > 0,
                        ledger,
                    );
                })
                .await
        }
        None => {
            if settle(hub, run.node, run.ledger).await {
                DrainOutcome::Carried
            } else {
                DrainOutcome::Idle
            }
        }
    }
}

/// Drive one lane: admit into free slots, poll, and settle a round whenever
/// nothing this lane owns can progress.
async fn drive_lane(run: &LaneRun<'_, '_>, worker: usize) -> Result<LaneOutcome, String> {
    let lanes = run.plan.limits.effective_workers();
    let capacity = if run.plan.bounded {
        run.plan.limits.lane_capacity(worker)
    } else {
        usize::MAX
    };
    let mut queue: VecDeque<usize> = (worker..run.rows.len()).step_by(lanes).collect();
    let local = run.shared.is_none().then(|| {
        ReadHub::new(
            run.provider,
            ReadAnchor::at(run.rows.first().map_or(0, |r| r.block)),
        )
    });
    let hub: &ReadHub<'_> = run
        .shared
        .unwrap_or_else(|| local.as_ref().expect("a per-worker lane owns its hub"));
    // A no-op waker: this driver polls every parked fiber on every pass and
    // drains a round whenever the pass neither completed nor admitted anything,
    // so it never needs the wake hint. (A counting waker is unusable here:
    // re-polling a read that is waiting on its round re-arms a wake each time,
    // which would make "somebody woke me" permanently true and starve the
    // drain.)
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut active: Vec<(usize, WalkFuture<'_>)> = Vec::new();
    let mut digests = Digests::new();
    let mut idle_rounds = 0usize;
    let mut passes = 0u64;
    let debug = run.plan.debug;
    while !queue.is_empty() || !active.is_empty() {
        passes += 1;
        if debug && passes % 200 == 1 {
            println!(
                "    debug: lane {worker} pass {passes} active={} queued={} done={}",
                active.len(),
                queue.len(),
                digests.len()
            );
        }
        while active.len() < capacity {
            let Some(index) = queue.pop_front() else {
                break;
            };
            active.push((index, walk_fiber(hub, &run.rows[index], run.ledger)));
            run.gauge.admitted();
        }
        let mut ready: Vec<(usize, usize, B256)> = Vec::new();
        for (position, entry) in active.iter_mut().enumerate() {
            let index = entry.0;
            match entry.1.as_mut().poll(&mut cx) {
                Poll::Ready(Ok(digest)) => ready.push((position, index, digest)),
                Poll::Ready(Err(error)) => return Err(error),
                Poll::Pending => {}
            }
        }
        let completed = !ready.is_empty();
        for (position, index, digest) in ready.into_iter().rev() {
            let _dropped = active.swap_remove(position);
            run.gauge.released();
            digests.insert((run.rows[index].block, run.base + index), digest);
        }
        if debug && passes % 200 == 1 {
            let stats = hub.stats();
            println!(
                "    debug: lane {worker} pass {passes} later completed={completed} \
                 rounds={} memo={} coalesced={} slots={}",
                stats.rounds,
                stats.memo_hits,
                stats.coalesced,
                hub.memoized_slots()
            );
        }
        if completed {
            idle_rounds = 0;
            continue;
        }
        let settled = settle_lane_round(run, hub).await;
        match settled {
            DrainOutcome::Carried | DrainOutcome::Coordinated => idle_rounds = 0,
            DrainOutcome::Idle => {
                // A pass that found nothing still hands the task a fresh
                // cooperative poll budget before its nothing counts as idle: a
                // budget-starved poll reports Pending whatever its read holds,
                // so counting those passes without a yield manufactures stalls.
                tokio::task::yield_now().await;
                idle_rounds += 1;
                if idle_rounds > MAX_IDLE_ROUNDS {
                    let mut waiting: Vec<usize> = queue.iter().copied().collect();
                    waiting.extend(active.iter().map(|(index, _)| *index));
                    waiting.sort_unstable();
                    return Err(format!(
                        "lane {worker} stalled with candidates {waiting:?} waiting and no read round to settle"
                    ));
                }
            }
        }
    }
    Ok(LaneOutcome {
        digests,
        stats: local.as_ref().map(ReadHub::stats),
    })
}

/// The summed hub counters of a per-worker-hub leg.
fn sum_stats(left: ReadHubStats, right: ReadHubStats) -> ReadHubStats {
    ReadHubStats {
        memo_hits: left.memo_hits + right.memo_hits,
        coalesced: left.coalesced + right.coalesced,
        rounds: left.rounds + right.rounds,
        storage_members: left.storage_members + right.storage_members,
        account_members: left.account_members + right.account_members,
        storage_member_errors: left.storage_member_errors + right.storage_member_errors,
        account_member_errors: left.account_member_errors + right.account_member_errors,
    }
}

/// Drive `rows` with `plan`'s shape and report what the schedule did.
///
/// `base` is the index of `rows[0]` within its anchor's full fan-out, so a
/// chunked submission keys its digests exactly as the whole fan-out would.
/// `external` hands the drive a hub the caller built (a seeded or carried
/// snapshot); `None` mints the leg's own hub per `plan`.
#[expect(
    clippy::too_many_arguments,
    reason = "the drive's inputs are the comparison's controls: schedule, corpus, model, hub, ledger"
)]
pub async fn drive_lanes(
    label: &'static str,
    plan: LanePlan,
    rows: &[Recorded],
    base: usize,
    provider: &AlloyProvider,
    node: Node,
    external: Option<&ReadHub<'_>>,
    ledger: &WallLedger,
) -> Result<LegFacts, String> {
    let limits = plan.limits;
    let lanes = limits.effective_workers();
    let gauge = Gauge::default();
    let block = rows.first().map_or(0, |row| row.block);
    // A per-worker-hub leg mints one hub per lane inside the drive; a shared
    // hub is either the caller's (a seeded or carried snapshot) or a fresh one.
    let owned = (external.is_none() && plan.shared_hub)
        .then(|| ReadHub::new(provider, ReadAnchor::at(block)));
    let shared = external.or(owned.as_ref());
    let drain = shared.map(|hub| HubDrain::new(hub, plan.policy));
    let run = LaneRun {
        plan,
        base,
        rows,
        node,
        provider,
        shared,
        drain: drain.as_ref(),
        gauge: &gauge,
        ledger,
    };
    let started = std::time::Instant::now();
    let outcomes: Vec<Result<LaneOutcome, String>> = if lanes == 1 {
        vec![drive_lane(&run, 0).await]
    } else {
        // The lane's futures are thread-local by construction (they borrow the
        // hub), so each lane is built and polled on the thread that owns it, the
        // way the committed driver does it.
        let handle = tokio::runtime::Handle::current();
        std::thread::scope(|scope| {
            let joins: Vec<_> = (0..lanes)
                .map(|worker| {
                    let handle = handle.clone();
                    let run = &run;
                    scope.spawn(move || handle.block_on(drive_lane(run, worker)))
                })
                .collect();
            joins
                .into_iter()
                .map(|join| match join.join() {
                    Ok(outcome) => outcome,
                    Err(panic) => std::panic::resume_unwind(panic),
                })
                .collect()
        })
    };
    let wall = started.elapsed();
    let mut digests = Digests::new();
    let mut stats = ReadHubStats::default();
    for outcome in outcomes {
        let outcome = outcome?;
        digests.extend(outcome.digests);
        if let Some(lane_stats) = outcome.stats {
            stats = sum_stats(stats, lane_stats);
        }
    }
    if let Some(hub) = shared {
        // A shared hub's counters are already session-wide: read them once.
        stats = hub.stats();
    }
    Ok(LegFacts {
        label,
        wall,
        rounds: stats.rounds,
        storage_fetches: stats.storage_members,
        account_fetches: stats.account_members,
        memo_hits: stats.memo_hits,
        coalesced: stats.coalesced,
        peak_active: gauge.peak(),
        lanes,
        slots: plan.bounded.then_some(limits.slots()),
        digests,
    })
}
