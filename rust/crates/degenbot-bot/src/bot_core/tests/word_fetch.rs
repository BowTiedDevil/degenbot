use super::*;

// --- T2 (FBJTUM): write-path sparse backfill — ensure_word_known ---
#[test]
#[expect(clippy::expect_used, clippy::too_many_lines)]
fn staged_word_fetch_install_races_on_interleaved_pool_write() {
    use crate::bot_core::InstallWordOutcome;
    use ::degenbot_pools::tick_fetch::{FetchedTickWord, TickWordFetcher};

    // Scripted fetcher (RATR5A Finding-1 red shape): attempt 1 -> empty
    // word (checked-empty, RACED); attempt 2 -> the stale tick-60 value
    // (block 99) the retried context returns.
    #[derive(Debug)]
    struct ScriptedWordFetcher {
        script: std::sync::Mutex<std::collections::VecDeque<FetchedTickWord>>,
    }
    impl ScriptedWordFetcher {
        fn new_scripted() -> Self {
            Self {
                script: std::sync::Mutex::new(std::collections::VecDeque::from([
                    FetchedTickWord {
                        word: 0,
                        ticks: HashMap::new(),
                    },
                    FetchedTickWord {
                        word: 0,
                        ticks: HashMap::from_iter([(
                            60,
                            TickInfo {
                                liquidity_gross: alloy::primitives::U128::from(100u128),
                                liquidity_net: 100i128,
                                block: 99,
                            },
                        )]),
                    },
                ])),
            }
        }
    }
    impl TickWordFetcher for ScriptedWordFetcher {
        fn fetch_missing_tick_word(
            &self,
            _pool_id: u64,
            word: i32,
            _block: u64,
        ) -> Result<FetchedTickWord, ::degenbot_pools::tick_fetch::FetchTickWordError> {
            self.script
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .map(|mut w| {
                    w.word = word;
                    w
                })
                .ok_or(::degenbot_pools::tick_fetch::FetchTickWordError::FetchFailed)
        }
    }

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
            fetcher: Some(std::sync::Arc::new(ScriptedWordFetcher::new_scripted())),
            ..Default::default()
        })
        .expect("test setup: V3 registration");

    // Stage under the (simulated) short write, then a pump event for the
    // SAME pool lands during the fetch window (the RATR5A race shape).
    let staged = core
        .stage_word_fetch_by_pool_id(pool_id, 0, 99, false)
        .expect("sparse pool stores a fetcher");
    core.apply_v3_liquidity_update_by_pool_id(pool_id, -60, 60, 1_000, 100);
    let fetched = staged.fetch().expect("empty-word fetch");
    assert_eq!(
        core.install_word_fetch(&staged, &fetched),
        InstallWordOutcome::Raced,
        "an interleaved pool write must force a retry, never a clobbering overlay"
    );

    // Retry shape (RATR5A Finding 1): the stage re-derives the fetch
    // context from the pool clock - the companion block passed above is
    // deliberately bogus (9_999) so a failed re-derivation is loud - and
    // the scripted second fetch returns the stale tick-60 value (block
    // 99). The stamp guard (Finding-1(a)) must keep the event values
    // (gross 1_000 @ block 100).
    let staged = core
        .stage_word_fetch_by_pool_id(pool_id, 0, 9_999, true)
        .expect("sparse pool stores a fetcher");
    assert_eq!(
        staged.block, 99,
        "retry fetch context re-derives from the pool clock (update_block - 1)"
    );
    let fetched = staged.fetch().expect("scripted stale fetch");
    let outcome = core.install_word_fetch(&staged, &fetched);
    assert_eq!(
        outcome,
        InstallWordOutcome::Merged,
        "quiet retry merges (fingerprint unchanged since restage)"
    );
    match core.pools.get(&pool_id) {
        Some(PoolEntry::V3(p)) => {
            let state = &p.1;
            let tick = state
                .tick_data
                .get(&60)
                .expect("merged word carries tick 60 after the retry");
            assert_eq!(
                (tick.liquidity_gross.to::<u128>(), tick.block),
                (u128::from(1_000u64), 100),
                "the event-applied tick must NOT be regressed by the stale overlay"
            );
        }
        _ => panic!("test setup: V3 pool missing"),
    }
    let known: Vec<i32> = match core.pools.get(&pool_id) {
        Some(PoolEntry::V3(p)) => p.1.known_bitmap_words().iter().copied().collect(),
        _ => panic!("test setup: V3 pool missing"),
    };
    assert!(
        known.contains(&0),
        "the retry's install marks the word known (T2 FBJTUM parity)"
    );
}

#[test]
fn ensure_word_known_no_fetcher_returns_false() {
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
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");

    assert!(
        !core.ensure_word_known_by_pool_id(pool_id, 0, 99),
        "no stored fetcher → False (the Python gate raises)"
    );
}

