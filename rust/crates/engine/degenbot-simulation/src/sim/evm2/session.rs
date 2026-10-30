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
use ::evm2::{BaseEvmTypes, Evm, ExecutionConfig};

use crate::sim::RunVerdict;

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
    /// A journaled sequence step refused; the accumulated overlay was
    /// rolled back before the error escaped.
    #[error("sequence step {index} refused: {source}")]
    Sequence {
        /// The refusing step's index.
        index: usize,
        /// The refusal class.
        source: Box<Evm2SimError>,
    },
}

impl Evm2SimError {
    /// The refusing step's index (mirrors `SimError::step_index`).
    #[must_use]
    pub fn step_index(&self) -> Option<usize> {
        match self {
            Self::Sequence { index, .. } => Some(*index),
            Self::Database(_) | Self::Transact(_) => None,
        }
    }
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

/// The capture a journaled sequence drains from its throwaway EVM's
/// composite inspector (the settlement bundle's byproducts — the revm
/// side's handle-drained trio plus the access list).
#[derive(Debug, Default)]
pub struct Evm2SequenceCapture {
    /// The call trace (revert attribution).
    pub trace: crate::sim::capture::CallTrace,
    /// The committed swap events.
    pub swaps: Vec<crate::sim::CapturedSwap>,
    /// The reverted-frame swap events (the divergence diagnostic).
    pub reverted_swaps: Vec<crate::sim::CapturedSwap>,
    /// Every `log` hook invocation count (the revm-side `log_full_count`
    /// analog).
    pub log_count: usize,
    /// The EIP-2930 access list the sequence touched.
    pub access_list: alloy::rpc::types::eth::AccessList,
}

/// One step of a journaled sequence (the revm-side `SimStep` analog):
/// `Transact` steps run bare; `Inspect` steps run with the composite
/// capture inspector borrowed for that step — the settlement bundle's
/// balance reads stay out of the execute()-only access list (ADR-019 D3).
#[derive(Debug)]
pub enum Evm2SimStep {
    /// A bare step (no capture).
    Transact(RecoveredTxEnvelope),
    /// A captured step (the composite inspector rides just this step).
    Inspect(RecoveredTxEnvelope),
}

/// One journaled sequence's settled shape — the revm-side `SimSequence`
/// analog: per-step results in order plus the accumulated writes.
#[derive(Debug)]
pub struct Evm2SimSequence {
    /// The steps' results, in step order.
    pub calls: Vec<Evm2SimRun>,
    /// The accumulated storage writes, merged in step order (a later step
    /// overwrites an earlier step's value for the same slot), sorted for
    /// deterministic comparison.
    pub storage_writes: Vec<(Address, U256, U256)>,
    /// The drained inspector capture (see [`Evm2SequenceCapture`]).
    pub capture: Evm2SequenceCapture,
}

/// The settle-verb session over the shared evm2 EVM (see the module doc).
pub struct Evm2SimSession<'h, 'a> {
    evm: &'h mut Evm<'a, BaseEvmTypes>,
    /// The block env a journaled sequence's throwaway EVM runs under (the
    /// shared EVM's own block env — captured at construction because the
    /// EVM exposes no block-env getter).
    block_env: ::evm2::env::BlockEnvExt,
}

impl<'h, 'a> Evm2SimSession<'h, 'a> {
    /// Wrap the EVM the caller owns for the session's scope. The journaled
    /// sequence verb runs under a default block env; production callers
    /// carrying a pumped block use [`Self::with_block_env`].
    #[must_use]
    pub fn new(evm: &'h mut Evm<'a, BaseEvmTypes>) -> Self {
        Self::with_block_env(evm, ::evm2::env::BlockEnvExt::default())
    }

