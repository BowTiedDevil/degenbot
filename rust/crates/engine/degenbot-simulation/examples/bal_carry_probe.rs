#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a live probe prints its report and fails loudly on a broken fixture"
)]

//! The parent-linked BAL carry-forward live probe: EVERY canonical block of the
//! captured corpus range advanced through the production decode/advance
//! pipeline ([`ReadHubCache::advance_bal`]) against live reth, with every
//! candidate anchor's carried/cold values compared against INDEPENDENT
//! cold-node reads at that anchor.
//!
//! # What is driven
//!
//! `/tmp/fanout_trace.log` (444 rows, 14 anchors) is the recorded `fanout-trace`
//! stream: per candidate, the storage identities the simulation seam drained
//! (`address|slot,slot,...`). This probe processes EVERY canonical block from
//! 26375033 through 26375053 — candidate anchors AND non-candidate intermediate
//! blocks — so the parent links are real: each block's actual header hash and
//! parent hash drive the advance, and each block's exact-hash canonical block
//! access list is fetched and decoded by the production pipeline. Each CANDIDATE
//! anchor's relevance set is its own recorded candidate addresses (what the
//! carry may seed new entries for); intermediate blocks update already-known
//! keys only. At a candidate anchor every recorded identity is read through the
//! carried snapshot and, separately, cold-read at the same anchor through an
//! independent provider; any value mismatch is a HARD failure.
//!
//! # What this cannot prove
//!
//! This is recorded storage-read replay, NOT full EVM verdict parity: the trace
//! carries no calldata, no opcode runs, and no write of the engine's own is
//! exercised. What it measures is the carry-forward's value fidelity and fetch
//! economy over one real canonical stretch.
//!
//! # The trust boundary under test
//!
//! The carry assumes a syntactically valid list from the trusted node is
//! COMPLETE for writes (see `advance_bal`'s docs) — an assumption the cold
//! comparison exercises over 21 real blocks but cannot prove in general.
//!
//! # Usage
//!
//! ```sh
//! just bal-carry-probe
//! BAL_CARRY_TRACE=/tmp/fanout_trace.log BAL_CARRY_FIRST=26375033 BAL_CARRY_LAST=26375053 just bal-carry-probe
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, U256};
use serde_json::Value;

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::bal_prefetch::BalSeed;
use degenbot_simulation::sim::evm2::read_hub::{CarryOutcome, ReadHubCache};
use degenbot_simulation::sim::ReadAnchor;

/// One recorded row's read groups: an address and the slots the trace lists
/// beside it.
type ReadGroups = Vec<(Address, Vec<U256>)>;

/// The tag the bot's tracing formatter prefixes each recorded row with.
const TRACE_TAG: &str = "fanout-trace ";

/// The recorded corpus this probe is defined over.
const DEFAULT_TRACE: &str = "/tmp/fanout_trace.log";

/// The corpus's canonical stretch: every block in this range is advanced.
const FIRST_BLOCK: u64 = 26_375_033;
const LAST_BLOCK: u64 = 26_375_053;

/// The manager's set analysis to verify (a hypothesis, not a hard-coded
/// result): cold anchored reads and the estimated unbroken-chain fetches.
const HYPOTHESIS_COLD_READS: usize = 5_938;
const HYPOTHESIS_FETCHES: usize = 2_541;

/// The synthetic snapshot-size scaling leg's entry count.
const SYNTHETIC_ENTRIES: usize = 100_000;

/// The cold leg's batch member bound (one packet per chunk).
const COLD_CHUNK: usize = 500;

