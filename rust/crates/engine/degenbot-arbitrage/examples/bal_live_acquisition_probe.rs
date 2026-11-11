//! Live reth BAL acquisition probe: what it costs to acquire ONE fixed
//! historical block's EIP-7928 block access list from a live node in BOTH
//! official representations, and what the per-fan-out storage packets cost at
//! that same anchor.
//!
//! # What is measured, at one fixed historical hash
//!
//! Every BAL request is anchored on the block's exact HASH (the acquisition
//! contract's own reorg fence); the per-fan-out storage reads are number-tagged
//! at the anchor's own number, the shape the production prefetch dispatches.
//!
//! - **Raw acquisition** — `debug_getRawBlockAccessList` fetch, then the
//!   production raw decode ([`decode_bal_seed`]) over its bytes: the two walls
//!   separately, the payload size, and the decoded seed's writes/reads.
//! - **JSON acquisition** — `eth_getBlockAccessList` fetch, then the production
//!   JSON decode ([`decode_bal_seed_json`]): the same figures for the official
//!   JSON representation.
//! - **Seed parity** — the two representations' decode seeds must be IDENTICAL,
//!   entry for entry, and the production negotiated acquisition
//!   ([`fetch_bal_seed`], raw-first) must produce the same seed again. A
//!   disagreement is a hard failure.
//! - **Settled-value parity** — every decoded final write is compared against an
//!   independent cold `eth_getStorageAt` batch at the anchor's number, through a
//!   second provider instance.
//! - **Per-fan-out packet prices** at the same anchor: the one header/identity
//!   read every fan-out pays, the dynamic serial read (the delta lane's fallback
//!   shape on this corpus), and the legacy prefetch batch over the EXISTING
//!   corpus's own derived identities at every matrix width
//!   ([`scaling::prefetch_identity_set`]).
//!
//! # What is deliberately NOT measured
//!
//! No live fan-out comparison. The matched corpus is value-sensitive over
//! SCRIPTED words at synthetic identities, and its covered/unknown controls need
//! a generation list carrying the read key: none of that exists at a real
//! historical hash, and transplanting it there would mean inventing state. A
//! live lane pair would therefore measure disjoint-coverage artifacts, not the
//! lanes. The legacy-versus-delta comparison stays the MODELED one the matched
//! matrix records (`logs/bal_matched_parity_matrix.md`), priced at the live
//! packet costs below in `docs/architecture/bal-matched-parity-result.md` —
//! never a paired live fan-out speedup.
//!
//! # Usage
//!
//! ```sh
//! just bal-live-acquisition-probe
//! BAL_ACQ_ANCHOR=26375043 BAL_ACQ_REPS=7 just bal-live-acquisition-probe
//! ```

#![expect(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a live probe prints its report and fails loudly on a broken fixture"
)]

#[path = "shared/dispatch_probe.rs"]
#[expect(dead_code)]
mod probe;
#[path = "shared/dispatch_scaling.rs"]
#[expect(dead_code)]
mod scaling;

use std::time::Instant;

use alloy::primitives::{Address, B256, U256};

use degenbot_rpc::provider::{AlloyProvider, BlockAccessListResponse};
use degenbot_simulation::sim::evm2::bal_prefetch::{
    decode_bal_seed, decode_bal_seed_json, fetch_bal_seed, BalPlan, BalSeed,
};

use scaling::{median, prefetch_identity_set, read_identity};

/// The default anchor: the committed real-coverage capture's middle block — a
/// historical mainnet block with real block-access-list content.
const DEFAULT_ANCHOR: u64 = 26_375_043;

/// Interleaved repetitions per acquisition and per packet price.
const DEFAULT_REPS: usize = 7;

/// The corpus widths whose derived prefetch identities price the batch.
const DEFAULT_COUNTS: [usize; 7] = [1, 4, 16, 50, 100, 500, 1000];

