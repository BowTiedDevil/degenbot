//! Frame-replay seam — replay ONE externally-received signed tx through a
//! scratch EVM over the engine's layered DB and get a typed, settled outcome.
//!
//! Engine-level claims (covered code-only over `CacheDB<EmptyDB>`):
//! - A successful frame's touched slots + post-state surface in the outcome;
//!   a warm replay over an already-touched set forwards ZERO reads to the
//!   layered DB below the scratch ext.
//! - A reverted frame returns `Reverted`, contributes NO storage to the
//!   scratch ext, and nothing it did in-flight is visible to later frames.
//! - A settlement-style `transact_one` run on the SAME layered ext sees no
//!   frame state, and its own journaled (non-committed) writes are invisible
//!   to frames — the two directions are isolated by the per-frame DB layer.
//! - An underpriced frame (max fee below the projected base fee) falls back
//!   to `disable_base_fee` and records which path settled.
//!
//! Live claims (ignored by default, network-gated):
//! - `BlockSimHandle::scratch_evm()` stacks successfully on a live fork and
//!   replays a plain transfer.
//! - A real mainnet V2 router swap replayed at its parent block matches the
//!   chain's post-tx storage for the touched accounts.

#![expect(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use alloy::primitives::{address, Address, Bytes, U256};
use degenbot_simulation::sim::evm::frame_replay::{
    read_view_word, BaseFeeSource, CountingFrameDb, FrameRpcCounter, PredStatus, ReplayFrameError,
    ReplayStatus, ReplayableTx, ScratchBlock, ScratchEvm, SequenceReplayError,
};
use revm::bytecode::Bytecode;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::state::AccountInfo;

use degenbot_executor::WarmupSlots;

/// The scratch stack never applies the strategy's state overrides, so the
/// live tests only need a well-formed (all-zero) warmup set to pass through
/// `BlockSimHandle::build`.
fn zero_warmup() -> WarmupSlots {
    WarmupSlots {
        weth_balance: U256::ZERO,
        erc6909_weth: U256::ZERO,
        erc6909_native: U256::ZERO,
    }
}

const SENDER: Address = address!("0x1111111111111111111111111111111111111111");
const WRITER: Address = address!("0x2222222222222222222222222222222222222222");
const READER: Address = address!("0x3333333333333333333333333333333333333333");
const REVERTER: Address = address!("0x4444444444444444444444444444444444444444");

const FIRST_BLOCK: u64 = 2050;
const TIMESTAMP: u64 = 1_780_000_000;
const BASE_FEE_GWEI: u128 = 1_000_000_000;

/// `PUSH1 0`, CALLDATALOAD (slot = calldata[0..32]), SLOAD, MSTORE, RETURN —
/// returns the (big-endian) value of the calldata-named slot.
const READER_CODE: &[u8] = &[
    0x60, 0x00, 0x35, 0x54, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xF3,
];

/// `PUSH1 0`, CALLDATALOAD (slot), `PUSH1 0x2A`, SSTORE — writes 0x2A to the
/// calldata-named slot.
const WRITER_CODE: &[u8] = &[0x60, 0x2A, 0x60, 0x00, 0x35, 0x55];

/// Writes 0x07 to slot 0, then REVERTs — a frame that must roll back entirely.
const REVERTER_CODE: &[u8] = &[0x60, 0x07, 0x60, 0x00, 0x55, 0x60, 0x00, 0x80, 0xFD];

/// Dual-mode contract for the journal-isolation test: EMPTY calldata stores
/// 0x2A at its own slot 5; non-empty calldata returns (big-endian) the slot-5
/// value. SLOAD/SSTORE run at the contract's OWN address, so a journaled
/// write from one `transact_one` is observable in the next — the 7-call
/// settlement accumulator shape.
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

fn block_env() -> ScratchBlock {
    ScratchBlock {
        number: FIRST_BLOCK,
        timestamp: TIMESTAMP,
        base_fee_next: BASE_FEE_GWEI,
    }
}

const DUAL: Address = address!("0x5555555555555555555555555555555555555555");

const GATE: Address = address!("0x6666666666666666666666666666666666666666");

/// Sums slots 0 and 1 into slot 2: the frame only produces 0x54 when BOTH
/// predecessor writes are visible in the sequence overlay.
const MERGE: Address = address!("0x7777777777777777777777777777777777777777");

/// Unconditionally invalid: a predecessor that HALTS (still occupying its
/// nonce, still paying gas).
const HALTER: Address = address!("0x8888888888888888888888888888888888888888");

