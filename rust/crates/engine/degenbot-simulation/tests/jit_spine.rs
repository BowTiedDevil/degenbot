//! The JIT spine's fail-soft + parity gate: a session carrying an
//! interpreter runner produces run-for-run identical sequence outcomes to
//! a runner-less session — every lookup miss interprets (and every
//! compiled program must agree with the interpreter), so the runner can
//! never diverge a verdict. Runs the dual contract (SLOAD/SSTORE per
//! frame, so every step crosses the runner's lookup path) in `Seam`
//! capture mode, the production configuration.

#![expect(clippy::expect_used)]

use std::sync::Arc;

use ::evm2::bytecode::Bytecode;
use ::evm2::ethereum::{RecoveredTxEnvelope, TxEnvelope};
use ::evm2::evm::{AccountInfo, InMemoryDB};
use alloy::consensus::transaction::Recovered;
use alloy::consensus::TxEip1559;
use alloy::primitives::{address, Address, Bytes, TxKind, U256};

use ::evm2_jit::evm2_evm::JitInterpreterRunner;
use ::evm2_jit::runtime::{JitBackend, RuntimeConfig};

use degenbot_simulation::sim::evm2::build_block_evm2;
use degenbot_simulation::sim::evm2::session::{Evm2CaptureMode, Evm2SimSession, Evm2SimStep};

const SENDER: Address = address!("0x3333333333333333333333333333333333333333");
const DUAL: Address = address!("0x4444444444444444444444444444444444444444");

const BLOCK: u64 = 2050;
const TIMESTAMP: u64 = 1_780_000_000;
const BASE_FEE_GWEI: u128 = 1_000_000_000;

/// The session_reads dual contract: empty calldata stores 0x2A at slot 5;
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

fn envelope(data: &[u8]) -> RecoveredTxEnvelope {
    Recovered::new_unchecked(
        TxEnvelope::Eip1559(TxEip1559 {
            chain_id: 1,
            nonce: 0,
            gas_limit: 1_000_000,
            max_fee_per_gas: BASE_FEE_GWEI,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(DUAL),
            value: U256::ZERO,
            input: Bytes::copy_from_slice(data),
            ..Default::default()
        }),
        SENDER,
    )
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
