//! The black-box V4 leg quota — the parity census against the chain's own
//! quoter (ADR-090's acceptance evidence).
//!
//! One question: does [`V4BlackBoxQuoter`](degenbot_simulation::V4BlackBoxQuoter)
//! answer a hooked leg exactly as the NODE answers the same deployed quoter
//! call, at the same anchor?
//!
//! Every census pool is a REAL mainnet hooked V4 pool. For each pool and
//! direction the harness asks both sides the same question — a spread of
//! amounts scaled to the input currency's decimals — and compares:
//!
//! - **evm2 side:** the primitive executes the deployed `Quoter`'s
//!   `quoteExactInputSingle` frame over an anchor-pinned DB
//!   (`CacheDB<Db<Evm2CountingFrameDb<BlockingAsyncDb<AlloyAsyncDb>>>>`), which
//!   runs the real `PoolManager`, the real pool, and the real hook contracts.
//! - **node side:** `eth_call` of the identical calldata at the identical
//!   block tag, from the identical (null) caller.
//!
//! Parity is asserted on the answer (`amountOut`, to the wei) and on the
//! verdict (both sides price the leg, or neither does — a hook the chain cannot
//! price is a revert on both sides, not a mismatch). The census reports both
//! counts, because the refused share is a finding in its own right: some hook
//! families revert inside `beforeSwap` for one swap direction outright, and the
//! deployed quoter inherits that refusal (so does evm2, identically).
//!
//! # The census
//!
//! `tests/fixtures/v4_hook_census.tsv` carries the table and its generator
//! query: the three hooks with the most mainnet V4 pools (5,737 / 5,114 / 2,240
//! pools) plus a seeded draw of 20 hooks from the 4,387 amount-modifying hooks
//! (`hooks & 0xCC != 0`, ADR-037's mask) that hold a recorded liquidity
//! position — out of the 10,628 such hooks in the registration DB, the
//! sub-population with something to price. Candidates are ordered by recorded
//! position count, 8 per top hook and 2 per long-tail hook; a pool whose
//! in-range liquidity is zero at the anchor is reported as skipped, never
//! silently dropped, since nothing can price it. Each row's pool id is
//! re-derived from its key at parse time, so a stale or mistyped row fails
//! loudly instead of quoting a different pool.
//!
//! # Running it
//!
//! ```sh
//! cargo test -p degenbot-simulation --test evm2_v4_black_box_parity \
//!   parity_census_black_box_v4_legs -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `DEGENBOT_RPC_HTTP_CHAINID_1` (or `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`)
//! selects the node; the anchor needs archive state.

#![expect(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines
)]

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use alloy::primitives::{Address, Bytes, B256, U256};

use ::evm2::evm::{CacheDB, Database, Db};

use degenbot_pools::{v4_pool_id, V4PoolKey};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_simulation::sim::evm2::frame_replay::{Evm2CountingFrameDb, Evm2ScratchEvm};
use degenbot_simulation::sim::evm2::v4_black_box::{
    decode_quote_return, leg_state_slots, quote_calldata, V4BlackBoxQuoter, V4Leg, V4QuoteError,
    V4QuoterConfig, V4_POOL_MANAGER, V4_QUOTER,
};
use degenbot_simulation::sim::frame_replay::{FrameRpcCounter, ScratchBlock};

// ─────────────────────────────────────────────────────────────────────────
// The anchor + the census
// ──────────────────────────────────────────────────────────────────────────

/// The archived block every read pins: past the registration DB's last V4
/// liquidity update, so every census pool is live, and deep enough in the
/// archive window to be cheap to re-read.
const PIN: u64 = 26_320_000;

/// The seed the census fixture's long-tail draw was generated with (the
/// splitmix64 stream's start over the address-ordered hook list).
const SAMPLE_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// `hook  currency0  currency1  fee  tick_spacing  pool_id` — one line per
/// candidate pool, grouped by hook in censary order.
const CENSUS: &str = include_str!("fixtures/v4_hook_census.tsv");

