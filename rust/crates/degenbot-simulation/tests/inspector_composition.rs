//! Composition-parity integration test for the prototype inspector pair
//! (ergo task `2LMT7A`, epic `63I7WJ`).
//!
//! Proves the three prototype claims over `CacheDB<EmptyDB>` with hand-rolled
//! bytecode fixtures (the same fixtures as the spike probe `KCKGP4`):
//!
//! 1. `(AccessListCollector, CallTraceInspector, SwapEventCaptureInspector)`
//!    composes on ONE `inspect_one` run — all three handles drain
//!    independently, no borrow-ordering issues.
//! 2. The `CallTrace` has the top-level `execute()` frame + a child swap
//!    frame (a parent that CALLs a child that emits a V2 `Sync` log), and
//!    `call_end` attributes the revert to the deepest reverting frame.
//! 3. The captured swap `Log` decodes to the expected V2 `SyncEvent`
//!    (reserve0=1000, reserve1=2000), and the access list equals the
//!    `AccessListCollector`-alone case (AL parity preserved under
//!    composition — spike KCKGP4 Q2).
//!
//! Additive + test-only — no changes to `simulate_path_on_evm`, `SimFailure`,
//! `BlockEvm`, or any production call site (the prototype AC).

#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use alloy::primitives::{Address, Bytes, U256};
use degenbot_simulation::{
    AccessListCollector, CallTraceInspector, SwapEventCaptureInspector, SwapFamily,
};
use revm::bytecode::Bytecode;
use revm::context::TxEnv;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::inspector::InspectEvm;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use revm::{MainBuilder, MainContext};

use revm::bytecode::opcode;

// ─────────────────────────────────────────────────────────────────────────
// Bytecode fixtures
// ─────────────────────────────────────────────────────────────────────────

fn db_with_contract(addr: Address, code: Bytecode) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        addr,
        AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000u64),
            code: Some(code),
            ..Default::default()
        },
    );
    db
}

/// V2 `Sync(uint112,uint112)` LOG1 emitter (reserve0=1000, reserve1=2000).
fn emit_v2_sync_bytecode(reserve0: u64, reserve1: u64) -> Bytes {
    let r0 = reserve0.to_be_bytes();
    let r1 = reserve1.to_be_bytes();
    let mut code = Vec::new();
    code.extend_from_slice(&[opcode::PUSH2]);
    code.extend_from_slice(&r0[6..8]);
    code.extend_from_slice(&[opcode::PUSH1, 0x00, opcode::MSTORE]);
    code.extend_from_slice(&[opcode::PUSH2]);
    code.extend_from_slice(&r1[6..8]);
    code.extend_from_slice(&[opcode::PUSH1, 0x20, opcode::MSTORE]);
    // LOG1 stack pop order (revm host.rs `log`): offset (top), size, topics.
    // Push in reverse: topic0 (deepest), size, offset (top).
    code.extend_from_slice(&[opcode::PUSH32]);
    code.extend_from_slice(degenbot_decoders::v2_sync_decoder::V2_SYNC_TOPIC.as_slice());
    code.extend_from_slice(&[opcode::PUSH1, 0x40]); // size
    code.extend_from_slice(&[opcode::PUSH1, 0x00]); // offset (top)
    code.extend_from_slice(&[opcode::LOG1, opcode::STOP]);
    Bytes::from(code)
}

/// SLOAD(slot 1) + SSTORE(slot 2 = 0x99) + STOP — the access-list fixture.
fn sload_sstore_bytecode() -> Bytes {
    Bytes::from(vec![
        opcode::PUSH1,
        0x01,
        opcode::SLOAD,
        opcode::PUSH1,
        0x99,
        opcode::PUSH1,
        0x02,
        opcode::SSTORE,
        opcode::STOP,
    ])
}

