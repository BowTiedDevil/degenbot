//! The evm2 frame-replay scratch stack — the port of
//! `sim::evm::frame_replay` (`ScratchEvm` + `CountingFrameDb`).
//!
//! Same verbs, same error taxonomy, same outcome vocabulary: every shared
//! type (`ReplayableTx`, `ScratchBlock`, `FrameRpcCounter`,
//! `ReplayFrameError`, `SequenceReplayError`, `PredStatus`,
//! `BaseFeeSource`, `RunVerdict`) is the engine-neutral shape from the revm
//! module — a divergence between engines is an engine bug, not a port skew.
//!
//! Two engine-native simplifications fall out of evm2's surface:
//!
//! - The balance/base-fee relaxations are RUNTIME flags
//!   (`Version::features` — `BALANCE_CHECK` / `BASE_FEE_CHECK`), where revm
//!   needed the compile-time `optional_balance_check`/`optional_no_base_fee`
//!   cargo features. The scratch builds its `Version` per attempt.
//! - The sequence overlay is evm2's own internal accepted state:
//!   predecessors `commit()` into ONE persistent EVM's accepted overlay
//!   (evm2's native pending-state merge) instead of revm's `SequenceDb` +
//!   `EvmState` re-application. The overlay drops with the EVM when the
//!   call returns, so the ext receives reads only — the same isolation as
//!   revm's throwaway `CacheDB`. Because the overlay is EVM-internal, the
//!   base-fee fallback cannot rebuild mid-sequence: a fee-cap rejection
//!   restarts the WHOLE sequence with the check disabled (validation-only
//!   — execution semantics are unchanged, and re-reads hit the ext's warmed
//!   caches, so cold-read accounting is preserved).

use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::consensus::transaction::Recovered;
use alloy::consensus::TxEip1559;
use alloy::primitives::{Address, Bytes, TxKind, B256, U256};

use ::evm2::env::BlockEnvExt;
use ::evm2::ethereum::{ethereum_tx_registry, RecoveredTxEnvelope, TxEnvelope};
use ::evm2::evm::{Database, Db, DynDatabase, ExecutedTx};
use ::evm2::registry::{HandlerError, HandlerResult};
use ::evm2::{BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Precompiles, Version};

use super::handle::EVM2_PRODUCTION_SPEC as FRAME_SPEC;
use crate::sim::frame_replay::{
    BaseFeeSource, FrameRpcCounter, PredStatus, ReplayFrameError, ReplayableTx, ScratchBlock,
    SequenceReplayError,
};
use crate::sim::RunVerdict;

/// The cold-path bridge of a scratch stack: counts forwarded reads and
/// otherwise forwards untouched (the revm-side `CountingFrameDb` analog —
/// code-by-hash and block-hash reads are NOT counted, same rule).
#[derive(Debug)]
pub struct Evm2CountingFrameDb<ExtDb> {
    inner: ExtDb,
    counter: Arc<FrameRpcCounter>,
}

impl<ExtDb> Evm2CountingFrameDb<ExtDb> {
    /// Wrap `inner`'s cold reads with `counter`.
    #[must_use]
    pub fn new(inner: ExtDb, counter: Arc<FrameRpcCounter>) -> Self {
        Self { inner, counter }
    }
}

impl<ExtDb: Database> Database for Evm2CountingFrameDb<ExtDb> {
    type Error = ExtDb::Error;

    fn get_account(
        &mut self,
        address: &Address,
    ) -> Result<Option<::evm2::evm::AccountInfo>, Self::Error> {
        self.counter.count_basic();
        self.inner.get_account(address)
    }

    fn get_code_by_hash(
        &mut self,
        code_hash: &B256,
    ) -> Result<::evm2::bytecode::Bytecode, Self::Error> {
        self.inner.get_code_by_hash(code_hash)
    }

    fn get_storage(
        &mut self,
        address: &Address,
        key: &::evm2::interpreter::Word,
    ) -> Result<::evm2::interpreter::Word, Self::Error> {
        self.counter.count_storage();
        self.inner.get_storage(address, key)
    }

