//! The strict sparse-word apply gate (`pump.strict_sparse_apply`) on the Rust
//! event path.
//!
//! A Mint/Burn (V3) or `ModifyLiquidity` (V4) whose boundary bitmap word is not
//! in a Sparse pool's checked set must BACKFILL that word before the event is
//! applied, and a word that cannot be backfilled must leave the event
//! unapplied as the typed [`ApplyOutcome::MissingTickWord`] — applying over an
//! unknown word journals per-tick priors for a region whose other initialized
//! ticks are absent (the reorg rollback would restore a state the chain never
//! had). A Tracked pool's bitmap is complete, so the gate is inert there: an
//! absent word is known-empty.
//!
//! The events are synthetic Mint / `ModifyLiquidity` logs driven through the one
//! dispatcher entry (`LogDispatcher::dispatch` — the path `Bot::dispatch_log`
//! takes), so every assertion is about the routing table plus the fetch
//! choreography, not a hand-built apply call.

use super::*;

use std::sync::Arc;

use alloy::primitives::{Address, B256, I256, U128, U256};
use degenbot_pools::sim::tick_fetch::{
    FetchedTickWord, TickWordFetchError, TickWordKey, TickWordSource,
};

use crate::concentrated_liquidity_route::{ApplyOutcome, Phase};
use crate::log_dispatcher::LogDispatcher;
use crate::state_lock::LockSite;
use crate::state_lock::StateLock;
use crate::{
    BotState, BufferedV3LiquidityUpdate, BufferedV3PoolEvent, PoolLookup, PoolTickCoverage,
    RegisterV3PoolParams, RegisterV4PoolParams, TickInfo, V4PoolKey,
};

const MINT_AMOUNT: u128 = 50;
const MINT_DELTA: i128 = 50;
const EVENT_BLOCK: u64 = 10;

fn pool_addr() -> Address {
    Address::from([0x22u8; 20])
}

fn manager_addr() -> Address {
    Address::from([0x33u8; 20])
}

fn v4_pool_id() -> degenbot_decoders::v4_swap_decoder::V4PoolId {
    [0x77u8; 32]
}

fn tick_info(gross: u128, net: i128) -> TickInfo {
    TickInfo {
        liquidity_gross: U128::from(gross),
        liquidity_net: net,
        block: 0,
    }
}

/// The scripted stand-in for the Rust-stored on-chain tick fetcher: it records
/// every `(word, block)` request and answers with one fixed word — or fails, the
/// RPC-down shape.
#[derive(Debug)]
struct GateTickWordSource {
    calls: std::sync::Mutex<Vec<(i32, u64)>>,
    ticks: Vec<(i32, TickInfo)>,
    fail: bool,
}

impl GateTickWordSource {
    fn returning(ticks: Vec<(i32, TickInfo)>) -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            ticks,
            fail: false,
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            ticks: Vec::new(),
            fail: true,
        })
    }

    fn calls(&self) -> Vec<(i32, u64)> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl TickWordSource for GateTickWordSource {
    fn fetch_tick_word(
        &self,
        _key: &TickWordKey,
        word: i32,
        block: u64,
    ) -> Result<Option<FetchedTickWord>, TickWordFetchError> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((word, block));
        if self.fail {
            return Err(TickWordFetchError::FetchFailed);
        }
        Ok(Some(FetchedTickWord {
            word,
            ticks: self.ticks.iter().cloned().collect(),
            bitmap: None,
        }))
    }
}

/// The stored fetcher handle: the `Arc<GateTickWordSource>` a test keeps to assert on
/// the recorded calls, widened to the trait object the pool stores.
fn word_fetcher(fetcher: &Arc<GateTickWordSource>) -> Arc<dyn TickWordSource> {
    let concrete: Arc<GateTickWordSource> = Arc::clone(fetcher);
    concrete
}

