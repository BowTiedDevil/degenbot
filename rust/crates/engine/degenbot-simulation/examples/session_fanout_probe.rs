#![expect(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a benchmark probe prints its report and fails loudly on a broken fixture"
)]

//! The session-scheduler captured-data benchmark: the recorded production
//! fan-out read walks driven through the committed `ReadHub` under the
//! committed W/F bound, walled against the serial and per-worker reads.
//!
//! # What is driven, and what the trace cannot prove
//!
//! `/tmp/fanout_trace.log` (444 rows, 14 anchors) is `logs/bot_run.log`'s
//! `fanout-trace` stream: per candidate, the EIP-2930 access list the
//! simulation seam drained (`reads` = `address|slot,slot,...`) plus the wall the
//! seam measured for that candidate (`sim_ns`, encode + sequence + assess). It
//! carries **no calldata and no chain snapshot**, so it cannot reconstruct an
//! EVM call: no opcode runs, and no recorded read is *caused* by a recorded
//! frame. This probe therefore drives the read SCHEDULE — the hub's coalescing
//! and batching under a bounded slot budget — and charges the recorded
//! execution time as wall. Actual EVM call/slot parity on this same scheduler is
//! covered by the committed `tests/async_fanout.rs` / `tests/session_async.rs`
//! suite, which owns real frames; nothing here claims to reproduce a production
//! verdict. `AsyncEvmFanout` itself cannot be driven honestly from these fields
//! (its `PreparedCandidate` carries `Evm2SimStep`s that need calldata), so this
//! probe drives the read-hub scheduling seam one layer below it: the same
//! `ReadHub` rounds, the same `FanoutLimits` bound, the same drain discipline.
//!
//! # The modeled node
//!
//! The stub transport in [`modeled_chain`] answers every recorded key at its
//! pinned block with the modeled chain's value (so a value is identical in
//! every leg at the same anchor: the digest gate catches a dropped, duplicated,
//! or reordered read, never a value drift). The transport is reached through
//! the real `AlloyProvider` batch primitives the hub dispatches, so the batches,
//! member sets, and single-flight joins counted here are the production
//! adapter's own.
//!
//! The latency model ([`wall_ledger::Node`]) is one round trip per dispatched
//! packet:
//!
//! - a storage batch costs `storage_us`, a multi-account batch costs
//!   `account_us`;
//! - `ReadHub::drain_round` issues its storage and account batches
//!   CONCURRENTLY, so a round costs the max of the kinds it carried;
//! - a serial single read costs its own kind's latency (one packet each).
//!
//! The model is charged by the drivers, not by the transport, so every leg pays
//! identical prices for identical work. The round is charged once its drain
//! settled (the waiters are woken but not polled during the charge), which is
//! the same wall a round trip in flight would cost.
//!
//! # Exec as wall
//!
//! The recorded `sim_ns` is SPENT, not merely summed: every leg sleeps it inside
//! the candidate's own slot. That is what makes the walls comparable — a leg
//! that overlaps candidates overlaps their execution too — and it is the same
//! rule the earlier `worker_fanout_probe` replay used. Execution is CPU work on
//! the lane thread (the committed prototype runs each slot's EVM in place), so
//! exec overlap is bounded by the lane count W, while F bounds how many
//! candidates hold a slot (reads in flight plus lane queueing).
//!
//! # Legs
//!
//! - `serial` — today's shape: one read in flight at a time, accounts always
//!   forwarded, storage served from the leg's anchor memo (`StorageMemo`'s
//!   contract; accounts have no per-anchor memo on the old path).
//! - `hybrid` — the earlier probe's hybrid re-measured here: W OS threads, each
//!   with its own hub, its own rounds, and its own slot set (every candidate of
//!   the lane in flight).
//! - `fibers` — one lane, one shared hub, every candidate in flight (no slot
//!   bound): the committed `fibers` leg's shape.
//! - `session` — W lanes over ONE shared hub and F total slots
//!   (`FanoutLimits::lane_capacity`), one drain role for the whole session, a
//!   lane draining only when nothing it owns can progress.
//! - `per-lane` — the same W lanes and F slots, with each lane draining its own
//!   rounds concurrently ([`DrainPolicy::PerLane`] on the scheduler's own
//!   coordinator): an empty report waits for the round that owns the lane's
//!   reads instead of spinning into a false stall.
//!
//! # Gate
//!
//! Every leg's per-candidate read digest (`keccak256` of the account triples and
//! storage words in recorded order) must equal the serial leg's.
//! `SESSION_FANOUT_PLANT_DIVERGENCE=1` makes the modeled node answer storage one
//! wei high during the `session` leg only, which must turn the gate RED and the
//! process exit nonzero (the negative-probe idiom).
//!
//! # Usage
//!
//! ```sh
//! just session-fanout-probe                  # the full report
//! SESSION_FANOUT_TRACE=/tmp/fanout_trace.log \
//! SESSION_FANOUT_STORAGE_US=700 SESSION_FANOUT_ACCOUNT_US=1200 \
//! SESSION_FANOUT_W=2 SESSION_FANOUT_F=8 just session-fanout-probe
//! SESSION_FANOUT_PLANT_DIVERGENCE=1 just session-fanout-probe   # exits 1
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::U256;

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::async_fanout::{DrainPolicy, FanoutLimits};

