//! Spike probe for ergo epic 63I7WJ (task KCKGP4): revm `Inspector` hooks on
//! the simulation stack.
//!
//! Throwaway, `#[ignore]` — run with `cargo test -p degenbot-simulation
//! --test inspector_spike_probe -- --ignored --nocapture`.
//!
//! Answers four empirical questions gating the inspector-diagnostics epic, over
//! `CacheDB<EmptyDB>` with hand-rolled bytecode fixtures (no live RPC):
//!
//! 1. **LOG capture inside a call.** Does `Inspector::log` + `log_full` fire
//!    for a LOG opcode emitted by a contract called via `inspect_one`? Does
//!    the captured `Log` round-trip through `degenbot_decoders::decode_sync_log`?
//! 2. **Tuple composition.** Does `(AccessListCollector, ProbeInspector)` on
//!    ONE `inspect_one` coexist, both handles drainable independently, AND
//!    the access list parity-equal to the collector-alone case?
//! 3. **`call_end` revert attribution at depth.** Does `call_end` receive the
//!    `CallOutcome` of the *deepest reverting frame* (a child contract), not
//!    just the top-level bubble?
//! 4. **V4 swap-event correctness.** DEFERRED — requires the real V4
//!    PoolManager bytecode + the production DB stack + the transient seeder
//!    (task 5RI47E). Recorded in the spike doc, not tested here.

// revm/Solidity identifiers (LOG, SLOAD, CALL, REVERT, Inspector, etc.) are
// ubiquitous here — match the degenbot-simulation convention.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::cell::RefCell;
use std::rc::Rc;

use alloy::primitives::{Address, Bytes, Log, B256, U256};
use alloy::rpc::types::Log as RpcLog;
use degenbot_decoders::v2_sync_decoder::{decode_sync_log, V2_SYNC_TOPIC};
use degenbot_simulation::AccessListCollector;
use revm::bytecode::opcode;
use revm::bytecode::Bytecode;
use revm::context::TxEnv;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::inspector::Inspector;
use revm::interpreter::{CallInputs, CallOutcome, Interpreter, InterpreterResult};
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use revm::{InspectEvm, MainBuilder, MainContext};