    fn get_block_hash(&mut self, number: &::evm2::interpreter::Word) -> Result<B256, Self::Error> {
        self.inner.get_block_hash(number)
    }
}

/// One settled frame replay: verdict, gas, touched set, writes, and the
/// cold-read accounting (the revm-side `ReplayOutcome` analog, minus the
/// revm-typed state map — the writes + touched set carry the comparable
/// state).
#[derive(Debug)]
pub struct Evm2ReplayOutcome {
    /// How the frame settled.
    pub status: RunVerdict,
    /// Gas used by the frame.
    pub gas_used: u64,
    /// Top-level return data.
    pub output: Bytes,
    /// The frame's storage writes, sorted for deterministic comparison.
    pub storage_writes: Vec<(Address, U256, U256)>,
    /// Every touched slot's settled value (reads included — the journal
    /// words `journal_pools` extracts pool post-states from), sorted.
    pub slot_values: Vec<(Address, U256, U256)>,
    /// The settled account infos of the accounts the frame touched
    /// (nonce/balance after the run — the revm `state.info` analog), sorted.
    pub accounts: Vec<(Address, ::evm2::evm::AccountInfo)>,
    /// The deterministic touched-slot set (mirrors `SimRun::touched`).
    pub touched: Vec<(Address, Vec<U256>)>,
    /// Cold reads forwarded to the cold path during this replay.
    pub rpc_reads: u64,
    /// Wall time of the replay.
    pub wall: Duration,
    /// Whether the projected base fee settled or the disabled-check
    /// fallback ran.
    pub base_fee_source: BaseFeeSource,
}

/// Read ONE word through a layered DB view — the admission layer's read
/// for connector state from the SAME chain view the frames replay over
/// (the revm-side `read_view_word` analog). `None` on a read failure; a
/// zero word reads through as `Some(U256::ZERO)` — the caller decides
/// whether zero is usable pool state.
#[must_use]
pub fn read_view_word<Ext: Database>(ext: &mut Ext, address: Address, slot: U256) -> Option<U256> {
    ext.get_storage(&address, &slot).ok()
}

/// The settled sequence: per-predecessor status + gas, then the frame.
#[derive(Debug)]
pub struct Evm2SequenceOutcome {
    /// Each predecessor's disposition, in prefix order.
    pub predecessors: Vec<PredStatus>,
    /// Each predecessor's gas used, in prefix order.
    pub predecessor_gas: Vec<u64>,
    /// The frame's own outcome over the accumulated view.
    pub frame: Evm2ReplayOutcome,
}

/// A scratch engine over a layered DB: replays ONE frame at a time (see the
/// revm-side module doc for the contract this mirrors).
#[derive(Debug)]
pub struct Evm2ScratchEvm<Ext> {
    ext: Ext,
    block: ScratchBlock,
    counter: Arc<FrameRpcCounter>,
}

impl<Ext: Database> Evm2ScratchEvm<Ext> {
    /// A scratch whose cold reads are NOT accounted (ext carries no
    /// [`Evm2CountingFrameDb`] — `Evm2ReplayOutcome::rpc_reads` stays 0).
    #[must_use]
    pub fn new(ext: Ext, block: ScratchBlock) -> Self {
        Self::with_counter(ext, block, Arc::new(FrameRpcCounter::default()))
    }

    /// A scratch whose cold reads are counted through the shared handle.
    #[must_use]
    pub fn with_counter(ext: Ext, block: ScratchBlock, counter: Arc<FrameRpcCounter>) -> Self {
        Self {
            ext,
            block,
            counter,
        }
    }

    /// The layered ext this scratch serves frames over (mutable — drive
    /// settlement-style reads over the SAME ext).
    #[must_use]
    pub fn ext_mut(&mut self) -> &mut Ext {
        &mut self.ext
    }