    /// Wrap the EVM with the block env its sequences replay under.
    #[must_use]
    pub fn with_block_env(
        evm: &'h mut Evm<'a, BaseEvmTypes>,
        block_env: ::evm2::env::BlockEnvExt,
    ) -> Self {
        Self { evm, block_env }
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

    /// Run `steps` in order over ONE accumulating accepted overlay and
    /// settle — the revm-side `SimSession::run_sequence` analog (journaled
    /// accumulation across calls: a step sees every earlier step's writes).
    ///
    /// The accumulation runs on a THROWAWAY EVM built per sequence over the
    /// shared EVM's own database handle: each step commits into the
    /// throwaway's accepted overlay, reads fall through to the shared DB
    /// stack (warming it exactly as `run` does), and the whole throwaway —
    /// overlay included — drops when the sequence ends. The shared EVM's
    /// settled view never carries a sequence's writes into the next path
    /// (the same isolation contract `run` states). A step that reverts
    /// still commits (a revert's nonce/gas effects persist, matching the
    /// revm journal) and keeps the sequence running.
    ///
    /// # Errors
    ///
    /// [`Evm2SimError::Sequence`] carrying the refusing step's index.
    pub fn run_sequence(&mut self, steps: &[Evm2SimStep]) -> Result<Evm2SimSequence, Evm2SimError> {
        let spec = self.evm.spec_id();
        // The throwaway EVM: the shared DB by mutable handle (reads warm
        // the shared stack; the accumulation dies with it), configured
        // from the shared EVM's own `Version` so runtime feature
        // relaxations (the frame-replay checks) carry over.
        let exec = ExecutionConfig::<BaseEvmTypes>::for_spec_and_version(spec, *self.evm.version());
        let mut seq_evm = Evm::new_with_execution_config(
            exec,
            spec,
            self.block_env,
            ::evm2::ethereum::ethereum_tx_registry(spec),
            self.evm.state_mut().initial_mut(),
            ::evm2::Precompiles::base(spec),
        );
        // The bundle's capture is a sequence byproduct: a fresh composite
        // inspector rides the sequence as a BORROWED slot
        // (`transact_with_inspector`), drained from the local value after
        // the loop — no typed retrieval off the locally-borrowed EVM.
        let mut inspector = crate::sim::evm2::inspectors::Evm2SimInspector::default();
        let mut calls = Vec::with_capacity(steps.len());
        let mut writes = std::collections::BTreeMap::new();
        for (index, step) in steps.iter().enumerate() {
            let settled = match step {
                Evm2SimStep::Transact(tx) => seq_evm.transact(tx),
                Evm2SimStep::Inspect(tx) => seq_evm.transact_with_inspector(tx, &mut inspector),
            };
            let executed = match settled {
                Ok(executed) => executed,
                Err(err) => {
                    return Err(Evm2SimError::Sequence {
                        index,
                        source: Box::new(Evm2SimError::Transact(err.to_string())),
                    });
                }
            };
            // The writes stream through a sink (`commit_with`) rather than
            // `account_changes`: the iterator's `impl Trait` over-captures
            // the EVM borrow under the Rust 2024 capture rules.
            let mut sink = StepWrites { writes: Vec::new() };
            let Ok(result) = executed.commit_with(&mut sink);
            let storage_writes = sink.writes;
            for (address, key, value) in &storage_writes {
                writes.insert((*address, *key), *value);
            }
            calls.push(Evm2SimRun {
                verdict: verdict_of(&result),
                gas_used: result.tx_gas_used(),
                output: result.output.clone(),
                storage_writes,
            });
        }
        let capture = Evm2SequenceCapture {
            trace: inspector.take_trace(),
            swaps: inspector.take_swaps(),
            reverted_swaps: inspector.take_reverted_swaps(),
            log_count: inspector.log_count(),
            access_list: inspector.take_access_list(),
        };
        Ok(Evm2SimSequence {
            calls,
            storage_writes: writes
                .into_iter()
                .map(|((address, key), value)| (address, key, value))
                .collect(),
            capture,
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

/// The per-step write collector for `commit_with` — the changed-slot set
/// the sequence merges into its accumulated writes.
struct StepWrites {
    writes: Vec<(Address, U256, U256)>,
}

impl ::evm2::evm::StateChangeSink for StepWrites {
    type Error = core::convert::Infallible;

    fn storage(&mut self, change: ::evm2::evm::StorageChange) -> Result<(), Self::Error> {
        self.writes
            .push((change.address, change.key, change.current));
        Ok(())
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