// ─────────────────────────────────────────────────────────────────────────
// Tiny hex encoder (no `hex` crate available to this crate's dev-deps).
// ─────────────────────────────────────────────────────────────────────────
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────
// The probe inspector (mirrors the three reference shapes from
// revm-inspector-42 `src/{test_inspector,eip3155,gas}.rs`)
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CapturedFrame {
    depth: usize,
    caller: Address,
    target: Address,
    selector: [u8; 4],
    gas_limit: u64,
    outcome: Option<FrameOutcome>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum FrameOutcome {
    Success { gas_used: u64, output: Bytes },
    Revert { gas_used: u64, data: Bytes },
    Halt { gas_used: u64 },
}

impl FrameOutcome {
    fn from_result(res: &InterpreterResult) -> Self {
        use revm::interpreter::InstructionResult::*;
        let gas_used = res.gas.total_gas_spent();
        match res.result {
            Revert => Self::Revert {
                gas_used,
                data: res.output.clone(),
            },
            Stop | Return | SelfDestruct => Self::Success {
                gas_used,
                output: res.output.clone(),
            },
            _ => Self::Halt { gas_used },
        }
    }
}

#[derive(Debug, Default)]
struct ProbeRecords {
    frames: Vec<CapturedFrame>,
    logs: Vec<Log>,
    log_full_fired: usize,
    log_fired: usize,
}

/// A probe inspector capturing `call`/`call_end` frames + `log`/`log_full`,
/// mirroring revm's `TestInspector`. Shares an `Rc<RefCell<ProbeRecords>>`
/// (the same handle shape `AccessListCollector` uses) so the caller drains
/// after `inspect_one` moves the inspector into the EVM.
#[derive(Debug, Clone)]
struct ProbeInspector {
    records: Rc<RefCell<ProbeRecords>>,
    depth: Rc<RefCell<usize>>,
}

struct ProbeHandle {
    records: Rc<RefCell<ProbeRecords>>,
}

impl Default for ProbeInspector {
    fn default() -> Self {
        Self {
            records: Rc::new(RefCell::new(ProbeRecords::default())),
            depth: Rc::new(RefCell::new(0)),
        }
    }
}

impl ProbeInspector {
    fn new() -> (Self, ProbeHandle) {
        let records = Rc::new(RefCell::new(ProbeRecords::default()));
        let depth = Rc::new(RefCell::new(0usize));
        (
            Self {
                records: Rc::clone(&records),
                depth,
            },
            ProbeHandle { records },
        )
    }
}

impl ProbeHandle {
    fn drain(&self) -> ProbeRecords {
        let mut r = self.records.borrow_mut();
        let frames = std::mem::take(&mut r.frames);
        let logs = std::mem::take(&mut r.logs);
        let log_full_fired = r.log_full_fired;
        let log_fired = r.log_fired;
        ProbeRecords {
            frames,
            logs,
            log_full_fired,
            log_fired,
        }
    }
}

impl<CTX, INTR: revm::interpreter::InterpreterTypes> Inspector<CTX, INTR> for ProbeInspector {
    fn call(&mut self, _ctx: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        let depth = {
            let mut d = self.depth.borrow_mut();
            *d += 1;
            *d
        };
        let selector: [u8; 4] = match &inputs.input {
            revm::interpreter::CallInput::Bytes(b) => {
                let mut s = [0u8; 4];
                if b.len() >= 4 {
                    s.copy_from_slice(&b[..4]);
                } else if !b.is_empty() {
                    s[..b.len()].copy_from_slice(b);
                }
                s
            }
            revm::interpreter::CallInput::SharedBuffer(_) => [0u8; 4],
        };
        self.records.borrow_mut().frames.push(CapturedFrame {
            depth,
            caller: inputs.caller,
            target: inputs.target_address,
            selector,
            gas_limit: inputs.gas_limit,
            outcome: None,
        });
        None
    }

    fn call_end(&mut self, _ctx: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        // `call_end` fires LIFO (innermost call before its parent). Find the
        // most-recently-pushed frame whose outcome is still unset (the
        // innermost unmatched) and set it — this correctly pairs parent/child
        // even when the parent's `call_end` fires after the child's.
        let mut frames = self.records.borrow_mut();
        if let Some(frame) = frames.frames.iter_mut().rev().find(|f| f.outcome.is_none()) {
            frame.outcome = Some(FrameOutcome::from_result(&outcome.result));
        }
        drop(frames);
        let mut d = self.depth.borrow_mut();
        *d = d.saturating_sub(1);
    }

    fn log(&mut self, _ctx: &mut CTX, log: Log) {
        self.records.borrow_mut().log_fired += 1;
        self.records.borrow_mut().logs.push(log);
    }

    fn log_full(&mut self, _interp: &mut Interpreter<INTR>, _ctx: &mut CTX, log: Log) {
        self.records.borrow_mut().log_full_fired += 1;
        // `log_full` is called AFTER `log` for the same event (the default
        // `log_full` delegates to `log`). If `log` did not fire (a precompile
        // path), push the log here so it's not lost.
        if self.records.borrow().logs.is_empty() {
            self.records.borrow_mut().logs.push(log);
        }
    }
}

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

/// V2 `Sync(uint112,uint112)` LOG1 emitter: stores reserve0 at mem[0..32],
/// reserve1 at mem[32..64], topic at... wait — LOG1 takes topic0 as a stack
/// value (PUSHed), not from memory. So: PUSH32 topic0, PUSH1 size=0x40,
/// PUSH1 offset=0x00, LOG1.
fn emit_v2_sync_bytecode(reserve0: u64, reserve1: u64) -> Bytes {
    let r0 = reserve0.to_be_bytes();
    let r1 = reserve1.to_be_bytes();
    let mut code = Vec::new();
    // MSTORE reserve0 at 0x00 (PUSH2 = 2-byte immediate)
    code.extend_from_slice(&[opcode::PUSH2]);
    code.extend_from_slice(&r0[6..8]);
    code.extend_from_slice(&[opcode::PUSH1, 0x00, opcode::MSTORE]);
    // MSTORE reserve1 at 0x20
    code.extend_from_slice(&[opcode::PUSH2]);
    code.extend_from_slice(&r1[6..8]);
    code.extend_from_slice(&[opcode::PUSH1, 0x20, opcode::MSTORE]);
    // LOG1 stack pop order (revm host.rs `log`): offset (top), size,
    // then topics. So push in REVERSE: topics first (deepest), size, offset
    // (top). Push order: PUSH32 topic0, PUSH1 size=0x40, PUSH1 offset=0x00.
    code.extend_from_slice(&[opcode::PUSH32]);
    code.extend_from_slice(V2_SYNC_TOPIC.as_slice());
    code.extend_from_slice(&[opcode::PUSH1, 0x40]); // size
    code.extend_from_slice(&[opcode::PUSH1, 0x00]); // offset (top)
    code.extend_from_slice(&[opcode::LOG1]);
    code.extend_from_slice(&[opcode::STOP]);
    Bytes::from(code)
}

/// STATICCALL a child that reverts, then POP the flag + STOP.
fn call_reverting_child_bytecode(child_addr: Address) -> Bytes {
    let mut code = Vec::new();
    // STATICCALL(retLen=0x20, retOffset=0x80, argsLen=0x00, argsOffset=0x00, addr, gas)
    code.extend_from_slice(&[opcode::PUSH1, 0x20]); // retLen
    code.extend_from_slice(&[opcode::PUSH1, 0x80]); // retOffset
    code.extend_from_slice(&[opcode::PUSH1, 0x00]); // argsLen
    code.extend_from_slice(&[opcode::PUSH1, 0x00]); // argsOffset
    code.extend_from_slice(&[opcode::PUSH20]);
    code.extend_from_slice(child_addr.as_slice());
    code.extend_from_slice(&[opcode::PUSH2, 0xff, 0xff]); // gas
    code.extend_from_slice(&[opcode::STATICCALL]);
    code.extend_from_slice(&[opcode::POP]);
    code.extend_from_slice(&[opcode::STOP]);
    Bytes::from(code)
}

/// Always REVERT with 0x00..00_deadbeef (32 bytes, right-aligned).
fn reverting_bytecode() -> Bytes {
    let mut code = Vec::new();
    code.extend_from_slice(&[opcode::PUSH4, 0xde, 0xad, 0xbe, 0xef]);
    code.extend_from_slice(&[opcode::PUSH1, 0x00, opcode::MSTORE]);
    code.extend_from_slice(&[opcode::PUSH1, 0x20, opcode::PUSH1, 0x00, opcode::REVERT]);
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

// ─────────────────────────────────────────────────────────────────────────
// The EVM builder
// ─────────────────────────────────────────────────────────────────────────

type SpikeCtx = revm::handler::MainnetContext<CacheDB<EmptyDB>>;

fn build_evm_with_default_inspector<I: Default>(
    db: CacheDB<EmptyDB>,
) -> revm::MainnetEvm<SpikeCtx, I> {
    revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet_with_inspector(I::default())
}

fn tx_to(addr: Address) -> TxEnv {
    TxEnv::builder()
        .kind(TxKind::Call(addr))
        .gas_limit(1_000_000)
        .build()
        .expect("well-formed tx")
}

// ─────────────────────────────────────────────────────────────────────────
// Q1 — LOG capture inside a call
// ─────────────────────────────────────────────────────────────────────────

/// Q1: `Inspector::log` + `log_full` fire for a LOG1 emitted inside
/// `inspect_one`, and the captured `Log` round-trips through `decode_sync_log`.
#[test]
#[ignore = "spike probe (KCKGP4) — run with --ignored --nocapture"]
fn spike_q1_log_capture_inside_call() {
    let contract = Address::repeat_byte(0x42);
    let bc = emit_v2_sync_bytecode(1000, 2000);
    println!("[Q1] bytecode hex = 0x{}", hex_encode(bc.as_ref()));
    let db = db_with_contract(contract, Bytecode::new_raw(bc));

    // Build with a default (unused) inspector, then swap in the probe via
    // `inspect_one(tx, probe)` — the access_list.rs parity-test pattern.
    let mut evm: revm::MainnetEvm<SpikeCtx, ProbeInspector> = build_evm_with_default_inspector(db);
    let (probe, handle) = ProbeInspector::new();
    let result = evm
        .inspect_one(tx_to(contract), probe)
        .expect("inspect runs");
    let records = handle.drain();

    println!("\n[Q1] log_fired={}", records.log_fired);
    println!("[Q1] log_full_fired={}", records.log_full_fired);
    println!("[Q1] captured {} logs:", records.logs.len());
    println!("[Q1] frames captured: {}", records.frames.len());
    println!(
        "[Q1] result success={} gas_used={}",
        result.is_success(),
        result.tx_gas_used()
    );
    match result.output() {
        Some(o) => println!("[Q1] result output=0x{}", hex_encode(o)),
        None => println!("[Q1] result output=<none>"),
    }
    for (i, log) in records.logs.iter().enumerate() {
        println!(
            "[Q1]   log[{i}] addr={} topic0={} data=0x{}",
            log.address,
            log.topics()
                .first()
                .map(|t| hex_encode(t.as_slice()))
                .unwrap_or_default(),
            hex_encode(log.data.data.as_ref()),
        );
    }
    println!("[Q1] inspect_one result success? {}", result.is_success());

    assert!(
        result.is_success(),
        "the emit-and-stop contract must succeed"
    );
    assert!(
        !records.logs.is_empty(),
        "at least one LOG must be captured"
    );
    // FINDING: for LOG opcodes, revm calls `log_full` (with the interpreter),
    // NOT `log`. `log` fires only for frame-init value-transfer logs
    // (interpreter = None path in `inspect_logs`). So the swap-event capture
    // inspector must override `log_full`, and it receives the interpreter
    // (for the emitter address via `interp.input.target_address`).
    assert!(
        records.log_full_fired >= 1,
        "Inspector::log_full must fire for LOG opcodes"
    );
    // `log` does NOT fire for instruction-emitted logs — no assertion on it.

    // KEY FINDING: `Inspector::log` hands out `primitives::Log`, but the
    // decoders consume `alloy::rpc::types::Log` (an RPC wrapper around
    // `primitives::Log` + optional block/tx metadata). The captured log must
    // be wrapped (all metadata `None`) before decoding. The impl-definition
    // task should decide whether the decoders accept `primitives::Log`
    // directly (they only read `.topics()`/`.data`, both on `primitives::Log`).
    let rpc_log = RpcLog {
        inner: records.logs[0].clone(),
        block_hash: None,
        block_number: None,
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    };
    let decoded = decode_sync_log(&rpc_log).expect("decode must succeed");
    println!("[Q1] decoded SyncEvent: {decoded:?}");
    assert_eq!(decoded.pool_address, contract);
    assert_eq!(decoded.reserve0.to::<u64>(), 1000);
    assert_eq!(decoded.reserve1.to::<u64>(), 2000);

    println!("[Q1] ANSWER: for LOG opcodes, Inspector::log_full fires (with the");
    println!("[Q1]   interpreter); Inspector::log does NOT fire for instruction-emitted");
    println!("[Q1]   logs (it fires only for frame-init value-transfer logs, the");
    println!("[Q1]   interpreter=None path in inspect_logs). The captured Log is a");
    println!("[Q1]   primitives::Log; the decoders consume alloy::rpc::types::Log (an");
    println!("[Q1]   RPC wrapper) — a wrap (all metadata None) is needed before");
    println!("[Q1]   decode_sync_log. decode round-trips (addr + reserves match).");
}

// ─────────────────────────────────────────────────────────────────────────
// Q2 — tuple composition
// ─────────────────────────────────────────────────────────────────────────

/// Q2: `(AccessListCollector, ProbeInspector)` composes on one `inspect_one`;
/// both handles drain independently; the access list is parity-equal to the
/// collector-alone case.
#[test]
#[ignore = "spike probe (KCKGP4) — run with --ignored --nocapture"]
fn spike_q2_tuple_composition() {
    let contract = Address::repeat_byte(0x42);
    let bc = Bytecode::new_raw(sload_sstore_bytecode());

    // Path A — AccessListCollector alone (the parity baseline).
    let db_a = db_with_contract(contract, bc.clone());
    let mut evm_a: revm::MainnetEvm<SpikeCtx, AccessListCollector> =
        build_evm_with_default_inspector(db_a);
    let (al_a, handle_a) = AccessListCollector::new();
    let _ = evm_a.inspect_one(tx_to(contract), al_a).expect("A runs");
    let al_alone = handle_a.take_access_list();

    // Path B — composed tuple `(AccessListCollector, ProbeInspector)`.
    let db_b = db_with_contract(contract, bc);
    let mut evm_b: revm::MainnetEvm<SpikeCtx, (AccessListCollector, ProbeInspector)> =
        build_evm_with_default_inspector(db_b);
    let (al_b, handle_al_b) = AccessListCollector::new();
    let (probe_b, handle_probe_b) = ProbeInspector::new();
    let _ = evm_b
        .inspect_one(tx_to(contract), (al_b, probe_b))
        .expect("B runs");

    let al_composed = handle_al_b.take_access_list();
    let probe_records = handle_probe_b.drain();

    println!(
        "\n[Q2] access list (collector alone): {} items",
        al_alone.len()
    );
    println!(
        "[Q2] access list (composed tuple): {} items",
        al_composed.len()
    );
    println!(
        "[Q2] probe frames captured (composed): {}",
        probe_records.frames.len()
    );

    let mut slots_alone: Vec<B256> = al_alone
        .iter()
        .flat_map(|i| i.storage_keys.clone())
        .collect();
    let mut slots_composed: Vec<B256> = al_composed
        .iter()
        .flat_map(|i| i.storage_keys.clone())
        .collect();
    slots_alone.sort();
    slots_composed.sort();
    assert_eq!(
        slots_alone, slots_composed,
        "AL slot set must be parity-equal"
    );
    assert!(
        !probe_records.frames.is_empty(),
        "probe captured frames in the composed tuple"
    );

    println!("[Q2] ANSWER: (AccessListCollector, ProbeInspector) composes on one");
    println!("[Q2]   inspect_one. Both handles drain independently. The AL is");
    println!("[Q2]   parity-equal to the collector-alone case (slot sets match).");
}

// ─────────────────────────────────────────────────────────────────────────
// Q3 — call_end revert attribution at depth
// ─────────────────────────────────────────────────────────────────────────

/// Q3: `call_end` receives the `CallOutcome` of the *deepest reverting frame*
/// (the child), not just the top-level bubble.
#[test]
#[ignore = "spike probe (KCKGP4) — run with --ignored --nocapture"]
fn spike_q3_call_end_revert_at_depth() {
    let parent = Address::repeat_byte(0x10);
    let child = Address::repeat_byte(0x20);

    let mut db = CacheDB::new(EmptyDB::default());
    for (addr, code) in [
        (parent, call_reverting_child_bytecode(child)),
        (child, reverting_bytecode()),
    ] {
        db.insert_account_info(
            addr,
            AccountInfo {
                balance: U256::from(1_000_000_000_000_000_000u64),
                code: Some(Bytecode::new_raw(code)),
                ..Default::default()
            },
        );
    }

    let mut evm: revm::MainnetEvm<SpikeCtx, ProbeInspector> = build_evm_with_default_inspector(db);
    let (probe, handle) = ProbeInspector::new();
    let result = evm.inspect_one(tx_to(parent), probe).expect("inspect runs");
    let records = handle.drain();

    println!("\n[Q3] top-level success? {}", result.is_success());
    println!("[Q3] captured {} call frames:", records.frames.len());
    for f in &records.frames {
        println!(
            "[Q3]   depth={} caller={} target={} selector=0x{} gas_limit={} outcome={:?}",
            f.depth,
            f.caller,
            f.target,
            hex_encode(&f.selector),
            f.gas_limit,
            f.outcome
        );
    }

    // The top-level STATICCALL swallows the child revert (parent POPs the 0
    // success flag + STOPs → top-level Success). The CHILD frame's call_end
    // carries the Revert outcome with 0xdeadbeef.
    let child_frame = records
        .frames
        .iter()
        .find(|f| f.target == child)
        .expect("a frame targeting the child must be captured");
    println!(
        "[Q3] child frame depth={} outcome={:?}",
        child_frame.depth, child_frame.outcome
    );
    assert_eq!(child_frame.depth, 2, "child call is at depth 2 (parent=1)");
    match &child_frame.outcome {
        Some(FrameOutcome::Revert { data, .. }) => {
            println!("[Q3] child revert data = 0x{}", hex_encode(data));
            assert!(
                data.ends_with(&[0xde, 0xad, 0xbe, 0xef]),
                "revert data must end with 0xdeadbeef"
            );
        }
        other => panic!("child frame must be a Revert, got {other:?}"),
    }

    // Contrast: a top-LEVEL revert (no child) — the reverting frame is depth 1.
    let mut db2 = CacheDB::new(EmptyDB::default());
    db2.insert_account_info(
        child,
        AccountInfo {
            balance: U256::from(1_000_000_000_000_000_000u64),
            code: Some(Bytecode::new_raw(reverting_bytecode())),
            ..Default::default()
        },
    );
    let mut evm2: revm::MainnetEvm<SpikeCtx, ProbeInspector> =
        build_evm_with_default_inspector(db2);
    let (probe2, handle2) = ProbeInspector::new();
    let _ = evm2
        .inspect_one(tx_to(child), probe2)
        .expect("inspect runs");
    let records2 = handle2.drain();
    let top = &records2.frames[0];
    println!(
        "[Q3] top-level-only revert: depth={} target={} outcome={:?}",
        top.depth, top.target, top.outcome
    );

    println!("[Q3] ANSWER: call_end receives the CallOutcome of the DEEPEST");
    println!("[Q3]   reverting frame (the child at depth 2), carrying its revert");
    println!("[Q3]   data — not just the top-level bubble. The reverting target");
    println!("[Q3]   + selector are visible at the reverting frame's call_end.");
}

// ─────────────────────────────────────────────────────────────────────────
// Q4 — V4 swap-event correctness (DEFERRED)
// ─────────────────────────────────────────────────────────────────────────

/// Q4: V4 Swap-event capture — DEFERRED. Requires the real V4 PoolManager
/// bytecode over the production DB stack with the transient seeder (5RI47E).
#[test]
#[ignore = "spike probe (KCKGP4) — run with --ignored --nocapture"]
fn spike_q4_v4_swap_event_deferred() {
    println!("\n[Q4] DEFERRED: V4 Swap-event capture requires the real V4");
    println!("[Q4]   PoolManager bytecode over the production DB stack with the");
    println!("[Q4]   transient seeder (task 5RI47E). This CacheDB<EmptyDB> probe");
    println!("[Q4]   cannot emit a V4 Swap event. Recorded as blocked on the");
    println!("[Q4]   production stack / 5RI47E in the spike doc.");
}