#[test]
fn ensure_word_known_merges_ticks_and_marks_word_known() {
    use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};
    use ::degenbot_pools::TickInfo;
    use alloy::primitives::U128;

    #[derive(Debug)]
    struct WordFetcher;
    impl TickWordFetcher for WordFetcher {
        fn fetch_missing_tick_word(
            &self,
            _pool_id: u64,
            word: i32,
            _block: u64,
        ) -> Result<FetchedTickWord, FetchTickWordError> {
            Ok(FetchedTickWord {
                word,
                ticks: HashMap::from_iter([(
                    60,
                    TickInfo {
                        liquidity_gross: U128::from(100u128),
                        liquidity_net: 100i128,
                        block: 99,
                    },
                )]),
            })
        }
    }

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
            fetcher: Some(std::sync::Arc::new(WordFetcher)),
            ..Default::default()
        })
        .expect("test setup: V3 registration");

    let ok = core.ensure_word_known_by_pool_id(pool_id, 0, 99);
    assert!(ok, "a successful fetch must return True");

    let tick_data: HashMap<i32, TickInfo> = match core.pools.get(&pool_id) {
        Some(PoolEntry::V3(p)) => p.1.tick_data().clone(),
        _ => panic!("test setup: V3 pool missing"),
    };
    assert!(
        tick_data.contains_key(&60),
        "the fetched word's ticks must land in tick_data (core merge reused)"
    );
    let known: Vec<i32> = match core.pools.get(&pool_id) {
        Some(PoolEntry::V3(p)) => p.1.known_bitmap_words().iter().copied().collect(),
        _ => panic!("test setup: V3 pool missing"),
    };
    assert!(known.contains(&0), "the fetched word must be marked known");
}

#[test]
fn ensure_word_known_fetch_error_returns_false() {
    use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};

    #[derive(Debug)]
    struct FailingWordFetcher;
    impl TickWordFetcher for FailingWordFetcher {
        fn fetch_missing_tick_word(
            &self,
            _pool_id: u64,
            _word: i32,
            _block: u64,
        ) -> Result<FetchedTickWord, FetchTickWordError> {
            Err(FetchTickWordError::FetchFailed)
        }
    }

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
            fetcher: Some(std::sync::Arc::new(FailingWordFetcher)),
            ..Default::default()
        })
        .expect("test setup: V3 registration");

    assert!(
        !core.ensure_word_known_by_pool_id(pool_id, 0, 99),
        "a fetch failure must return False (the Python gate RAISES, never applies)"
    );
    let (len, known) = match core.pools.get(&pool_id) {
        Some(PoolEntry::V3(p)) => {
            let state = &p.1;
            (state.tick_data().len(), state.known_bitmap_words().len())
        }
        _ => panic!("test setup: V3 pool missing"),
    };
    assert_eq!((len, known), (0, 0), "no state mutation on a failed fetch");
}

#[test]
fn ensure_word_known_checked_empty_marks_word_known() {
    use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};

    #[derive(Debug)]
    struct EmptyWordFetcher;
    impl TickWordFetcher for EmptyWordFetcher {
        fn fetch_missing_tick_word(
            &self,
            _pool_id: u64,
            word: i32,
            _block: u64,
        ) -> Result<FetchedTickWord, FetchTickWordError> {
            Ok(FetchedTickWord {
                word,
                ticks: HashMap::new(),
            })
        }
    }

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
            fetcher: Some(std::sync::Arc::new(EmptyWordFetcher)),
            ..Default::default()
        })
        .expect("test setup: V3 registration");

    assert!(
        core.ensure_word_known_by_pool_id(pool_id, 3, 99),
        "a checked-empty fetch is a success: the word is known (T1 semantics)"
    );
    let known: Vec<i32> = match core.pools.get(&pool_id) {
        Some(PoolEntry::V3(p)) => p.1.known_bitmap_words().iter().copied().collect(),
        _ => panic!("test setup: V3 pool missing"),
    };
    assert!(known.contains(&3), "checked-empty word 3 must be known");
}

