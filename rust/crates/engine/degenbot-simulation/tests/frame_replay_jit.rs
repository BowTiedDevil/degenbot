//! The frame-replay path's compiled-path gate: a scratch carrying the
//! process-shared interpreter runner executes a resident program on every
//! replay after the first, and returns the runner-less verdict.
//!
//! Program residency is the backend's, not the scratch's — each replay builds
//! a FRESH frame EVM, so the first replay's lookup is the only one that can
//! compile. The second replay must therefore take the compiled path (a lookup
//! hit, zero new compilations) with the outcome the runner-less scratch
//! produces; that is the property the strategy loop's per-candidate frame
//! replays depend on.

#![expect(clippy::expect_used)]

use std::sync::Arc;

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, InMemoryDB};
use alloy::primitives::{address, Address, Bytes, U256};

use ::evm2_jit::evm2_evm::JitInterpreterRunner;
use ::evm2_jit::runtime::{JitBackend, RuntimeConfig};

use degenbot_simulation::sim::evm2::frame_replay::Evm2ScratchEvm;
use degenbot_simulation::sim::frame_replay::{ReplayableTx, ScratchBlock};
use degenbot_simulation::sim::RunVerdict;

const SENDER: Address = address!("0x3333333333333333333333333333333333333333");
const DUAL: Address = address!("0x4444444444444444444444444444444444444444");

const BLOCK: u64 = 100;
const BASE_FEE: u128 = 1_000_000_000;

/// The `session_reads` dual contract: empty calldata stores 0x2A at slot 5;
/// non-empty returns it. Code-bearing, so every replay crosses the runner's
/// lookup path.
const DUAL_CODE: &[u8] = &[
    0x36, // CALLDATASIZE
    0x60, 0x0A, // PUSH1 0x0A
    0x57, // JUMPI
    0x60, 0x2A, 0x60, 0x05, 0x55, // SSTORE slot 5 = 0x2A
    0x00, // STOP
    0x5B, // JUMPDEST
    0x60, 0x05, 0x54, // SLOAD slot 5
    0x60, 0x00, 0x52, // MSTORE
    0x60, 0x20, 0x60, 0x00, 0xF3, // RETURN 32
];

fn scratch_db() -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &SENDER,
        AccountInfo::default().with_balance(U256::from(1_000_000_000_000_000_000u64)),
    );
    db.insert_account_info(
        &DUAL,
        AccountInfo::default().with_code(Bytecode::new_raw(Bytes::from_static(DUAL_CODE))),
    );
    db
}

fn scratch_block() -> ScratchBlock {
    ScratchBlock {
        number: BLOCK + 1,
        timestamp: 0,
        base_fee_next: BASE_FEE,
    }
}

fn frame(data: &[u8], nonce: u64) -> ReplayableTx {
    ReplayableTx {
        from: SENDER,
        to: Some(DUAL),
        value: U256::ZERO,
        data: Bytes::copy_from_slice(data),
        gas_limit: 1_000_000,
        max_fee_per_gas: BASE_FEE,
        max_priority_fee_per_gas: 0,
        nonce,
    }
}

fn compiled_backend() -> JitBackend {
    JitBackend::new(RuntimeConfig {
        enabled: true,
        blocking: true,
        ..RuntimeConfig::default()
    })
    .expect("the JIT backend starts")
}