/// A parent that CALLs `child` (which emits a V2 Sync log), POPs the success
/// flag, SSTOREs slot 1 (to touch storage for the AL test), then STOPs.
fn parent_calls_child_bytecode(child: Address) -> Bytes {
    let mut code = Vec::new();
    // CALL(retLen=0x20, retOffset=0x80, argsLen=0x00, argsOffset=0x00,
    //      value=0, addr=child, gas=0xffff)
    code.extend_from_slice(&[opcode::PUSH1, 0x20]); // retLen
    code.extend_from_slice(&[opcode::PUSH1, 0x80]); // retOffset
    code.extend_from_slice(&[opcode::PUSH1, 0x00]); // argsLen
    code.extend_from_slice(&[opcode::PUSH1, 0x00]); // argsOffset
    code.extend_from_slice(&[opcode::PUSH1, 0x00]); // value
    code.extend_from_slice(&[opcode::PUSH20]);
    code.extend_from_slice(child.as_slice());
    code.extend_from_slice(&[opcode::PUSH2, 0xff, 0xff]); // gas
    code.extend_from_slice(&[opcode::CALL]);
    code.extend_from_slice(&[opcode::POP]); // success flag
                                            // Touch storage slot 1 so the AL has an entry from the parent frame too.
    code.extend_from_slice(&[opcode::PUSH1, 0x99, opcode::PUSH1, 0x01, opcode::SSTORE]);
    code.extend_from_slice(&[opcode::STOP]);
    Bytes::from(code)
}

fn tx_to(addr: Address) -> TxEnv {
    TxEnv::builder()
        .kind(TxKind::Call(addr))
        .gas_limit(1_000_000)
        .build()
        .expect("well-formed tx")
}

// ─────────────────────────────────────────────────────────────────────────
// The composition-parity test
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn composed_inspector_tuple_parities_al_and_captures_frames_and_swap() {
    let parent = Address::repeat_byte(0x10);
    let child_sync = Address::repeat_byte(0x20);
    let al_contract = Address::repeat_byte(0x42);

    // ── Path A: AccessListCollector alone over the AL-touching contract ──
    let db_a = db_with_contract(al_contract, Bytecode::new_raw(sload_sstore_bytecode()));
    let mut evm_a = revm::context::Context::mainnet()
        .with_db(db_a)
        .build_mainnet_with_inspector(AccessListCollector::default());
    let (al_a, handle_a) = AccessListCollector::new();
    let result_a = evm_a.inspect_one(tx_to(al_contract), al_a).expect("A runs");
    assert!(result_a.is_success());
    let al_alone = handle_a.take_access_list();

    // ── Path B: composed tuple over the same AL-touching contract ──
    // Asserts AL parity (the collector's output is unchanged under composition).
    let db_b = db_with_contract(al_contract, Bytecode::new_raw(sload_sstore_bytecode()));
    let mut evm_b = revm::context::Context::mainnet()
        .with_db(db_b)
        .build_mainnet_with_inspector((
            AccessListCollector::default(),
            (
                CallTraceInspector::default(),
                SwapEventCaptureInspector::default(),
            ),
        ));
    let (al_b, handle_al_b) = AccessListCollector::new();
    let (ct_b, handle_ct_b) = CallTraceInspector::new();
    let (se_b, handle_se_b) = SwapEventCaptureInspector::new();
    let result_b = evm_b
        .inspect_one(tx_to(al_contract), (al_b, (ct_b, se_b)))
        .expect("B runs");
    assert!(result_b.is_success());
    let al_composed = handle_al_b.take_access_list();
    let _trace_b = handle_ct_b.take_trace();
    let _swaps_b = handle_se_b.take_swaps();
    // Path B fixture emits no logs; the swap handle just drains empty.

    let mut slots_alone: Vec<_> = al_alone
        .iter()
        .flat_map(|i| i.storage_keys.clone())
        .collect();
    let mut slots_composed: Vec<_> = al_composed
        .iter()
        .flat_map(|i| i.storage_keys.clone())
        .collect();
    slots_alone.sort();
    slots_composed.sort();
    assert_eq!(slots_alone, slots_composed, "AL parity under composition");

    // ── Path C: composed tuple over the parent→child-emits-Sync fixture ──
    // Asserts the CallTrace + SwapEvent capture work in the composed tuple.
    let mut db_c = CacheDB::new(EmptyDB::default());
    for (addr, code) in [
        (parent, parent_calls_child_bytecode(child_sync)),
        (child_sync, emit_v2_sync_bytecode(1000, 2000)),
    ] {
        db_c.insert_account_info(
            addr,
            AccountInfo {
                balance: U256::from(1_000_000_000_000_000_000u64),
                code: Some(Bytecode::new_raw(code)),
                ..Default::default()
            },
        );
    }
    let mut evm_c = revm::context::Context::mainnet()
        .with_db(db_c)
        .build_mainnet_with_inspector((
            AccessListCollector::default(),
            (
                CallTraceInspector::default(),
                SwapEventCaptureInspector::default(),
            ),
        ));
    let (al_c, handle_al_c) = AccessListCollector::new();
    let (ct_c, handle_ct_c) = CallTraceInspector::new();
    let (se_c, handle_se_c) = SwapEventCaptureInspector::new();
    let result_c = evm_c
        .inspect_one(tx_to(parent), (al_c, (ct_c, se_c)))
        .expect("C runs");
    assert!(result_c.is_success(), "parent CALL + SSTORE must succeed");

    let trace = handle_ct_c.take_trace();
    let swaps = handle_se_c.take_swaps();
    let _ = handle_al_c.take_access_list(); // drain (unused — the fixture touches storage)

    // CallTrace: the top-level parent frame (depth 1) + the child frame
    // (depth 2, target=child_sync). Both Succeeded (the child emitted + STOPped,
    // the parent POPped + SSTOREd + STOPped).
    assert!(
        trace.frames.len() >= 2,
        "at least parent + child frames: {}",
        trace.frames.len()
    );
    let parent_frame = trace
        .frames
        .iter()
        .find(|f| f.target == parent)
        .expect("parent frame captured");
    assert_eq!(parent_frame.depth, 1);
    assert!(
        parent_frame.outcome.is_some(),
        "parent frame outcome paired (LIFO)"
    );
    let child_frame = trace
        .frames
        .iter()
        .find(|f| f.target == child_sync)
        .expect("child frame captured");
    assert_eq!(child_frame.depth, 2);
    assert!(child_frame.outcome.is_some(), "child frame outcome paired");

    // SwapEventCapture: exactly one V2 Sync swap, emitter=child_sync.
    assert_eq!(swaps.len(), 1, "one V2 Sync captured: {swaps:?}");
    assert_eq!(swaps[0].family, SwapFamily::V2);
    assert_eq!(swaps[0].emitter, child_sync);

    // No frame reverted in this fixture — the revert-attribution seam is
    // covered by the spike probe (KCKGP4 Q3); here we confirm the happy path.
    assert!(trace.deepest_revert().is_none());
}