    /// The layered ext as a read view.
    #[must_use]
    pub fn ext(&self) -> &Ext {
        &self.ext
    }

    /// Replay ONE frame: transact + discard over the ext (frame writes
    /// leave with the executed tx; the ext keeps read caches only).
    ///
    /// # Errors
    ///
    /// The shared [`ReplayFrameError`] taxonomy; a base-fee-projection
    /// rejection falls back once with the check disabled.
    pub fn replay(&mut self, tx: &ReplayableTx) -> Result<Evm2ReplayOutcome, ReplayFrameError> {
        let started = Instant::now();
        let reads_before = self.counter.reads();
        let (settled, base_fee_source) = run_frame_with_fallback(&mut self.ext, &self.block, tx)?;
        Ok(outcome(
            settled,
            self.counter.reads().saturating_sub(reads_before),
            started.elapsed(),
            base_fee_source,
        ))
    }

    /// Replay a hydrated predecessor prefix, ascending, then `frame` over
    /// the accumulated view. Predecessors `commit()` into a throwaway
    /// accepted-state layer over the ext; the frame discards.
    ///
    /// # Errors
    ///
    /// [`SequenceReplayError::Predecessor`] aborts before the frame runs;
    /// [`SequenceReplayError::Frame`] is the frame's own failure.
    pub fn replay_sequence(
        &mut self,
        prefix: &[ReplayableTx],
        frame: &ReplayableTx,
    ) -> Result<Evm2SequenceOutcome, SequenceReplayError> {
        let started = Instant::now();
        let reads_before = self.counter.reads();
        // One persistent EVM: predecessor commits accumulate in its internal
        // accepted overlay, reads fall through to the ext (warming its
        // caches), the overlay drops on return. A fee-cap rejection restarts
        // the whole sequence with the check disabled (see the module doc).
        let mut disable_base_fee = false;
        'restart: loop {
            let mut evm = build_frame_evm(Db::new(&mut self.ext), &self.block, disable_base_fee);
            let mut predecessors = Vec::with_capacity(prefix.len());
            let mut predecessor_gas = Vec::with_capacity(prefix.len());
            for predecessor in prefix {
                match settle(evm.transact(&envelope(predecessor)), true) {
                    Ok(settled) => {
                        predecessors.push(settled.status.into());
                        predecessor_gas.push(settled.gas_used);
                    }
                    Err(FrameAbort::BaseFeeRejected) if !disable_base_fee => {
                        disable_base_fee = true;
                        continue 'restart;
                    }
                    Err(FrameAbort::BaseFeeRejected) => {
                        return Err(SequenceReplayError::Predecessor {
                            nonce: predecessor.nonce,
                            source: ReplayFrameError::Mispriced {
                                raw: "base-fee projection rejected (fallback also failed)".into(),
                            },
                        });
                    }
                    Err(FrameAbort::OtherError(ReplayFrameError::AlreadySettled { .. })) => {
                        predecessors.push(PredStatus::AlreadyMined);
                        predecessor_gas.push(0);
                    }
                    Err(FrameAbort::OtherError(source)) => {
                        return Err(SequenceReplayError::Predecessor {
                            nonce: predecessor.nonce,
                            source,
                        });
                    }
                }
            }
            let settled = match settle(evm.transact(&envelope(frame)), false) {
                Ok(settled) => settled,
                Err(FrameAbort::BaseFeeRejected) if !disable_base_fee => {
                    disable_base_fee = true;
                    continue 'restart;
                }
                Err(FrameAbort::BaseFeeRejected) => {
                    return Err(SequenceReplayError::Frame(ReplayFrameError::Mispriced {
                        raw: "base-fee projection rejected (fallback also failed)".into(),
                    }));
                }
                Err(FrameAbort::OtherError(source)) => {
                    return Err(SequenceReplayError::Frame(source));
                }
            };
            let base_fee_source = if disable_base_fee {
                BaseFeeSource::DisabledFallback
            } else {
                BaseFeeSource::Projected
            };
            return Ok(Evm2SequenceOutcome {
                predecessors,
                predecessor_gas,
                frame: outcome(
                    settled,
                    self.counter.reads().saturating_sub(reads_before),
                    started.elapsed(),
                    base_fee_source,
                ),
            });
        }
    }
}

