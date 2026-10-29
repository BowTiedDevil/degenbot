//! The tick-word backfill's STATE-OWNER surfaces: the pool-id dispatch, the
//! retried fetch context, and the family drive loop that reach the ONE merge
//! policy. The policy's own branches (merged / checked-empty / raced /
//! fetch-failed / no-source) are pinned beside the owner, behind the pools
//! root door (`degenbot_pools::{WordFetchOutcome, fetch_and_merge,
//! stage_word_fetch, install_staged_word}`); what these tests pin is what a
//! registered pool adds on top — the stored read handle, the fingerprint-gated
//! install under a released lock, and the bool contract the Python gate raises
//! on.

use super::*;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use alloy::primitives::U128;
use degenbot_pools::{
    FetchedTickWord, TickInfo, TickWordFetchError, TickWordKey, TickWordSource, WordFetchOutcome,
};

/// One scripted source reply.
#[derive(Debug)]
enum Reply {
    /// The source answers the word with these `(tick, stamp)` rows.
    Word(Vec<(i32, u64)>),
    /// The source's bitmap is all-zero (`Ok(None)`) — a checked-empty word.
    Miss,
    /// The read itself failed.
    Fail,
}

/// The scripted tick-word source the whole battery shares: one queued reply per
/// call, every call counted. A scripted source is the only way to drive the
/// multi-second read path without a node.
#[derive(Debug)]
struct ScriptedSource {
    replies: Mutex<VecDeque<Reply>>,
    calls: AtomicUsize,
}