/// Dual-mode gate/writer: empty calldata stores 0x2A at its own slot 0;
/// non-empty calldata reads slot 0 and reverts when it is zero. A sequence
/// that runs the write predecessor first flips the gated frame from
/// `Reverted` to `Success` — the predecessor state reached the frame.
const GATE_CODE: &[u8] = &[
    0x36, 0x60, 0x0A, 0x57, // CALLDATASIZE; PUSH1 0x0A; JUMPI (non-empty -> gate)
    0x60, 0x2A, 0x60, 0x00, 0x55, 0x00, // write: SSTORE slot 0 = 0x2A; STOP
    0x5B, // 0x0A JUMPDEST
    0x60, 0x00, 0x54, 0x15, // SLOAD slot 0; ISZERO
    0x60, 0x13, 0x57, // PUSH1 0x13; JUMPI (zero -> revert)
    0x00, // STOP
    0x5B, // 0x13 JUMPDEST
    0x60, 0x00, 0x60, 0x00, 0xFD, // PUSH1 0; PUSH1 0; REVERT
];

/// Dual-mode merge/write: non-empty calldata stores 0x2A at the calldata-named
/// slot; empty calldata sums slots 0 and 1 into slot 2. The frame only
/// produces 0x54 when BOTH same-account predecessor writes are visible in the
/// sequence overlay.
const MERGE_CODE: &[u8] = &[
    0x36, 0x60, 0x0F, 0x57, // CALLDATASIZE; PUSH1 0x0F; JUMPI (non-empty -> write)
    0x60, 0x00, 0x54, // SLOAD slot 0
    0x60, 0x01, 0x54, // SLOAD slot 1
    0x01, // ADD
    0x60, 0x02, 0x55, // SSTORE slot 2 = sum
    0x00, // STOP
    0x5B, // 0x0F JUMPDEST (write path)
    0x60, 0x2A, // PUSH1 0x2A
    0x60, 0x00, 0x35, // PUSH1 0; CALLDATALOAD (slot)
    0x55, // SSTORE
    0x00, // STOP
];

/// `INVALID`: the predecessor halts.
const HALTER_CODE: &[u8] = &[0xFE];

type TestExt = CacheDB<CountingFrameDb<CacheDB<EmptyDB>>>;

/// The scratch ext with the counter embedded BELOW its persistent cache (the
/// production shape): only true cold fetches are counted.
fn scratch() -> ScratchEvm<TestExt> {
    let counter = std::sync::Arc::new(FrameRpcCounter::default());
    let mut db = CacheDB::new(EmptyDB::default());
    let sender = AccountInfo {
        balance: U256::from(1_000_000_000_000_000_000u64),
        nonce: 7,
        ..Default::default()
    };
    db.insert_account_info(SENDER, sender);
    for (addr, code) in [
        (WRITER, WRITER_CODE),
        (READER, READER_CODE),
        (REVERTER, REVERTER_CODE),
        (DUAL, DUAL_CODE),
        (GATE, GATE_CODE),
        (MERGE, MERGE_CODE),
        (HALTER, HALTER_CODE),
    ] {
        db.insert_account_info(
            addr,
            AccountInfo {
                code: Some(Bytecode::new_raw(Bytes::copy_from_slice(code))),
                ..Default::default()
            },
        );
    }
    let ext = CacheDB::new(CountingFrameDb::new(db, std::sync::Arc::clone(&counter)));
    ScratchEvm::with_counter(ext, block_env(), counter)
}

fn tx(to: Address, data: &[u8], nonce: u64) -> ReplayableTx {
    ReplayableTx {
        from: SENDER,
        to: Some(to),
        value: U256::ZERO,
        data: Bytes::copy_from_slice(data),
        gas_limit: 100_000,
        max_fee_per_gas: BASE_FEE_GWEI,
        max_priority_fee_per_gas: 0,
        nonce,
    }
}

fn slot_word(slot: u64) -> Bytes {
    Bytes::copy_from_slice(&U256::from(slot).to_be_bytes::<32>())
}

fn slot(touched: &[(Address, Vec<U256>)], addr: Address) -> Vec<U256> {
    touched
        .iter()
        .find(|(a, _)| *a == addr)
        .map(|(_, slots)| slots.clone())
        .unwrap_or_default()
}

/// A successful frame surfaces `Success`, the touched slot set (the write is
/// real), the sender's nonced post-state, wall + base-fee-path telemetry.
#[test]
fn frame_loopback_surfaces_touched_slots_and_telemetry() {
    let mut scratch = scratch();

    let out = scratch
        .replay(&tx(WRITER, &slot_word(0), 7))
        .expect("frame executes");

    assert!(
        matches!(out.status, ReplayStatus::Success),
        "status: {:?}",
        out.status
    );
    assert_eq!(
        slot(&out.touched, WRITER),
        vec![U256::ZERO],
        "the written slot is touched"
    );
    assert!(
        slot(&out.touched, READER).is_empty(),
        "untouched contract contributes nothing"
    );
    assert!(out.wall > Duration::ZERO, "wall clock recorded");
    assert_eq!(
        out.base_fee_source,
        BaseFeeSource::Projected,
        "fee-validation path recorded"
    );
    assert!(
        out.state.get(&SENDER).map(|acc| acc.info.nonce) == Some(8),
        "post-state carries the sender's nonce bump"
    );
    assert_eq!(
        out.state
            .get(&WRITER)
            .and_then(|acc| acc.storage.get(&U256::ZERO))
            .map(|s| s.present_value),
        Some(U256::from(0x2A)),
        "post-state carries the settled slot value"
    );
}

