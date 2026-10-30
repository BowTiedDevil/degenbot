//! The evm2 session — the settle verb + typed view reads over the shared
//! per-block EVM, mirroring `sim::evm::session` (`SimSession`).
//!
//! The isolation contract is the same shape stated in evm2's own verbs:
//! [`Evm2SimSession::run`] is `transact` + `discard` — the run's writes
//! leave in the returned value's account changes and the shared DB keeps
//! READ caches only, so a run is invisible to the next run and to view
//! reads. The escape hatch ([`Evm2SimSession::evm_mut`]) hands out the raw
//! EVM for bundles that need journaled accumulation across calls; its
//! caller owns the same contract the revm side documents.

use alloy::primitives::{Address, Bytes, U256};
use degenbot_pools::{
    decode_v2_reserves_word, decode_v3_slot0, ConcentratedLiquiditySlotLayout, V2ReservesParts,
    V3Slot0Parts, V2_RESERVES_SLOT,
};

use ::evm2::ethereum::RecoveredTxEnvelope;
use ::evm2::evm::TxResult;
use ::evm2::{BaseEvmTypes, Evm};

use crate::sim::evm::RunVerdict;

/// The session's error type — the DB read class and the transact-refusal
/// class, mirroring `SimError`'s split.
#[derive(Debug, thiserror::Error)]
pub enum Evm2SimError {
    /// The layered DB could not answer a view read.
    #[error("database read failed: {0}")]
    Database(String),
    /// The EVM refused the envelope (validation or handler error).
    #[error("transact refused: {0}")]
    Transact(String),
}

/// One settled run: the verdict, the gas burned, the top-level return data,
/// and the storage writes it produced (captured from the transaction's
/// account changes before the scratch was discarded).
#[derive(Debug)]
pub struct Evm2SimRun {
    /// How the run ended (the engine-neutral verdict shared with the revm
    /// session).
    pub verdict: RunVerdict,
    /// The envelope's gas used.
    pub gas_used: u64,
    /// The call's return bytes, the revert payload, or empty for a halt.
    pub output: Bytes,
    /// Storage writes the run produced: `(address, slot, value)` per written
    /// slot whose value changed, in unspecified order. The settled result the
    /// discard contract removes from everywhere else.
    pub storage_writes: Vec<(Address, U256, U256)>,
}

/// The settle-verb session over the shared evm2 EVM (see the module doc).
pub struct Evm2SimSession<'h, 'a> {
    evm: &'h mut Evm<'a, BaseEvmTypes>,
}

impl<'h, 'a> Evm2SimSession<'h, 'a> {
    /// Wrap the EVM the caller owns for the session's scope.
    #[must_use]
    pub fn new(evm: &'h mut Evm<'a, BaseEvmTypes>) -> Self {
        Self { evm }
    }

    /// Run ONE transaction envelope and settle it — `transact` + `discard`
    /// (the never-commit contract; see the module doc).
    ///
    /// # Errors
    ///
    /// [`Evm2SimError::Transact`] when the EVM refuses the envelope.
    pub fn run(&mut self, tx: &RecoveredTxEnvelope) -> Result<Evm2SimRun, Evm2SimError> {
        let executed = self
            .evm
            .transact(tx)
            .map_err(|err| Evm2SimError::Transact(err.to_string()))?;
        let storage_writes = executed
            .account_changes()
            .flat_map(|changes| {
                let address = changes.address;
                changes
                    .storage
                    .slots
                    .iter()
                    .filter(move |(_, slot)| slot.value.is_changed())
                    .map(move |(key, slot)| (address, *key, slot.value.current))
            })
            .collect();
        let result = executed.discard();
        Ok(Evm2SimRun {
            verdict: verdict_of(&result),
            gas_used: result.tx_gas_used(),
            output: result.output.clone(),
            storage_writes,
        })
    }

    /// Read one storage word from the settled view — the shared read cache
    /// first, cold misses forwarded down the layered stack. Untracked: the
    /// read never enters a transaction overlay.
    ///
    /// # Errors
    ///
    /// [`Evm2SimError::Database`] when the layered DB cannot answer.
    pub fn read_word(&mut self, address: Address, slot: U256) -> Result<U256, Evm2SimError> {
        self.evm
            .state_mut()
            .storage_slot_untracked(&address, &slot)
            .map_err(|err| Evm2SimError::Database(err.to_string()))
    }

    /// The V2 pair's packed `reserves` word, decoded through the shared slot
    /// table (mirrors `SimSession::read_v2_reserves`).
    ///
    /// # Errors
    ///
    /// [`Evm2SimError::Database`] when the layered DB cannot answer.
    pub fn read_v2_reserves(&mut self, pair: Address) -> Result<V2ReservesParts, Evm2SimError> {
        self.read_word(pair, U256::from(V2_RESERVES_SLOT))
            .map(decode_v2_reserves_word)
    }

    /// A concentrated-liquidity pool's `slot0` word, decoded through the
    /// shared slot table (mirrors `SimSession::read_slot0`).
    ///
    /// # Errors
    ///
    /// [`Evm2SimError::Database`] when the layered DB cannot answer.
    pub fn read_slot0(
        &mut self,
        pool: Address,
        layout: ConcentratedLiquiditySlotLayout,
    ) -> Result<V3Slot0Parts, Evm2SimError> {
        self.read_word(pool, U256::from(layout.slot0_slot()))
            .map(decode_v3_slot0)
    }

    /// The raw evm2 EVM — the documented escape hatch (see the module doc).
    pub fn evm_mut(&mut self) -> &mut Evm<'a, BaseEvmTypes> {
        self.evm
    }
}

/// Map the evm2 result to the engine-neutral verdict.
fn verdict_of(result: &TxResult) -> RunVerdict {
    if result.status {
        RunVerdict::Success
    } else if result.stop == ::evm2::interpreter::InstrStop::Revert {
        RunVerdict::Reverted
    } else {
        RunVerdict::Halted
    }
}
