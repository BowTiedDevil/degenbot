//! The engine's settled-run verb + typed view reads, exercised over a
//! code-only `CacheDB` (no node): the reads decode the shared slot-table rows
//! and observe the shared view without warming it.

#![expect(clippy::expect_used)]

use alloy::primitives::{address, aliases::U112, Address, U256};
use degenbot_pools::{encode_v3_slot0, pack_v2_reserves_word, V3Slot0Parts, V2_RESERVES_SLOT};
use degenbot_simulation::sim::evm::frame_replay::{CountingFrameDb, FrameRpcCounter};
use degenbot_simulation::SimSession;
use revm::context::Context as RevmContext;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::state::AccountInfo;
use revm::{MainBuilder, MainContext};

const PAIR: Address = address!("0x1111111111111111111111111111111111111111");
const POOL: Address = address!("0x2222222222222222222222222222222222222222");

type TestExt = CacheDB<CountingFrameDb<CacheDB<EmptyDB>>>;

/// The ext with the cold-read counter embedded below its persistent cache (the
/// production shape) plus the two pool accounts a view read addresses.
fn ext() -> (TestExt, std::sync::Arc<FrameRpcCounter>) {
    let counter = std::sync::Arc::new(FrameRpcCounter::default());
    let mut db = CacheDB::new(EmptyDB::default());
    for account in [PAIR, POOL] {
        db.insert_account_info(
            account,
            AccountInfo {
                balance: U256::from(1u64),
                ..Default::default()
            },
        );
    }
    let ext = CacheDB::new(CountingFrameDb::new(db, std::sync::Arc::clone(&counter)));
    (ext, counter)
}

/// A V2 pair's packed `reserves` word reads back through the shared slot row as
/// typed fields — no caller-owned slot literal or 112-bit mask.
#[test]
fn read_v2_reserves_decodes_the_shared_slot_row() {
    let (mut ext, _counter) = ext();
    let packed = pack_v2_reserves_word(U112::from(1_000u64), U112::from(2_000u64));
    ext.insert_account_storage(
        PAIR,
        U256::from(V2_RESERVES_SLOT),
        U256::from_be_bytes(packed.0),
    )
    .expect("seed the reserves word");

    let mut evm = RevmContext::mainnet().with_db(&mut ext).build_mainnet();
    let mut session = SimSession::new(&mut evm);

    let reserves = session.read_v2_reserves(PAIR).expect("reserves read");
    assert_eq!(reserves.reserve0.to::<u128>(), 1_000);
    assert_eq!(reserves.reserve1.to::<u128>(), 2_000);
    assert_eq!(
        reserves.block_timestamp_last, 0,
        "the pack direction carries no timestamp"
    );
    assert_eq!(
        session
            .read_word(PAIR, U256::from(V2_RESERVES_SLOT))
            .expect("raw read"),
        U256::from_be_bytes(packed.0),
        "the typed read is the shared row, decoded"
    );
}

/// Both fork layouts keep `slot0` at their own word-0 slot, and the decode
/// recovers `sqrtPriceX96`/`tick` from it.
#[test]
fn read_slot0_decodes_the_fork_word() {
    let (mut ext, _counter) = ext();
    let sqrt_price = U256::from(1u128 << 96);
    let tick = -8_876;
    let word = encode_v3_slot0(V3Slot0Parts {
        sqrt_price_x96: sqrt_price,
        tick,
        ..Default::default()
    });
    ext.insert_account_storage(POOL, U256::ZERO, word)
        .expect("seed the slot0 word");

    let mut evm = RevmContext::mainnet().with_db(&mut ext).build_mainnet();
    let mut session = SimSession::new(&mut evm);

    for layout in [
        degenbot_pools::ConcentratedLiquiditySlotLayout::UniswapV3,
        degenbot_pools::ConcentratedLiquiditySlotLayout::PancakeV3,
    ] {
        let slot0 = session.read_slot0(POOL, layout).expect("slot0 read");
        assert_eq!(slot0.sqrt_price_x96, sqrt_price, "layout {layout:?}");
        assert_eq!(slot0.tick, tick, "layout {layout:?}");
    }
}

/// A view read observes the shared view and never warms it: two reads of the
/// same un-cached slot each forward a cold read, so observing the per-block
/// view cannot change what the next run sees.
#[test]
fn a_view_read_never_warms_the_shared_cache() {
    let (mut ext, counter) = ext();
    let mut evm = RevmContext::mainnet().with_db(&mut ext).build_mainnet();
    let mut session = SimSession::new(&mut evm);

    let before = counter.reads();
    session
        .read_word(PAIR, U256::from(7u64))
        .expect("first read");
    let after_first = counter.reads();
    session
        .read_word(PAIR, U256::from(7u64))
        .expect("second read");
    let after_second = counter.reads();

    assert!(after_first > before, "a cold read forwards");
    assert!(
        after_second > after_first,
        "the read did not cache the word it observed"
    );
}
