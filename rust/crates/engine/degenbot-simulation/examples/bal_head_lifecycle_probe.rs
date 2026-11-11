#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live probe prints its report, and the shared chain stub and capture \
              fixtures it includes fail loudly on a broken fixture"
)]

//! The canonical-head lifecycle probe: real mainnet heads driven through
//! [`BalHeadDriver`] against live reth, measuring the prepare (BAL fetch and
//! decode) and apply stages — and the readiness a caller sees between them —
//! separately from each other.
//!
//! # What is driven
//!
//! Three legs. The first two read the node's canonical chain through real
//! headers:
//!
//! - the HISTORICAL leg ([`BAL_HEAD_FIRST`](Config::from_env) ..
//!   [`BAL_HEAD_LAST`](Config::from_env), default `26375033..=26375053`): blocks
//!   the node has not served a list for in this process, so a first request pays
//!   the node's own cold list work and a repeated request pays its cache;
//! - the FRESH-HEAD leg ([`BAL_HEAD_FRESH`](Config::from_env), default `6`): the
//!   most recent canonical heads, whose lists reth precomputes as it processes
//!   each block, so a first request is already warm. `BAL_HEAD_FRESH=0` DISABLES
//!   the leg: it selects no head and asks the node for no tip head at all.
//!
//! The third is offline by construction:
//!
//! - the WAITER leg ([`BAL_HEAD_WAIT_SAMPLES`](Config::from_env), default `21`,
//!   the capture's own stretch): the committed capture's heads over the chain
//!   stub's controllably deferred transport — deliver, a CLAIMED advance parked on
//!   the closed gate, a [`wait_for_head`](BalHeadDriver::wait_for_head) registered
//!   on that live attempt, then the release. It reports the wait's resolution NET
//!   of the prepare wall it overlapped, which the two live legs cannot see: a
//!   same-block caller joins the head's preparation, and what it pays after that
//!   preparation finishes is the settle, the wake, and its own re-read.
//!
//! For every live head the probe delivers it, proves the delivered head is COLD
//! (its generation carries nothing — no parent value is served before the list
//! applies), fetches and decodes the list, applies it, and reports the two stage
//! walls plus the report the apply produced. Every identity the block's list
//! wrote is then read through the SETTLED generation and compared against an
//! independent cold read at the same anchor; any mismatch is a hard failure, as
//! is any head that served a carried value before its apply.
//!
//! The historical leg's apply wall is the WHOLE [`driver.apply`](BalHeadDriver::apply)
//! — the driver lock, the snapshot transition, and any compaction it pays — and
//! the waiter leg's number is a real wait resolution over a transport this probe
//! opens and closes, never a stand-in for one.
//!
//! # What this cannot prove
//!
//! This is storage-read fidelity, not full EVM verdict parity: the probe reads
//! no calldata and runs no candidate. It measures when a head becomes ready and
//! what its generation answers, not how a candidate's wall time would change.
//! No timing is asserted anywhere; the numbers are the report.
//!
//! The waiter leg dials no node: its heads, hashes, lists, and relevance set are
//! the committed capture's, and its transport is the offline chain stub. It
//! measures the lifecycle's settle/wake path, not a real block's list work — the
//! historical leg owns that.
//!
//! # Usage
//!
//! ```sh
//! just bal-head-lifecycle-probe
//! BAL_HEAD_FRESH=0 BAL_HEAD_FIRST=26375033 BAL_HEAD_LAST=26375053 just bal-head-lifecycle-probe
//! BAL_HEAD_FIRST=26382028 BAL_HEAD_LAST=26382051 BAL_HEAD_FRESH=6 just bal-head-lifecycle-probe
//! BAL_HEAD_WAIT_SAMPLES=0 just bal-head-lifecycle-probe
//! ```

use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, U256};

use degenbot_rpc::provider::AlloyProvider;
#[path = "shared/chain_stub.rs"]
#[expect(dead_code)]
mod chain_stub;
#[path = "shared/head_legs.rs"]
mod head_legs;
#[path = "shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;
#[path = "shared/real_coverage.rs"]
#[expect(dead_code)]
mod real_coverage;
use degenbot_simulation::sim::evm2::delta_snapshot::{
    BalHeadDriver, DeltaReport, HeadOutcome, HeadReadiness,
};
use degenbot_simulation::sim::ReadAnchor;
use head_legs::{fresh_heads, percentile, waiter_leg, WaiterChain, WaiterSample};
use real_coverage::RealCoverage;

/// The default historical stretch: the committed carry probe's own 21 blocks.
const FIRST_BLOCK: u64 = 26_375_033;
const LAST_BLOCK: u64 = 26_375_053;