// The shared benchmark modules carry more surface than one consumer drives
// (the probes divide it); the parts this probe does not replay stay unmarked
// only where every item has a caller here.
#[path = "shared/modeled_chain.rs"]
mod modeled_chain;
#[path = "shared/probe_report.rs"]
#[expect(dead_code)]
mod probe_report;
#[path = "shared/recorded_corpus.rs"]
#[expect(dead_code)]
mod recorded_corpus;
#[path = "shared/session_drive.rs"]
mod session_drive;
#[path = "shared/wall_ledger.rs"]
#[expect(dead_code)]
mod wall_ledger;

use modeled_chain::{ChainState, StubChain};
use probe_report::identity_gate;
use recorded_corpus::{group_by_anchor, load_rows, Recorded, DEFAULT_TRACE};
use session_drive::{drive_lanes, extend_account, LanePlan, LegFacts};
use wall_ledger::{Digests, Node, ReadKind, WallLedger};

// ── Configuration ─────────────────────────────────────────────────────────

/// The optimistic model (the earlier probe's calibrated point).
const OPTIMISTIC: (u64, u64) = (200, 400);

/// The pessimistic model (the earlier probe's slow point).
const PESSIMISTIC: (u64, u64) = (700, 1200);

/// The fan-out size the multi-fan-out submission splits one anchor into.
const FANOUTS_PER_SUBMISSION: usize = 6;

fn env_us(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_micros)
}

fn env_count(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or(fallback)
}

/// Whether `SESSION_FANOUT_DEBUG` asks for the lane progress trace.
fn debug_enabled() -> bool {
    std::env::var("SESSION_FANOUT_DEBUG").is_ok()
}

/// What one invocation drives.
struct Config {
    trace: String,
    /// The `(storage, account)` latency points to report.
    points: Vec<(Duration, Duration)>,
    limits: FanoutLimits,
    /// Some(flag) when the negative probe is armed: the flag is raised for the
    /// `session` leg only, and the matrix narrows to the first point.
    plant: Option<Arc<AtomicBool>>,
}

impl Config {
    fn from_env() -> Self {
        let storage = env_us("SESSION_FANOUT_STORAGE_US");
        let account = env_us("SESSION_FANOUT_ACCOUNT_US");
        let points = match (storage, account) {
            (Some(storage), Some(account)) => vec![(storage, account)],
            (Some(storage), None) => vec![(storage, Duration::from_micros(OPTIMISTIC.1))],
            (None, Some(account)) => vec![(Duration::from_micros(OPTIMISTIC.0), account)],
            (None, None) => vec![
                (
                    Duration::from_micros(OPTIMISTIC.0),
                    Duration::from_micros(OPTIMISTIC.1),
                ),
                (
                    Duration::from_micros(PESSIMISTIC.0),
                    Duration::from_micros(PESSIMISTIC.1),
                ),
            ],
        };
        let limits = FanoutLimits::new(
            env_count("SESSION_FANOUT_W", 2),
            env_count("SESSION_FANOUT_F", 8),
        );
        let plant = std::env::var("SESSION_FANOUT_PLANT_DIVERGENCE")
            .is_ok()
            .then(|| Arc::new(AtomicBool::new(false)));
        Self {
            trace: std::env::var("SESSION_FANOUT_TRACE").unwrap_or_else(|_| DEFAULT_TRACE.into()),
            points,
            limits,
            plant,
        }
    }
}