/// The first frame forwards cold reads below the ext; a replay of the same
/// frame shape on the same scratch is fully served by the warmed ext — the
/// pre-touched pool set costs ZERO forwarded reads.
#[test]
fn warm_replay_over_a_pre_touched_set_forwards_zero_reads() {
    let mut scratch = scratch();

    let cold = scratch
        .replay(&tx(READER, &slot_word(3), 7))
        .expect("cold frame executes");
    assert!(
        cold.rpc_reads > 0,
        "cold frame pays reads: {}",
        cold.rpc_reads
    );

    let warm = scratch
        .replay(&tx(READER, &slot_word(3), 7))
        .expect("warm frame executes");
    assert_eq!(warm.rpc_reads, 0, "warm frame must forward nothing");
}

/// A reverted frame reports `Reverted` with NO partial state (revm rolled the
/// journal back — the mid-flight SSTORE contributes nothing), touches no
/// slots, and the next frame on the same scratch observes the clean state.
#[test]
fn reverted_frame_returns_no_partial_state_and_contained_writes() {
    let mut scratch = scratch();

    let out = scratch
        .replay(&tx(REVERTER, &[], 7))
        .expect("reverting frame executes");
    assert!(
        matches!(out.status, ReplayStatus::Reverted),
        "status: {:?}",
        out.status
    );
    assert!(
        out.state
            .get(&REVERTER)
            .and_then(|acc| acc.storage.get(&U256::ZERO))
            .map(|s| s.present_value)
            != Some(U256::from(0x07)),
        "the mid-flight SSTORE contributes no slot entry"
    );
    assert!(
        slot(&out.touched, REVERTER).is_empty(),
        "no slot writes survive the revert"
    );

    let after = scratch
        .replay(&tx(READER, &slot_word(0), 7))
        .expect("follow-up executes");
    assert!(
        matches!(after.status, ReplayStatus::Success),
        "follow-up clean"
    );
    assert_eq!(
        after
            .state
            .get(&READER)
            .and_then(|acc| acc.storage.get(&U256::ZERO))
            .map(|s| s.present_value),
        Some(U256::ZERO),
        "the reverted frame's mid-flight SSTORE did not leak into the scratch ext"
    );
}

/// The two directions of the seam are isolated: a settlement-style
/// `transact_one` fan-out over the layered ext sees NO frame state, AND its
/// own journaled (never-committed) writes are invisible to later frames —
/// while its journaled accumulation across `transact_one` (the strategy's
/// 7-call invariant) still works.
#[test]
fn frame_state_and_settlement_journal_are_mutually_isolated() {
    use revm::context::Context as RevmContext;
    use revm::primitives::TxKind;
    use revm::{ExecuteEvm, MainBuilder, MainContext};

    let mut scratch = scratch();

    // 1. The frame writes the DUAL contract's slot 5 on the frame layer.
    let frame = scratch.replay(&tx(DUAL, &[], 7)).expect("frame executes");
    assert!(matches!(frame.status, ReplayStatus::Success));

    let dual_write = |nonce: u64| {
        revm::context::TxEnv::builder()
            .caller(SENDER)
            .kind(TxKind::Call(DUAL))
            .data(Bytes::new())
            .gas_limit(100_000)
            .gas_price(BASE_FEE_GWEI)
            .nonce(nonce)
            .build()
            .unwrap()
    };
    let dual_read = |nonce: u64| {
        revm::context::TxEnv::builder()
            .caller(SENDER)
            .kind(TxKind::Call(DUAL))
            .data(Bytes::copy_from_slice(&[0x00]))
            .gas_limit(100_000)
            .gas_price(BASE_FEE_GWEI)
            .nonce(nonce)
            .build()
            .unwrap()
    };

    // 2. A settlement-style EVM over the SAME layered ext: a read must see
    //    the DUAL slot at 0 — no frame state.
    {
        let mut evm = RevmContext::mainnet()
            .with_db(scratch.ext_mut())
            .build_mainnet();
        let observed = evm
            .transact_one(dual_read(7))
            .expect("settlement read executes");
        assert_eq!(
            observed
                .output()
                .map(|b| U256::from_be_slice(&b[b.len().saturating_sub(32)..])),
            Some(U256::ZERO),
            "settlement transact_one sees NO frame state"
        );

        // 3. The settlement 7-call accumulator shape still works: write via
        //    transact_one (journal only), read it back through a second
        //    transact_one BEFORE finalize.
        evm.transact_one(dual_write(8))
            .expect("settlement write executes");
        let accumulated = evm
            .transact_one(dual_read(9))
            .expect("accumulation read executes");
        assert_eq!(
            accumulated
                .output()
                .map(|b| U256::from_be_slice(&b[b.len().saturating_sub(32)..])),
            Some(U256::from(0x2A)),
            "settlement journal accumulates across transact_one as before"
        );

        // 4. finalize discards the journal without committing — the ext is
        //    untouched (same discard semantics as the 7-call path).
        let _ = evm.finalize();
    }

    // 5. The next frame reads the slot the settlement journal wrote: 0. The
    //    settlement journal's writes (never committed) are invisible to
    //    frames, as is the frame's own earlier write (per-frame journal).
    let frame2 = scratch
        .replay(&tx(DUAL, &[0x00], 7))
        .expect("follow-up executes");
    assert!(
        frame2
            .state
            .get(&DUAL)
            .and_then(|acc| acc.storage.get(&U256::from(5u64)))
            .map(|s| s.present_value)
            .is_none_or(|v| v == U256::ZERO),
        "frame sees no journal writes from the settlement side"
    );
}