fn main() {
    // The existing corpus's derived prefetch identities, at every matrix width,
    // from their own scripted chains — setup, before any live runtime exists.
    let derived: Vec<(usize, Vec<(Address, U256)>)> = env_counts()
        .into_iter()
        .map(|count| (count, prefetch_identity_set(count)))
        .collect();
    let runtime = crate::probe::runtime();
    if let Err(failure) = runtime.block_on(run(derived)) {
        println!("\nprobe FAILED: {failure}");
        std::process::exit(1);
    }
}

/// The node: the chain-1 endpoint env family, or the conftest name. Neither
/// means no declared endpoint — fail loud.
fn rpc_url() -> Result<String, String> {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .map_err(|_| {
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) \
             to an archive mainnet endpoint"
                .to_string()
        })
}

fn env_usize(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(fallback)
}

fn env_u64(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(fallback)
}

/// Every corpus width the batch prices.
fn env_counts() -> Vec<usize> {
    match std::env::var("BAL_ACQ_CORPUS_COUNTS") {
        Ok(raw) => raw
            .split(',')
            .map(|part| {
                part.trim()
                    .parse::<usize>()
                    .expect("a comma-separated corpus width list")
            })
            .collect(),
        Err(_) => DEFAULT_COUNTS.to_vec(),
    }
}

/// Wall microseconds of one measured step.
fn us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// The slowest wall of a series.
fn slowest(series: &[u64]) -> u64 {
    series.iter().copied().max().unwrap_or(0)
}

/// One packet price row: what one request shape cost at the anchor.
struct PacketPrice {
    /// The request shape's report name.
    label: &'static str,
    /// The method(s) on the wire.
    wire: &'static str,
    /// Members the packet carried.
    members: usize,
    /// Every repetition's wall.
    walls: Vec<u64>,
}

/// One representation's acquisition of one repetition: the fetch and decode
/// walls apart, the payload size, and the decoded seed.
struct RepAcquisition {
    fetch_us: u64,
    decode_us: u64,
    payload_bytes: usize,
    seed: BalSeed,
}

/// One representation's figures across every repetition: the walls as series,
/// the payload size of the last answer, and the seed every repetition agreed on.
#[derive(Default)]
struct Representation {
    fetch_us: Vec<u64>,
    decode_us: Vec<u64>,
    payload_bytes: usize,
    seed: Option<BalSeed>,
}

impl Representation {
    /// Fold one repetition's acquisition into the series.
    fn push(&mut self, repetition: RepAcquisition) {
        self.fetch_us.push(repetition.fetch_us);
        self.decode_us.push(repetition.decode_us);
        self.payload_bytes = repetition.payload_bytes;
        self.seed = Some(repetition.seed);
    }
}

/// The whole acquisition measurement: both official representations' figures
/// beside the production negotiated path's.
#[derive(Default)]
struct Acquisition {
    raw: Representation,
    json: Representation,
    prepared_us: Vec<u64>,
    seed_prepared: Option<BalSeed>,
}

