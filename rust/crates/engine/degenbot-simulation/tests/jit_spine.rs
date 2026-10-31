//! The JIT spine's fail-soft + parity gate: a session carrying an
//! interpreter runner produces run-for-run identical sequence outcomes to
//! a runner-less session — every lookup miss interprets (and every
//! compiled program must agree with the interpreter), so the runner can
//! never diverge a verdict. Runs the dual contract (SLOAD/SSTORE per
//! frame, so every step crosses the runner's lookup path) in `Seam`
//! capture mode, the production configuration.
//!
//! The override-key leg pins the AOT cache key: the production sim injects
//! the executor's runtime bytecode straight into the `CacheDB` (never through
//! `CREATE`), and the runner keys its lookup on the executing account's code
//! hash — so the injected account's `code_hash` must be exactly
//! `keccak256(injected bytecode)`, the hash an AOT artifact is keyed by under
//! [`EVM2_PRODUCTION_SPEC`].

#![expect(clippy::expect_used)]

use std::sync::Arc;

use ::evm2::bytecode::Bytecode;
use ::evm2::ethereum::{RecoveredTxEnvelope, TxEnvelope};
use ::evm2::evm::{AccountInfo, Database, InMemoryDB};
use alloy::consensus::transaction::Recovered;
use alloy::consensus::TxEip1559;
use alloy::primitives::{address, keccak256, Address, Bytes, TxKind, KECCAK256_EMPTY, U256};

use ::evm2_jit::evm2_evm::JitInterpreterRunner;
use ::evm2_jit::runtime::{JitBackend, RuntimeCacheKey, RuntimeConfig};

use degenbot_executor::WarmupSlots;
use degenbot_simulation::sim::evm2::build_block_evm2;
use degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC;
use degenbot_simulation::sim::evm2::session::{Evm2CaptureMode, Evm2SimSession, Evm2SimStep};
use degenbot_simulation::sim::evm2::state_override::apply_simulation_overrides;
use degenbot_simulation::sim::state_override::SimulationOverrideParams;
use degenbot_simulation::sim::RunVerdict;

const SENDER: Address = address!("0x3333333333333333333333333333333333333333");
const DUAL: Address = address!("0x4444444444444444444444444444444444444444");
/// The executor address the override injects bytecode at (the same shape the
/// production handle uses: `params.injected_address`).
const EXECUTOR: Address = address!("0x5555555555555555555555555555555555555555");
const WETH: Address = address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const POOL_MANAGER: Address = address!("0x000000000004444c5dc75cB358380D2e3dE08A90");

const BLOCK: u64 = 2050;
const TIMESTAMP: u64 = 1_780_000_000;
const BASE_FEE_GWEI: u128 = 1_000_000_000;

/// The `session_reads` dual contract: empty calldata stores 0x2A at slot 5;
/// non-empty returns it.
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

/// The injected executor's runtime: returns the 32-byte word `0x2A`.
const EXECUTOR_CODE: &[u8] = &[
    0x60, 0x2A, // PUSH1 0x2A
    0x5F, // PUSH0
    0x52, // MSTORE
    0x60, 0x20, 0x5F, // PUSH1 0x20, PUSH0
    0xF3, // RETURN 32
];

fn envelope_to(to: Address, data: &[u8]) -> RecoveredTxEnvelope {
    Recovered::new_unchecked(
        TxEnvelope::Eip1559(TxEip1559 {
            chain_id: 1,
            nonce: 0,
            gas_limit: 1_000_000,
            max_fee_per_gas: BASE_FEE_GWEI,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(to),
            value: U256::ZERO,
            input: Bytes::copy_from_slice(data),
            ..Default::default()
        }),
        SENDER,
    )
}

fn envelope(data: &[u8]) -> RecoveredTxEnvelope {
    envelope_to(DUAL, data)
}

/// The production override shape: the owner (here also the tx sender) is
/// funded, the executor's runtime bytecode is injected at [`EXECUTOR`], and
/// the three warmup slots are touched — the same `SimulationOverrideParams`
/// the handle feeds `apply_simulation_overrides`.
fn override_params() -> SimulationOverrideParams {
    SimulationOverrideParams {
        owner: SENDER,
        inject_code: true,
        injected_address: Some(EXECUTOR),
        runtime_bytecode: Bytes::copy_from_slice(EXECUTOR_CODE),
        warmup: WarmupSlots {
            weth_balance: U256::from(1),
            erc6909_weth: U256::from(2),
            erc6909_native: U256::from(3),
        },
        weth_address: WETH,
        pool_manager_address: POOL_MANAGER,
    }
}