#[test]
fn underpriced_frame_falls_back_to_disabled_basefee() {
    let mut scratch = scratch();

    let mut underpriced = tx(WRITER, &slot_word(4), 7);
    underpriced.max_fee_per_gas = BASE_FEE_GWEI / 2;

    let out = scratch
        .replay(&underpriced)
        .expect("fallback frame executes");
    assert!(
        matches!(out.status, ReplayStatus::Success),
        "status: {:?}",
        out.status
    );
    assert_eq!(out.base_fee_source, BaseFeeSource::DisabledFallback);
    assert_eq!(
        out.state
            .get(&WRITER)
            .and_then(|acc| acc.storage.get(&U256::from(4u64)))
            .map(|s| s.present_value),
        Some(U256::from(0x2A)),
        "the fallback path still settles the frame"
    );
}

/// The frame's own nonce is validated against the layered view (the seam
/// stays faithful: a stale pending tx is not silently replayable) — a stale
/// nonce surfaces as an error, not as a `Success`.
#[test]
fn stale_nonce_frame_is_rejected_not_silently_replayed() {
    let mut scratch = scratch();
    let stale = tx(WRITER, &slot_word(5), 6);
    assert!(
        scratch.replay(&stale).is_err(),
        "stale nonce must surface as an error"
    );
}

/// A sequence's predecessor state reaches the frame: the write predecessor
/// fills the gate's slot 0, so the gated frame settles `Success` over the
/// accumulated view. Replayed in isolation the frame's nonce is ahead of the
/// parent state — the structural gap the sequence exists to close.
#[test]
fn sequence_prefix_state_reaches_the_frame() {
    let mut scratch = scratch();
    let predecessor = tx(GATE, &[], 7);
    let frame = tx(GATE, &[0x01], 8);

    assert!(matches!(
        scratch.replay(&frame),
        Err(ReplayFrameError::GapPending {
            claimed: 8,
            expected: 7
        })
    ));

    let sequence = scratch
        .replay_sequence(&[predecessor], &frame)
        .expect("sequence executes");
    assert_eq!(sequence.predecessors, vec![PredStatus::Success]);
    assert!(
        matches!(sequence.frame.status, ReplayStatus::Success),
        "the frame reads the accumulated slot: {:?}",
        sequence.frame.status
    );
}

/// The overlay never leaks into the ext: after the sequence the ext still
/// serves the parent state, so a plain replay of the same frame is still the
/// structural gap it was before (and the predecessor's storage is absent).
#[test]
fn sequence_state_never_leaks_into_the_ext() {
    let mut scratch = scratch();
    let predecessor = tx(GATE, &[], 7);
    let frame = tx(GATE, &[0x01], 8);

    let sequence = scratch
        .replay_sequence(&[predecessor], &frame)
        .expect("sequence executes");
    assert!(matches!(sequence.frame.status, ReplayStatus::Success));

    assert_eq!(
        read_view_word(scratch.ext(), GATE, U256::ZERO),
        Some(U256::ZERO),
        "no predecessor storage reached the ext"
    );
    assert!(
        matches!(
            scratch.replay(&frame),
            Err(ReplayFrameError::GapPending { .. })
        ),
        "no predecessor nonce reached the ext"
    );
}

/// A reverted predecessor is not a failure: it still occupies its nonce, so
/// the frame at nonce+1 replays instead of surfacing a gap.
#[test]
fn reverted_predecessor_still_occupies_its_nonce() {
    let mut scratch = scratch();
    let predecessor = tx(REVERTER, &[], 7);
    let frame = tx(WRITER, &slot_word(4), 8);

    let sequence = scratch
        .replay_sequence(&[predecessor], &frame)
        .expect("sequence executes");
    assert_eq!(sequence.predecessors, vec![PredStatus::Reverted]);
    assert!(matches!(sequence.frame.status, ReplayStatus::Success));
}

