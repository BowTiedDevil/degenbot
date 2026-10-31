//! The engine's settled-run verb + typed view reads, exercised over a
//! code-only in-memory stack (no node): the reads decode the shared
//! slot-table rows and observe the shared view.

#![expect(clippy::expect_used)]

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, InMemoryDB};
use alloy::primitives::{address, aliases::U112, Address, Bytes, TxKind, U256};
use degenbot_pools::{encode_v3_slot0, pack_v2_reserves_word, V3Slot0Parts, V2_RESERVES_SLOT};
use degenbot_simulation::sim::evm2::frame_replay::Evm2CountingFrameDb;
use degenbot_simulation::sim::evm2::session::{Evm2SimSession, Evm2SimStep};
use degenbot_simulation::sim::frame_replay::FrameRpcCounter;

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

type TestExt = ::evm2::evm::CacheDB<::evm2::evm::Db<Evm2CountingFrameDb<InMemoryDB>>>;

/// The ext with the cold-read counter embedded below its persistent cache (the
/// production shape) plus the two pool accounts a view read addresses.
fn ext() -> (TestExt, std::sync::Arc<FrameRpcCounter>) {
    let counter = std::sync::Arc::new(FrameRpcCounter::default());
    let mut db = InMemoryDB::default();
    for account in [PAIR, POOL] {
        db.insert_account_info(
            &account,
            AccountInfo::default().with_balance(U256::from(1u64)),
        );
    }
    db.insert_account_info(
        &SENDER,
        AccountInfo::default().with_balance(U256::from(1_000_000_000_000_000_000u64)),
    );
    db.insert_account_info(
        &DUAL,
        AccountInfo::default().with_code(Bytecode::new_raw(Bytes::copy_from_slice(DUAL_CODE))),
    );
    let ext = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(Evm2CountingFrameDb::new(
        db,
        std::sync::Arc::clone(&counter),
    )));
    (ext, counter)
}

/// A session over a per-test EVM (the caller keeps the EVM alive).
macro_rules! with_session {
    ($ext:expr, $evm:ident, $session:ident) => {
        let mut $evm = degenbot_simulation::sim::evm2::build_block_evm2(
            ::evm2::evm::Db::new($ext),
            BASE_FEE_GWEI,
            BLOCK,
            TIMESTAMP,
        );
        let mut $session = Evm2SimSession::new(&mut $evm);
    };
}

/// A V2 pair's packed `reserves` word reads back through the shared slot row as
/// typed fields — no caller-owned slot literal or 112-bit mask.
#[test]
fn read_v2_reserves_decodes_the_shared_slot_row() {
    let (mut ext, _counter) = ext();
    let packed = pack_v2_reserves_word(U112::from(1_000u64), U112::from(2_000u64));
    ext.insert_account_storage(
        &PAIR,
        &U256::from(V2_RESERVES_SLOT),
        &U256::from_be_bytes(packed.0),
    );

    with_session!(&mut ext, evm, session);

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
    ext.insert_account_storage(&POOL, &U256::ZERO, &word);

    with_session!(&mut ext, evm, session);

    for layout in [
        degenbot_pools::ConcentratedLiquiditySlotLayout::UniswapV3,
        degenbot_pools::ConcentratedLiquiditySlotLayout::PancakeV3,
    ] {
        let slot0 = session.read_slot0(POOL, layout).expect("slot0 read");
        assert_eq!(slot0.sqrt_price_x96, sqrt_price, "layout {layout:?}");
        assert_eq!(slot0.tick, tick, "layout {layout:?}");
    }
}

/// A view read observes the shared view and cannot change what the next run
/// sees: both reads return the same word. evm2's read-through cache DOES warm
/// on a view read (the second read forwards nothing — a documented engine
/// difference from revm's never-caching `storage_ref`; safe because the
/// cached word is the same backing answer at the same block).
#[test]
fn a_view_read_observes_without_changing_the_view() {
    let (mut ext, counter) = ext();
    with_session!(&mut ext, evm, session);

    let before = counter.reads();
    let first = session
        .read_word(PAIR, U256::from(7u64))
        .expect("first read");
    let after_first = counter.reads();
    let second = session
        .read_word(PAIR, U256::from(7u64))
        .expect("second read");
    let after_second = counter.reads();

    assert!(after_first > before, "a cold read forwards");
    assert_eq!(after_second, after_first, "the second read is warm");
    assert_eq!(first, second, "observing the view cannot change it");
}

/// A transaction for [`DUAL`]: empty calldata writes, non-empty reads.
fn dual_tx(data: &[u8]) -> ::evm2::ethereum::RecoveredTxEnvelope {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(DUAL),
            input: Bytes::copy_from_slice(data),
            gas_limit: 100_000,
            gas_price: BASE_FEE_GWEI,
            ..Default::default()
        }),
        SENDER,
    )
}

/// The 32-byte word a call returned (big-endian, right-aligned).
fn returned_word(output: &Bytes) -> U256 {
    U256::from_be_slice(&output[output.len().saturating_sub(32)..])
}

