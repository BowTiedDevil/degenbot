//! Backfill-batch validation: every backfilled event is placed in time by its
//! OWN log's block number, never by the chunk's boundary.

use super::*;

use crate::log_dispatcher::LogDispatcher;

use alloy::primitives::{Bytes, B256, I256, U128};
use alloy::rpc::types::Log;

use degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC;
use degenbot_decoders::v4_swap_decoder::{V4PoolId, V4_SWAP_TOPIC};

/// A V3 `Swap` log carrying post-swap scalars at `block_number`.
///
/// data = abi.encode(int256 amount0, int256 amount1, uint160 sqrtPriceX96,
/// uint128 liquidity, int24 tick) — the amounts are filler; the routing path
/// consumes the scalars.
fn v3_swap_log(
    pool_address: Address,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    block_number: u64,
) -> Log {
    let mut data = Vec::with_capacity(160);
    data.extend_from_slice(&[0u8; 32]);
    data.extend_from_slice(&[0u8; 32]);
    data.extend_from_slice(&sqrt_price_x96.to_be_bytes::<32>());
    let mut liq_word = [0u8; 32];
    liq_word[16..32].copy_from_slice(&liquidity.to_be_bytes());
    data.extend_from_slice(&liq_word);
    let mut tick_word = [0u8; 32];
    tick_word[28..32].copy_from_slice(&tick.to_be_bytes());
    data.extend_from_slice(&tick_word);

    let inner = alloy::primitives::Log::new_unchecked(
        pool_address,
        vec![
            V3_SWAP_TOPIC,
            B256::left_padding_from(&[0xaau8; 20]),
            B256::left_padding_from(&[0xbbu8; 20]),
        ],
        Bytes::from(data),
    );
    Log {
        inner,
        block_hash: None,
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    }
}

/// A V4 `Swap` log emitted by `pool_manager` at `block_number`.
fn v4_swap_log(
    pool_manager: Address,
    pool_id: V4PoolId,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    block_number: u64,
) -> Log {
    let mut data = Vec::with_capacity(192);
    data.extend_from_slice(&I256::ZERO.to_be_bytes::<32>());
    data.extend_from_slice(&I256::ZERO.to_be_bytes::<32>());
    data.extend_from_slice(&sqrt_price_x96.to_be_bytes::<32>());
    let mut liq_word = [0u8; 32];
    liq_word[16..32].copy_from_slice(&U128::from(liquidity).to_be_bytes::<16>());
    data.extend_from_slice(&liq_word);
    data.extend_from_slice(&I256::unchecked_from(i128::from(tick)).to_be_bytes::<32>());
    let mut fee_word = [0u8; 32];
    fee_word[28..32].copy_from_slice(&3_000u32.to_be_bytes());
    data.extend_from_slice(&fee_word);

    let inner = alloy::primitives::Log::new_unchecked(
        pool_manager,
        vec![
            V4_SWAP_TOPIC,
            B256::from(pool_id),
            Address::from([0xbbu8; 20]).into_word(),
        ],
        Bytes::from(data),
    );
    Log {
        inner,
        block_hash: None,
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    }
}

/// Register a `Sparse` (hence direct-apply `Live`) V4 pool, returning its id.
fn register_v4_on_core(
    core: &mut BotState,
    pool_manager: Address,
    pool_id: V4PoolId,
    update_block: u64,
) -> u64 {
    use crate::{RegisterV4PoolParams, V4PoolKey};

    core.register_v4_pool(&RegisterV4PoolParams {
        pool_manager,
        pool_id,
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
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        known_bitmap_words: Vec::new(),
    })
    .expect("test setup: V4 registration")
}

/// A malformed (`block_number`-less) copy of `log`, carrying the identity an
/// `eth_getLogs` response would have supplied for it.
fn without_block_number(mut log: Log) -> Log {
    log.block_number = None;
    log.transaction_hash = Some(B256::from([0xabu8; 32]));
    log.log_index = Some(7);
    log
}

/// A batch whose only log is malformed is refused with the offending log's
/// identity, and the pool state it would have moved is untouched.
#[test]
fn backfill_refuses_log_without_block_number() {
    let mut core = BotState::new();
    let pool_addr = Address::from([0x77u8; 20]);
    let seed_block = 5u64;
    let pool_id = register_v3_on_core(&mut core, pool_addr, seed_block);
    let dispatcher = LogDispatcher::with_uniswap_decoders();
    let base_price = U256::from(1u128) << 96;

    let malformed = without_block_number(v3_swap_log(
        pool_addr,
        base_price + U256::from(1u64),
        1_100_000,
        1,
        10,
    ));
    let err = core
        .process_backfill_logs(&dispatcher, &[malformed], 2_000)
        .expect_err("a log with no block_number cannot be placed in time");

    assert_eq!(err.position, 0, "the refusal names the offending position");
    assert_eq!(err.address, pool_addr);
    assert_eq!(err.transaction_hash, Some(B256::from([0xabu8; 32])));
    assert_eq!(err.log_index, Some(7));
    assert_eq!(
        err.log.block_number, None,
        "the error carries the log itself"
    );
    let rendered = err.to_string();
    assert!(
        rendered.contains("no block_number"),
        "the failure names the missing field: {rendered}"
    );
    assert!(
        rendered.contains(&format!("{:?}", B256::from([0xabu8; 32]))),
        "the failure carries the transaction hash when the response supplied one: {rendered}"
    );

    let state = core
        .pool_slice::<V3PoolState>(pool_id)
        .expect("V3 pool registered");
    assert_eq!(
        state.sqrt_price_x96, base_price,
        "the malformed log must not move the pool's scalars"
    );
    assert!(
        state.journal.is_empty(),
        "the malformed log must not push a journal delta"
    );
    assert_eq!(
        state.update_block, seed_block,
        "the malformed log must not advance the pool's price clock"
    );
}

