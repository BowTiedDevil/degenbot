//! The engine's settled-run verb + typed view reads over the per-block EVM.
//!
//! ADR-019 D4/D7 puts the *mechanics* here and the *policy* in the strategy
//! crate: a [`SimSession`] runs one transaction envelope through the shared
//! EVM and answers typed storage reads; what a strategy does with the verdict
//! (profit arithmetic, calldata, fan-out, prioritisation) stays the
//! strategy's. The engine names no strategy type.
//!
//! # One run, one journal, one read view
//!
//! revm splits journalling into `transact_one` (accumulate into the journal) →
//! `finalize` (return the journal's `State` and CLEAR it) → `commit` (write a
//! `State` into the `Database`). On a shared per-block `CacheDB` the third step
//! is the hazard: committing one candidate's `execute()` SSTOREs would leave
//! its post-swap reserves in the read cache every later candidate reads.
//!
//! [`SimSession::run`] is `transact_one` + `finalize` — the pipeline lives
//! inside the verb and the session has no `commit`, so a settled run leaves its
//! results in the returned [`SimRun`] and nowhere else. The view a session
//! answers reads from is therefore always the settled chain view (the shared
//! `CacheDB`'s read cache), never an in-flight journal, and a run is invisible
//! to the next run. `tests/frame_replay.rs`'s
//! `a_run_is_invisible_to_the_next_run_and_to_frames` drives exactly that
//! property through this interface.
//!
//! The escape hatch — [`SimSession::evm_mut`] — is for the settlement bundle
//! whose 7-call accumulation genuinely needs raw revm: three balance reads, an
//! inspector-attached `execute()`, three balance reads, all accumulating in ONE
//! journal. Its safety is the caller's, and its contract is the one `run`
//! performs internally: `finalize` (never `commit`) before returning the EVM.

use alloy::primitives::{Address, Bytes, U256};
use degenbot_pools::{
    decode_v2_reserves_word, decode_v3_slot0, ConcentratedLiquiditySlotLayout, V2ReservesParts,
    V3Slot0Parts, V2_RESERVES_SLOT,
};
use revm::context::TxEnv;
use revm::context_interface::result::{
    EVMError, ExecutionResult, InvalidHeader, InvalidTransaction, ResultAndState,
};
use revm::database_interface::{Database, DatabaseRef};
use revm::state::EvmState;
use revm::ExecuteEvm;

use super::inspectors::SimInspector;

/// The EVM shape a session drives — revm's mainnet EVM over a layered DB.
/// `Db` is the per-block production stack
/// ([`ProductionBlockDb`](super::simulator::ProductionBlockDb)) for the
/// handle's session, and a scratch stack (`&mut ScratchDb`) for a replayed
/// frame.
pub struct SimSession<'h, Db: Database, Insp = SimInspector> {
    evm: &'h mut revm::MainnetEvm<revm::handler::MainnetContext<Db>, Insp>,
}

impl<'h, Db: Database, Insp> SimSession<'h, Db, Insp> {
    /// Wrap the EVM the caller owns for the session's scope. The caller keeps
    /// the block env and cfg (they are the engine's per-block policy); the
    /// session adds the settle verb + the view reads.
    #[must_use]
    pub fn new(evm: &'h mut revm::MainnetEvm<revm::handler::MainnetContext<Db>, Insp>) -> Self {
        Self { evm }
    }

    /// Run ONE transaction envelope and settle it — `transact_one` +
    /// `finalize`, with no `commit` reachable from this surface (see the
    /// module doc).
    ///
    /// The EVM's attached inspector is not driven (a run is not an
    /// `inspect_one`): inspector-captured detail — the access list, the call
    /// trace, the swap events — is what the escape hatch exists for.
    ///
    /// # Errors
    ///
    /// [`SimError::InvalidTransaction`] / [`SimError::InvalidHeader`] when
    /// revm refuses the envelope at this block state;
    /// [`SimError::Database`] when the layered DB cannot answer a read (the
    /// cold-miss class); [`SimError::Custom`] for a non-standard EVM failure.
    pub fn run(&mut self, tx: TxEnv) -> Result<SimRun, SimError> {
        let settled = self.evm.transact(tx).map_err(SimError::from)?;
        Ok(SimRun::settle(settled))
    }

    /// The raw revm EVM — the documented escape hatch.
    ///
    /// Take it only for a bundle that needs what [`Self::run`] deliberately
    /// does not expose (journaled accumulation ACROSS calls, an
    /// inspector-attached `execute()`, access-list emission from the settled
    /// state). The caller owns the isolation contract: `finalize` before
    /// returning, never `commit`.
    pub fn evm_mut(&mut self) -> &mut revm::MainnetEvm<revm::handler::MainnetContext<Db>, Insp> {
        self.evm
    }
}