// --- ADR-005 sparse-map parity, slice 2: fetch-callback seam ---
#[test]
fn swap_simulation_fills_missing_word_and_retries() {
    // A sparse V3 pool (empty tick_data, start tick 0, word 0 unknown)
    // misses on the starting word. The gate's fetch seam fills the missing
    // word (via a fake fetcher), merges, and retries; the result must be
    // non-zero, record the fetch, and carry the sparse caveat.
    use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};

    #[derive(Debug)]
    struct FakeFetcher;
    impl TickWordFetcher for FakeFetcher {
        fn fetch_missing_tick_word(
            &self,
            _pool_id: u64,
            word: i32,
            _block: u64,
        ) -> Result<FetchedTickWord, FetchTickWordError> {
            // Mark the word known with no initialized ticks (empty word).
            Ok(FetchedTickWord {
                word,
                ticks: HashMap::new(),
            })
        }
    }

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
            tick_data: HashMap::new(), // fully sparse — word 0 unknown
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: Some(std::sync::Arc::new(FakeFetcher)),
            ..Default::default()
        })
        .expect("test setup: V3 registration");

    // Through the gate: the miss is recovered automatically — computed,
    // non-zero, the fetched word recorded, sparse coverage caveated.
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
            // The walk may legitimately miss both word 0 and the adjacent
            // word (-1) before finding liquidity; both fetches are recorded.
            assert!(
                payload.fetched_words.contains(&0),
                "fetch of word 0 recorded, got {:?}",
                payload.fetched_words
            );
            assert!(
                payload.caveats.contains(Caveats::SPARSE_COVERAGE),
                "sparse coverage must be caveated"
            );
        }
        other => panic!("gate must recover via fetch+retry, got {other:?}"),
    }
    // The fetch merged word 0 into known_bitmap_words (no further miss).
    let state = core.get_v3_pool(pool_id).expect("pool registered");
    assert!(
        state.known_bitmap_words.contains(&0),
        "fetched word 0 must be marked known"
    );
}

#[test]
fn swap_simulation_fetcher_error_surfaces_fetch_failed() {
    // If the fetcher cannot satisfy the missing word (RPC error / out of
    // range), the calc must give up with `U256::ZERO` rather than panic or
    // spin. Covers the `Err(_)` arm of the fetch+retry loop.
    use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};

    #[derive(Debug)]
    struct FailingFetcher;
    impl TickWordFetcher for FailingFetcher {
        fn fetch_missing_tick_word(
            &self,
            _pool_id: u64,
            _word: i32,
            _block: u64,
        ) -> Result<FetchedTickWord, FetchTickWordError> {
            Err(FetchTickWordError::FetchFailed)
        }
    }

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
            fetcher: Some(std::sync::Arc::new(FailingFetcher)),
            ..Default::default()
        })
        .expect("test setup: V3 registration");

    // Through the gate: a failing fetcher is OBSERVABLE as FetchFailed
    // (word 0) — the legacy seam collapsed this into silent ZERO.
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
fn swap_simulation_empty_word_not_refetched() {
    // A fetcher that returns an empty word (checked-but-empty) marks the
    // word known in `known_bitmap_words`. A second solve must NOT re-invoke
    // the fetcher — the empty word survived in the bitmap (ADR-006/005
    // stored-tick-fetcher ). This is the bitmap empty-word fix
    // that lets the companion delete `_bitmap_override`.
    use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[derive(Debug)]
    struct CountingFetcher {
        calls: AtomicU32,
    }
    impl TickWordFetcher for CountingFetcher {
        fn fetch_missing_tick_word(
            &self,
            _pool_id: u64,
            word: i32,
            _block: u64,
        ) -> Result<FetchedTickWord, FetchTickWordError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(FetchedTickWord {
                word,
                ticks: HashMap::new(),
            })
        }
    }

    let counter = Arc::new(CountingFetcher {
        calls: AtomicU32::new(0),
    });
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
            fetcher: Some(counter.clone() as Arc<dyn TickWordFetcher>),
            ..Default::default()
        })
        .expect("test setup: V3 registration");

    // First solve: misses word 0, fetches (empty), retries → computes.
    // The fetched word rides on the outcome; the empty (checked) word
    // survives in known_bitmap_words.
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
        SwapRead::Computed(SwapOutcome::V3(p)) => p.delivered.into_raw(),
        other => panic!("first solve must compute via fetch+retry, got {other:?}"),
    };
    let calls_after_first = counter.calls.load(Ordering::SeqCst);
    assert!(calls_after_first >= 1, "first solve must fetch word 0");
    match read {
        SwapRead::Computed(SwapOutcome::V3(p)) => {
            assert!(
                p.fetched_words.contains(&0),
                "word 0 fetched once, got {:?}",
                p.fetched_words
            );
        }
        _ => unreachable!(),
    }

    // Second solve: word 0 is now known → NO fetch should happen and the
    // outcome records no fetched words.
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
        counter.calls.load(Ordering::SeqCst),
        calls_after_first,
        "second solve must NOT re-invoke the fetcher — the empty word survived in known_bitmap_words"
    );
    match second {
        SwapRead::Computed(SwapOutcome::V3(p)) => {
            assert_eq!(
                p.delivered.into_raw(),
                first,
                "second solve must match the first"
            );
            assert!(p.fetched_words.is_empty());
        }
        other => panic!("second solve must compute without fetching, got {other:?}"),
    }
    assert_eq!(
        counter.calls.load(Ordering::SeqCst),
        calls_after_first,
        "second solve must NOT re-invoke the fetcher — the empty word survived in known_bitmap_words"
    );
}
