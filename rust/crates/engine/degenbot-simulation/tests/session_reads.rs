//! The engine's settled-run verb + typed view reads, exercised over a
//! code-only `CacheDB` (no node): the reads decode the shared slot-table rows
//! and observe the shared view without warming it.

#![expect(clippy::expect_used)]

use alloy::primitives::{address, aliases::U112, Address, Bytes, U256};
use degenbot_pools::{encode_v3_slot0, pack_v2_reserves_word, V3Slot0Parts, V2_RESERVES_SLOT};
use degenbot_simulation::sim::evm::frame_replay::{CountingFrameDb, FrameRpcCounter};
use degenbot_simulation::{AccessListCollector, SimSession, SimStep};
use revm::context::Context as RevmContext;
use revm::context::TxEnv;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::state::AccountInfo;
use revm::{MainBuilder, MainContext};

const PAIR: Address = address!("0x1111111111111111111111111111111111111111");
const POOL: Address = address!("0x2222222222222222222222222222222222222222");
const SENDER: Address = address!("0x3333333333333333333333333333333333333333");
const DUAL: Address = address!("0x4444444444444444444444444444444444444444");

const BLOCK: u64 = 2050;
const TIMESTAMP: u64 = 1_780_000_000;
const BASE_FEE_GWEI: u128 = 1_000_000_000;

/// Dual-mode contract: EMPTY calldata stores 0x2A at its own slot 5;
/// non-empty calldata returns that slot's value. SLOAD/SSTORE run at the
/// contract's own address, so a journaled write from one step is observable in
/// the next — the settlement bundle's accumulator shape.
const DUAL_CODE: &[u8] = &[
    0x36, // CALLDATASIZE
    0x60, 0x0A, // PUSH1 0x0A (read path)
    0x57, // JUMPI (calldata non-empty -> read)
    0x60, 0x2A, 0x60, 0x05, 0x55, // write: SSTORE slot 5 = 0x2A
    0x00, // STOP
    0x5B, // 0x0A JUMPDEST
    0x60, 0x05, 0x54, // SLOAD slot 5
    0x60, 0x00, 0x52, // MSTORE
    0x60, 0x20, 0x60, 0x00, 0xF3, // RETURN 32 bytes
];

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
    db.insert_account_info(
        SENDER,
        AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000u64),
            ..Default::default()
        },
    );
    db.insert_account_info(
        DUAL,
        AccountInfo {
            code: Some(revm::bytecode::Bytecode::new_raw(Bytes::copy_from_slice(
                DUAL_CODE,
            ))),
            ..Default::default()
        },
    );
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

/// A transaction for [`DUAL`]: empty calldata writes, non-empty reads.
fn dual_tx(data: &[u8]) -> TxEnv {
    TxEnv::builder()
        .caller(SENDER)
        .kind(revm::primitives::TxKind::Call(DUAL))
        .data(Bytes::copy_from_slice(data))
        .gas_limit(100_000)
        .gas_price(BASE_FEE_GWEI)
        .build()
        .expect("valid dual TxEnv")
}

/// The 32-byte word a call returned (big-endian, right-aligned).
fn returned_word(output: &Bytes) -> U256 {
    U256::from_be_slice(&output[output.len().saturating_sub(32)..])
}

/// A sequence runs its steps over ONE journal: a write step's SSTORE is visible
/// to a later read step, every step's result comes back unsummarized, and the
/// closing settle clears the journal so the next run sees the pre-state.
#[test]
fn a_sequence_accumulates_across_steps_and_settles_once() {
    let (mut ext, _counter) = ext();
    let mut evm = degenbot_simulation::build_block_evm(&mut ext, BASE_FEE_GWEI, BLOCK, TIMESTAMP);
    let mut session = SimSession::new(&mut evm);

    let sequence = session
        .run_sequence(vec![
            SimStep::Transact(dual_tx(&[])),
            SimStep::Transact(dual_tx(&[0x00])),
        ])
        .expect("the sequence settles");

    assert_eq!(sequence.calls.len(), 2, "every step's result is returned");
    assert_eq!(
        sequence.calls[1].output().map(returned_word),
        Some(U256::from(0x2A)),
        "the read step observes the write step's journal"
    );
    assert_eq!(
        sequence
            .state
            .get(&DUAL)
            .and_then(|account| account.storage.get(&U256::from(5u64)))
            .map(|slot| slot.present_value),
        Some(U256::from(0x2A)),
        "the settled state carries the accumulated write"
    );

    let after = session
        .run(dual_tx(&[0x00]))
        .expect("the follow-up run settles");
    assert_eq!(
        returned_word(&after.output),
        U256::ZERO,
        "the sequence settled (finalized, never committed) its journal"
    );
}

/// A step revm refuses ends the sequence carrying the failing step's index, and
/// still settles: the steps before it left nothing behind for the next run.
#[test]
fn a_refused_step_reports_its_index_and_still_settles() {
    let (mut ext, _counter) = ext();
    let mut evm = degenbot_simulation::build_block_evm(&mut ext, BASE_FEE_GWEI, BLOCK, TIMESTAMP);
    let mut session = SimSession::new(&mut evm);

    let mut refused = dual_tx(&[0x00]);
    refused.gas_limit = 100; // below the intrinsic floor: revm refuses the envelope
    let err = session
        .run_sequence(vec![
            SimStep::Transact(dual_tx(&[])),
            SimStep::Transact(refused),
        ])
        .expect_err("a refused step fails the sequence");
    assert_eq!(
        err.step_index(),
        Some(1),
        "the error carries the failing step's index"
    );

    let after = session
        .run(dual_tx(&[0x00]))
        .expect("the follow-up run settles");
    assert_eq!(
        returned_word(&after.output),
        U256::ZERO,
        "the failed sequence finalized its partial journal"
    );
}

/// An `Inspect` step drives the attached inspector and the caller's paired
/// handle drains what it captured — the per-step injection the settlement
/// bundle's `execute()` step relies on.
#[test]
fn an_inspect_step_drives_the_attached_inspector() {
    let (mut ext, _counter) = ext();
    let mut evm = RevmContext::mainnet()
        .with_db(&mut ext)
        .build_mainnet_with_inspector(AccessListCollector::default());
    let mut session = SimSession::new(&mut evm);

    let (collector, handle) = AccessListCollector::new();
    session
        .run_sequence(vec![SimStep::Inspect(dual_tx(&[0x00]), collector)])
        .expect("the inspect step settles");

    let access_list = handle.take_access_list();
    assert!(
        access_list
            .0
            .iter()
            .any(|item| item.address == DUAL && !item.storage_keys.is_empty()),
        "the inspected call's warmed slots reach the paired handle"
    );
}