/// `decimal()` — the only token-metadata read the census needs (to scale each
/// amount to the input currency).
const DECIMALS_CALLDATA: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];

/// One census row.
struct CensusRow {
    hook: Address,
    key: V4PoolKey,
}

/// Parse the census, re-deriving each pool id from its key (the row's
/// integrity check).
fn census() -> Vec<CensusRow> {
    let mut rows = Vec::new();
    for line in CENSUS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 6, "census row shape: {line:?}");
        let hook = Address::from_str(fields[0]).expect("hook address");
        let key = V4PoolKey {
            currency0: Address::from_str(fields[1]).expect("currency0"),
            currency1: Address::from_str(fields[2]).expect("currency1"),
            fee: fields[3].parse().expect("fee"),
            tick_spacing: fields[4].parse().expect("tick spacing"),
            hooks: hook,
        };
        let pinned = B256::from_str(fields[5]).expect("pool id");
        assert_eq!(
            v4_pool_id(&key),
            pinned,
            "census row's key does not hash to its recorded pool id: {line:?}"
        );
        rows.push(CensusRow { hook, key });
    }
    rows
}

/// The amount spread for a currency of `decimals`: two orders of magnitude
/// below a whole unit, one below, and a whole unit.
fn amount_ladder(decimals: u32) -> Vec<U256> {
    let d = decimals.min(30);
    [
        d.saturating_sub(2).max(1),
        d.saturating_sub(1).max(1),
        d.max(1),
    ]
    .into_iter()
    .map(|exp| U256::from(10u64).pow(U256::from(exp)))
    .collect()
}

fn rpc_url() -> String {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .expect(
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) \
             to an archive mainnet endpoint",
        )
}

/// The census stack's ext: the cold path counts every forwarded read.
type ParityExt<'a> = CacheDB<Db<Evm2CountingFrameDb<BlockingAsyncDb<'a>>>>;

/// What one side answered for one quote point.
#[derive(Clone, Debug)]
enum Side {
    /// Answered with this amount out.
    Amount(U256),
    /// Could not price the leg (revert or halt) — only its class for reporting.
    Refused(String),
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC parity census over archive mainnet state; run with --ignored"]
async fn parity_census_black_box_v4_legs() {
    let started = Instant::now();
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    assert_eq!(
        provider.get_chain_id().await.unwrap(),
        1,
        "the census pools are mainnet fixtures"
    );
    let block = provider
        .get_block(PIN)
        .await
        .unwrap()
        .expect("the anchor block is archived");
    let scratch_block = ScratchBlock {
        number: PIN,
        timestamp: block.header.timestamp,
        base_fee_next: u128::from(block.header.base_fee_per_gas.unwrap_or(0)),
    };

    let rows = census();
    let hooks: Vec<Address> = {
        let mut seen: Vec<Address> = Vec::new();
        for row in &rows {
            if !seen.contains(&row.hook) {
                seen.push(row.hook);
            }
        }
        seen
    };
    println!("=== black-box V4 leg parity census ===");
    println!(
        "anchor    : {PIN} (timestamp {}, base fee {} wei/gas)",
        scratch_block.timestamp, scratch_block.base_fee_next
    );
    println!(
        "census    : {} candidate pools across {} hooks (3 top by pool count + 20 seeded long-tail, seed 0x{SAMPLE_SEED:016x})",
        rows.len(),
        hooks.len()
    );
    println!(
        "sides     : evm2 (deployed Quoter 0x{}) vs the node's eth_call of the same calldata",
        alloy::hex::encode(V4_QUOTER)
    );

    let counter = Arc::new(FrameRpcCounter::default());
    let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(&provider, PIN))
        .expect("a multi-threaded runtime is ambient");
    let cold_path = Evm2CountingFrameDb::new(blocking, Arc::clone(&counter));
    let ext: ParityExt<'_> = CacheDB::new(Db::new(cold_path));
    let mut scratch = Evm2ScratchEvm::with_counter(ext, scratch_block, Arc::clone(&counter));