/// The default fresh-head sample size.
const FRESH_HEADS: u64 = 6;

/// The default waiter-leg sample size: the committed capture's own stretch, so
/// the leg walks every head that capture recorded. Capped by the capture.
const WAIT_SAMPLES: u64 = 21;

/// The default parked window each waiter sample waits out on the closed
/// transport gate: long enough that the wait's registration is observable well
/// before its notification, and neutral in the reported difference because it
/// lands inside the wait and inside the prepare alike.
const WAIT_HOLD_MS: u64 = 20;

/// The prepare-wall thresholds the wait cost is reported against.
const DEADLINES_MS: &[u64] = &[25, 100, 500, 1_000, 2_000];

/// The committed real write-coverage capture whose write addresses stand in for
/// the caller's relevance set: the same ORACLE-labelled input the live carry
/// probe uses, so the probe's new writes are the ones a tracked-address fan-out
/// would care about instead of every address a block touched.
const RELEVANCE_CAPTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/bal_real_coverage.json"
);

/// The caller's relevance set: every distinct write address the committed
/// capture recorded, sorted so the set is reproducible.
fn relevance_set() -> Result<Vec<Address>, String> {
    let real = RealCoverage::load(RELEVANCE_CAPTURE)?;
    let mut addresses: Vec<Address> = (real.first()..=real.last())
        .flat_map(|number| real.writes_at(number).iter().map(|(address, _)| *address))
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    Ok(addresses)
}

/// Which leg a row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leg {
    /// A block the node had not served a list for: a cold first request.
    Historical,
    /// A recent head whose list reth precomputed with the block: a warm first
    /// request.
    Fresh,
}

impl Leg {
    const fn label(self) -> &'static str {
        match self {
            Self::Historical => "historical",
            Self::Fresh => "fresh",
        }
    }
}

/// What one head measured.
struct Row {
    leg: Leg,
    block: u64,
    outcome: String,
    carried: usize,
    writes: usize,
    snapshot: usize,
    /// The delivery itself: minting the head's cold generation.
    deliver: Duration,
    /// The prepare phase: BAL fetch and full decode.
    prepare: Duration,
    /// A second fetch of the same list: the node's cache floor.
    repeat: Duration,
    /// The apply: the snapshot transition, no RPC.
    apply: Duration,
    /// Identities compared against an independent cold read.
    checks: usize,
}

impl Row {
    /// Whether the first request paid the node's cold list work.
    fn is_cold_first(&self) -> bool {
        self.prepare > self.repeat
    }
}

fn env_count(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(fallback)
}

/// Whether the negative probe is armed: the settled value compared against the
/// cold read is skewed by one wei, so the comparison must fail. The red path the
/// gate checks, not a mode anything else runs in.
fn plant_divergence() -> bool {
    std::env::var("BAL_HEAD_PLANT_DIVERGENCE").is_ok_and(|value| value == "1")
}

/// The node: the chain-1 endpoint env family the live legs dial, or the
/// conftest name. Neither means no declared endpoint — fail loud.
fn rpc_url() -> Result<String, String> {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .map_err(|_| {
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) \
             to an archive mainnet endpoint"
                .to_string()
        })
}

/// The probe's inputs.
struct Config {
    url: String,
    first: u64,
    last: u64,
    fresh: u64,
    wait_samples: u64,
    wait_hold: Duration,
}

impl Config {
    fn from_env() -> Result<Self, String> {
        Ok(Self {
            url: rpc_url()?,
            first: env_count("BAL_HEAD_FIRST", FIRST_BLOCK),
            last: env_count("BAL_HEAD_LAST", LAST_BLOCK),
            fresh: env_count("BAL_HEAD_FRESH", FRESH_HEADS),
            wait_samples: env_count("BAL_HEAD_WAIT_SAMPLES", WAIT_SAMPLES),
            wait_hold: Duration::from_millis(env_count("BAL_HEAD_WAIT_HOLD_MS", WAIT_HOLD_MS)),
        })
    }
}

/// What the fresh knob asks for, as the header line states it: a zero sample
/// size is the leg's OFF switch, not a window over the tip head.
fn fresh_label(fresh: u64) -> String {
    if fresh == 0 {
        "disabled (BAL_HEAD_FRESH=0)".to_string()
    } else {
        format!("{fresh} heads")
    }
}

/// One block header's two identities.
async fn header(provider: &AlloyProvider, number: u64) -> Result<(B256, B256), String> {
    let block = provider
        .get_block(number)
        .await
        .map_err(|error| format!("get_block({number}): {error}"))?
        .ok_or_else(|| format!("no canonical block at {number}"))?;
    Ok((block.header.hash, block.header.inner.parent_hash))
}