// ── The leg facts ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct Totals {
    label: &'static str,
    wall: Duration,
    rounds: u64,
    storage_fetches: u64,
    account_fetches: u64,
    memo_hits: u64,
    coalesced: u64,
    peak_active: usize,
    lanes: usize,
    slots: Option<usize>,
}

impl Totals {
    fn add(&mut self, facts: &LegFacts) {
        self.label = facts.label;
        self.wall += facts.wall;
        self.rounds += facts.rounds;
        self.storage_fetches += facts.storage_fetches;
        self.account_fetches += facts.account_fetches;
        self.memo_hits += facts.memo_hits;
        self.coalesced += facts.coalesced;
        self.peak_active = self.peak_active.max(facts.peak_active);
        self.lanes = self.lanes.max(facts.lanes);
        self.slots = facts.slots.or(self.slots);
    }
}

// ── The serial leg ────────────────────────────────────────────────────────

/// Today's shape: one read in flight at a time over the production provider
/// primitives, accounts always forwarded, storage memoized per anchor.
async fn drive_serial(
    rows: &[Recorded],
    provider: &AlloyProvider,
    node: Node,
) -> Result<LegFacts, String> {
    let block = rows.first().map_or(0, |row| row.block);
    let mut memo: HashMap<(alloy::primitives::Address, U256), U256> = HashMap::new();
    let mut digests = Digests::new();
    let mut rounds = 0u64;
    let mut storage_fetches = 0u64;
    let mut account_fetches = 0u64;
    let mut memo_hits = 0u64;
    let ledger = WallLedger::new();
    let started = Instant::now();
    for (index, row) in rows.iter().enumerate() {
        let mut material: Vec<u8> = Vec::new();
        for (address, slots) in &row.reads {
            let read = provider
                .get_account_at(address, block)
                .await
                .map_err(|error| format!("serial account read of {address}: {error}"))?;
            node.charge_read(ReadKind::Account, &ledger);
            account_fetches += 1;
            rounds += 1;
            extend_account(&mut material, &read);
            for slot in slots {
                let value = if let Some(value) = memo.get(&(*address, *slot)).copied() {
                    memo_hits += 1;
                    value
                } else {
                    let word = provider
                        .get_storage_at(address, *slot, Some(block))
                        .await
                        .map_err(|error| {
                            format!("serial storage read of {address}/{slot}: {error}")
                        })?;
                    node.charge_read(ReadKind::Storage, &ledger);
                    storage_fetches += 1;
                    rounds += 1;
                    let value = U256::from_be_bytes(word.0);
                    memo.insert((*address, *slot), value);
                    value
                };
                material.extend_from_slice(&value.to_be_bytes::<32>());
            }
        }
        ledger.spend_exec(row.sim_ns);
        digests.insert((row.block, index), alloy::primitives::keccak256(&material));
    }
    Ok(LegFacts {
        label: "serial",
        wall: started.elapsed(),
        rounds,
        storage_fetches,
        account_fetches,
        memo_hits,
        coalesced: 0,
        peak_active: 1,
        lanes: 1,
        slots: None,
        digests,
    })
}

// ── One anchor's five legs ────────────────────────────────────────────────

async fn drive_fanout(
    config: &Config,
    block: u64,
    rows: &[Recorded],
    node: Node,
    provider: &AlloyProvider,
) -> Result<Vec<LegFacts>, String> {
    let debug = debug_enabled();
    if debug {
        println!(
            "    debug: anchor {block} {} candidates: serial",
            rows.len()
        );
    }
    let serial = drive_serial(rows, provider, node).await?;
    if debug {
        println!("    debug: anchor {block}: hybrid");
    }
    let ledger = WallLedger::new();
    let hybrid = drive_lanes(
        "hybrid",
        LanePlan {
            limits: config.limits,
            shared_hub: false,
            bounded: false,
            policy: DrainPolicy::PerLane,
            debug,
        },
        rows,
        0,
        provider,
        node,
        None,
        &ledger,
    )
    .await?;
    let fibers = drive_lanes(
        "fibers",
        LanePlan {
            limits: FanoutLimits::new(1, 1),
            shared_hub: true,
            bounded: false,
            policy: DrainPolicy::SessionWide,
            debug,
        },
        rows,
        0,
        provider,
        node,
        None,
        &ledger,
    )
    .await?;
    if debug {
        println!("    debug: anchor {block}: session");
    }
    if let Some(flag) = &config.plant {
        flag.store(true, Ordering::Relaxed);
    }
    let session = drive_lanes(
        "session",
        LanePlan {
            limits: config.limits,
            shared_hub: true,
            bounded: true,
            policy: DrainPolicy::SessionWide,
            debug,
        },
        rows,
        0,
        provider,
        node,
        None,
        &ledger,
    )
    .await?;
    if let Some(flag) = &config.plant {
        flag.store(false, Ordering::Relaxed);
    }
    if debug {
        println!("    debug: anchor {block}: per-lane");
    }
    let per_lane = drive_lanes(
        "per-lane",
        LanePlan {
            limits: config.limits,
            shared_hub: true,
            bounded: true,
            policy: DrainPolicy::PerLane,
            debug,
        },
        rows,
        0,
        provider,
        node,
        None,
        &ledger,
    )
    .await?;
    Ok(vec![serial, hybrid, fibers, session, per_lane])
}