    let config = V4QuoterConfig::default();
    let mut quoter = V4BlackBoxQuoter::new(config);

    let mut decimals: HashMap<Address, u32> = HashMap::new();
    let mut priced_pools = 0usize;
    let mut empty_pools = 0usize;
    let mut points = 0usize;
    let mut answered = 0usize;
    let mut both_refused = 0usize;
    let mut mismatches = 0usize;
    let mut max_diff = U256::ZERO;
    let mut halts = 0usize;
    let mut priced_hooks: Vec<Address> = Vec::new();
    let mut grounded: Vec<(V4Leg, U256)> = Vec::new();

    println!();
    println!(
        "{:<44} {:>6} {:>7} {:>8} {:>8} {:>6}",
        "pool (id prefix)", "legs", "points", "answered", "refused", "mism"
    );

    for row in &rows {
        // The computable PoolManager word: in-range liquidity. A pool with no
        // in-range liquidity cannot price anything (both sides agree on zero or
        // revert), so it is not part of the amount-parity census.
        let liquidity_slot = leg_state_slots(&row.key)[1];
        let liquidity = scratch
            .ext_mut()
            .get_storage(&V4_POOL_MANAGER, &liquidity_slot)
            .unwrap_or(U256::ZERO);
        let id = v4_pool_id(&row.key);
        if liquidity.is_zero() {
            empty_pools += 1;
            println!(
                "{:<44}  no in-range liquidity at the anchor — skipped",
                format!("0x{}", alloy::hex::encode(&id[..6]))
            );
            continue;
        }
        priced_pools += 1;

        let mut pool_points = 0usize;
        let mut pool_answered = 0usize;
        let mut pool_refused = 0usize;
        let mut pool_mismatch = 0usize;
        let mut legs_quoted = 0usize;

        for zero_for_one in [true, false] {
            let leg = V4Leg::canonical(row.key.clone(), zero_for_one);
            let input = leg.amount_in_currency();
            let input_decimals = if let Some(d) = decimals.get(&input) {
                *d
            } else {
                let d = token_decimals(&provider, input).await;
                decimals.insert(input, d);
                d
            };
            legs_quoted += 1;
            for amount_in in amount_ladder(input_decimals) {
                points += 1;
                pool_points += 1;

                let evm_side = match quoter.quote(&mut scratch, &leg, amount_in) {
                    Ok(quote) => {
                        grounded.push((leg.clone(), amount_in));
                        Side::Amount(quote.amount_out)
                    }
                    Err(V4QuoteError::Reverted { .. }) => Side::Refused("revert".to_string()),
                    Err(V4QuoteError::Halted { .. }) => {
                        halts += 1;
                        Side::Refused("halt".to_string())
                    }
                    Err(other) => Side::Refused(format!("frame: {other}")),
                };
                let node_side = match provider
                    .eth_call(&V4_QUOTER, quote_calldata(&leg, amount_in), Some(PIN))
                    .await
                {
                    Ok(output) => match decode_quote_return(&output) {
                        Some((amount_out, _)) => Side::Amount(amount_out),
                        None => Side::Refused(format!("node returned {} bytes", output.len())),
                    },
                    Err(_) => Side::Refused("revert".to_string()),
                };

                match (&evm_side, &node_side) {
                    (Side::Amount(ours), Side::Amount(node)) => {
                        answered += 1;
                        pool_answered += 1;
                        if !priced_hooks.contains(&row.hook) {
                            priced_hooks.push(row.hook);
                        }
                        if ours != node {
                            pool_mismatch += 1;
                            let diff = if ours > node {
                                *ours - *node
                            } else {
                                *node - *ours
                            };
                            if diff > max_diff {
                                max_diff = diff;
                            }
                            println!(
                                "  MISMATCH 0x{} zfo={zero_for_one} in={amount_in}: \
                                 evm2 {ours} vs node {node} (diff {diff})",
                                alloy::hex::encode(&id[..6])
                            );
                        }
                    }
                    (Side::Refused(_), Side::Refused(_)) => {
                        both_refused += 1;
                        pool_refused += 1;
                    }
                    (Side::Refused(reason), Side::Amount(node)) => {
                        pool_mismatch += 1;
                        println!(
                            "  MISMATCH 0x{} zfo={zero_for_one} in={amount_in}: \
                             evm2 refused ({reason}) but the node priced it at {node}",
                            alloy::hex::encode(&id[..6])
                        );
                    }
                    (Side::Amount(ours), Side::Refused(reason)) => {
                        pool_mismatch += 1;
                        println!(
                            "  MISMATCH 0x{} zfo={zero_for_one} in={amount_in}: \
                             evm2 priced {ours} but the node refused ({reason})",
                            alloy::hex::encode(&id[..6])
                        );
                    }
                }
            }
        }

        mismatches += pool_mismatch;
        println!(
            "{:<44} {:>6} {:>7} {:>8} {:>8} {:>6}",
            format!(
                "0x{} (hook 0x{})",
                alloy::hex::encode(&id[..6]),
                alloy::hex::encode(&row.hook.as_slice()[..3])
            ),
            legs_quoted,
            pool_points,
            pool_answered,
            pool_refused,
            pool_mismatch
        );
    }

