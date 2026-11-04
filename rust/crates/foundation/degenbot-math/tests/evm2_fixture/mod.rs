//! Offline EVM fixture driver for the Tier-3 oracles.
//!
//! Each Tier-3 oracle deploys committed solc bytecode into a throwaway EVM and compares what that
//! bytecode computes against the Rust math. This module is the throwaway EVM: a fresh in-memory
//! database, contract deployment from creation bytecode, account/storage seeding for fixtures that
//! are installed rather than created, and one call that returns either the output (with its logs)
//! or the revert data.
//!
//! Fixtures execute at [`SPEC`] — Cancun, the hardfork the modern harness builds target — with the
//! nonce check off, because one fixture drives several transactions (`deploy` → `setup` → `probe`)
//! from a single unfunded caller: the transactions are `eth_call`-shaped, not block-shaped.

#![expect(clippy::expect_used, clippy::panic)]

use alloy::consensus::transaction::Recovered;
use alloy::consensus::TxEip1559;
use alloy::primitives::{Address, Bytes, Log, TxKind, U256};

use evm2::bytecode::Bytecode;
use evm2::env::BlockEnvExt;
use evm2::ethereum::{ethereum_tx_registry, RecoveredTxEnvelope, TxEnvelope};
use evm2::evm::{AccountInfo, InMemoryDB};
use evm2::interpreter::InstrStop;
use evm2::{BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Precompiles, SpecId, Version};

/// The hardfork fixtures execute at. Cancun is the target of the modern harness builds (`PUSH0`
/// and `MCOPY` in solc 0.8.2x output) and every older harness's opcodes are long-stable.
pub const SPEC: SpecId = SpecId::CANCUN;

/// Gas ceiling for a creation — the largest fixture (the Balancer/Curve harnesses embed whole math
/// libraries) needs headroom that a 21k-shaped call transaction would never reach.
const DEPLOY_GAS: u64 = 16_700_000;

/// The one caller every fixture transaction comes from. A fixture never asserts on the sender, so
/// a fixed dedicated address (rather than the zero address, which EIP-161 treats as an empty
/// account) keeps the caller's account lifecycle out of the picture.
const CALLER: Address = Address::repeat_byte(0x11);

/// The verdict of one fixture transaction.
#[derive(Debug)]
pub enum Outcome {
    /// The transaction succeeded; `output` is its return data and `logs` its emitted logs.
    Ok { output: Bytes, logs: Vec<Log> },
    /// The transaction reverted; the payload is the raw revert return data.
    Revert(Bytes),
    /// The transaction halted for a reason other than `REVERT` (out of gas, invalid opcode, …).
    Halt(InstrStop),
}

/// A throwaway EVM: one in-memory database plus the state overlay its committed transactions
/// accumulate in.
pub struct Fixture {
    evm: Evm<'static, BaseEvmTypes>,
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Fixture {
    /// Open an empty fixture (no accounts, no storage).
    #[must_use]
    pub fn new() -> Self {
        let mut version = Version::new(SPEC);
        version.features.remove(EvmFeatures::NONCE_CHECK);
        let evm = Evm::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(SPEC, version),
            SPEC,
            BlockEnvExt::default(),
            ethereum_tx_registry(SPEC),
            InMemoryDB::default(),
            Precompiles::base(SPEC),
        );
        Self { evm }
    }

    /// Install `runtime_code` at `address` with `balance` — the fixture form of a contract that
    /// exists before the fixture opens (as opposed to [`Self::deploy`], which runs init code).
    pub fn insert_contract(&mut self, address: Address, runtime_code: &[u8], balance: U256) {
        let info = AccountInfo::default()
            .with_balance(balance)
            .with_code(Bytecode::new_raw(Bytes::copy_from_slice(runtime_code)));
        self.evm
            .state_mut()
            .overlay_db_mut()
            .insert_account_info(&address, info);
    }

    /// Set one persistent storage slot of `address`.
    pub fn set_storage(&mut self, address: Address, slot: U256, value: U256) {
        self.evm
            .state_mut()
            .overlay_db_mut()
            .insert_account_storage(&address, &slot, &value);
    }

    /// Set `address`'s native balance, preserving any code and storage already seeded for it.
    pub fn set_balance(&mut self, address: Address, wei: U256) {
        let overlay = self.evm.state_mut().overlay_db_mut();
        let mut info = overlay.account_info(&address).cloned().unwrap_or_default();
        info.balance = wei;
        overlay.insert_account_info(&address, info);
    }

    /// Run creation bytecode and return the address of the deployed contract, with the resulting
    /// account committed so later calls see the code.
    ///
    /// # Panics
    ///
    /// Panics when the creation fails — a fixture-side problem (bad artifact, out of gas), never a
    /// math divergence, so it must not be reported as a probe result.
    pub fn deploy(&mut self, init_code: impl Into<Bytes>) -> Address {
        let executed = self
            .evm
            .transact(&tx(TxKind::Create, DEPLOY_GAS, init_code.into()))
            .expect("fixture creation transaction is well-formed");
        let result = executed.commit();
        let created = result.created_address;
        created.unwrap_or_else(|| {
            panic!(
                "fixture deploy did not create a contract: status={} stop={:?}",
                result.status, result.stop
            )
        })
    }

    /// Execute one call against `to` with `gas_limit` and commit its state writes, so a multi-step
    /// fixture (`setup` → act → read back) observes each step's post-state.
    ///
    /// # Panics
    ///
    /// Panics when the transaction itself is invalid (a malformed envelope, never a verdict of the
    /// called bytecode): a revert or halt is an [`Outcome`], not a panic.
    pub fn call(&mut self, to: Address, gas_limit: u64, data: impl Into<Bytes>) -> Outcome {
        let executed = self
            .evm
            .transact(&tx(TxKind::Call(to), gas_limit, data.into()))
            .expect("fixture call transaction is well-formed");
        let result = executed.commit();
        if result.status {
            return Outcome::Ok {
                output: result.output,
                logs: result.logs,
            };
        }
        if result.stop == InstrStop::Revert {
            return Outcome::Revert(result.output);
        }
        Outcome::Halt(result.stop)
    }
}

/// The fixture transaction envelope: a zero-fee EIP-1559 call or creation from [`CALLER`].
fn tx(to: TxKind, gas_limit: u64, input: Bytes) -> RecoveredTxEnvelope {
    Recovered::new_unchecked(
        TxEnvelope::Eip1559(TxEip1559 {
            chain_id: 1,
            nonce: 0,
            gas_limit,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            to,
            value: U256::ZERO,
            input,
            ..Default::default()
        }),
        CALLER,
    )
}
