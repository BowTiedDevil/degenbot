#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a live probe prints its report and fails loudly on a read-value disagreement"
)]

//! The real-candidate READ-SET oracle replay: the post-fix soak's own
//! `fanout-trace` rows replayed through exact-anchor BAL generations, every
//! recorded read compared against an independent cold read of the same block.
//!
//! # What is driven
//!
//! `logs/bal_postfix_soak_trace.log` records 2606 rows over 246 real candidate
//! anchors, each carrying the read identities the simulation seam drained AND
//! the production relevance set it derived from the candidate's path and the
//! deployment's settlement facts BEFORE the candidate ran. A bounded selection
//! (two rows from each of six anchors spread across the recorded range) keeps
//! each row's own relevance set and read identities; no identity is invented
//! and no post-read walk is used as the filter.
//!
//! At every selected anchor the exact-hash canonical block access list is
//! acquired through the production negotiation ([`fetch_bal_seed`], raw-first)
//! and a fresh [`DeltaSnapshotCache`] generation is built with the row's own
//! relevance set — the same write policy production applies. Every recorded
//! read is then registered against that generation: a word the list seeded is
//! answered from the snapshot with no RPC, and an unknown (or, in the expiry
//! leg, expired) identity takes the dynamic fallback through the generation's
//! read hub. Both classes are compared against a separate provider's cold batch
//! read at the same exact block; any disagreement is a hard failure. Naturally
//! zero words are compared like any other.
//!
//! One anchor also fetches the raw and the official JSON representation and
//! decodes both through the production decoders, proving the two seeds and the
//! negotiated one agree; the negotiated representation is reported, and the
//! raw preference is the provider's own.
//!
//! # What this cannot prove
//!
//! This is diverse REAL-CANDIDATE STORAGE-READ parity, NOT executable
//! path/verdict parity: a trace row carries no calldata, no amounts and no
//! deployment state, so nothing here executes a candidate. Exact executable
//! parity remains the scripted value-sensitive matrix
//! (`rust/crates/engine/degenbot-arbitrage/tests/bal_dispatch_scaling.rs`) and
//! the matched-parity report. No speedup is claimed: the probe measures
//! correctness, and its walls are one probe's schedule.
//!
//! # Usage
//!
//! ```sh
//! just bal-real-read-oracle-probe
//! BAL_READ_ORACLE_ANCHORS=8 BAL_READ_ORACLE_ROWS=3 just bal-real-read-oracle-probe
//! BAL_READ_ORACLE_PLANT_WEI=1 just bal-real-read-oracle-probe   # negative control: must exit 1
//! ```
//!
//! Knobs: `BAL_READ_ORACLE_TRACE` (corpus path), `BAL_READ_ORACLE_ANCHORS`,
//! `BAL_READ_ORACLE_ROWS`. The negative control adds one wei to each leg's
//! first generation-side answer, and the run exits non-zero unless the
//! comparison catches every plant.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, U256};

use degenbot_rpc::provider::{AlloyProvider, BlockAccessListResponse};
use degenbot_simulation::sim::evm2::bal_prefetch::{
    decode_bal_seed, decode_bal_seed_json, fetch_bal_seed, BalPlan,
};
use degenbot_simulation::sim::evm2::delta_snapshot::{
    DeltaHub, DeltaSnapshotCache, ReadOrigin as ChainOrigin, Retention,
};
use degenbot_simulation::sim::evm2::read_hub::CarryOutcome;
use degenbot_simulation::sim::ReadAnchor;

#[path = "shared/recorded_corpus.rs"]
#[expect(dead_code)]
mod recorded_corpus;

#[path = "shared/read_oracle.rs"]
mod read_oracle;

use read_oracle::{
    compare, fingerprint, load_tagged_rows, plant_one_wei, select, write_overlap, Key, Mismatch,
    ReadAnswer, ReadOrigin,
};

/// The post-fix soak's trace stream.
const DEFAULT_TRACE: &str = "logs/bal_postfix_soak_trace.log";

/// The cold leg's batch member bound (one packet per chunk).
const COLD_CHUNK: usize = 500;