fn env_count(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(fallback)
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

// ── The recorded rows ─────────────────────────────────────────────────────

/// One recorded candidate's read groups, as the trace carries them.
fn parse_trace_line(line: &str) -> Option<(u64, ReadGroups)> {
    let start = line.find(TRACE_TAG)? + TRACE_TAG.len();
    let value: Value = serde_json::from_str(line[start..].trim()).ok()?;
    let block = value.get("block")?.as_u64()?;
    let reads = value
        .get("reads")?
        .as_array()?
        .iter()
        .filter_map(|entry| {
            let mut parts = entry.as_str()?.split('|');
            let address = parts.next()?.parse::<Address>().ok()?;
            let slots = parts
                .next()?
                .split(',')
                .filter(|slot| !slot.is_empty())
                .filter_map(|slot| slot.parse::<U256>().ok())
                .collect();
            Some((address, slots))
        })
        .collect();
    Some((block, reads))
}

/// Every anchor's DISTINCT recorded identities, in first-seen order — the
/// coalesced read set one hub at that anchor would carry.
fn load_anchor_keys(path: &str) -> Result<BTreeMap<u64, Vec<(Address, U256)>>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "read the recorded fan-out trace {path}: {error} (this probe has no corpus without it)"
        )
    })?;
    let mut anchors: BTreeMap<u64, BTreeSet<(Address, U256)>> = BTreeMap::new();
    let mut rows = 0usize;
    for line in text.lines() {
        if let Some((block, groups)) = parse_trace_line(line) {
            rows += 1;
            let keys = anchors.entry(block).or_default();
            for (address, slots) in groups {
                for slot in slots {
                    keys.insert((address, slot));
                }
            }
        }
    }
    if rows == 0 {
        return Err(format!("no `{TRACE_TAG}` rows in {path}"));
    }
    println!("corpus: {rows} rows, {} anchors", anchors.len());
    Ok(anchors
        .into_iter()
        .map(|(block, keys)| (block, keys.into_iter().collect()))
        .collect())
}

// ── The replay ────────────────────────────────────────────────────────────

/// What one candidate anchor's replay measured.
struct AnchorRow {
    block: u64,
    keys: usize,
    carried_hits: usize,
    write_hits: usize,
    dynamic: usize,
    batch_wall: Duration,
    cold_wall: Duration,
    mismatches: usize,
}