/// A V3 `Mint` log: `topics = [sig, owner, tickLower, tickUpper]`,
/// `data = abi.encode(sender, amount, amount0, amount1)`.
fn v3_mint_log(
    tick_lower: i32,
    tick_upper: i32,
    amount: u128,
    block: u64,
) -> alloy::rpc::types::Log {
    let tick_word = |tick: i32| {
        B256::from(
            I256::try_from(i128::from(tick))
                .expect("int24 fits i128")
                .to_be_bytes::<32>(),
        )
    };
    let mut data = Vec::with_capacity(128);
    data.extend_from_slice(&[0u8; 32]); // sender
    data.extend_from_slice(&[0u8; 16]); // amount high half
    data.extend_from_slice(&amount.to_be_bytes());
    data.extend_from_slice(&[0u8; 32]); // amount0
    data.extend_from_slice(&[0u8; 32]); // amount1
    let inner = alloy::primitives::Log::new_unchecked(
        pool_addr(),
        vec![
            degenbot_decoders::v3_mint_burn_decoder::V3_MINT_TOPIC,
            B256::ZERO,
            tick_word(tick_lower),
            tick_word(tick_upper),
        ],
        alloy::primitives::Bytes::from(data),
    );
    alloy::rpc::types::Log {
        inner,
        block_hash: None,
        block_number: Some(block),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    }
}

/// A V4 `ModifyLiquidity` log:
/// `topics = [sig, pool_id, sender]`,
/// `data = abi.encode(tickLower, tickUpper, liquidityDelta, salt)`.
fn v4_modify_liquidity_log(
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: i128,
    block: u64,
) -> alloy::rpc::types::Log {
    let i256 = |v: i128| I256::try_from(v).expect("fits i256").to_be_bytes::<32>();
    let mut data = Vec::with_capacity(128);
    data.extend_from_slice(&i256(i128::from(tick_lower)));
    data.extend_from_slice(&i256(i128::from(tick_upper)));
    data.extend_from_slice(&i256(liquidity_delta));
    data.extend_from_slice(&[0u8; 32]); // salt
    let inner = alloy::primitives::Log::new_unchecked(
        manager_addr(),
        vec![
            degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC,
            B256::from(v4_pool_id()),
            B256::ZERO,
        ],
        alloy::primitives::Bytes::from(data),
    );
    alloy::rpc::types::Log {
        inner,
        block_hash: None,
        block_number: Some(block),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    }
}

/// A Sparse V3 pool (no tick rows at registration, so no bitmap word is
/// checked) carrying the scripted fetcher; `strict` arms the gate on the owner.
fn sparse_v3_state(fetcher: Option<Arc<dyn TickWordSource>>, strict: bool) -> StateLock<BotState> {
    let mut core = BotState::new();
    core.set_strict_sparse_apply(strict);
    core.register_v3_pool(&RegisterV3PoolParams {
        address: pool_addr(),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3_000,
        tick_spacing: 60,
        factory: Address::from([0x11u8; 20]),
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 10_000_000_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher,
        ..Default::default()
    })
    .expect("test setup: sparse V3 registration");
    StateLock::new(core)
}

/// A V4 twin of [`sparse_v3_state`], keyed `(pool_manager, pool_id)`.
fn sparse_v4_state(fetcher: Option<Arc<dyn TickWordSource>>, strict: bool) -> StateLock<BotState> {
    let mut core = BotState::new();
    core.set_strict_sparse_apply(strict);
    core.register_v4_pool(&RegisterV4PoolParams {
        pool_manager: manager_addr(),
        pool_id: v4_pool_id(),
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([1u8; 20]),
            fee: 3_000,
            tick_spacing: 60,
            hooks: Address::ZERO,
        },
        state_view: Address::ZERO,
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 10_000_000_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher,
        known_bitmap_words: Vec::new(),
    })
    .expect("test setup: sparse V4 registration");
    StateLock::new(core)
}

fn v3_tick_gross(state: &StateLock<BotState>, tick: i32) -> Option<u128> {
    let core = state.read_at(LockSite::Core);
    let pool_id = core.pool_id_by_address(&pool_addr())?;
    core.pool_slice::<degenbot_pools::state::v3_state::V3PoolState>(pool_id)?
        .tick_data
        .get(&tick)
        .map(|t| t.liquidity_gross.to::<u128>())
}

// --- V3 ------------------------------------------------------------------