/// How many expired identities the expiry leg adds to the row's own reads.
const EXPIRY_KEYS: usize = 16;

fn env_count(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(fallback)
}

/// Whether the negative control is armed.
fn plant_armed() -> bool {
    std::env::var("BAL_READ_ORACLE_PLANT_WEI").is_ok_and(|value| value == "1")
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

/// One block header's two identities.
async fn header(provider: &AlloyProvider, number: u64) -> Result<(B256, B256), String> {
    let block = provider
        .get_block(number)
        .await
        .map_err(|error| format!("get_block({number}): {error}"))?
        .ok_or_else(|| format!("no canonical block at {number}"))?;
    Ok((block.header.hash, block.header.inner.parent_hash))
}

/// The independent cold leg: the identities at the anchor's own number, batched
/// through a provider that shares nothing with the generation under test.
async fn cold_values(
    cold: &AlloyProvider,
    number: u64,
    keys: &[Key],
) -> Result<BTreeMap<Key, U256>, String> {
    let mut values = BTreeMap::new();
    for chunk in keys.chunks(COLD_CHUNK) {
        let outcomes = cold
            .get_storage_batch_results_at(chunk, number)
            .await
            .map_err(|error| format!("cold batch at {number}: {error}"))?;
        for (key, outcome) in chunk.iter().zip(outcomes) {
            values.insert(
                *key,
                outcome.map_err(|error| format!("cold read {key:?} at {number}: {error}"))?,
            );
        }
    }
    Ok(values)
}

/// Read every identity through one generation: register them all, drain ONE
/// round, then await every answer. A snapshot answer costs no RPC; an unknown
/// or expired identity is a dynamic member of that round.
async fn read_generation(hub: &DeltaHub, keys: &[Key]) -> Result<Vec<ReadAnswer>, String> {
    let mut handles = Vec::with_capacity(keys.len());
    for &(address, slot) in keys {
        let read = hub.read_storage_handle(address, slot);
        let origin = read.origin();
        handles.push((origin, read));
    }
    let round = hub.drain_round().await;
    let dynamic = handles
        .iter()
        .filter(|(origin, _)| matches!(origin, ChainOrigin::Hub))
        .count();
    if dynamic != round.storage_members {
        return Err(format!(
            "{dynamic} dynamic reads but {} round members",
            round.storage_members
        ));
    }
    let mut answers = Vec::with_capacity(keys.len());
    for (index, (origin, read)) in handles.into_iter().enumerate() {
        let key = keys[index];
        let value = read
            .await
            .map_err(|error| format!("generation read {key:?}: {error}"))?;
        let origin = match origin {
            ChainOrigin::Snapshot => ReadOrigin::Snapshot,
            ChainOrigin::Hub => ReadOrigin::Dynamic,
        };
        answers.push(ReadAnswer { key, origin, value });
    }
    Ok(answers)
}

/// One leg's measured row.
struct LegRow {
    label: String,
    candidate: String,
    block: u64,
    relevance: usize,
    keys: usize,
    served: usize,
    dynamic: usize,
    zeros: usize,
    overlap_seed: usize,
    overlap_applied: usize,
    seeds_applied: usize,
    seeds_skipped: usize,
    expired: usize,
    mismatches: Vec<Mismatch>,
    read_wall: Duration,
}

/// The facts a finished comparison hands to its leg row.
struct LegFacts<'a> {
    label: String,
    candidate: String,
    block: u64,
    relevance: usize,
    keys: &'a [Key],
    comparison: read_oracle::Comparison,
    seed_writes: &'a BTreeSet<Key>,
    applied: &'a BTreeSet<Key>,
    seeds_skipped: usize,
    expired: usize,
    read_wall: Duration,
}