/// A malformed LATE log refuses the whole batch: the valid log that precedes
/// it is not decoded or applied, so no partial chunk reaches pool state.
#[test]
fn backfill_refuses_late_malformed_log_before_applying_earlier_logs() {
    let mut core = BotState::new();
    let pool_addr = Address::from([0x77u8; 20]);
    let seed_block = 5u64;
    let pool_id = register_v3_on_core(&mut core, pool_addr, seed_block);
    let dispatcher = LogDispatcher::with_uniswap_decoders();
    let base_price = U256::from(1u128) << 96;

    let valid = v3_swap_log(pool_addr, base_price + U256::from(1u64), 1_100_000, 1, 10);
    let malformed = without_block_number(v3_swap_log(
        pool_addr,
        base_price + U256::from(2u64),
        1_200_000,
        2,
        11,
    ));
    let err = core
        .process_backfill_logs(&dispatcher, &[valid, malformed], 2_000)
        .expect_err("the batch carries a malformed log");

    assert_eq!(err.position, 1, "the refusal names the malformed position");

    let state = core
        .pool_slice::<V3PoolState>(pool_id)
        .expect("V3 pool registered");
    assert_eq!(
        state.sqrt_price_x96, base_price,
        "the valid first log stays unapplied: the whole batch is refused before any apply"
    );
    assert!(
        state.journal.is_empty(),
        "no journal delta from a refused batch"
    );
    assert_eq!(state.update_block, seed_block);
}

/// A valid batch mixing V3 and V4 logs lands each event at its own block: the
/// per-pool journals keep the logs' own order and stamps (`chunk_end` is far
/// past every log and must not appear).
#[test]
fn backfill_applies_mixed_v3_v4_logs_at_their_own_blocks() {
    let mut core = BotState::new();
    let v3_addr = Address::from([0x77u8; 20]);
    let v3_id = register_v3_on_core(&mut core, v3_addr, 0);
    let pool_manager = Address::from([0x44u8; 20]);
    let v4_pool_id: V4PoolId = [0xeeu8; 32];
    let v4_id = register_v4_on_core(&mut core, pool_manager, v4_pool_id, 0);
    let dispatcher = LogDispatcher::with_uniswap_decoders();
    let base_price = U256::from(1u128) << 96;
    let chunk_end = 2_000u64;

    let v3_first = 10u64;
    let v4_only = 11u64;
    let v3_last = 12u64;
    let logs = vec![
        v3_swap_log(
            v3_addr,
            base_price + U256::from(1u64),
            1_100_000,
            1,
            v3_first,
        ),
        v4_swap_log(
            pool_manager,
            v4_pool_id,
            base_price + U256::from(2u64),
            2_100_000,
            2,
            v4_only,
        ),
        v3_swap_log(
            v3_addr,
            base_price + U256::from(3u64),
            1_300_000,
            3,
            v3_last,
        ),
    ];
    core.process_backfill_logs(&dispatcher, &logs, chunk_end)
        .expect("every log carries its own block number");

    let v3 = core
        .pool_slice::<V3PoolState>(v3_id)
        .expect("V3 pool registered");
    assert_eq!(
        v3.journal.len(),
        2,
        "two distinct-block V3 swaps keep two deltas (a chunk stamp would collapse them at {chunk_end})"
    );
    assert_eq!(v3.journal.earliest_block(), Some(v3_first));
    assert_eq!(v3.journal.newest_block(), Some(v3_last));
    assert_eq!(v3.sqrt_price_x96, base_price + U256::from(3u64));
    assert_eq!(v3.liquidity, 1_300_000);
    assert_eq!(v3.tick, 3);
    assert_eq!(v3.update_block, v3_last);

    let v4 = core
        .pool_slice::<V4PoolState>(v4_id)
        .expect("V4 pool registered");
    assert_eq!(v4.journal.len(), 1);
    assert_eq!(v4.journal.earliest_block(), Some(v4_only));
    assert_eq!(v4.journal.newest_block(), Some(v4_only));
    assert_eq!(v4.sqrt_price_x96, base_price + U256::from(2u64));
    assert_eq!(v4.liquidity, 2_100_000);
    assert_eq!(v4.tick, 2);
    assert_eq!(v4.update_block, v4_only);
}