/// The per-attempt settled shape the outcome settles from.
struct Settled {
    status: RunVerdict,
    gas_used: u64,
    output: Bytes,
    storage_writes: Vec<(Address, U256, U256)>,
    slot_values: Vec<(Address, U256, U256)>,
    accounts: Vec<(Address, ::evm2::evm::AccountInfo)>,
    touched: Vec<(Address, Vec<U256>)>,
}

enum FrameAbort {
    BaseFeeRejected,
    OtherError(ReplayFrameError),
}

impl From<HandlerError> for FrameAbort {
    fn from(err: HandlerError) -> Self {
        match err {
            HandlerError::FeeCapLessThanBaseFee { .. } => Self::BaseFeeRejected,
            HandlerError::InvalidNonce { expected, got } if got > expected => {
                Self::OtherError(ReplayFrameError::GapPending {
                    claimed: got,
                    expected,
                })
            }
            HandlerError::InvalidNonce { expected, got } => {
                Self::OtherError(ReplayFrameError::AlreadySettled {
                    frame: got,
                    parent: expected,
                })
            }
            HandlerError::IntrinsicGasTooLow { required, got } => {
                Self::OtherError(ReplayFrameError::MalformedTransaction {
                    raw: format!("intrinsic gas ({required}) exceeds the gas limit ({got})"),
                })
            }
            other => Self::OtherError(ReplayFrameError::Other {
                raw: other.to_string(),
            }),
        }
    }
}

/// Build the scratch frame EVM over `db` with the runtime relaxations:
/// `BALANCE_CHECK` always off (a replay never cares about sender funds),
/// `BASE_FEE_CHECK` off only for the fallback attempt.
fn build_frame_evm<'a>(
    db: impl DynDatabase + 'a,
    block: &ScratchBlock,
    disable_base_fee: bool,
) -> Evm<'a, BaseEvmTypes> {
    let mut version = Version::new(FRAME_SPEC);
    version.features.remove(EvmFeatures::BALANCE_CHECK);
    if disable_base_fee {
        version.features.remove(EvmFeatures::BASE_FEE_CHECK);
    }
    Evm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(FRAME_SPEC, version),
        FRAME_SPEC,
        BlockEnvExt {
            basefee: U256::from(block.base_fee_next),
            number: U256::from(block.number),
            timestamp: U256::from(block.timestamp),
            ..Default::default()
        },
        ethereum_tx_registry(FRAME_SPEC),
        db,
        Precompiles::base(FRAME_SPEC),
    )
}

/// The EIP-1559 envelope for a [`ReplayableTx`] (the revm side's `tx_env`
/// analog: `max_fee_per_gas` in the unified fee field).
fn envelope(tx: &ReplayableTx) -> RecoveredTxEnvelope {
    Recovered::new_unchecked(
        TxEnvelope::Eip1559(TxEip1559 {
            chain_id: 1,
            nonce: tx.nonce,
            gas_limit: tx.gas_limit,
            max_fee_per_gas: tx.max_fee_per_gas,
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas,
            to: match tx.to {
                Some(to) => TxKind::Call(to),
                None => TxKind::Create,
            },
            value: tx.value,
            input: tx.data.clone(),
            ..Default::default()
        }),
        tx.from,
    )
}