// ── Report ────────────────────────────────────────────────────────────────

fn slots_label(slots: Option<usize>) -> String {
    slots.map_or_else(|| "-".to_string(), |slots| slots.to_string())
}

fn print_leg(block: u64, cands: usize, facts: &LegFacts) {
    println!(
        "  {:>8}  {:>5}  {:<8} {:>10}us {:>8} {:>8} {:>7} {:>7} {:>8} {}/{} {:>5}",
        block,
        cands,
        facts.label,
        facts.wall.as_micros(),
        facts.rounds,
        facts.storage_fetches,
        facts.account_fetches,
        facts.memo_hits,
        facts.coalesced,
        facts.lanes,
        slots_label(facts.slots),
        facts.peak_active,
    );
}

/// `leg / whole` as a speedup multiple, in hundredths (no float casts).
fn speedup(whole: Duration, part: Duration) -> String {
    if part.is_zero() {
        return "-".to_string();
    }
    let hundredths = whole.as_micros() * 100 / part.as_micros().max(1);
    format!("x{}.{:02}", hundredths / 100, hundredths % 100)
}

fn print_totals(totals: &[Totals]) {
    let base = totals.first().map_or(Duration::ZERO, |one| one.wall);
    print!("  totals  ");
    for total in totals {
        print!(
            "| {:<8} {:>9}us {}  ",
            total.label,
            total.wall.as_micros(),
            speedup(base, total.wall)
        );
    }
    println!();
    for total in totals {
        println!(
            "    {:<8} rounds={} storage_fetches={} account_fetches={} memo_hits={} \
             coalesced={} lanes={} slots={} peak={}",
            total.label,
            total.rounds,
            total.storage_fetches,
            total.account_fetches,
            total.memo_hits,
            total.coalesced,
            total.lanes,
            slots_label(total.slots),
            total.peak_active,
        );
    }
}

// ── The matrix ────────────────────────────────────────────────────────────

/// Every leg of one latency point over every recorded anchor.
async fn drive_point(
    config: &Config,
    fanouts: &[(u64, Vec<Recorded>)],
    node: Node,
    provider: &AlloyProvider,
    observed: &mut Vec<(String, Digests)>,
) -> Result<[Totals; 5], String> {
    let mut totals: [Totals; 5] = Default::default();
    for (block, rows) in fanouts {
        let legs = drive_fanout(config, *block, rows, node, provider).await?;
        for (index, facts) in legs.iter().enumerate() {
            print_leg(*block, rows.len(), facts);
            totals[index].add(facts);
            observed.push((format!("{}/{block}", facts.label), facts.digests.clone()));
        }
    }
    Ok(totals)
}

/// The slot-count sensitivity: the same anchor at a range of slot budgets.
async fn sensitivity(
    config: &Config,
    anchor: &(u64, Vec<Recorded>),
    node: Node,
    provider: &AlloyProvider,
    observed: &mut Vec<(String, Digests)>,
) -> Result<(), String> {
    let (block, rows) = anchor;
    println!(
        "\nslot-count sensitivity (anchor {block}, {} candidates, W={}):",
        rows.len(),
        config.limits.workers()
    );
    println!(
        "  {:>4} {:>6} {:>11} {:>8} {:>8} {:>8} {:>6} {:>10}",
        "F", "lanes", "wall_us", "rounds", "storage", "account", "peak", "vs F=1"
    );
    let mut serial_wall = Duration::ZERO;
    let ledger = WallLedger::new();
    for slots in [1usize, 2, 4, 8, 16, 32] {
        let plan = LanePlan {
            limits: FanoutLimits::new(config.limits.workers(), slots),
            shared_hub: true,
            bounded: true,
            policy: DrainPolicy::SessionWide,
            debug: debug_enabled(),
        };
        let facts = drive_lanes("session", plan, rows, 0, provider, node, None, &ledger).await?;
        if slots == 1 {
            serial_wall = facts.wall;
        }
        println!(
            "  {:>4} {:>6} {:>11} {:>8} {:>8} {:>8} {:>6} {:>10}",
            slots,
            facts.lanes,
            facts.wall.as_micros(),
            facts.rounds,
            facts.storage_fetches,
            facts.account_fetches,
            facts.peak_active,
            speedup(serial_wall, facts.wall),
        );
        observed.push((format!("session-f{slots}/{block}"), facts.digests));
    }
    Ok(())
}