/// A sequence runs its steps over ONE accumulating overlay: a write step's
/// SSTORE is visible to a later read step, every step's result comes back
/// unsummarized, and the closing settle drops the overlay so the next run
/// sees the pre-state.
#[test]
fn a_sequence_accumulates_across_steps_and_settles_once() {
    let (mut ext, _counter) = ext();
    with_session!(&mut ext, evm, session);

    let sequence = session
        .run_sequence(
            &[
                Evm2SimStep::Transact(dual_tx(&[])),
                Evm2SimStep::Transact(dual_tx(&[0x00])),
            ],
            degenbot_simulation::sim::evm2::session::Evm2CaptureMode::Seam,
        )
        .expect("the sequence settles");

    assert_eq!(sequence.calls.len(), 2, "every step's result is returned");
    assert_eq!(
        returned_word(&sequence.calls[1].output),
        U256::from(0x2A),
        "the read step observes the write step's journal"
    );
    assert!(
        sequence.storage_writes.iter().any(|(address, key, value)| {
            *address == DUAL && *key == U256::from(5u64) && *value == U256::from(0x2A)
        }),
        "the settled state carries the accumulated write"
    );

    let after = session
        .run(&dual_tx(&[0x00]))
        .expect("the follow-up run settles");
    assert_eq!(
        returned_word(&after.output),
        U256::ZERO,
        "the sequence settled (never committed) its journal"
    );
}

/// A refused step ends the sequence carrying the failing step's index, and
/// still settles: the steps before it left nothing behind for the next run.
#[test]
fn a_refused_step_reports_its_index_and_still_settles() {
    let (mut ext, _counter) = ext();
    with_session!(&mut ext, evm, session);

    let refused_envelope = {
        use ::evm2::ethereum::TxEnvelope;
        use alloy::consensus::transaction::Recovered;
        use alloy::consensus::TxLegacy;
        Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(DUAL),
                input: Bytes::copy_from_slice(&[0x00]),
                gas_limit: 100, // below the intrinsic floor: the engine refuses
                gas_price: BASE_FEE_GWEI,
                ..Default::default()
            }),
            SENDER,
        )
    };
    let err = session
        .run_sequence(
            &[
                Evm2SimStep::Transact(dual_tx(&[])),
                Evm2SimStep::Transact(refused_envelope),
            ],
            degenbot_simulation::sim::evm2::session::Evm2CaptureMode::Seam,
        )
        .expect_err("a refused step fails the sequence");
    assert_eq!(
        err.step_index(),
        Some(1),
        "the error carries the failing step's index"
    );

    let after = session
        .run(&dual_tx(&[0x00]))
        .expect("the follow-up run settles");
    assert_eq!(
        returned_word(&after.output),
        U256::ZERO,
        "the failed sequence dropped its partial journal"
    );
}

/// An `Inspect` step drives the capture, and the sequence result carries the
/// drained access list — the per-step capture the settlement bundle's
/// `execute()` step relies on (ADR-019 D3).
#[test]
fn an_inspect_step_drives_the_capture() {
    let (mut ext, _counter) = ext();
    with_session!(&mut ext, evm, session);

    let sequence = session
        .run_sequence(
            &[Evm2SimStep::Inspect(dual_tx(&[0x00]))],
            degenbot_simulation::sim::evm2::session::Evm2CaptureMode::Inspector,
        )
        .expect("the inspect step settles");

    assert!(
        sequence
            .capture
            .access_list
            .0
            .iter()
            .any(|item| item.address == DUAL && !item.storage_keys.is_empty()),
        "the inspected call's warmed slots reach the drained access list"
    );
}

/// The learning path at the engine seam: a completed capture's access list
/// teaches the session's hot-slot store, attributed to the session's replay
/// height — the slot identity (never a value) is what the next anchor's prewarm
/// consumes.
#[test]
fn a_completed_capture_teaches_the_session_hot_slot_store() {
    use degenbot_simulation::sim::evm2::session::Evm2CaptureMode;
    use degenbot_simulation::sim::hot_slots::HOT_SLOT_DECAY_BLOCKS;
    use degenbot_simulation::HotSlotStore;

    let (mut ext, _counter) = ext();
    let hot = std::sync::Arc::new(HotSlotStore::new());
    let mut evm = degenbot_simulation::sim::evm2::build_block_evm2(
        ::evm2::evm::Db::new(&mut ext),
        BASE_FEE_GWEI,
        BLOCK,
        TIMESTAMP,
    );
    let mut session = Evm2SimSession::with_block_env(
        &mut evm,
        ::evm2::env::BlockEnvExt {
            number: U256::from(BLOCK),
            ..Default::default()
        },
    )
    .with_hot_slots(std::sync::Arc::clone(&hot));

    // The production capture mechanism (`Seam`): the captured step's slots come
    // back from the state layer — the write path SSTOREs the dual contract's
    // own slot 5.
    let sequence = session
        .run_sequence(&[Evm2SimStep::Inspect(dual_tx(&[]))], Evm2CaptureMode::Seam)
        .expect("the inspect step settles");
    assert!(
        !sequence.capture.access_list.0.is_empty(),
        "the seam capture recorded the step's slots"
    );

    // Every captured `(address, slot)` is now hot at the session's replay
    // height — the exact set, no more.
    for item in &sequence.capture.access_list.0 {
        for key in &item.storage_keys {
            assert!(
                hot.slots_for(BLOCK, [item.address])
                    .contains(&(item.address, U256::from_be_bytes(key.0))),
                "the capture's slot {key} at {} did not reach the store",
                item.address
            );
        }
    }
    assert!(
        hot.slots_for(BLOCK, [DUAL])
            .contains(&(DUAL, U256::from(5u64))),
        "the captured step's own SSTORE slot is hot"
    );

    // Drift decay: the same store, asked past the window, has dropped the set.
    assert!(
        hot.slots_for(BLOCK + HOT_SLOT_DECAY_BLOCKS, [DUAL])
            .is_empty(),
        "an uncaptured slot decays out of the prewarm's hot set"
    );
}