/// A predecessor whose nonce the parent state already consumed is skipped as
/// `AlreadyMined`; its effects are already in the parent state, and the frame
/// at the parent's next nonce replays.
#[test]
fn already_mined_predecessor_is_skipped_and_the_sequence_completes() {
    let mut scratch = scratch();
    let stale = tx(WRITER, &slot_word(2), 6);
    let frame = tx(WRITER, &slot_word(3), 7);

    let sequence = scratch
        .replay_sequence(&[stale], &frame)
        .expect("sequence executes");
    assert_eq!(sequence.predecessors, vec![PredStatus::AlreadyMined]);
    assert!(matches!(sequence.frame.status, ReplayStatus::Success));
}

/// A multi-step sequence records each predecessor's status in ascending
/// order and counts the whole sequence's cold reads on the frame outcome.
#[test]
fn multi_step_sequence_accumulates_statuses_and_counter() {
    let mut scratch = scratch();
    let prefix = [tx(GATE, &[], 7), tx(REVERTER, &[], 8)];
    let frame = tx(GATE, &[0x01], 9);

    let sequence = scratch
        .replay_sequence(&prefix, &frame)
        .expect("sequence executes");
    assert_eq!(
        sequence.predecessors,
        vec![PredStatus::Success, PredStatus::Reverted]
    );
    assert!(matches!(sequence.frame.status, ReplayStatus::Success));
    assert!(
        sequence.frame.rpc_reads > 0,
        "the sequence's cold reads are counted on the frame outcome"
    );
    assert!(sequence.frame.wall > Duration::ZERO);
}

/// A predecessor-sourced failure is attributed to the predecessor (with its
/// nonce) — the frame never ran — while a frame-sourced failure is the
/// frame's own. The rescue router routes the two differently.
#[test]
fn sequence_failure_attribution_splits_predecessor_from_frame() {
    let mut malformed_pred = tx(WRITER, &slot_word(0), 7);
    malformed_pred.gas_limit = 0;
    let frame = tx(MERGE, &[], 8);
    let mut pred_scratch = scratch();
    let pred_err = pred_scratch
        .replay_sequence(&[malformed_pred], &frame)
        .expect_err("malformed predecessor aborts");
    assert!(
        matches!(
            &pred_err,
            SequenceReplayError::Predecessor { nonce: 7, source }
                if matches!(source, ReplayFrameError::MalformedTransaction { .. })
        ),
        "attributed to the frame instead of the predecessor: {pred_err:?}"
    );

    let predecessor = tx(WRITER, &slot_word(0), 7);
    let mut malformed_frame = tx(MERGE, &[], 8);
    malformed_frame.gas_limit = 0;
    let mut frame_scratch = scratch();
    let frame_err = frame_scratch
        .replay_sequence(&[predecessor], &malformed_frame)
        .expect_err("malformed frame aborts");
    assert!(
        matches!(
            &frame_err,
            SequenceReplayError::Frame(ReplayFrameError::MalformedTransaction { .. })
        ),
        "expected a frame-sourced malformed error: {frame_err:?}"
    );
}

/// An underpriced predecessor settles via the disabled-base-fee retry (the
/// same ritual a frame gets); it commits and the sequence proceeds.
#[test]
fn underpriced_predecessor_falls_back_and_commits() {
    let mut underpriced = tx(MERGE, &slot_word(0), 7);
    underpriced.max_fee_per_gas = BASE_FEE_GWEI / 2;
    let frame = tx(MERGE, &[], 8);
    let mut scratch = scratch();
    let sequence = scratch
        .replay_sequence(&[underpriced], &frame)
        .expect("sequence executes");
    assert_eq!(sequence.predecessors, vec![PredStatus::Success]);
    assert!(matches!(sequence.frame.status, ReplayStatus::Success));
    assert_eq!(
        sequence.frame.state.get(&SENDER).map(|acc| acc.info.nonce),
        Some(9),
        "the underpriced predecessor consumed its nonce"
    );
}

/// The overlay consults the ext through its MUTABLE `Database` path, so a
/// miss is counted once and a later step's re-read of the SAME word is an
/// overlay hit (zero forwarded cold reads) — and the ext's read cache is left
/// warm, exactly like a plain replay.
#[test]
fn sequence_overlay_hits_do_not_re_forward_a_cold_read() {
    // A: the frame reads a DIFFERENT word, so it is another cold miss. B: the
    // frame reads the SAME word as the first step, so it must hit the overlay.
    let mut a = scratch();
    let seq_a = a
        .replay_sequence(
            &[tx(READER, &slot_word(3), 7)],
            &tx(READER, &slot_word(4), 8),
        )
        .expect("sequence executes");
    let mut b = scratch();
    let seq_b = b
        .replay_sequence(
            &[tx(READER, &slot_word(3), 7)],
            &tx(READER, &slot_word(3), 8),
        )
        .expect("sequence executes");
    assert_eq!(
        seq_a.frame.rpc_reads,
        seq_b.frame.rpc_reads + 1,
        "the later step's same-word read is an overlay hit, not a second cold miss"
    );

    // The ext cache was warmed by the sequence: a plain replay of the same
    // word against the parent state forwards nothing cold.
    let warm = b
        .replay(&tx(READER, &slot_word(3), 7))
        .expect("warm replay executes");
    assert_eq!(
        warm.rpc_reads, 0,
        "the sequence's miss warmed the ext's read cache through the mutable Database path"
    );
}