    // The per-anchor memo, checked rather than assumed: re-quoting a point that
    // already ran on this scratch must forward ZERO cold reads to the node.
    let memo_check = if let Some((leg, amount_in)) = grounded.first() {
        let before = counter.reads();
        let again = quoter
            .quote(&mut scratch, leg, *amount_in)
            .expect("a priced leg stays priced on a warm scratch");
        let reads = counter.reads() - before;
        println!();
        println!(
            "memo check: re-quote of a priced leg forwarded {reads} cold read(s) \
             (0 = the per-anchor memo holds the leg's words)"
        );
        Some((reads, again.amount_out))
    } else {
        None
    };

    println!();
    println!(
        "totals: {} candidate pools ({} with in-range liquidity, {} empty) across {} hooks \
         ({} hooks answered) | {} quote points ({} answered, {} refused both sides, {} evm2 halts) \
         | mismatches {} | max |diff| {} wei",
        rows.len(),
        priced_pools,
        empty_pools,
        hooks.len(),
        priced_hooks.len(),
        points,
        answered,
        both_refused,
        halts,
        mismatches,
        max_diff
    );
    println!(
        "totals: {} cold reads forwarded over the whole census | wall {:.1} s",
        counter.reads(),
        started.elapsed().as_secs_f64()
    );

    assert_eq!(
        mismatches, 0,
        "the primitive must agree with the node's own quoter call at the anchor"
    );
    assert!(
        answered >= 20,
        "the census must actually price legs ({answered} answered of {points})"
    );
    if let Some((reads, _)) = memo_check {
        assert_eq!(
            reads, 0,
            "a re-quote on a warm scratch must not reach the RPC leaf"
        );
    }
}

/// A token's decimals at the anchor, defaulting to 18 where the metadata is
/// absent (native ETH has no `decimals()`, and a non-standard token may revert).
async fn token_decimals(provider: &AlloyProvider, token: Address) -> u32 {
    if token.is_zero() {
        return 18;
    }
    let Ok(output) = provider
        .eth_call(&token, Bytes::from_static(&DECIMALS_CALLDATA), Some(PIN))
        .await
    else {
        return 18;
    };
    output.get(..32).map_or(18, |word| {
        U256::from_be_slice(word).saturating_to::<u32>().min(30)
    })
}