impl LegRow {
    fn from_facts(facts: LegFacts<'_>) -> Self {
        Self {
            label: facts.label,
            candidate: facts.candidate,
            block: facts.block,
            relevance: facts.relevance,
            keys: facts.keys.len(),
            served: facts.comparison.served,
            dynamic: facts.comparison.dynamic,
            zeros: facts.comparison.zeros,
            overlap_seed: write_overlap(facts.keys, facts.seed_writes),
            overlap_applied: write_overlap(facts.keys, facts.applied),
            seeds_applied: facts.applied.len(),
            seeds_skipped: facts.seeds_skipped,
            expired: facts.expired,
            mismatches: facts.comparison.mismatches,
            read_wall: facts.read_wall,
        }
    }
}

/// One exact-anchor leg: acquire the anchor's real list through the production
/// advance, read the row's own identities through the generation, and compare
/// them against the independent cold leg.
#[expect(
    clippy::too_many_arguments,
    reason = "the leg's inputs are the anchor, its record, and the two shared legs"
)]
async fn replay_leg(
    provider: &AlloyProvider,
    anchor: ReadAnchor,
    parent_hash: B256,
    relevance: &[Address],
    keys: &[Key],
    cold: &BTreeMap<Key, U256>,
    seed_writes: &BTreeSet<Key>,
    label: String,
    candidate: String,
    plant: bool,
) -> Result<LegRow, String> {
    let cache = DeltaSnapshotCache::new(provider);
    let (hub, report) = cache.advance_bal(anchor, parent_hash, relevance).await;
    if !matches!(report.outcome, CarryOutcome::Cold { .. }) {
        return Err(format!(
            "the exact-anchor generation at {} settled {:?}, not cold",
            anchor.number, report.outcome
        ));
    }
    let started = Instant::now();
    let mut answers = read_generation(&hub, keys).await?;
    let read_wall = started.elapsed();
    if plant && !plant_one_wei(&mut answers) {
        return Err(format!(
            "the plant found no answer to skew at {}",
            anchor.number
        ));
    }
    let comparison = compare(&candidate, &answers, cold);
    let applied: BTreeSet<Key> = report.writes.iter().copied().collect();
    Ok(LegRow::from_facts(LegFacts {
        label,
        candidate,
        block: anchor.number,
        relevance: relevance.len(),
        keys,
        comparison,
        seed_writes,
        applied: &applied,
        seeds_skipped: report.writes_skipped,
        expired: 0,
        read_wall,
    }))
}