/// What one block's advance measured.
struct BlockRow {
    block: u64,
    outcome: String,
    carried_values: usize,
    /// Final writes applied (known-key overwrites plus relevant new seeds).
    writes: usize,
    /// Final writes left unknown: unknown slot, address outside the relevance set.
    writes_skipped: usize,
    /// Applied writes that replaced a carried (or previously applied) value.
    writes_overwritten: usize,
    snapshot: usize,
    /// Snapshot entries candidates have read somewhere in the chain.
    working_set: usize,
    /// Snapshot entries only a BAL final write ever produced.
    bal_only: usize,
    clone_time: Duration,
    bal_latency: Duration,
    advance_wall: Duration,
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

/// A percentile of a duration series (nearest-rank over the sorted copy);
/// `percent` in 1..=100.
fn percentile(series: &mut [Duration], percent: usize) -> Duration {
    series.sort_unstable();
    let rank = (series.len() * percent).div_ceil(100).max(1);
    series[rank.min(series.len()) - 1]
}

#[expect(
    clippy::too_many_lines,
    reason = "the probe's corpus, replay, report, and scaling leg read top-down"
)]
async fn run() -> Result<(), String> {
    let total_started = Instant::now();
    let trace = std::env::var("BAL_CARRY_TRACE").unwrap_or_else(|_| DEFAULT_TRACE.into());
    let first = env_count("BAL_CARRY_FIRST", FIRST_BLOCK);
    let last = env_count("BAL_CARRY_LAST", LAST_BLOCK);
    let url = rpc_url()?;
    let anchors = load_anchor_keys(&trace)?;

    let cold_anchored_reads: usize = anchors.values().map(Vec::len).sum();
    let global_distinct: BTreeSet<_> = anchors.values().flatten().copied().collect();
    println!(
        "set analysis: cold anchored reads {cold_anchored_reads} (hypothesis {HYPOTHESIS_COLD_READS}), \
         global distinct keys {} (hypothesis 2543)",
        global_distinct.len()
    );
    if cold_anchored_reads != HYPOTHESIS_COLD_READS || global_distinct.len() != 2_543 {
        return Err("the parsed corpus does not match the manager's set analysis".into());
    }

    // Two independent providers: the carry pipeline under test, and the cold
    // comparison leg that must never share its snapshot.
    let carry_provider = AlloyProvider::new(&url, 3)
        .await
        .map_err(|error| format!("connect {url}: {error}"))?;
    let cold_provider = AlloyProvider::new(&url, 3)
        .await
        .map_err(|error| format!("connect {url}: {error}"))?;
    let chain_id = carry_provider
        .get_chain_id()
        .await
        .map_err(|error| format!("eth_chainId: {error}"))?;
    println!("node: {url} (chain {chain_id})");

    let cache = ReadHubCache::new(&carry_provider);
    let mut block_rows: Vec<BlockRow> = Vec::new();
    let mut anchor_rows: Vec<AnchorRow> = Vec::new();
    let mut mismatches: Vec<String> = Vec::new();

    for number in first..=last {
        let (hash, parent_hash) = header(&carry_provider, number).await?;
        // The relevance set is the anchor's own recorded candidate addresses;
        // an intermediate block updates already-known keys only.
        let relevance: Vec<Address> = anchors
            .get(&number)
            .map(|keys| {
                keys.iter()
                    .map(|(address, _)| *address)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default();
        let started = Instant::now();
        let (hub, report) = cache
            .advance_bal(
                ReadAnchor::identified(number, hash),
                parent_hash,
                &relevance,
            )
            .await;
        let advance_wall = started.elapsed();
        let write_set: BTreeSet<_> = report.writes.iter().copied().collect();
        let outcome = match &report.outcome {
            CarryOutcome::AlreadyCurrent => "already-current".into(),
            CarryOutcome::Carried { carried_values } => format!("carried({carried_values})"),
            CarryOutcome::Cold { lineage, bal } => {
                format!("cold(lineage={lineage:?}, bal={bal:?})")
            }
        };
        let retention = hub.snapshot_breakdown();
        block_rows.push(BlockRow {
            block: number,
            outcome,
            carried_values: match report.outcome {
                CarryOutcome::Carried { carried_values } => carried_values,
                _ => 0,
            },
            writes: report.writes.len(),
            writes_skipped: report.writes_skipped,
            writes_overwritten: report.writes_overwritten,
            snapshot: hub.memoized_slots(),
            working_set: retention.working_set,
            bal_only: retention.bal_only,
            clone_time: report.clone_time,
            bal_latency: report.bal_latency,
            advance_wall,
        });

        let Some(keys) = anchors.get(&number) else {
            continue;
        };

        // Read every recorded identity through the carried snapshot, and let
        // the hub's own memo-hit counter classify: a first-poll probe is NOT a
        // sound hit classifier (a satisfied oneshot can answer Pending at
        // tokio's coop gate). A hit is either the child's own final write or a
        // carried parent value; everything else settles through the ordinary
        // batch in ONE drain.
        let hits_before = hub.stats().memo_hits;
        let futures: Vec<_> = keys
            .iter()
            .map(|&key| (key, hub.read_storage(key.0, key.1)))
            .collect();
        let hits = usize::try_from(hub.stats().memo_hits - hits_before).unwrap_or(0);
        let write_hits = keys.iter().filter(|key| write_set.contains(key)).count();
        let carried_hits = hits.saturating_sub(write_hits);
        let batch_started = Instant::now();
        let round = hub.drain_round().await;
        let batch_wall = batch_started.elapsed();
        let mut values: HashMap<(Address, U256), U256> = HashMap::new();
        for (key, read) in futures {
            values.insert(
                key,
                read.await
                    .map_err(|error| format!("hub read at {number}: {error}"))?,
            );
        }
        let dynamic = round.storage_members;
        if dynamic != keys.len() - hits {
            return Err(format!(
                "at {number}: {dynamic} dynamic members but {hits} snapshot hits of {} keys",
                keys.len()
            ));
        }
        if values.len() != keys.len() {
            return Err(format!(
                "at {number}: {} values for {} keys",
                values.len(),
                keys.len()
            ));
        }

        // The independent cold leg: the same identities at the same anchor,
        // through a provider that shares nothing with the snapshot.
        let cold_started = Instant::now();
        let mut cold_values: HashMap<(Address, U256), U256> = HashMap::new();
        for chunk in keys.chunks(COLD_CHUNK) {
            let outcomes = cold_provider
                .get_storage_batch_results_at(chunk, number)
                .await
                .map_err(|error| format!("cold batch at {number}: {error}"))?;
            for (key, outcome) in chunk.iter().zip(outcomes) {
                cold_values.insert(
                    *key,
                    outcome.map_err(|error| format!("cold read {key:?} at {number}: {error}"))?,
                );
            }
        }
        let cold_wall = cold_started.elapsed();
        let mut anchor_mismatches = 0usize;
        for &key in keys {
            if values.get(&key) != cold_values.get(&key) {
                anchor_mismatches += 1;
                mismatches.push(format!(
                    "{number} {key:?}: carried {:?} != cold {:?}",
                    values.get(&key),
                    cold_values.get(&key)
                ));
            }
        }
        anchor_rows.push(AnchorRow {
            block: number,
            keys: keys.len(),
            carried_hits,
            write_hits,
            dynamic,
            batch_wall,
            cold_wall,
            mismatches: anchor_mismatches,
        });
    }

    // ── The report ────────────────────────────────────────────────────────
    println!();
    println!("per-block advance (BAL cost is INSIDE advance wall; writes = applied, skip = left unknown outside the relevance set):");
    println!(
        "  {:>8}  {:<28} {:>8} {:>7} {:>6} {:>9} {:>8} {:>9} {:>9} {:>9} {:>9}",
        "block",
        "outcome",
        "carried",
        "writes",
        "skip",
        "snapshot",
        "work",
        "bal-only",
        "clone",
        "bal",
        "advance"
    );
    for row in &block_rows {
        println!(
            "  {:>8}  {:<28} {:>8} {:>7} {:>6} {:>9} {:>8} {:>9} {:>9.2?} {:>9.2?} {:>9.2?}",
            row.block,
            row.outcome,
            row.carried_values,
            row.writes,
            row.writes_skipped,
            row.snapshot,
            row.working_set,
            row.bal_only,
            row.clone_time,
            row.bal_latency,
            row.advance_wall
        );
    }
    println!();
    println!("per-anchor read replay (storage batch cost is batch; cold is the comparison leg):");
    println!(
        "  {:>8}  {:>6} {:>8} {:>7} {:>8} {:>10} {:>10} {:>11}",
        "block", "keys", "carried", "writes", "dynamic", "batch", "cold", "mismatches"
    );
    for row in &anchor_rows {
        println!(
            "  {:>8}  {:>6} {:>8} {:>7} {:>8} {:>10.2?} {:>10.2?} {:>11}",
            row.block,
            row.keys,
            row.carried_hits,
            row.write_hits,
            row.dynamic,
            row.batch_wall,
            row.cold_wall,
            row.mismatches
        );
    }

    let carried_hits: usize = anchor_rows.iter().map(|row| row.carried_hits).sum();
    let write_hits: usize = anchor_rows.iter().map(|row| row.write_hits).sum();
    let dynamic: usize = anchor_rows.iter().map(|row| row.dynamic).sum();
    let writes_applied: usize = block_rows.iter().map(|row| row.writes).sum();
    let writes_skipped: usize = block_rows.iter().map(|row| row.writes_skipped).sum();
    let writes_overwritten: usize = block_rows.iter().map(|row| row.writes_overwritten).sum();
    let bal_total: Duration = block_rows.iter().map(|row| row.bal_latency).sum();
    let clone_total: Duration = block_rows.iter().map(|row| row.clone_time).sum();
    let advance_total: Duration = block_rows.iter().map(|row| row.advance_wall).sum();
    let batch_total: Duration = anchor_rows.iter().map(|row| row.batch_wall).sum();
    let cold_total: Duration = anchor_rows.iter().map(|row| row.cold_wall).sum();
    let mut bal_series: Vec<Duration> = block_rows.iter().map(|row| row.bal_latency).collect();
    let fewer_tenths = 1_000 * (cold_anchored_reads - dynamic) / cold_anchored_reads;
    println!();
    println!(
        "totals over {} blocks ({} with recorded reads):",
        block_rows.len(),
        anchor_rows.len()
    );
    println!(
        "  snapshot: carried hits {carried_hits}, final-write hits {write_hits} — BAL writes seen {} = applied {writes_applied} (overwrote {writes_overwritten}, seeded {}) + skipped {writes_skipped} outside the relevance set",
        writes_applied + writes_skipped,
        writes_applied - writes_overwritten
    );
    if let Some(final_row) = block_rows.last() {
        println!(
            "  retention: working-set {} of a {}-entry snapshot (BAL-only {}); growth is unbounded within the working set and relevant writes, and the clone cost grows with it",
            final_row.working_set, final_row.snapshot, final_row.bal_only
        );
    }
    println!(
        "  fetches:  dynamic storage members {dynamic} vs cold anchored reads {cold_anchored_reads} \
         ({}.{:01}% fewer; manager hypothesis {HYPOTHESIS_FETCHES} = 57.21% fewer)",
        fewer_tenths / 10,
        fewer_tenths % 10
    );
    println!(
        "  wall:     total {:.2?}, BAL fetch+decode {:.2?} (median {:.2?}, p90 {:.2?}), clone {:.2?}, \
         storage batch {:.2?}, cold comparison {:.2?}, advance total {:.2?}",
        total_started.elapsed(),
        bal_total,
        percentile(&mut bal_series, 50),
        percentile(&mut bal_series, 90),
        clone_total,
        batch_total,
        cold_total,
        advance_total
    );

    // ── The synthetic snapshot-size scaling leg ────────────────────────────
    // A synthetic parent of SYNTHETIC_ENTRIES settled words carried into the
    // corpus's last real child anchor: pure clone/advance cost, no timing
    // assertion.
    if block_rows.len() >= 2 {
        let synth_cache = ReadHubCache::new(&carry_provider);
        let (parent_number, child_number) = (last - 1, last);
        let (parent_hash, parent_parent) = header(&carry_provider, parent_number).await?;
        let (child_hash, child_parent) = header(&carry_provider, child_number).await?;
        if child_parent != parent_hash {
            return Err("the synthetic leg's two blocks are not linked".into());
        }
        let _ = parent_parent;
        let parent = synth_cache.for_anchor(ReadAnchor::identified(parent_number, parent_hash));
        let writes = (1..=SYNTHETIC_ENTRIES)
            .map(|i| {
                (
                    Address::new([0x5c; 20]),
                    U256::from(i as u64),
                    U256::from(i as u64),
                )
            })
            .collect();
        parent.seed_bal(&BalSeed {
            writes,
            ..BalSeed::default()
        });
        let started = Instant::now();
        let (child, report) = synth_cache
            .advance_bal(
                ReadAnchor::identified(child_number, child_hash),
                parent_hash,
                &[],
            )
            .await;
        let wall = started.elapsed();
        println!();
        println!(
            "synthetic scaling leg: {} entries seeded, carried {}, clone {:.2?}, \
             advance {:.2?} (BAL {:.2?}), snapshot {}",
            SYNTHETIC_ENTRIES,
            match report.outcome {
                CarryOutcome::Carried { carried_values } => carried_values,
                _ => 0,
            },
            report.clone_time,
            wall,
            report.bal_latency,
            child.memoized_slots()
        );
    }

    println!();
    if mismatches.is_empty() {
        println!("mismatches: 0 — every carried/cold value agreed at every anchor");
        Ok(())
    } else {
        for line in mismatches.iter().take(20) {
            println!("MISMATCH {line}");
        }
        Err(format!(
            "{} value mismatches against the cold leg",
            mismatches.len()
        ))
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("bal_carry_probe: {error}");
        std::process::exit(1);
    }
}