/// RED-capable half: with the gate armed, a Mint over an unchecked word fetches
/// the word at `event_block - 1` BEFORE the apply, then applies the event.
#[test]
fn v3_sparse_gate_backfills_the_boundary_word_before_applying() {
    // Tick 30 is in word 0 but outside the event's range, so a gross of 1_000
    // at it is pure backfill evidence; the boundary ticks carry the event alone.
    let fetcher = GateTickWordSource::returning(vec![(30, tick_info(1_000, 0))]);
    let state = Arc::new(sparse_v3_state(Some(word_fetcher(&fetcher)), true));
    let dispatcher = LogDispatcher::with_uniswap_decoders();

    dispatcher.dispatch(&v3_mint_log(0, 60, MINT_AMOUNT, EVENT_BLOCK), &state, None);

    assert_eq!(
        fetcher.calls(),
        vec![(0, EVENT_BLOCK - 1)],
        "the gate must fetch word 0 at the state the event applies over (block - 1)"
    );
    assert_eq!(
        v3_tick_gross(&state, 30),
        Some(1_000),
        "the backfilled word's tick must be merged before the apply"
    );
    assert_eq!(
        v3_tick_gross(&state, 0),
        Some(MINT_AMOUNT),
        "the event itself must land after the backfill"
    );
    assert_eq!(
        dispatcher.snapshot_epoch_logs_and_reset().applied,
        1,
        "a fetched-then-applied event is an apply, not a miss"
    );
}

/// The refusal half: a failed fetch leaves the event UNAPPLIED, and the routing
/// door reports the typed outcome rather than dropping the event silently.
#[test]
fn v3_sparse_gate_fetch_failure_refuses_and_does_not_apply() {
    let fetcher = GateTickWordSource::failing();
    let state = Arc::new(sparse_v3_state(Some(word_fetcher(&fetcher)), true));
    let dispatcher = LogDispatcher::with_uniswap_decoders();

    dispatcher.dispatch(&v3_mint_log(0, 60, MINT_AMOUNT, EVENT_BLOCK), &state, None);

    assert_eq!(
        fetcher.calls(),
        vec![(0, EVENT_BLOCK - 1)],
        "the gate attempts the backfill before refusing"
    );
    assert_eq!(
        v3_tick_gross(&state, 0),
        None,
        "a failed fetch must NOT apply the event over the unknown word"
    );
    let counts = dispatcher.snapshot_epoch_logs_and_reset();
    assert_eq!(counts.applied, 0, "nothing applied");
    assert_eq!(counts.apply_missed, 1, "the refusal is counted, not silent");

    // The routing door names the refusal: the pool id plus the unknown word.
    let pool_id = state
        .read_at(LockSite::Core)
        .pool_id_by_address(&pool_addr())
        .expect("registered");
    let outcome = state.write_at(LockSite::Core).route_event(
        Phase::Live,
        PoolLookup::from_address(pool_addr()),
        degenbot_pools::families::buffered_event::BufferedPoolEvent::V3(
            BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                tick_lower: 0,
                tick_upper: 60,
                liquidity_delta: MINT_DELTA,
                block_number: EVENT_BLOCK,
            }),
        ),
        &[],
    );
    assert_eq!(
        outcome.expect("an address key with a V3 payload cannot mismatch"),
        ApplyOutcome::MissingTickWord { pool_id, word: 0 }
    );
    assert_eq!(
        v3_tick_gross(&state, 0),
        None,
        "the refusal applied nothing"
    );
}

/// A word already checked (the first event's backfill installed it) is never
/// re-fetched by a later event — the second Mint's boundary words are both in
/// word 0.
#[test]
fn v3_sparse_gate_does_not_refetch_a_known_word() {
    let fetcher = GateTickWordSource::returning(vec![(60, tick_info(1_000, 0))]);
    let state = Arc::new(sparse_v3_state(Some(word_fetcher(&fetcher)), true));
    let dispatcher = LogDispatcher::with_uniswap_decoders();

    dispatcher.dispatch(&v3_mint_log(0, 60, MINT_AMOUNT, EVENT_BLOCK), &state, None);
    dispatcher.dispatch(
        &v3_mint_log(60, 120, MINT_AMOUNT, EVENT_BLOCK),
        &state,
        None,
    );

    assert_eq!(
        fetcher.calls(),
        vec![(0, EVENT_BLOCK - 1)],
        "a checked word must not be refetched"
    );
    assert_eq!(v3_tick_gross(&state, 120), Some(MINT_AMOUNT));
}