/// The outcome's one-word shape, for the table.
fn outcome_label(report: &DeltaReport) -> String {
    match &report.outcome {
        degenbot_simulation::sim::evm2::read_hub::CarryOutcome::AlreadyCurrent => {
            "already-current".to_string()
        }
        degenbot_simulation::sim::evm2::read_hub::CarryOutcome::Carried { carried_values } => {
            format!("carried({carried_values})")
        }
        degenbot_simulation::sim::evm2::read_hub::CarryOutcome::Cold { lineage, bal } => {
            format!("cold(lineage={lineage:?}, bal={bal:?})")
        }
    }
}

/// Compare every identity the block's list wrote against an independent cold
/// read at the same anchor; returns the comparisons made.
async fn verify_settled_values(
    driver: &BalHeadDriver,
    cold: &AlloyProvider,
    anchor: ReadAnchor,
    report: &DeltaReport,
) -> Result<usize, String> {
    if report.writes.is_empty() {
        return Ok(0);
    }
    let keys: Vec<(Address, U256)> = report.writes.clone();
    let view = driver.snapshot(anchor);
    let plant = plant_divergence();
    let outcomes = cold
        .get_storage_batch_results_at(&keys, anchor.number)
        .await
        .map_err(|error| format!("cold batch at {}: {error}", anchor.number))?;
    for ((address, slot), cold_value) in keys.iter().zip(outcomes) {
        let cold_value =
            cold_value.map_err(|error| format!("cold read {address}/{slot}: {error}"))?;
        let snapshot = view.hub.snapshot_value(*address, *slot).map(|value| {
            if plant {
                value ^ U256::from(1u8)
            } else {
                value
            }
        });
        if snapshot != Some(cold_value) {
            return Err(format!(
                "block {}: snapshot {snapshot:?} != cold {cold_value} at {address}/{slot}",
                anchor.number
            ));
        }
    }
    Ok(keys.len())
}

/// One head's whole lifecycle: deliver, prove the cold path, prepare, apply, and
/// verify the settled generation.
async fn walk_head(
    driver: &BalHeadDriver,
    cold: &AlloyProvider,
    leg: Leg,
    number: u64,
    relevance: &[Address],
) -> Result<Row, String> {
    let (hash, parent_hash) = header(cold, number).await?;
    let anchor = ReadAnchor::identified(number, hash);

    let deliver_started = Instant::now();
    let ticket = driver.deliver(anchor, parent_hash);
    let deliver = deliver_started.elapsed();

    let cold_view = driver.snapshot(anchor);
    if cold_view.readiness.is_ready() || cold_view.hub.snapshot_len() != 0 {
        return Err(format!(
            "block {number}: a delivered head served {:?} with {} carried entries",
            cold_view.readiness,
            cold_view.hub.snapshot_len()
        ));
    }

    let prepare_started = Instant::now();
    let prepared = driver.prepare(ticket).await;
    let prepare = prepare_started.elapsed();
    // A second fetch of the same list: the node's own cache floor for this head.
    let repeat_started = Instant::now();
    let again = driver.prepare(ticket).await;
    let repeat = repeat_started.elapsed();
    if again.final_writes() != prepared.final_writes() {
        return Err(format!("block {number}: the two list fetches disagreed"));
    }

    let apply_started = Instant::now();
    let outcome = driver.apply(ticket, &prepared, relevance);
    let apply = apply_started.elapsed();
    let HeadOutcome::Settled(report) = outcome else {
        return Err(format!(
            "block {number}: the prepared list did not settle: {outcome:?}"
        ));
    };
    let readiness = driver.snapshot(anchor).readiness;
    if let HeadReadiness::Cold(reason) = &readiness {
        println!("  block {number}: settled cold ({reason:?})");
    }
    let checks = verify_settled_values(driver, cold, anchor, &report).await?;
    Ok(Row {
        leg,
        block: number,
        outcome: outcome_label(&report),
        carried: match report.outcome {
            degenbot_simulation::sim::evm2::read_hub::CarryOutcome::Carried { carried_values } => {
                carried_values
            }
            _ => 0,
        },
        writes: report.writes.len(),
        snapshot: driver.snapshot(anchor).hub.snapshot_len(),
        deliver,
        prepare,
        repeat,
        apply,
        checks,
    })
}