/// The whole measurement, or the one failure that stopped it.
async fn run(derived: Vec<(usize, Vec<(Address, U256)>)>) -> Result<(), String> {
    let url = rpc_url()?;
    let anchor_block = env_u64("BAL_ACQ_ANCHOR", DEFAULT_ANCHOR);
    let reps = env_usize("BAL_ACQ_REPS", DEFAULT_REPS);
    let counts: Vec<usize> = derived.iter().map(|(count, _)| *count).collect();
    let provider = AlloyProvider::new(&url, 3)
        .await
        .map_err(|error| format!("connect {url}: {error}"))?;
    let cold = AlloyProvider::new(&url, 3)
        .await
        .map_err(|error| format!("connect {url}: {error}"))?;
    let chain_id = provider
        .get_chain_id()
        .await
        .map_err(|error| format!("eth_chainId: {error}"))?;
    let lineage = provider
        .get_block_lineage_at(anchor_block)
        .await
        .map_err(|error| format!("eth_getBlockByNumber({anchor_block}): {error}"))?
        .ok_or_else(|| format!("no canonical block at {anchor_block}"))?;
    let (number, hash) = (lineage.number, lineage.hash);
    println!("# Live reth BAL acquisition at one fixed historical anchor\n");
    println!("node: {url} (chain id {chain_id})\n");
    println!(
        "anchor: block {number}, hash {hash}, parent {}. Repetitions: {reps} \
         (interleaved). Corpus widths: {counts:?}.\n",
        lineage.parent_hash
    );
    let plan = BalPlan::default();
    let mut failures = Vec::new();
    let acquisition = acquire(&provider, hash, &plan, reps, &mut failures).await?;
    // The negotiated representation this node resolves to (one extra request).
    let negotiated = provider
        .get_block_access_list(hash)
        .await
        .map_err(|error| format!("acquisition negotiation: {error}"))?;
    let negotiated_representation = match negotiated {
        BlockAccessListResponse::Raw(_) => "raw",
        BlockAccessListResponse::Json(_) => "json",
    };
    let seed_raw = acquisition
        .raw
        .seed
        .as_ref()
        .expect("at least one repetition ran");
    let seed_json = acquisition
        .json
        .seed
        .as_ref()
        .expect("at least one repetition ran");
    let seed_prepared = acquisition
        .seed_prepared
        .as_ref()
        .expect("at least one repetition ran");
    check_seed_parity(seed_raw, seed_json, seed_prepared, &mut failures);
    let (members, verify_us) = verify_settled(&cold, number, seed_raw, &mut failures).await?;
    // Per-fan-out packet prices at the same anchor.
    let mut packets = vec![
        price_header_read(&provider, number, reps).await?,
        price_dynamic_fresh(&provider, number, reps).await?,
        price_dynamic_repeat(&provider, number, reps).await?,
        PacketPrice {
            label: "real-write batch (the cold verification's own packet)",
            wire: "eth_getStorageAt batch",
            members,
            walls: vec![verify_us],
        },
    ];
    packets.extend(price_prefetch_batches(&cold, number, reps, &derived, &mut failures).await?);
    // The report.
    report_acquisition(&acquisition.raw, &acquisition.json, seed_raw, seed_json);
    report_parity(
        seed_raw,
        seed_json,
        seed_prepared,
        negotiated_representation,
        verify_us,
    );
    report_prepared(&acquisition.prepared_us, reps);
    report_packets(&packets);
    report_gates(&failures)
}

/// ONE repetition's raw acquisition: the `debug_getRawBlockAccessList` fetch,
/// then the production raw decode, the two walls apart.
async fn acquire_raw(
    provider: &AlloyProvider,
    hash: B256,
    plan: &BalPlan,
) -> Result<RepAcquisition, String> {
    let started = Instant::now();
    let raw = provider
        .get_raw_block_access_list(hash)
        .await
        .map_err(|error| format!("debug_getRawBlockAccessList({hash}): {error}"))?;
    let fetch_us = us(started);
    let started = Instant::now();
    let seed = decode_bal_seed(&raw, plan).map_err(|error| format!("raw decode: {error}"))?;
    let decode_us = us(started);
    Ok(RepAcquisition {
        fetch_us,
        decode_us,
        payload_bytes: raw.len(),
        seed,
    })
}

/// ONE repetition's JSON acquisition: the `eth_getBlockAccessList` fetch, then
/// the production JSON decode, the two walls apart.
async fn acquire_json(
    provider: &AlloyProvider,
    hash: B256,
    plan: &BalPlan,
) -> Result<RepAcquisition, String> {
    let started = Instant::now();
    let json = provider
        .get_block_access_list_json(hash)
        .await
        .map_err(|error| format!("eth_getBlockAccessList({hash}): {error}"))?;
    let fetch_us = us(started);
    let started = Instant::now();
    let seed =
        decode_bal_seed_json(&json, plan).map_err(|error| format!("json decode: {error}"))?;
    let decode_us = us(started);
    Ok(RepAcquisition {
        fetch_us,
        decode_us,
        payload_bytes: json.to_string().len(),
        seed,
    })
}