/// Settle an executed-tx result into the per-attempt shape: capture the
/// writes + touched set from the account changes, then resolve with the
/// `commit` verb (predecessors) or `discard` (frames).
fn settle(
    executed: HandlerResult<ExecutedTx<'_, '_, BaseEvmTypes>>,
    commit: bool,
) -> Result<Settled, FrameAbort> {
    let executed = executed.map_err(FrameAbort::from)?;
    let mut storage_writes: Vec<(Address, U256, U256)> = executed
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
    storage_writes.sort();
    let mut touched: Vec<(Address, Vec<U256>)> = executed
        .account_changes()
        .map(|changes| {
            let mut slots: Vec<U256> = changes.storage.slots.keys().copied().collect();
            slots.sort();
            (changes.address, slots)
        })
        .collect();
    touched.sort();
    let mut slot_values: Vec<(Address, U256, U256)> = executed
        .account_changes()
        .flat_map(|changes| {
            let address = changes.address;
            changes
                .storage
                .slots
                .iter()
                .map(move |(key, slot)| (address, *key, slot.value.current))
        })
        .collect();
    slot_values.sort();
    let mut accounts: Vec<(Address, ::evm2::evm::AccountInfo)> = executed
        .account_changes()
        .filter(|changes| changes.touched)
        .filter_map(|changes| changes.current.map(|info| (changes.address, info.clone())))
        .collect();
    accounts.sort_by_key(|(a, _)| *a);
    let result = if commit {
        executed.commit()
    } else {
        executed.discard()
    };
    let status = if result.status {
        RunVerdict::Success
    } else if result.stop == ::evm2::interpreter::InstrStop::Revert {
        RunVerdict::Reverted
    } else {
        RunVerdict::Halted
    };
    if status != RunVerdict::Success {
        // The journal-flattening parity rule (revm's settled reverted/
        // halted frame): no pool state survives — storage writes, slot
        // values, and the per-account slot sets all drop; the account
        // infos (nonce/gas effects) stay.
        storage_writes.clear();
        slot_values.clear();
        for (_, slots) in &mut touched {
            slots.clear();
        }
    }
    Ok(Settled {
        status,
        gas_used: result.tx_gas_used(),
        output: result.output.clone(),
        storage_writes,
        slot_values,
        accounts,
        touched,
    })
}

/// One attempt of the plain `replay` verb: a fresh EVM over `&mut ext`,
/// transact + discard.
fn run_frame<Ext: Database>(
    ext: &mut Ext,
    block: &ScratchBlock,
    tx: &ReplayableTx,
    disable_base_fee: bool,
) -> Result<Settled, FrameAbort> {
    let mut evm = build_frame_evm(Db::new(ext), block, disable_base_fee);
    settle(evm.transact(&envelope(tx)), false)
}

/// The projection ritual (the revm side's `run_frame_with_fallback`):
/// projected base fee first, the disabled-check retry once on a fee-cap
/// rejection.
fn run_frame_with_fallback<Ext: Database>(
    ext: &mut Ext,
    block: &ScratchBlock,
    tx: &ReplayableTx,
) -> Result<(Settled, BaseFeeSource), ReplayFrameError> {
    match run_frame(ext, block, tx, false) {
        Ok(settled) => Ok((settled, BaseFeeSource::Projected)),
        Err(FrameAbort::BaseFeeRejected) => match run_frame(ext, block, tx, true) {
            Ok(settled) => Ok((settled, BaseFeeSource::DisabledFallback)),
            Err(FrameAbort::BaseFeeRejected) => Err(ReplayFrameError::Mispriced {
                raw: "base-fee projection rejected (fallback also failed)".into(),
            }),
            Err(FrameAbort::OtherError(e)) => Err(e),
        },
        Err(FrameAbort::OtherError(e)) => Err(e),
    }
}

/// Settle the per-attempt shape into the typed outcome.
fn outcome(
    settled: Settled,
    rpc_reads: u64,
    wall: Duration,
    base_fee_source: BaseFeeSource,
) -> Evm2ReplayOutcome {
    Evm2ReplayOutcome {
        status: settled.status,
        gas_used: settled.gas_used,
        output: settled.output,
        storage_writes: settled.storage_writes,
        slot_values: settled.slot_values,
        accounts: settled.accounts,
        touched: settled.touched,
        rpc_reads,
        wall,
        base_fee_source,
    }
}