/// Merge several legs of one submission into one report line.
fn merge_legs(label: &'static str, facts: Vec<LegFacts>, lanes: usize, slots: usize) -> LegFacts {
    let mut merged = LegFacts {
        label,
        wall: Duration::ZERO,
        rounds: 0,
        storage_fetches: 0,
        account_fetches: 0,
        memo_hits: 0,
        coalesced: 0,
        peak_active: 0,
        lanes,
        slots: Some(slots),
        digests: Digests::new(),
    };
    for one in facts {
        merged.wall = merged.wall.max(one.wall);
        merged.rounds += one.rounds;
        merged.storage_fetches += one.storage_fetches;
        merged.account_fetches += one.account_fetches;
        merged.memo_hits += one.memo_hits;
        merged.coalesced += one.coalesced;
        merged.peak_active += one.peak_active;
        merged.digests.extend(one.digests);
    }
    merged
}

/// The multi-fan-out submission: the same candidates and the same anchor, once
/// as ONE submission under the session's single bound and once as one session
/// per fan-out (the shape whose workers multiply).
async fn multi_fan_out(
    config: &Config,
    anchor: &(u64, Vec<Recorded>),
    node: Node,
    provider: &AlloyProvider,
    observed: &mut Vec<(String, Digests)>,
) -> Result<(), String> {
    let (block, rows) = anchor;
    let chunk = rows.len().div_ceil(FANOUTS_PER_SUBMISSION);
    let chunks: Vec<Vec<Recorded>> = rows.chunks(chunk).map(<[Recorded]>::to_vec).collect();
    let plan = LanePlan {
        limits: config.limits,
        shared_hub: true,
        bounded: true,
        policy: DrainPolicy::SessionWide,
        debug: debug_enabled(),
    };
    println!(
        "\nmulti-fan-out submission (anchor {block}: {} candidates as {} fan-outs of <= {chunk}):",
        rows.len(),
        chunks.len()
    );
    let ledger = WallLedger::new();
    let pooled = drive_lanes("pooled", plan, rows, 0, provider, node, None, &ledger).await?;
    print_leg(*block, rows.len(), &pooled);
    println!(
        "    one submission: {} fan-outs share ONE hub, {} lanes and {} slots",
        chunks.len(),
        pooled.lanes,
        slots_label(pooled.slots)
    );
    observed.push((format!("pooled/{block}"), pooled.digests.clone()));

    let started = Instant::now();
    let futures: Vec<_> = chunks
        .iter()
        .enumerate()
        .map(|(index, part)| {
            drive_lanes(
                "chunk",
                plan,
                part,
                index * chunk,
                provider,
                node,
                None,
                &ledger,
            )
        })
        .collect();
    let legs = futures_util::future::join_all(futures)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, String>>()?;
    let mut split = merge_legs(
        "per-fanout",
        legs,
        pooled.lanes * chunks.len(),
        config.limits.slots() * chunks.len(),
    );
    split.wall = started.elapsed();
    print_leg(*block, rows.len(), &split);
    println!(
        "    per-fan-out sessions: {} x ({} lanes, {} slots) = {} lanes, {} slots; peak is the \
         sum of the sessions' own peaks",
        chunks.len(),
        pooled.lanes,
        slots_label(pooled.slots),
        split.lanes,
        slots_label(split.slots)
    );
    println!(
        "    pooled rounds={} vs per-fan-out rounds={} for the same reads",
        pooled.rounds, split.rounds
    );
    observed.push((format!("per-fanout/{block}"), split.digests));
    Ok(())
}

// ── Entry ─────────────────────────────────────────────────────────────────