/// The whole acquisition: raw and JSON interleaved per repetition with the
/// production negotiated acquisition, decode walls apart from fetch walls, and
/// every repetition's seeds pinned against drift.
async fn acquire(
    provider: &AlloyProvider,
    hash: B256,
    plan: &BalPlan,
    reps: usize,
    failures: &mut Vec<String>,
) -> Result<Acquisition, String> {
    let mut acquisition = Acquisition::default();
    for _rep in 0..reps {
        let raw = acquire_raw(provider, hash, plan).await?;
        let json = acquire_json(provider, hash, plan).await?;
        let started = Instant::now();
        let prepared = fetch_bal_seed(provider, hash, plan)
            .await
            .map_err(|error| format!("production acquisition: {error}"))?;
        let prepared_us = us(started);
        // Every repetition must decode to the same seed on every path, or the
        // node's answer is not a fact about one block.
        if let Some(reference) = &acquisition.raw.seed {
            if *reference != raw.seed {
                failures.push("the raw decode seed drifted between repetitions".to_string());
            }
        }
        if let Some(reference) = &acquisition.json.seed {
            if *reference != json.seed {
                failures.push("the JSON decode seed drifted between repetitions".to_string());
            }
        }
        if let Some(reference) = &acquisition.seed_prepared {
            if *reference != prepared {
                failures.push(
                    "the production acquisition seed drifted between repetitions".to_string(),
                );
            }
        }
        acquisition.raw.push(raw);
        acquisition.json.push(json);
        acquisition.prepared_us.push(prepared_us);
        acquisition.seed_prepared = Some(prepared);
    }
    Ok(acquisition)
}

/// Seed parity: the two representations and the production path must fold into
/// ONE seed, entry for entry, and the anchor's list must carry content.
fn check_seed_parity(
    seed_raw: &BalSeed,
    seed_json: &BalSeed,
    seed_prepared: &BalSeed,
    failures: &mut Vec<String>,
) {
    if seed_raw != seed_json {
        failures.push(format!(
            "the raw and JSON decode seeds disagree (raw {} writes / {} reads, json {} / {})",
            seed_raw.writes.len(),
            seed_raw.reads.len(),
            seed_json.writes.len(),
            seed_json.reads.len()
        ));
    }
    if seed_raw != seed_prepared {
        failures
            .push("the production acquisition's seed disagrees with the raw decode".to_string());
    }
    if seed_raw.writes.is_empty() && seed_raw.reads.is_empty() {
        failures.push(
            "the anchor's list carries no content — a different anchor is needed".to_string(),
        );
    }
}

/// Settled-value parity: every decoded final write against an independent cold
/// read at the anchor's number, in one batch (also a real-identity batch price
/// at the anchor's own width). Returns the batch's member count and its wall.
async fn verify_settled(
    cold: &AlloyProvider,
    number: u64,
    seed_raw: &BalSeed,
    failures: &mut Vec<String>,
) -> Result<(usize, u64), String> {
    let keys: Vec<(Address, U256)> = seed_raw
        .writes
        .iter()
        .map(|(address, slot, _)| (*address, *slot))
        .collect();
    let started = Instant::now();
    let cold_words = cold
        .get_storage_batch_results_at(&keys, number)
        .await
        .map_err(|error| format!("cold verification batch at {number}: {error}"))?;
    let verify_us = us(started);
    for ((address, slot, value), cold_word) in seed_raw.writes.iter().zip(cold_words) {
        let cold_word =
            cold_word.map_err(|error| format!("cold read {address}/{slot}: {error}"))?;
        if cold_word != *value {
            failures.push(format!(
                "block {number}: decoded write {address}/{slot} = {value}, cold read = {cold_word}"
            ));
        }
    }
    Ok((keys.len(), verify_us))
}