/// The expiry leg: a two-generation advance under `IdleBlocks(0)`, so the
/// parent's seeded words are expired at the child and MUST take the dynamic
/// fallback — which is then compared against the same anchor's cold read.
#[expect(
    clippy::too_many_arguments,
    reason = "the leg's inputs are the anchor, its record, and the two shared legs"
)]
async fn replay_expiry_leg(
    provider: &AlloyProvider,
    cold_provider: &AlloyProvider,
    anchor: ReadAnchor,
    parent_hash: B256,
    relevance: &[Address],
    keys: &[Key],
    cold: &BTreeMap<Key, U256>,
    seed_writes: &BTreeSet<Key>,
    candidate: String,
    plant: bool,
) -> Result<LegRow, String> {
    let parent_number = anchor
        .number
        .checked_sub(1)
        .ok_or_else(|| format!("{} has no parent block", anchor.number))?;
    let (parent_anchor_hash, grandparent_hash) = header(provider, parent_number).await?;
    let parent_anchor = ReadAnchor::identified(parent_number, parent_anchor_hash);
    let cache = DeltaSnapshotCache::with_retention(provider, Retention::IdleBlocks(0));
    let (parent_hub, parent_report) = cache
        .advance_bal(parent_anchor, grandparent_hash, relevance)
        .await;
    if !matches!(parent_report.outcome, CarryOutcome::Cold { .. }) {
        return Err(format!(
            "the expiry leg's parent {parent_number} settled {:?}, not cold",
            parent_report.outcome
        ));
    }
    let (hub, report) = cache.advance_bal(anchor, parent_hash, relevance).await;
    if !matches!(report.outcome, CarryOutcome::Carried { .. }) {
        return Err(format!(
            "the expiry leg's child {} settled {:?}, not carried",
            anchor.number, report.outcome
        ));
    }
    // Identities the parent generation serves and the child neither writes nor
    // serves: under `IdleBlocks(0)` the child's generation has expired them, so
    // every read of one must take the dynamic fallback.
    let candidates: BTreeSet<Key> = keys
        .iter()
        .copied()
        .chain(parent_report.writes.iter().copied())
        .collect();
    let expired: Vec<Key> = candidates
        .iter()
        .copied()
        .filter(|&(address, slot)| {
            parent_hub.snapshot_value(address, slot).is_some()
                && hub.snapshot_value(address, slot).is_none()
        })
        .take(EXPIRY_KEYS)
        .collect();
    let read_keys: Vec<Key> = keys
        .iter()
        .copied()
        .chain(expired.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut cold_map = cold.clone();
    let missing: Vec<Key> = read_keys
        .iter()
        .copied()
        .filter(|key| !cold_map.contains_key(key))
        .collect();
    if !missing.is_empty() {
        cold_map.extend(cold_values(cold_provider, anchor.number, &missing).await?);
    }
    let started = Instant::now();
    let mut answers = read_generation(&hub, &read_keys).await?;
    let read_wall = started.elapsed();
    let expired_still_served = answers
        .iter()
        .filter(|answer| answer.origin == ReadOrigin::Snapshot && expired.contains(&answer.key))
        .count();
    if expired_still_served != 0 {
        return Err(format!(
            "{expired_still_served} expired identities were served from the snapshot at {}",
            anchor.number
        ));
    }
    if plant && !plant_one_wei(&mut answers) {
        return Err(format!(
            "the plant found no answer to skew at {}",
            anchor.number
        ));
    }
    let comparison = compare(&candidate, &answers, &cold_map);
    let applied: BTreeSet<Key> = report.writes.iter().copied().collect();
    Ok(LegRow::from_facts(LegFacts {
        label: "expiry".to_string(),
        candidate,
        block: anchor.number,
        relevance: relevance.len(),
        keys: &read_keys,
        comparison,
        seed_writes,
        applied: &applied,
        seeds_skipped: report.writes_skipped,
        expired: expired.len(),
        read_wall,
    }))
}

/// One anchor's raw-versus-JSON-versus-negotiated seed check.
struct SeedParity {
    raw_writes: usize,
    raw_reads: usize,
    json_writes: usize,
    json_reads: usize,
    negotiated_writes: usize,
    negotiated_reads: usize,
    agree: bool,
    raw_equals_negotiated: bool,
    representation: &'static str,
}

/// Fetch and decode one anchor's list in both official representations and
/// through the production negotiation, under the row's own relevance plan.
async fn seed_parity(
    provider: &AlloyProvider,
    hash: B256,
    relevance: &[Address],
) -> Result<SeedParity, String> {
    let plan = BalPlan {
        addresses: Some(relevance.to_vec()),
        ..BalPlan::default()
    };
    let raw = provider
        .get_raw_block_access_list(hash)
        .await
        .map_err(|error| format!("debug_getRawBlockAccessList({hash}): {error}"))?;
    let raw_seed = decode_bal_seed(&raw, &plan).map_err(|error| format!("raw decode: {error}"))?;
    let json = provider
        .get_block_access_list_json(hash)
        .await
        .map_err(|error| format!("eth_getBlockAccessList({hash}): {error}"))?;
    let json_seed =
        decode_bal_seed_json(&json, &plan).map_err(|error| format!("json decode: {error}"))?;
    let negotiated = fetch_bal_seed(provider, hash, &plan)
        .await
        .map_err(|skip| format!("negotiated acquisition: {skip:?}"))?;
    let representation = match provider.get_block_access_list(hash).await {
        Ok(BlockAccessListResponse::Raw(_)) => "raw",
        Ok(BlockAccessListResponse::Json(_)) => "json",
        Err(error) => return Err(format!("representation negotiation: {error}")),
    };
    Ok(SeedParity {
        raw_writes: raw_seed.writes.len(),
        raw_reads: raw_seed.reads.len(),
        json_writes: json_seed.writes.len(),
        json_reads: json_seed.reads.len(),
        negotiated_writes: negotiated.writes.len(),
        negotiated_reads: negotiated.reads.len(),
        agree: raw_seed == json_seed && raw_seed == negotiated,
        raw_equals_negotiated: raw_seed == negotiated,
        representation,
    })
}

/// The row's read identities in sorted order.
fn sorted_keys(rows: impl IntoIterator<Item = Key>) -> Vec<Key> {
    rows.into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "the probe's corpus, leg loop, report, and negative control read top-down"
)]
async fn run() -> Result<(), String> {
    let total_started = Instant::now();
    let trace = std::env::var("BAL_READ_ORACLE_TRACE").unwrap_or_else(|_| DEFAULT_TRACE.into());
    let anchors_wanted = env_count("BAL_READ_ORACLE_ANCHORS", 6);
    let rows_per_anchor = env_count("BAL_READ_ORACLE_ROWS", 2);
    let plant = plant_armed();
    let url = rpc_url()?;

    let text = std::fs::read_to_string(&trace)
        .map_err(|error| format!("read the corpus {trace}: {error}"))?;
    let corpus_fingerprint = fingerprint(text.as_bytes());
    let rows = load_tagged_rows(&trace)?;
    let first_block = rows.iter().map(|row| row.row.block).min();
    let last_block = rows.iter().map(|row| row.row.block).max();
    let anchors: BTreeSet<u64> = rows.iter().map(|row| row.row.block).collect();
    println!("trace: {trace} (fingerprint {corpus_fingerprint})");
    println!(
        "corpus: {} rows, {} anchors over {}..={}",
        rows.len(),
        anchors.len(),
        first_block.unwrap_or(0),
        last_block.unwrap_or(0)
    );

    let selection = select(&rows, anchors_wanted, rows_per_anchor)?;
    let selection_material = read_oracle::selection_material(&selection);
    println!(
        "selection: {} anchors x <= {} rows = {} rows, {} distinct (block,cand) candidates, \
         {} relevance addresses, {} read addresses, {} read identities, {} oracle-fallback rows \
         (fingerprint {})",
        selection.anchors.len(),
        rows_per_anchor,
        selection.rows(),
        selection.distinct_candidates().len(),
        selection.relevance_addresses().len(),
        selection.read_addresses().len(),
        selection.keys().len(),
        selection.oracle_rows(),
        fingerprint(selection_material.as_bytes())
    );
    if selection.oracle_rows() != 0 {
        println!(
            "relevance scope: recorded-read-oracle for those rows — the corpus predates the \
             production field and NO number here may be read as production relevance"
        );
    } else {
        println!(
            "relevance scope: production — every selected row carries the set the seam derived \
             before the candidate ran"
        );
    }

    let generation_provider = AlloyProvider::new(&url, 3)
        .await
        .map_err(|error| format!("connect {url}: {error}"))?;
    let cold_provider = AlloyProvider::new(&url, 3)
        .await
        .map_err(|error| format!("connect {url}: {error}"))?;
    let chain_id = cold_provider
        .get_chain_id()
        .await
        .map_err(|error| format!("eth_chainId: {error}"))?;
    let client = cold_provider
        .provider_arc()
        .get_client_version()
        .await
        .map_err(|error| format!("web3_clientVersion: {error}"))?;
    println!("node: {url} (chain {chain_id}, {client})");

    let mut legs: Vec<LegRow> = Vec::new();
    let mut parity: Option<SeedParity> = None;
    let mut expiry_done = false;

    for anchor_selection in &selection.anchors {
        let number = anchor_selection.block;
        let (hash, parent_hash) = header(&generation_provider, number).await?;
        let anchor = ReadAnchor::identified(number, hash);
        // The anchor's full list, unfiltered: the write identities a report's
        // read-set overlap is measured against, and the read-only census.
        let full_seed = fetch_bal_seed(&generation_provider, hash, &BalPlan::default())
            .await
            .map_err(|skip| format!("list at {number}: {skip:?}"))?;
        let seed_writes: BTreeSet<Key> = full_seed
            .writes
            .iter()
            .map(|&(address, slot, _)| (address, slot))
            .collect();
        let seed_reads = full_seed.reads.len();

        let mut anchor_keys: BTreeSet<Key> = BTreeSet::new();
        for row in &anchor_selection.rows {
            anchor_keys.extend(row.keys.iter().copied());
        }
        let union_relevance: Vec<Address> = {
            let mut seen = BTreeSet::new();
            let mut addresses = Vec::new();
            for row in &anchor_selection.rows {
                for address in &row.relevance.addresses {
                    if seen.insert(*address) {
                        addresses.push(*address);
                    }
                }
            }
            addresses
        };
        anchor_keys.extend(
            anchor_selection
                .rows
                .iter()
                .flat_map(|row| row.keys.iter().copied()),
        );
        let cold_keys = sorted_keys(anchor_keys.iter().copied());
        let cold_started = Instant::now();
        let cold = cold_values(&cold_provider, number, &cold_keys).await?;
        let cold_wall = cold_started.elapsed();
        println!(
            "\nanchor {number} (fan-out {} rows, {seed_reads} read-only list identities, {} writes): \
             {} selected rows, {} cold reads over {} identities in {cold_wall:.2?}",
            anchor_selection.fanout,
            seed_writes.len(),
            anchor_selection.rows.len(),
            cold_keys.len(),
            cold_keys.len()
        );

        for row in &anchor_selection.rows {
            let leg = replay_leg(
                &generation_provider,
                anchor,
                parent_hash,
                &row.relevance.addresses,
                &row.keys,
                &cold,
                &seed_writes,
                format!("row {}", row.candidate),
                row.candidate.clone(),
                plant,
            )
            .await?;
            println!(
                "  {}: relevance {}, keys {}, served {}, dynamic {}, zeros {}, \
                 seed-write overlap {}/{} (applied overlap {}), applied {} skipped {}, \
                 mismatches {}, reads {:.2?}",
                leg.label,
                leg.relevance,
                leg.keys,
                leg.served,
                leg.dynamic,
                leg.zeros,
                leg.overlap_seed,
                leg.seeds_applied,
                leg.overlap_applied,
                leg.seeds_applied,
                leg.seeds_skipped,
                leg.mismatches.len(),
                leg.read_wall
            );
            legs.push(leg);
        }

        if anchor_selection.rows.len() > 1 {
            let leg = replay_leg(
                &generation_provider,
                anchor,
                parent_hash,
                &union_relevance,
                &cold_keys,
                &cold,
                &seed_writes,
                "union".to_string(),
                format!("union@{}", anchor_selection.rows.len()),
                plant,
            )
            .await?;
            println!(
                "  {} (grouped rows, relevance union {}): keys {}, served {}, dynamic {}, zeros {}, \
                 seed-write overlap {}/{} (applied overlap {}), applied {} skipped {}, \
                 mismatches {}, reads {:.2?}",
                leg.label,
                leg.relevance,
                leg.keys,
                leg.served,
                leg.dynamic,
                leg.zeros,
                leg.overlap_seed,
                leg.seeds_applied,
                leg.overlap_applied,
                leg.seeds_applied,
                leg.seeds_skipped,
                leg.mismatches.len(),
                leg.read_wall
            );
            legs.push(leg);
        }

        if parity.is_none() {
            parity = Some(
                seed_parity(
                    &generation_provider,
                    hash,
                    &anchor_selection.rows[0].relevance.addresses,
                )
                .await?,
            );
        }

        if !expiry_done {
            if let Some(row) = anchor_selection.rows.first() {
                let leg = replay_expiry_leg(
                    &generation_provider,
                    &cold_provider,
                    anchor,
                    parent_hash,
                    &row.relevance.addresses,
                    &row.keys,
                    &cold,
                    &seed_writes,
                    row.candidate.clone(),
                    plant,
                )
                .await?;
                println!(
                    "  {} (two-generation IdleBlocks(0)): keys {} (row reads + {EXPIRY_KEYS} expired \
                     cap), served {}, dynamic {}, expired {} (none served), mismatches {}, reads {:.2?}",
                    leg.label,
                    leg.keys,
                    leg.served,
                    leg.dynamic,
                    leg.expired,
                    leg.mismatches.len(),
                    leg.read_wall
                );
                legs.push(leg);
                expiry_done = true;
            }
        }
    }

    // ── The report ────────────────────────────────────────────────────────
    let compared: usize = legs.iter().map(|leg| leg.keys).sum();
    let served: usize = legs.iter().map(|leg| leg.served).sum();
    let dynamic: usize = legs.iter().map(|leg| leg.dynamic).sum();
    let zeros: usize = legs.iter().map(|leg| leg.zeros).sum();
    let expired: usize = legs.iter().map(|leg| leg.expired).sum();
    let overlap_seed: usize = legs.iter().map(|leg| leg.overlap_seed).sum();
    let overlap_applied: usize = legs.iter().map(|leg| leg.overlap_applied).sum();
    let mismatches: Vec<&Mismatch> = legs.iter().flat_map(|leg| leg.mismatches.iter()).collect();

    println!();
    println!("seed parity (raw and official JSON decodes versus the production negotiation):");
    match &parity {
        Some(parity) => {
            println!(
                "  raw {} writes / {} reads, json {} / {}, negotiated {} / {}, \
                 representation {}, raw==json==negotiated {}, raw==negotiated {}",
                parity.raw_writes,
                parity.raw_reads,
                parity.json_writes,
                parity.json_reads,
                parity.negotiated_writes,
                parity.negotiated_reads,
                parity.representation,
                parity.agree,
                parity.raw_equals_negotiated
            );
        }
        None => println!("  no selected anchor resolved a parity leg"),
    }

    println!();
    println!("totals over {} legs:", legs.len());
    println!(
        "  reads {compared}: served {served} from the generation's snapshot, {dynamic} through the \
         dynamic fallback ({expired} of them expired entries), {zeros} naturally zero, \
         seed-write overlap {overlap_seed} (applied-write overlap {overlap_applied})"
    );
    println!(
        "  plant: {}",
        if plant {
            "ARMED (negative control)"
        } else {
            "not armed"
        }
    );

    if mismatches.is_empty() {
        println!("  mismatches: 0 — every generation answer agreed with its cold read");
    } else {
        for mismatch in mismatches.iter().take(20) {
            println!(
                "  MISMATCH {} at {} {}/{}: generation {} ({}) != cold {:?}",
                mismatch.candidate,
                legs.iter()
                    .find(|leg| leg.candidate == mismatch.candidate)
                    .map_or(0, |leg| leg.block),
                mismatch.key.0,
                mismatch.key.1,
                mismatch.generation,
                mismatch.origin.label(),
                mismatch.cold
            );
        }
    }

    if plant {
        if mismatches.is_empty() {
            return Err(
                "the one-wei plant was armed but the comparison reported no mismatch — the \
                 comparator is blind"
                    .to_string(),
            );
        }
        println!(
            "  negative control: {} mismatches caught — the comparator reads the generation's \
             value, not a copy of the cold leg",
            mismatches.len()
        );
        return Err(format!(
            "negative control armed: {} planted one-wei differences caught (expected non-zero exit)",
            mismatches.len()
        ));
    }

    if let Some(parity) = &parity {
        if !parity.agree {
            return Err(
                "the raw, JSON and negotiated seeds disagree at a selected anchor".to_string(),
            );
        }
    }
    if !mismatches.is_empty() {
        return Err(format!(
            "{} read-value mismatches against the cold leg",
            mismatches.len()
        ));
    }

    println!();
    println!(
        "verdict: diverse REAL-CANDIDATE STORAGE-READ parity over {} rows at {} anchors. This is \
         NOT executable path/verdict parity — trace rows carry no calldata, amounts or deployment \
         state; exact executable parity remains the scripted value-sensitive matrix and \
         docs/architecture/bal-matched-parity-result.md. Total wall {:.2?}",
        selection.rows(),
        selection.anchors.len(),
        total_started.elapsed()
    );
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("bal_real_read_oracle_probe: {error}");
        std::process::exit(1);
    }
}