/// The run's preamble: what was loaded, the bound, and the model's two rules.
fn print_header(config: &Config, candidates: usize, anchors: usize) {
    let limits = config.limits;
    println!(
        "session_fanout_probe: trace={} rows={candidates} anchors={anchors} W={} F={} \
         (lane capacity {}..{})",
        config.trace,
        limits.workers(),
        limits.slots(),
        limits.lane_capacity(0),
        limits.lane_capacity(limits.effective_workers() - 1),
    );
    println!(
        "  model: one round trip per dispatched packet; a hub round costs the max kind it \
         carried; the recorded sim_ns is SPENT as wall inside the candidate's slot"
    );
    println!(
        "  driven: the read-hub scheduling seam (the trace carries no calldata, so no EVM \
         sequence is reconstructable; call/slot parity is the committed test suite's)"
    );
}

/// Every latency point over every recorded anchor.
async fn drive_points(
    config: &Config,
    fanouts: &[(u64, Vec<Recorded>)],
    provider: &AlloyProvider,
    observed: &mut Vec<(String, Digests)>,
) -> Result<(), String> {
    let points: &[(Duration, Duration)] = if config.plant.is_some() {
        &config.points[..1]
    } else {
        &config.points
    };
    for (storage, account) in points {
        let node = Node::new(*storage, *account);
        println!(
            "\nlatency point: storage {}us / account {}us",
            storage.as_micros(),
            account.as_micros()
        );
        println!(
            "  {:>8}  {:>5}  {:<8} {:>10}   {:>8} {:>8} {:>7} {:>7} {:>8} {:>5} {:>5}",
            "anchor",
            "cands",
            "leg",
            "wall_us",
            "rounds",
            "st-fetch",
            "ac-fetch",
            "memo",
            "single",
            "W/F",
            "peak"
        );
        let totals = drive_point(config, fanouts, node, provider, observed).await?;
        print_totals(&totals);
    }
    Ok(())
}

/// Compare every leg against the serial baseline and report the gate.
fn report_gate(observed: Vec<(String, Digests)>, candidates: usize) -> Result<(), String> {
    // The baseline is every serial leg over every recorded anchor: one entry per
    // recorded candidate, keyed by `(anchor, index within the anchor)`.
    let baseline: Digests = observed
        .iter()
        .filter(|(label, _)| label.starts_with("serial/"))
        .flat_map(|(_, digests)| digests.iter().map(|(key, digest)| (*key, *digest)))
        .collect();
    if baseline.len() != candidates {
        return Err(format!(
            "the serial baseline replayed {} of {candidates} recorded candidates",
            baseline.len()
        ));
    }
    let legs: Vec<(String, Digests)> = observed
        .into_iter()
        .filter(|(label, _)| !label.starts_with("serial/"))
        .collect();
    let mismatches = identity_gate(&baseline, &legs);
    println!();
    if mismatches.is_empty() {
        println!(
            "identity gate: GREEN ({} legs x {} candidates)",
            legs.len(),
            baseline.len()
        );
        return Ok(());
    }
    println!("identity gate: RED ({} mismatches)", mismatches.len());
    for mismatch in mismatches.iter().take(5) {
        println!("  {mismatch}");
    }
    Err("replay identity gate red".to_string())
}

async fn run() -> Result<(), String> {
    let config = Config::from_env();
    let rows = load_rows(&config.trace)?;
    let (keys, first, last) = recorded_corpus::universe(&rows);
    let chain = Arc::new(ChainState::build(&keys, first, last)?);
    let fanouts = group_by_anchor(rows);
    let candidates: usize = fanouts.iter().map(|(_, rows)| rows.len()).sum();
    let plant = config
        .plant
        .clone()
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    let provider = StubChain { chain, plant }.provider();
    print_header(&config, candidates, fanouts.len());

    let mut observed: Vec<(String, Digests)> = Vec::new();
    drive_points(&config, &fanouts, &provider, &mut observed).await?;

    if config.plant.is_none() {
        let largest = fanouts
            .iter()
            .max_by_key(|(_, rows)| rows.len())
            .expect("a non-empty trace has at least one anchor");
        let node = Node::new(config.points[0].0, config.points[0].1);
        sensitivity(&config, largest, node, &provider, &mut observed).await?;
        multi_fan_out(&config, largest, node, &provider, &mut observed).await?;
    }
    report_gate(observed, candidates)
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("session_fanout_probe: {error}");
        std::process::exit(1);
    }
}