/// The one header/identity read every fan-out pays.
async fn price_header_read(
    provider: &AlloyProvider,
    number: u64,
    reps: usize,
) -> Result<PacketPrice, String> {
    let mut walls = Vec::new();
    for _ in 0..reps {
        let started = Instant::now();
        let _ = provider
            .get_block_lineage_at(number)
            .await
            .map_err(|error| format!("header read at {number}: {error}"))?;
        walls.push(us(started));
    }
    Ok(PacketPrice {
        label: "header read (every fan-out)",
        wire: "eth_getBlockByNumber",
        members: 1,
        walls,
    })
}

/// A dynamic fallback read of a FRESH identity per repetition: the production
/// provider stack's default response cache serves only byte-identical typed
/// reads, so distinct keys are one real round trip each — the packet price.
async fn price_dynamic_fresh(
    provider: &AlloyProvider,
    number: u64,
    reps: usize,
) -> Result<PacketPrice, String> {
    let (read_address, read_slot) = read_identity();
    let mut walls = Vec::new();
    for rep in 0..reps {
        let offset = U256::from(u64::try_from(rep).unwrap_or(u64::MAX) + 1);
        let started = Instant::now();
        let _ = provider
            .get_storage_at(&read_address, read_slot + offset, Some(number))
            .await
            .map_err(|error| format!("dynamic serial read at {number}: {error}"))?;
        walls.push(us(started));
    }
    Ok(PacketPrice {
        label: "dynamic serial read (distinct identities — one real round trip each)",
        wire: "eth_getStorageAt",
        members: 1,
        walls,
    })
}

/// The corpus's own read identity, repeated: one cold round trip, then the
/// response cache answers byte-identical repeats in-process.
async fn price_dynamic_repeat(
    provider: &AlloyProvider,
    number: u64,
    reps: usize,
) -> Result<PacketPrice, String> {
    let (read_address, read_slot) = read_identity();
    let mut walls = Vec::new();
    for _ in 0..reps {
        let started = Instant::now();
        let _ = provider
            .get_storage_at(&read_address, read_slot, Some(number))
            .await
            .map_err(|error| format!("dynamic serial read (repeat) at {number}: {error}"))?;
        walls.push(us(started));
    }
    Ok(PacketPrice {
        label: "dynamic serial read (one identity repeated — provider response cache)",
        wire: "eth_getStorageAt",
        members: 1,
        walls,
    })
}

/// The legacy prefetch batch over the corpus's own derived identities, at every
/// matrix width.
async fn price_prefetch_batches(
    cold: &AlloyProvider,
    number: u64,
    reps: usize,
    derived: &[(usize, Vec<(Address, U256)>)],
    failures: &mut Vec<String>,
) -> Result<Vec<PacketPrice>, String> {
    let mut packets = Vec::new();
    for (count, identities) in derived {
        let count = *count;
        let mut walls = Vec::new();
        for _ in 0..reps {
            let started = Instant::now();
            let answers = cold
                .get_storage_batch_results_at(identities.as_slice(), number)
                .await
                .map_err(|error| format!("prefetch batch (count {count}) at {number}: {error}"))?;
            walls.push(us(started));
            if answers.len() != identities.len() {
                failures.push(format!(
                    "prefetch batch (count {count}) answered {} of {} members",
                    answers.len(),
                    identities.len()
                ));
            }
        }
        packets.push(PacketPrice {
            label: "legacy prefetch batch (the existing corpus's derived identities)",
            wire: "eth_getStorageAt batch",
            members: identities.len(),
            walls,
        });
    }
    Ok(packets)
}