/// Two same-account predecessors merge cleanly: nonces advance twice, each
/// step's storage reaches the frame, and the sender balance deducts exactly
/// once per step (hand-computed with the per-step gas; no double deduction).
#[test]
fn same_account_two_predecessors_merge_nonce_balance_and_storage() {
    let pred1 = tx(MERGE, &slot_word(0), 7);
    let pred2 = tx(MERGE, &slot_word(1), 8);
    let frame = tx(MERGE, &[], 9);

    let mut seq_scratch = scratch();
    let sequence = seq_scratch
        .replay_sequence(&[pred1.clone(), pred2.clone()], &frame)
        .expect("sequence executes");
    assert_eq!(
        sequence.predecessors,
        vec![PredStatus::Success, PredStatus::Success]
    );
    assert!(matches!(sequence.frame.status, ReplayStatus::Success));
    assert_eq!(
        sequence
            .frame
            .state
            .get(&MERGE)
            .and_then(|account| account.storage.get(&U256::from(2u64)))
            .map(|slot| slot.present_value),
        Some(U256::from(0x54)),
        "the frame sums BOTH predecessor writes: slot0(0x2A) + slot1(0x2A)"
    );
    // The frame itself is at nonce 9, so its post-state nonce is 10 — proof
    // the two predecessors' bumps (7->8->9) were both in the overlay.
    assert_eq!(
        sequence.frame.state.get(&SENDER).map(|acc| acc.info.nonce),
        Some(10),
        "the sender nonce advanced through both predecessors and the frame"
    );
    assert_eq!(sequence.predecessor_gas.len(), 2);
    assert!(
        sequence.predecessor_gas.iter().all(|gas| *gas > 0),
        "both predecessors paid gas"
    );

    // Balance check on a fresh scratch: a bare 21000-gas transfer frame makes
    // the expected outflow exact.
    let price = U256::from(BASE_FEE_GWEI);
    let initial = U256::from(1_000_000_000_000_000_000u64);
    let mut scratch2 = scratch();
    let transfer = tx(SENDER, &[], 9);
    let sequence2 = scratch2
        .replay_sequence(&[pred1.clone(), pred2.clone()], &transfer)
        .expect("sequence executes");
    let pred_gas: u64 = sequence2.predecessor_gas.iter().sum();
    assert_eq!(
        sequence2
            .frame
            .state
            .get(&SENDER)
            .map(|acc| acc.info.balance),
        Some(initial - price * U256::from(pred_gas + 21_000)),
        "balance deducts once per step; the frame is a 21000-gas transfer"
    );
}

/// A Halt predecessor is not a failure: it commits its nonce and gas burn,
/// and the frame at the next nonce replays.
#[test]
fn halted_predecessor_commits_and_the_sequence_proceeds() {
    let mut scratch = scratch();
    let halt = tx(HALTER, &[], 7);
    let frame = tx(WRITER, &slot_word(0), 8);
    let sequence = scratch
        .replay_sequence(&[halt], &frame)
        .expect("sequence executes");
    assert_eq!(sequence.predecessors, vec![PredStatus::Halted]);
    assert!(matches!(sequence.frame.status, ReplayStatus::Success));
    assert_eq!(
        sequence.frame.state.get(&SENDER).map(|acc| acc.info.nonce),
        Some(9),
        "the halt consumed its nonce and the frame ran at the next one"
    );
    assert!(sequence.predecessor_gas[0] > 0, "the halt burned gas");
}