fn run_steps(with_runner: bool) -> degenbot_simulation::sim::evm2::session::Evm2SimSequence {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &SENDER,
        AccountInfo::default().with_balance(U256::from(1_000_000_000_000_000_000u64)),
    );
    db.insert_account_info(
        &DUAL,
        AccountInfo::default().with_code(Bytecode::new_raw(Bytes::copy_from_slice(DUAL_CODE))),
    );
    let mut evm = build_block_evm2(db, BASE_FEE_GWEI, BLOCK, TIMESTAMP);
    let mut session = Evm2SimSession::new(&mut evm);
    if with_runner {
        let backend = JitBackend::new(RuntimeConfig {
            enabled: true,
            blocking: true,
            ..Default::default()
        })
        .expect("jit backend init");
        session = session.with_jit(Some(Arc::new(JitInterpreterRunner::new(backend))));
    }
    session
        .run_sequence(
            &[
                Evm2SimStep::Transact(envelope(&[])),
                Evm2SimStep::Transact(envelope(&[0x00])),
                Evm2SimStep::Inspect(envelope(&[0x00])),
            ],
            Evm2CaptureMode::Seam,
        )
        .expect("the sequence settles")
}

#[test]
fn runner_installed_outcomes_match_runner_less() {
    let plain = run_steps(false);
    let jitted = run_steps(true);

    assert_eq!(plain.calls.len(), jitted.calls.len(), "step count");
    for (i, (a, b)) in plain.calls.iter().zip(jitted.calls.iter()).enumerate() {
        assert_eq!(a.verdict, b.verdict, "step {i} verdict");
        assert_eq!(a.gas_used, b.gas_used, "step {i} gas");
        assert_eq!(a.output, b.output, "step {i} output");
        assert_eq!(a.storage_writes, b.storage_writes, "step {i} writes");
    }
    assert_eq!(
        plain.storage_writes, jitted.storage_writes,
        "sequence writes"
    );
    assert_eq!(
        plain.capture.access_list, jitted.capture.access_list,
        "the seam capture's access list is runner-independent"
    );
    assert_eq!(
        plain.capture.swaps.len(),
        jitted.capture.swaps.len(),
        "swap count"
    );
    assert_eq!(
        plain.capture.log_count, jitted.capture.log_count,
        "log count"
    );
}

#[test]
fn overridden_executor_is_keyed_by_keccak_of_its_injected_bytecode() {
    let mut db = InMemoryDB::default();
    apply_simulation_overrides(&mut db, &override_params());

    let injected_hash = keccak256(EXECUTOR_CODE);
    assert_ne!(
        injected_hash, KECCAK256_EMPTY,
        "the fixture's injected bytecode is not empty"
    );
    let info = db
        .account_info(&EXECUTOR)
        .expect("the override caches the injected executor")
        .clone();
    assert_eq!(
        info.code_hash, injected_hash,
        "the injected account's code hash must be keccak256(injected bytecode) — the \
         hash the runner's lookup keys on, and the hash an AOT artifact is keyed by"
    );
    assert!(
        !info.is_empty_code_hash(),
        "the injected executor must not carry the empty-code hash"
    );

    // The runner keys its lookup on `RuntimeCacheKey { code_hash:
    // interpreter.original_bytecode_hash(), spec_id: host base spec }`, and the
    // interpreter loads the executing code through the account's code hash —
    // so the code resolved here is exactly the code the lookup keys on.
    let resolved = <InMemoryDB as Database>::get_code_by_hash(&mut db, &info.code_hash)
        .expect("the injected code resolves by its code hash");
    assert_eq!(
        resolved.original_bytes(),
        Bytes::copy_from_slice(EXECUTOR_CODE),
        "the code hash resolves to the injected runtime bytecode"
    );
    assert_eq!(resolved.hash_slow(), injected_hash);

    // The AOT artifact key the production spec asks for.
    let aot_key = RuntimeCacheKey {
        code_hash: injected_hash,
        spec_id: EVM2_PRODUCTION_SPEC,
    };
    assert_eq!(aot_key.code_hash, info.code_hash);
}

#[test]
fn overridden_executor_executes_through_the_jit_runner() {
    let mut db = InMemoryDB::default();
    apply_simulation_overrides(&mut db, &override_params());
    let mut evm = build_block_evm2(db, BASE_FEE_GWEI, BLOCK, TIMESTAMP);
    let backend = JitBackend::new(RuntimeConfig {
        enabled: true,
        blocking: true,
        ..Default::default()
    })
    .expect("jit backend init");
    let mut session =
        Evm2SimSession::new(&mut evm).with_jit(Some(Arc::new(JitInterpreterRunner::new(backend))));
    let sequence = session
        .run_sequence(
            &[Evm2SimStep::Transact(envelope_to(EXECUTOR, &[]))],
            Evm2CaptureMode::Seam,
        )
        .expect("the sequence settles");

    let call = &sequence.calls[0];
    assert_eq!(
        call.verdict,
        RunVerdict::Success,
        "the injected executor runs (the runner miss falls back to the interpreter)"
    );
    assert_eq!(
        call.output,
        Bytes::copy_from_slice(&U256::from(42u8).to_be_bytes::<32>()),
        "the executed code is the override-injected runtime"
    );
}