/// The historical leg: blocks the node has not cached a list for in this
/// process, pinned as the session's first head and then advanced block by block.
async fn historical_leg(
    config: &Config,
    driver: &BalHeadDriver,
    cold: &AlloyProvider,
    relevance: &[Address],
    rows: &mut Vec<Row>,
) -> Result<(), String> {
    for number in config.first..=config.last {
        let row = walk_head(driver, cold, Leg::Historical, number, relevance).await?;
        rows.push(row);
    }
    Ok(())
}

/// The fresh-head leg: the most recent canonical heads, whose lists reth
/// precomputed with the block.
///
/// A zero sample size returns here, BEFORE the tip head is selected or fetched:
/// the disabled leg walks no head and dials nothing.
async fn fresh_leg(
    config: &Config,
    driver: &BalHeadDriver,
    cold: &AlloyProvider,
    relevance: &[Address],
    rows: &mut Vec<Row>,
) -> Result<(), String> {
    let heads = fresh_heads(config.fresh, || async {
        cold.get_block_number()
            .await
            .map_err(|error| format!("eth_blockNumber: {error}"))
    })
    .await?;
    for number in heads {
        let row = walk_head(driver, cold, Leg::Fresh, number, relevance).await?;
        rows.push(row);
    }
    Ok(())
}

/// The waiter leg: one claimed advance per captured head over the deferred
/// transport, with the wait that joined it.
async fn wait_leg(config: &Config) -> Result<Vec<WaiterSample>, String> {
    if config.wait_samples == 0 {
        return Ok(Vec::new());
    }
    let chain = WaiterChain::load(RELEVANCE_CAPTURE, config.wait_samples)?;
    let driver = BalHeadDriver::new(&chain.provider());
    waiter_leg(&driver, &chain, config.wait_hold).await
}

/// The waiter leg's report: the wait's resolution against the prepare wall it
/// overlapped, per head and in summary. The prepare is reported once, by the
/// live legs; here it is only the wall the wait's own number is measured
/// against.
fn print_waiter(samples: &[WaiterSample], enabled: bool) {
    println!();
    if samples.is_empty() {
        if enabled {
            println!("waiter leg: no sample resolved (the leg failed above)");
        } else {
            println!("waiter leg: disabled (BAL_HEAD_WAIT_SAMPLES=0) — no wait was measured");
        }
        return;
    }
    let waits: Vec<Duration> = samples.iter().map(|sample| sample.wait).collect();
    let prepares: Vec<Duration> = samples.iter().map(|sample| sample.prepare).collect();
    let nets: Vec<Duration> = samples.iter().map(|sample| sample.net).collect();
    let hold = samples.first().map_or(Duration::ZERO, |sample| sample.hold);
    println!(
        "waiter leg: {} heads, one claimed advance each over the deferred transport \
         ({hold:?} parked), every wait resolved on its settle",
        samples.len()
    );
    println!(
        "  {:>8}  {:<11} {:>11} {:>11} {:>11}",
        "head", "answered", "wait", "prepare", "resolve"
    );
    for sample in samples {
        println!(
            "  {:>8}  {:<11} {:>11.2?} {:>11.2?} {:>11.2?}",
            sample.head, sample.readiness, sample.wait, sample.prepare, sample.net
        );
    }
    println!(
        "  prepare:  p50 {:.2?}  p95 {:.2?}  (the advance's own fetch and decode wall)",
        percentile(&prepares, 50),
        percentile(&prepares, 95)
    );
    println!(
        "  wait:     p50 {:.2?}  p95 {:.2?}  (wait_for_head, registration to terminal answer)",
        percentile(&waits, 50),
        percentile(&waits, 95)
    );
    println!(
        "  resolve:  p50 {:.2?}  p95 {:.2?}  (wait net of the prepare wall it overlapped)",
        percentile(&nets, 50),
        percentile(&nets, 95)
    );
}