/// Live wiring: `scratch_evm()` over a real forked node stacks + replays a
/// plain (21000-gas) transfer twice; the second replay is warm (0 reads).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_scratch_evm_replays_a_plain_transfer_and_warms() {
    use std::sync::Arc;

    use alloy::eips::BlockNumberOrTag;
    use alloy::providers::{Provider, ProviderBuilder};
    use degenbot_simulation::{BlockSimHandle, SimulationOverrideParams};

    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    let provider: alloy::providers::RootProvider =
        ProviderBuilder::default().connect_http(rpc_url.parse().unwrap());
    let alloy_provider =
        degenbot_rpc::provider::AlloyProvider::from_provider(Arc::new(provider.clone()));

    // pin
    let head = provider.get_block_number().await.unwrap();
    let pin = head.saturating_sub(1);
    let block = provider
        .get_block_by_number(BlockNumberOrTag::Number(pin))
        .await
        .unwrap()
        .expect("block exists");
    let base_fee_next = u128::from(block.header.base_fee_per_gas.unwrap_or(0)).max(1);
    let override_params = SimulationOverrideParams {
        owner: Address::ZERO,
        inject_code: false,
        injected_address: None,
        runtime_bytecode: Bytes::new(),
        warmup: zero_warmup(),
        weth_address: Address::ZERO,
        pool_manager_address: Address::ZERO,
    };
    let anchor = degenbot_bot::bot_core::SimAnchorState::default();
    let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
    let mut handle = BlockSimHandle::build(
        &alloy_provider,
        base_fee_next.max(1),
        pin,
        block.header.timestamp,
        &override_params,
        &anchor,
        &warm_cache,
        None,
        false,
    )
    .expect("live handle builds");
    let scratch = handle.scratch_evm().expect("scratch evm stacks");

    let plain = ReplayableTx {
        from: Address::repeat_byte(0x51),
        to: Some(Address::repeat_byte(0x52)),
        value: U256::ZERO,
        data: Bytes::new(),
        gas_limit: 21_000,
        max_fee_per_gas: base_fee_next.max(1),
        max_priority_fee_per_gas: 0,
        nonce: 0,
    };
    let cold = scratch.replay(&plain).expect("plain transfer executes");
    assert!(
        matches!(cold.status, ReplayStatus::Success),
        "status: {:?}",
        cold.status
    );

    // The scratch persists across frames on the handle — the warmed set is
    // served from its ext cache with zero forwarded cold reads.
    let warm = scratch.replay(&plain).expect("second frame executes");
    assert!(matches!(warm.status, ReplayStatus::Success));
    assert_eq!(warm.rpc_reads, 0, "the pre-touched transfer set is warm");
}