impl ScriptedSource {
    fn script(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(VecDeque::from(replies)),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TickWordSource for ScriptedSource {
    fn fetch_tick_word(
        &self,
        _key: &TickWordKey,
        word: i32,
        _block: u64,
    ) -> Result<Option<FetchedTickWord>, TickWordFetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let reply = self
            .replies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front();
        match reply {
            Some(Reply::Word(ticks)) => Ok(Some(FetchedTickWord {
                word,
                ticks: ticks
                    .into_iter()
                    .map(|(tick, stamp)| {
                        (
                            tick,
                            TickInfo {
                                liquidity_gross: U128::from(100u128),
                                liquidity_net: 100i128,
                                block: stamp,
                            },
                        )
                    })
                    .collect(),
                bitmap: None,
            })),
            Some(Reply::Miss) => Ok(None),
            Some(Reply::Fail) | None => Err(TickWordFetchError::FetchFailed),
        }
    }
}

/// Register the sparse V3 pool this battery drives: no tick data, so the walking
/// word 0 is unknown, with the scripted source as the stored fetcher when one is
/// given.
fn sparse_v3_pool(fetcher: Option<Arc<dyn TickWordSource>>) -> (BotState, u64) {
    let mut core = BotState::new();
    let pool_id = core
        .register_v3_pool(&RegisterV3PoolParams {
            address: Address::ZERO,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 10_000_000_000_000u128,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
    (core, pool_id)
}

/// The stamp of one registered tick row.
fn tick_block(core: &BotState, pool_id: u64, tick: i32) -> Option<u64> {
    match core.pools.get(pool_id) {
        Some(PoolEntry::V3(p)) => p.1.tick_data().get(&tick).map(|info| info.block),
        _ => None,
    }
}

/// The pool's checked bitmap words, in a comparable order.
fn known_words(core: &BotState, pool_id: u64) -> Vec<i32> {
    let mut words: Vec<i32> = match core.pools.get(pool_id) {
        Some(PoolEntry::V3(p)) => p.1.known_bitmap_words().iter().copied().collect(),
        _ => Vec::new(),
    };
    words.sort_unstable();
    words
}

/// The number of initialized tick rows in the pool's map.
fn tick_count(core: &BotState, pool_id: u64) -> usize {
    match core.pools.get(pool_id) {
        Some(PoolEntry::V3(p)) => p.1.tick_data().len(),
        _ => 0,
    }
}

// --- the staged fetch: lock-free read, fingerprint-gated install ---

#[test]
fn staged_word_fetch_install_races_on_interleaved_pool_write() {
    // Scripted: attempt 1 answers a checked-empty word (the install races), the
    // retry answers the stale tick-60 value (block 99) the retried context
    // returns.
    let source = ScriptedSource::script(vec![Reply::Miss, Reply::Word(vec![(60, 99)])]);
    let (mut core, pool_id) = sparse_v3_pool(Some(source));

    // Stage under the (simulated) short write, then a pump event for the SAME
    // pool lands during the fetch window — the race shape.
    let staged = core
        .stage_word_fetch_by_pool_id(pool_id, 0, 99, false)
        .expect("a sparse pool stores a fetcher");
    let _ = core.apply_liquidity_update_by_pool_id(pool_id, -60, 60, 1_000, 100);
    let fetched = staged.fetch().expect("empty-word fetch");
    assert_eq!(
        core.install_word_fetch(&staged, &fetched),
        WordFetchOutcome::Raced,
        "an interleaved pool write must force a retry, never a clobbering overlay"
    );

    // Retry shape: the stage re-derives the fetch context from the pool clock
    // (the companion block passed here is deliberately bogus so a failed
    // re-derivation is loud), and the stale tick-60 row the retry returns (block
    // 99) must not regress the event-applied row (gross 1_000 @ block 100).
    let staged = core
        .stage_word_fetch_by_pool_id(pool_id, 0, 9_999, true)
        .expect("a sparse pool stores a fetcher");
    assert_eq!(
        staged.block(),
        99,
        "the retry fetch context re-derives from the pool clock (update_block - 1)"
    );
    let fetched = staged.fetch().expect("scripted stale fetch");
    assert_eq!(
        core.install_word_fetch(&staged, &fetched),
        WordFetchOutcome::Merged,
        "a quiet retry merges (the fingerprint is unchanged since the restage)"
    );
    assert_eq!(
        tick_block(&core, pool_id, 60),
        Some(100),
        "the event-applied tick must NOT be regressed by the stale overlay"
    );
    assert!(
        known_words(&core, pool_id).contains(&0),
        "the retry's install marks the word known"
    );
}

// --- write-path sparse backfill: the pool-id bool contract ---

#[test]
fn ensure_word_known_without_a_fetcher_reports_false() {
    let (mut core, pool_id) = sparse_v3_pool(None);

    assert!(
        !core.ensure_word_known_by_pool_id(pool_id, 0, 99),
        "no stored fetcher → false (the Python gate raises)"
    );
}

#[test]
fn ensure_word_known_merges_the_word_and_marks_it_known() {
    let (mut core, pool_id) = sparse_v3_pool(Some(ScriptedSource::script(vec![Reply::Word(
        vec![(60, 99)],
    )])));

    assert!(
        core.ensure_word_known_by_pool_id(pool_id, 0, 99),
        "a successful fetch reports true"
    );
    assert!(
        tick_block(&core, pool_id, 60).is_some(),
        "the fetched word's ticks land in tick_data (the ONE merge)"
    );
    assert!(
        known_words(&core, pool_id).contains(&0),
        "the fetched word is marked known"
    );
}

#[test]
fn ensure_word_known_reports_a_failed_fetch_as_false() {
    let (mut core, pool_id) = sparse_v3_pool(Some(ScriptedSource::script(vec![Reply::Fail])));

    assert!(
        !core.ensure_word_known_by_pool_id(pool_id, 0, 99),
        "a fetch failure reports false (the Python gate raises, never applies)"
    );
    assert_eq!(
        (
            tick_count(&core, pool_id),
            known_words(&core, pool_id).len()
        ),
        (0, 0),
        "no state mutation on a failed fetch"
    );
}

#[test]
fn ensure_word_known_treats_a_checked_empty_word_as_known() {
    let (mut core, pool_id) = sparse_v3_pool(Some(ScriptedSource::script(vec![Reply::Miss])));

    assert!(
        core.ensure_word_known_by_pool_id(pool_id, 3, 99),
        "a checked-empty fetch is a success: the word is known"
    );
    assert_eq!(
        known_words(&core, pool_id),
        vec![3],
        "checked-empty word 3 must be known"
    );
    assert_eq!(tick_count(&core, pool_id), 0, "no ticks ride along");
}

// --- the family drive loop over the same policy ---

#[test]
fn swap_simulation_fills_missing_word_and_retries() {
    // A sparse pool misses on its starting word; the drive loop fetches the
    // word, merges it, and retries — computed, non-zero, the fetched word
    // recorded, sparse coverage caveated.
    let (mut core, pool_id) =
        sparse_v3_pool(Some(ScriptedSource::script(vec![Reply::Miss, Reply::Miss])));

    let read = core.swap_simulation(
        0,
        pool_id,
        SwapRequest {
            zero_for_one: true,
            amount_specified: -I256::try_from(U256::from(1000u64)).unwrap(),
            sqrt_price_limit: None,
        },
    );
    match read {
        SwapRead::Computed(SwapOutcome::V3(payload)) => {
            assert_ne!(
                payload.delivered,
                I256::ZERO,
                "the fetched sparse swap must produce a non-zero amount"
            );
            // The walk may legitimately miss two adjacent words before finding
            // liquidity; both fetches are recorded.
            assert!(
                payload.fetched_words.contains(&0),
                "the fetch of word 0 is recorded, got {:?}",
                payload.fetched_words
            );
            assert!(
                payload.caveats.contains(Caveats::SPARSE_COVERAGE),
                "sparse coverage must be caveated"
            );
        }
        other => panic!("the drive must recover via fetch+retry, got {other:?}"),
    }
    assert!(
        known_words(&core, pool_id).contains(&0),
        "the fetched word 0 is marked known"
    );
}

#[test]
fn swap_simulation_fetcher_error_surfaces_fetch_failed() {
    let (mut core, pool_id) = sparse_v3_pool(Some(ScriptedSource::script(vec![Reply::Fail])));

    let read = core.swap_simulation(
        0,
        pool_id,
        SwapRequest {
            zero_for_one: true,
            amount_specified: -I256::try_from(U256::from(1000u64)).unwrap(),
            sqrt_price_limit: None,
        },
    );
    assert_eq!(
        read,
        SwapRead::FetchFailed { word: 0 },
        "a failing fetcher must surface FetchFailed, not panic, spin, or silently zero"
    );
}

#[test]
fn swap_simulation_empty_word_is_not_refetched() {
    // A checked-empty word stays in `known_bitmap_words`, so a second solve must
    // NOT re-invoke the source.
    let source = ScriptedSource::script(vec![Reply::Miss, Reply::Miss]);
    let (mut core, pool_id) = sparse_v3_pool(Some(source.clone()));

    let read = core.swap_simulation(
        0,
        pool_id,
        SwapRequest {
            zero_for_one: true,
            amount_specified: -I256::try_from(U256::from(1000u64)).unwrap(),
            sqrt_price_limit: None,
        },
    );
    let first = match &read {
        SwapRead::Computed(SwapOutcome::V3(payload)) => payload.delivered.into_raw(),
        other => panic!("the first solve must compute via fetch+retry, got {other:?}"),
    };
    let calls_after_first = source.calls();
    assert!(calls_after_first >= 1, "the first solve must fetch word 0");

    let second = core.swap_simulation(
        0,
        pool_id,
        SwapRequest {
            zero_for_one: true,
            amount_specified: -I256::try_from(U256::from(1000u64)).unwrap(),
            sqrt_price_limit: None,
        },
    );
    assert_eq!(
        source.calls(),
        calls_after_first,
        "the second solve must NOT re-invoke the fetcher — the checked-empty word survived"
    );
    match second {
        SwapRead::Computed(SwapOutcome::V3(payload)) => {
            assert_eq!(
                payload.delivered.into_raw(),
                first,
                "the second solve must match the first"
            );
            assert!(payload.fetched_words.is_empty());
        }
        other => panic!("the second solve must compute without fetching, got {other:?}"),
    }
}