impl<Db: Database + DatabaseRef, Insp> SimSession<'_, Db, Insp> {
    /// Read one storage word from the settled view — the shared read cache
    /// first, cold misses forwarded down the layered stack.
    ///
    /// A view read never warms or writes the cache: observing the per-block
    /// view must not change what the next run sees.
    ///
    /// # Errors
    ///
    /// [`SimError::Database`] when the layered DB cannot answer the read.
    pub fn read_word(&mut self, address: Address, slot: U256) -> Result<U256, SimError> {
        self.evm
            .ctx
            .journaled_state
            .database
            .storage_ref(address, slot)
            .map_err(|err| SimError::Database(err.to_string()))
    }

    /// The V2 pair's packed `reserves` word, decoded through the shared slot
    /// table the `degenbot_pools` root door publishes ([`V2_RESERVES_SLOT`] +
    /// [`decode_v2_reserves_word`]) — the same row the pack direction writes,
    /// so a read here and a write there cannot drift apart.
    ///
    /// # Errors
    ///
    /// [`SimError::Database`] when the layered DB cannot answer the read.
    pub fn read_v2_reserves(&mut self, pair: Address) -> Result<V2ReservesParts, SimError> {
        self.read_word(pair, U256::from(V2_RESERVES_SLOT))
            .map(decode_v2_reserves_word)
    }

    /// A concentrated-liquidity pool's `slot0` word, decoded through the shared
    /// slot table the `degenbot_pools` root door publishes ([`decode_v3_slot0`]).
    /// `layout` selects the fork's word-0 slot
    /// ([`ConcentratedLiquiditySlotLayout::slot0_slot`]); both forks keep the
    /// price/tick fields at the same bits, so the decode is the same row for
    /// each.
    ///
    /// # Errors
    ///
    /// [`SimError::Database`] when the layered DB cannot answer the read.
    pub fn read_slot0(
        &mut self,
        pool: Address,
        layout: ConcentratedLiquiditySlotLayout,
    ) -> Result<V3Slot0Parts, SimError> {
        self.read_word(pool, U256::from(layout.slot0_slot()))
            .map(decode_v3_slot0)
    }
}

/// How one settled run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunVerdict {
    Success,
    Reverted,
    Halted,
}

/// One settled run: the verdict, the settled journal state, the gas the run
/// burned, and its top-level return data.
///
/// Everything the run produced lives HERE. The shared DB received reads only —
/// see the module doc.
#[derive(Debug)]
pub struct SimRun {
    pub verdict: RunVerdict,
    /// revm's settled account/storage map for the run (the journal `finalize`
    /// cleared). Discarded with the value unless the caller keeps it.
    pub state: EvmState,
    /// The envelope's gas used, whether it settled successfully or not.
    pub gas_used: u64,
    /// Top-level return data: the call's return bytes, the revert payload, or
    /// empty for a halt.
    pub output: Bytes,
}

impl SimRun {
    fn settle(settled: ResultAndState) -> Self {
        let verdict = verdict_of(&settled.result);
        let gas_used = settled.result.tx_gas_used();
        let output = settled.result.output().cloned().unwrap_or_default();
        Self {
            verdict,
            state: settled.state,
            gas_used,
            output,
        }
    }

    /// The accounts this run touched, each with the storage slots it accessed,
    /// sorted for deterministic consumption.
    #[must_use]
    pub fn touched(&self) -> Vec<(Address, Vec<U256>)> {
        let mut touched: Vec<(Address, Vec<U256>)> = self
            .state
            .iter()
            .filter(|(_, account)| account.is_touched())
            .map(|(address, account)| {
                let mut slots: Vec<U256> = account.storage.keys().copied().collect();
                slots.sort_unstable();
                (*address, slots)
            })
            .collect();
        touched.sort_unstable_by_key(|(address, _)| *address);
        touched
    }
}

/// Why the engine could not answer: a run that never settled, or a view read
/// the layered DB could not serve.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SimError {
    /// revm refused the envelope before executing it at this block state.
    #[error("invalid transaction: {0}")]
    InvalidTransaction(InvalidTransaction),
    /// revm refused the block environment.
    #[error("invalid block header: {0}")]
    InvalidHeader(InvalidHeader),
    /// The layered DB could not answer a read — a cold-miss failure, the
    /// `rpc-failed` class its consumers tally.
    #[error("database read failed: {0}")]
    Database(String),
    /// A non-standard EVM failure (a fatal precompile error, a handler
    /// register).
    #[error("{0}")]
    Custom(String),
}

impl<DbError: core::fmt::Display> From<EVMError<DbError>> for SimError {
    fn from(err: EVMError<DbError>) -> Self {
        match err {
            EVMError::Transaction(invalid) => Self::InvalidTransaction(invalid),
            EVMError::Header(invalid) => Self::InvalidHeader(invalid),
            EVMError::Database(err) => Self::Database(err.to_string()),
            EVMError::Custom(raw) => Self::Custom(raw),
            EVMError::CustomAny(err) => Self::Custom(err.to_string()),
        }
    }
}

fn verdict_of(result: &ExecutionResult) -> RunVerdict {
    match result {
        ExecutionResult::Success { .. } => RunVerdict::Success,
        ExecutionResult::Revert { .. } => RunVerdict::Reverted,
        ExecutionResult::Halt { .. } => RunVerdict::Halted,
    }
}