/// Default posture: the gate is OFF, so the event applies immediately and the
/// word is left for solve-time backfill (the lazy production behavior).
#[test]
fn v3_sparse_apply_gate_defaults_off_and_stays_lazy() {
    let fetcher = GateTickWordSource::returning(vec![(60, tick_info(1_000, 0))]);
    let state = Arc::new(sparse_v3_state(Some(word_fetcher(&fetcher)), false));
    assert!(
        !state.read_at(LockSite::Core).strict_sparse_apply(),
        "the schema default is OFF"
    );
    let dispatcher = LogDispatcher::with_uniswap_decoders();

    dispatcher.dispatch(&v3_mint_log(0, 60, MINT_AMOUNT, EVENT_BLOCK), &state, None);

    assert_eq!(
        fetcher.calls(),
        Vec::new(),
        "the lazy posture never fetches on the apply path"
    );
    assert_eq!(v3_tick_gross(&state, 0), Some(MINT_AMOUNT));
}

/// A Tracked pool's bitmap is complete, so the gate is inert even when armed:
/// an absent word is known-empty, and no fetcher exists to consult.
#[test]
fn v3_tracked_pool_gate_is_inert() {
    let mut core = BotState::new();
    core.set_strict_sparse_apply(true);
    let mut tick_data = HashMap::new();
    tick_data.insert(0, tick_info(1_000, 100));
    tick_data.insert(60, tick_info(1_000, -100));
    core.register_v3_pool(&RegisterV3PoolParams {
        address: pool_addr(),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3_000,
        tick_spacing: 60,
        factory: Address::from([0x11u8; 20]),
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 10_000_000_000_000,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    })
    .expect("test setup: tracked V3 registration");
    // A Tracked pool registers Quarantined (two-step verify); the steady-state
    // direct-apply posture is the gate's subject here.
    PoolLookup::from_address(pool_addr()).mark_registration_live(&mut core);
    let state = Arc::new(StateLock::new(core));
    let dispatcher = LogDispatcher::with_uniswap_decoders();

    dispatcher.dispatch(
        &v3_mint_log(120, 180, MINT_AMOUNT, EVENT_BLOCK),
        &state,
        None,
    );

    assert_eq!(
        v3_tick_gross(&state, 120),
        Some(MINT_AMOUNT),
        "a Tracked pool applies a Mint in an unchecked word (absent = known-empty)"
    );
    assert_eq!(v3_tick_gross(&state, 180), Some(MINT_AMOUNT));
    assert_eq!(dispatcher.snapshot_epoch_logs_and_reset().applied, 1);
}

// --- V4 (the twin) -------------------------------------------------------

fn v4_tick_gross(state: &StateLock<BotState>, tick: i32) -> Option<u128> {
    let core = state.read_at(LockSite::Core);
    let pool_id = core.pool_id_by_key(manager_addr(), &v4_pool_id())?;
    core.pool_slice::<degenbot_pools::state::v4_state::V4PoolState>(pool_id)?
        .tick_data
        .get(&tick)
        .map(|t| t.liquidity_gross.to::<u128>())
}

/// The V4 twin: a `ModifyLiquidity` over an unchecked word backfills the word
/// first, and a failed fetch leaves the event unapplied.
#[test]
fn v4_sparse_gate_backfills_and_refuses_like_v3() {
    let fetcher = GateTickWordSource::returning(vec![(30, tick_info(1_000, 0))]);
    let state = Arc::new(sparse_v4_state(Some(word_fetcher(&fetcher)), true));
    let dispatcher = LogDispatcher::with_uniswap_decoders();

    dispatcher.dispatch(
        &v4_modify_liquidity_log(0, 60, MINT_DELTA, EVENT_BLOCK),
        &state,
        None,
    );

    assert_eq!(
        fetcher.calls(),
        vec![(0, EVENT_BLOCK - 1)],
        "the V4 twin backfills word 0 at the state the event applies over"
    );
    assert_eq!(v4_tick_gross(&state, 30), Some(1_000), "the fetched row");
    assert_eq!(v4_tick_gross(&state, 0), Some(MINT_AMOUNT), "the event");

    let failing = GateTickWordSource::failing();
    let state = Arc::new(sparse_v4_state(Some(word_fetcher(&failing)), true));
    dispatcher.dispatch(
        &v4_modify_liquidity_log(0, 60, MINT_DELTA, EVENT_BLOCK),
        &state,
        None,
    );
    assert_eq!(failing.calls(), vec![(0, EVENT_BLOCK - 1)]);
    assert_eq!(
        v4_tick_gross(&state, 0),
        None,
        "a failed fetch must NOT apply the V4 event over the unknown word"
    );
}