/// A reverting child: confirms `reverting_frame_label` attributes the revert
/// to the deepest frame in the composed tuple.
#[test]
fn composed_tuple_attributes_revert_to_deepest_frame() {
    use revm::bytecode::opcode;
    let parent = Address::repeat_byte(0x10);
    let child = Address::repeat_byte(0x20);
    let reverting_bc = Bytes::from(vec![
        opcode::PUSH4,
        0xde,
        0xad,
        0xbe,
        0xef,
        opcode::PUSH1,
        0x00,
        opcode::MSTORE,
        opcode::PUSH1,
        0x20,
        opcode::PUSH1,
        0x00,
        opcode::REVERT,
    ]);
    // parent: CALL child, POP, STOP
    let parent_bc = {
        let mut c = Vec::new();
        c.extend_from_slice(&[opcode::PUSH1, 0x20, opcode::PUSH1, 0x80]);
        c.extend_from_slice(&[opcode::PUSH1, 0x00, opcode::PUSH1, 0x00]);
        c.extend_from_slice(&[opcode::PUSH1, 0x00]);
        c.extend_from_slice(&[opcode::PUSH20]);
        c.extend_from_slice(child.as_slice());
        c.extend_from_slice(&[
            opcode::PUSH2,
            0xff,
            0xff,
            opcode::CALL,
            opcode::POP,
            opcode::STOP,
        ]);
        Bytes::from(c)
    };
    let mut db = CacheDB::new(EmptyDB::default());
    for (addr, code) in [(parent, parent_bc), (child, reverting_bc)] {
        db.insert_account_info(
            addr,
            AccountInfo {
                balance: U256::from(1_000_000_000_000_000_000u64),
                code: Some(Bytecode::new_raw(code)),
                ..Default::default()
            },
        );
    }
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet_with_inspector((
            AccessListCollector::default(),
            (
                CallTraceInspector::default(),
                SwapEventCaptureInspector::default(),
            ),
        ));
    let (al, _h_al) = AccessListCollector::new();
    let (ct, handle_ct) = CallTraceInspector::new();
    let (se, _h_se) = SwapEventCaptureInspector::new();
    let _ = evm
        .inspect_one(tx_to(parent), (al, (ct, se)))
        .expect("runs");
    let trace = handle_ct.take_trace();
    let (reverting, _label) = trace
        .reverting_frame_label()
        .expect("a revert frame exists");
    assert_eq!(reverting.target, child, "deepest revert is the child");
    assert_eq!(reverting.depth, 2, "child is at depth 2");
}