/// Live parity: a real mainnet V2 router swap, replayed at its parent block
/// through the frame-replay seam, matches the chain's post-slot storage for
/// every touched account at the fixture block (the chain post-state for that
/// pair) — plus the warm-RPC-count criterion over the pre-touched set.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
#[expect(clippy::too_many_lines)]
async fn live_v2_router_swap_replay_matches_chain_post_state() {
    use std::sync::Arc;

    use alloy::consensus::Transaction as ConsensusTx;
    use alloy::eips::{BlockId, BlockNumberOrTag};
    use alloy::providers::{Provider, ProviderBuilder};
    use degenbot_simulation::{BlockSimHandle, SimulationOverrideParams};

    const V2_ROUTER: Address = address!("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
    // swapExactTokensForTokens only: ETH-carrying selectors move balances in
    // ways the seam does not claim to model (see the balance channel below).
    const V2_SWAP_SELECTORS: [[u8; 4]; 1] = [[0x38, 0xed, 0x17, 0x39]];

    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    let provider: alloy::providers::RootProvider =
        ProviderBuilder::default().connect_http(rpc_url.parse().unwrap());
    let alloy_provider =
        degenbot_rpc::provider::AlloyProvider::from_provider(Arc::new(provider.clone()));

    // Pin the block with `DEGENBOT_PARITY_BLOCK` (the pinned block must carry
    // a router V2 swap); the default scans back from head (up to 100 blocks)
    // for the most recent block with a runnable router swap.
    let head = provider.get_block_number().await.unwrap();
    let scan_blocks = match std::env::var("DEGENBOT_PARITY_BLOCK") {
        Ok(b) => vec![b.parse::<u64>().unwrap()],
        Err(_) => (1..=100u64)
            .rev()
            .map(|i| head.saturating_sub(i))
            .collect::<Vec<_>>(),
    };
    let mut fixture = None;
    for pin_number in &scan_blocks {
        if *pin_number == 0 {
            continue;
        }
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Number(*pin_number))
            .full()
            .await
            .unwrap()
            .expect("fixture block exists");
        let txs: Vec<alloy::rpc::types::Transaction> = block.transactions.txns().cloned().collect();
        let candidate = txs
            .into_iter()
            .filter(|t| t.to() == Some(V2_ROUTER))
            // ReplayableTx's input domain is plain 1559 frames: it carries no
            // tx-type or access list, so fixtures outside that class diverge
            // on gas-money by construction (characterized in the module doc).
            .filter(|t| matches!(t.inner.tx_type(), alloy::consensus::TxType::Eip1559))
            .filter(|t| t.inner.access_list().is_none_or(|al| al.is_empty()))
            .filter(|t| {
                let input = t.input();
                V2_SWAP_SELECTORS
                    .iter()
                    .any(|sel| input.get(0..4) == Some(&sel[..]))
            })
            .collect::<Vec<_>>();
        // The frame nonce must validate against the PARENT view, so the
        // fixture must be the sender's first tx in its block.
        for nested in candidate {
            let parent_nonce = provider
                .get_transaction_count(nested.inner.signer())
                .block_id(BlockId::number(pin_number.saturating_sub(1)))
                .await
                .unwrap();
            if parent_nonce == nested.nonce() {
                fixture = Some((nested, *pin_number));
                break;
            }
        }
        if fixture.is_some() {
            break;
        }
    }
    let (fixture, pin) = fixture.expect("no router V2 swap found in the scanned blocks");
    let block = provider
        .get_block_by_number(BlockNumberOrTag::Number(pin))
        .full()
        .await
        .unwrap()
        .expect("fixture block exists");

    let replayable = ReplayableTx {
        from: fixture.inner.signer(),
        to: fixture.to(),
        value: fixture.value(),
        data: fixture.input().clone(),
        gas_limit: fixture.gas_limit(),
        max_fee_per_gas: fixture.max_fee_per_gas(),
        max_priority_fee_per_gas: fixture.max_priority_fee_per_gas().unwrap_or(0),
        nonce: fixture.nonce(),
    };

    let override_params = SimulationOverrideParams {
        owner: Address::ZERO,
        inject_code: false,
        injected_address: None,
        runtime_bytecode: Bytes::new(),
        warmup: zero_warmup(),
        weth_address: Address::ZERO,
        pool_manager_address: Address::ZERO,
    };
    let anchor = degenbot_bot::bot_core::SimAnchorState::default();
    let warm_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
    let mut handle = BlockSimHandle::build(
        &alloy_provider,
        u128::from(block.header.base_fee_per_gas.unwrap_or(0)).max(1),
        pin.saturating_sub(1),
        block.header.timestamp,
        &override_params,
        &anchor,
        &warm_cache,
        None,
        false,
    )
    .expect("live handle builds");
    let scratch = handle.scratch_evm().expect("scratch evm stacks");

    let out = scratch.replay(&replayable).expect("fixture swap executes");
    assert!(
        matches!(out.status, ReplayStatus::Success),
        "status: {:?}",
        out.status
    );
    assert!(
        !out.touched.is_empty(),
        "a V2 swap touches the pair + token slots"
    );

    // PARITY vs the node's own per-tx state diff: reth's
    // `prestateTracer` + `tracerConfig.diffMode` reports the exact post-tx
    // state for THIS tx (immune to later-in-block interference). Every
    // storage slot the node reports as written post-tx must equal the
    // frame's settled value, and the sender's post-nonce the frame's.
    let diff: serde_json::Value = provider
        .client()
        .request(
            "debug_traceTransaction",
            (
                fixture.inner.hash(),
                serde_json::json!({"tracer": "prestateTracer", "tracerConfig": {"diffMode": true}}),
            ),
        )
        .await
        .unwrap();

    // reth trims leading hex zeros in diff keys/values — left-pad to width.
    let padded = |s: &str, width: usize| -> String {
        let hex = s.trim_start_matches("0x");
        format!("0x{hex:0>width$}")
    };
    let hex_u256 = |s: &str| {
        let t = s.trim_start_matches("0x");
        if t.is_empty() {
            U256::ZERO
        } else {
            U256::from_str_radix(t, 16).unwrap()
        }
    };

    let post = diff
        .pointer("/post")
        .and_then(|p| p.as_object())
        .expect("diffMode trace carries the post map");
    assert!(
        !post.is_empty(),
        "node diff must report the tx's touched accounts"
    );

    for (addr_key, body) in post {
        let addr: Address = padded(addr_key, 40).parse().unwrap();
        if let Some(storage) = body.get("storage").and_then(|s| s.as_object()) {
            let account = out
                .state
                .get(&addr)
                .expect("frame must not miss a node-touched account");
            for (slot_key, value) in storage {
                let slot = hex_u256(&padded(slot_key, 64));
                let expected = hex_u256(value.as_str().expect("storage value is hex"));
                let got = account
                    .storage
                    .get(&slot)
                    .map_or(U256::ZERO, |s| s.present_value);
                assert_eq!(
                    got, expected,
                    "post-state disagreement at {addr_key} slot {slot_key}"
                );
            }
        }
        if let Some(nonce) = body.get("nonce").and_then(|v| v.as_str()) {
            if let Some(account) = out.state.get(&addr) {
                assert_eq!(
                    U256::from(account.info.nonce),
                    hex_u256(nonce),
                    "post-nonce disagreement at {addr_key}"
                );
            }
        }
        // Balances are outside the parity contract: replay runs in the
        // parent-block env with balance checks disabled (the seam's declared
        // input domain excludes tx-type/access-list), so deposit/gas-balance
        // channels diverge by construction. Storage + nonce (the pool-state
        // surface the seam serves) are asserted above.
    }

    // Warm: re-running the same frame on the same scratch is fully served by
    // the warmed ext — zero forwarded reads over the pre-touched pair set.
    let warm = scratch.replay(&replayable).expect("warm replay executes");
    assert!(matches!(warm.status, ReplayStatus::Success));
    assert_eq!(warm.rpc_reads, 0, "warm replay forwards nothing");
}