/// The acquisition table: both representations' payload sizes beside their
/// fetch and decode walls and the seeds they decoded to.
fn report_acquisition(
    raw: &Representation,
    json: &Representation,
    seed_raw: &BalSeed,
    seed_json: &BalSeed,
) {
    let raw_bytes = raw.payload_bytes;
    let json_bytes = json.payload_bytes;
    println!("## Acquisition: raw versus JSON\n");
    println!(
        "| representation | method | payload bytes | fetch median us | fetch max us | decode median us | decode max us | writes | reads |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    println!(
        "| raw | debug_getRawBlockAccessList | {raw_bytes} | {} | {} | {} | {} | {} | {} |",
        median(&raw.fetch_us),
        slowest(&raw.fetch_us),
        median(&raw.decode_us),
        slowest(&raw.decode_us),
        seed_raw.writes.len(),
        seed_raw.reads.len()
    );
    println!(
        "| json | eth_getBlockAccessList | {json_bytes} | {} | {} | {} | {} | {} | {} |",
        median(&json.fetch_us),
        slowest(&json.fetch_us),
        median(&json.decode_us),
        slowest(&json.decode_us),
        seed_json.writes.len(),
        seed_json.reads.len()
    );
    println!();
}

/// The parity section: what the three decode paths agreed on, and the
/// settled-value parity's cold batch price.
fn report_parity(
    seed_raw: &BalSeed,
    seed_json: &BalSeed,
    seed_prepared: &BalSeed,
    negotiated_representation: &str,
    verify_us: u64,
) {
    println!("## Decode seeds and parity\n");
    println!(
        "raw seed == JSON seed == production negotiated seed, entry for entry: {}. The \
         production acquisition (raw-first negotiation) resolved to the **{negotiated_representation}** \
         representation on this node. Settled-value parity: {} decoded final writes compared \
         against independent cold reads at the anchor's number, {} us one batch.\n",
        if seed_raw == seed_json && seed_raw == seed_prepared {
            "IDENTICAL"
        } else {
            "MISMATCH"
        },
        seed_raw.writes.len(),
        verify_us
    );
}

/// The fixed per-head cost: what the production negotiated acquisition paid.
fn report_prepared(prepared_us: &[u64], reps: usize) {
    println!("## Fixed per-head BAL cost (production path)\n");
    println!(
        "prepare (list fetch + full decode) through the negotiated acquisition: median {} us, \
         max {} us over {reps} repetitions.\n",
        median(prepared_us),
        slowest(prepared_us)
    );
}

/// The packet table at the anchor, and what its rows do and do not price.
fn report_packets(packets: &[PacketPrice]) {
    println!("## Per-fan-out packet prices at the same anchor\n");
    println!(
        "| packet | wire | members | median us | max us |\n\
         | --- | --- | --- | --- | --- |"
    );
    for packet in packets {
        println!(
            "| {} | {} | {} | {} | {} |",
            packet.label,
            packet.wire,
            packet.members,
            median(&packet.walls),
            slowest(&packet.walls)
        );
    }
    println!();
    println!(
        "The corpus's derived identities have no real state at this historical anchor \
         (the node answers zero for every one), so the batch prices a member-count-shaped \
         packet of state misses; the real-write batch row prices real hits at the anchor's \
         own width. The repeated-read row is the production provider stack's own answer: \
         `AlloyProvider` wraps its typed calls in alloy's default response cache, so a \
         byte-identical repeat of one storage read costs no round trip while every fresh \
         identity pays the full serial price — the same asymmetry production sees on the \
         dynamic fallback. What is NOT measured here: any live fan-out comparison — the \
         matched corpus cannot be transplanted to a real hash without inventing state, so \
         the legacy-versus-delta comparison stays the modeled one in \
         `docs/architecture/bal-matched-parity-result.md`, never a paired live speedup.\n"
    );
}

/// The gates: the probe passes only when nothing failed.
fn report_gates(failures: &[String]) -> Result<(), String> {
    if failures.is_empty() {
        println!(
            "gates: both representations served the anchor's list, the three decode seeds \
             are identical entry for entry, every decoded final write matches its cold read, \
             and no seed drifted between repetitions."
        );
        Ok(())
    } else {
        println!("gates FAILED:");
        for failure in failures {
            println!("- {failure}");
        }
        Err(format!("{} acquisition gate failures", failures.len()))
    }
}