/// The first replay compiles (the lookup can only miss on a cold backend);
/// the second replay of the SAME program is a compiled-path hit that
/// dispatches no compilation, and its outcome is the runner-less outcome.
#[test]
fn the_second_replay_takes_the_compiled_path() {
    let backend = compiled_backend();
    let mut jitted = Evm2ScratchEvm::new(scratch_db(), scratch_block())
        .with_jit(Some(Arc::new(JitInterpreterRunner::new(backend.clone()))));
    let frame = frame(&[], 0);

    let first = jitted.replay(&frame).expect("the first replay settles");
    assert_eq!(
        first.status,
        RunVerdict::Success,
        "the frame writes a slot and stops"
    );
    assert!(
        !first.storage_writes.is_empty(),
        "the frame's write settled"
    );

    let after_first = backend.stats();
    assert_eq!(
        after_first.compilations_succeeded, 1,
        "the cold replay compiles the frame's program exactly once: {after_first:?}"
    );
    assert_eq!(
        after_first.resident_entries, 1,
        "the compiled program is resident in the process-shared backend"
    );

    // The frame EVM was dropped with the first replay; this second replay
    // builds a new one and must still find the program resident.
    let second = jitted.replay(&frame).expect("the second replay settles");
    let after_second = backend.stats();
    assert_eq!(
        after_second.lookup_hits,
        after_first.lookup_hits + 1,
        "the second replay's lookup hits the resident program: {after_first:?} -> {after_second:?}"
    );
    assert_eq!(
        after_second.compilations_dispatched, after_first.compilations_dispatched,
        "a resident program is never recompiled"
    );
    assert_eq!(after_second.compilations_succeeded, 1);

    // The verdict identity: the compiled path settles exactly as the
    // runner-less scratch does.
    let mut plain = Evm2ScratchEvm::new(scratch_db(), scratch_block());
    let baseline = plain
        .replay(&frame)
        .expect("the runner-less replay settles");
    assert_eq!(second.status, baseline.status, "verdict");
    assert_eq!(second.gas_used, baseline.gas_used, "gas");
    assert_eq!(second.output, baseline.output, "output");
    assert_eq!(
        second.storage_writes, baseline.storage_writes,
        "storage writes"
    );
    assert_eq!(second.slot_values, baseline.slot_values, "slot values");
    assert_eq!(second.touched, baseline.touched, "touched slots");
    assert_eq!(
        second.base_fee_source, baseline.base_fee_source,
        "the fallback disposition is runner-independent"
    );
}

/// The sequence verb (the strategy loop's prefix path) installs the same
/// runner: a predecessor and the frame both hit the resident program on the
/// second sequence replay, and the settled sequence matches the runner-less
/// sequence exactly.
#[test]
fn sequence_replays_share_the_resident_program() {
    let backend = compiled_backend();
    let mut jitted = Evm2ScratchEvm::new(scratch_db(), scratch_block())
        .with_jit(Some(Arc::new(JitInterpreterRunner::new(backend.clone()))));
    let prefix = [frame(&[], 0)];
    let frame = frame(&[0x01], 1);

    let first = jitted
        .replay_sequence(&prefix, &frame)
        .expect("the first sequence settles");
    assert_eq!(first.predecessors.len(), 1);
    assert_eq!(
        first.frame.status,
        RunVerdict::Success,
        "the frame reads the predecessor's slot and returns it"
    );
    let after_first = backend.stats();
    assert_eq!(
        after_first.compilations_succeeded, 1,
        "one program, one compile, however many frames executed: {after_first:?}"
    );

    let second = jitted
        .replay_sequence(&prefix, &frame)
        .expect("the second sequence settles");
    let after_second = backend.stats();
    assert_eq!(
        after_second.lookup_hits,
        after_first.lookup_hits + 2,
        "the predecessor and the frame both take the compiled path: \
         {after_first:?} -> {after_second:?}"
    );
    assert_eq!(
        after_second.compilations_succeeded, 1,
        "the second sequence recompiles nothing"
    );

    let mut plain = Evm2ScratchEvm::new(scratch_db(), scratch_block());
    let baseline = plain
        .replay_sequence(&prefix, &frame)
        .expect("the runner-less sequence settles");
    assert_eq!(second.predecessors, baseline.predecessors, "predecessors");
    assert_eq!(
        second.predecessor_gas, baseline.predecessor_gas,
        "predecessor gas"
    );
    assert_eq!(second.frame.status, baseline.frame.status, "frame verdict");
    assert_eq!(second.frame.gas_used, baseline.frame.gas_used, "frame gas");
    assert_eq!(second.frame.output, baseline.frame.output, "frame output");
    assert_eq!(
        second.frame.storage_writes, baseline.frame.storage_writes,
        "frame writes"
    );
}