/// Per-leg walls and readiness: the measured prepare walls a same-block
/// caller now waits out (it joins the head's preparation instead of taking a
/// timed fallback).
fn print_leg(leg: Leg, rows: &[Row]) {
    let leg_rows: Vec<&Row> = rows.iter().filter(|row| row.leg == leg).collect();
    if leg_rows.is_empty() {
        return;
    }
    let prepares: Vec<Duration> = leg_rows.iter().map(|row| row.prepare).collect();
    let repeats: Vec<Duration> = leg_rows.iter().map(|row| row.repeat).collect();
    let applies: Vec<Duration> = leg_rows.iter().map(|row| row.apply).collect();
    let delivers: Vec<Duration> = leg_rows.iter().map(|row| row.deliver).collect();
    println!();
    println!(
        "{} leg: {} heads, {} identities cold-verified, {} cold first requests",
        leg.label(),
        leg_rows.len(),
        leg_rows.iter().map(|row| row.checks).sum::<usize>(),
        leg_rows.iter().filter(|row| row.is_cold_first()).count()
    );
    println!(
        "  prepare:  p50 {:.2?}  p95 {:.2?}  max {:.2?}",
        percentile(&prepares, 50),
        percentile(&prepares, 95),
        prepares.iter().max().copied().unwrap_or_default()
    );
    println!(
        "  repeat:   p50 {:.2?}  p95 {:.2?}  (the node's own list-cache floor)",
        percentile(&repeats, 50),
        percentile(&repeats, 95)
    );
    println!(
        "  apply:    p50 {:.2?}  p95 {:.2?}   deliver: p50 {:.2?}",
        percentile(&applies, 50),
        percentile(&applies, 95),
        percentile(&delivers, 50)
    );
    print!("  prepare walls beyond a threshold (what a same-block wait pays):");
    for deadline in DEADLINES_MS {
        let deadline = Duration::from_millis(*deadline);
        let count = leg_rows.iter().filter(|row| row.prepare > deadline).count();
        print!(" {deadline:?}={count}/{}", leg_rows.len());
    }
    println!();
}

fn main_body(
    rows: &[Row],
    waits: &[WaiterSample],
    waiter_enabled: bool,
    mismatches: &[String],
    elapsed: Duration,
) -> Result<(), String> {
    println!();
    println!(
        "  {:>8}  {:<9} {:<44} {:>8} {:>7} {:>9} {:>10} {:>10} {:>10} {:>9}",
        "block",
        "leg",
        "outcome",
        "carried",
        "writes",
        "snapshot",
        "deliver",
        "prepare",
        "repeat",
        "apply"
    );
    for row in rows {
        println!(
            "  {:>8}  {:<9} {:<44} {:>8} {:>7} {:>9} {:>10.2?} {:>10.2?} {:>10.2?} {:>9.2?}",
            row.block,
            row.leg.label(),
            row.outcome,
            row.carried,
            row.writes,
            row.snapshot,
            row.deliver,
            row.prepare,
            row.repeat,
            row.apply
        );
    }
    print_leg(Leg::Historical, rows);
    print_leg(Leg::Fresh, rows);
    print_waiter(waits, waiter_enabled);
    println!();
    println!("total wall {elapsed:.2?} (timing is reported, never asserted)");
    if mismatches.is_empty() {
        println!("mismatches: 0 — every settled value agreed with an independent cold read");
        return Ok(());
    }
    for line in mismatches.iter().take(20) {
        println!("MISMATCH {line}");
    }
    Err(format!("{} lifecycle failures", mismatches.len()))
}

async fn run() -> Result<(), String> {
    let started = Instant::now();
    let config = Config::from_env()?;
    let provider = AlloyProvider::new(&config.url, 3)
        .await
        .map_err(|error| format!("connect {}: {error}", config.url))?;
    let cold = AlloyProvider::new(&config.url, 3)
        .await
        .map_err(|error| format!("connect {}: {error}", config.url))?;
    let chain_id = provider
        .get_chain_id()
        .await
        .map_err(|error| format!("eth_chainId: {error}"))?;
    println!(
        "node: {} (chain {chain_id}); historical {}..={}, fresh {}, waiter {} samples",
        config.url,
        config.first,
        config.last,
        fresh_label(config.fresh),
        config.wait_samples
    );
    println!(
        "prepare walls are reported as observations, never asserted: a same-block caller joins \
         the head's existing preparation and waits for its terminal settle"
    );

    // A session's first anchor is a settled head; every later head carries it.
    let driver = BalHeadDriver::new(&provider);
    let relevance = relevance_set()?;
    println!(
        "relevance: ORACLE — {} distinct write addresses from the committed capture {}",
        relevance.len(),
        RELEVANCE_CAPTURE
    );
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    let historical = historical_leg(&config, &driver, &cold, &relevance, &mut rows).await;
    let fresh = fresh_leg(&config, &driver, &cold, &relevance, &mut rows).await;
    for leg in [historical, fresh] {
        if let Err(error) = leg {
            failures.push(error);
        }
    }
    // The waiter leg owns its own driver over the offline chain stub: no node is
    // dialled, and the fresh knob says nothing about it.
    let mut waits = Vec::new();
    match wait_leg(&config).await {
        Ok(samples) => waits = samples,
        Err(error) => failures.push(error),
    }
    main_body(
        &rows,
        &waits,
        config.wait_samples != 0,
        &failures,
        started.elapsed(),
    )
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("bal_head_lifecycle_probe: {error}");
        std::process::exit(1);
    }
}
