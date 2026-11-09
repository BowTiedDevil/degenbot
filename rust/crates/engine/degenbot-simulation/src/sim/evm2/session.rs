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
//!
//! # Execution verbs
//!
//! One ordered `Evm2SimStep` sequence has three drivers over one accumulation:
//!
//! - [`Evm2SimSession::run_sequence`] is the SYNCHRONOUS driver — each step is
//!   `transact`/`transact_with_inspector` on the session's own thread.
//! - [`Evm2SimSession::run_sequence_async`] is the coroutine driver — each
//!   step is evm2's `transact_async`, so a cold database read suspends the
//!   sequence (the returned future reports `Pending`) instead of blocking the
//!   task that drives it. It builds a throwaway EVM per call and its
//!   accumulation dies with it.
//! - [`Evm2SimSession::run_sequence_async_in_place`] is the same coroutine
//!   driver on the caller's OWN EVM: the steps reuse that EVM's engine
//!   configuration and fiber stack instead of a per-call one, and the caller
//!   owns the reset of the accepted overlay the sequence leaves behind.
//!
//! All three feed the same settlement + capture accumulation
//! (`SequenceAccumulator`; the two async verbs share
//! `drive_sequence_async`), so what a step settles — accepted writes, gas,
//! output, verdict, the logs/swaps/access list a capture drains — and how a
//! refusal is reported (step-indexed [`Evm2SimError::Sequence`]) are the same
//! facts on every driver. The verbs differ in scheduling and EVM ownership only.
//!
//! ## Migrating a sequence to the async verb
//!
//! Pick the driver from the session's database stack, not from taste: the
//! async verb is the one that helps when the stack bottoms out in evm2's
//! `AsyncDb` adapter (the RPC cold-miss fallback of the evm2 stack), because
//! that adapter can only poll its reads from inside an EVM async fiber. A
//! synchronous stack ([`super::blocking_async_db::BlockingAsyncDb`], or a
//! purely in-memory one) gains nothing: `transact_async` over a synchronous
//! database is the synchronous transaction on a fiber. A caller migrates by
//! awaiting the verb where it previously called it —
//! `session.run_sequence(&steps, mode)` becomes
//! `session.run_sequence_async(&steps, mode).await` — and the result is the
//! same value. The synchronous verb and every existing consumer stay as they
//! are; neither verb is a wrapper of the other.
//!
//! A caller that runs many sequences over ONE EVM — a reusable slot a fan-out
//! recycles candidate after candidate — takes the in-place verb instead of the
//! throwaway one: `run_sequence_async` rebuilds an EVM (and reserves a fiber
//! stack) per call, which is exactly the cost the slot exists to avoid. The
//! in-place verb's contract is stated on the verb: the sequence's writes land
//! in the caller's accepted overlay, and the caller's reset
//! (`Evm::set_database`, i.e. the next candidate's installation) is what keeps
//! two candidates from sharing a write. A reused slot therefore pays one EVM and
//! one fiber stack for its whole life, and each candidate gets a fresh
//! accepted-state layer.
//!
//! ## Bare state reads on the async verb
//!
//! A [`Evm2SimStep::ReadNativeBalance`] step is not a transaction, but it is
//! still a state read — and on an `AsyncDb`-backed stack a cold account's read
//! is a database round trip that only exists inside an EVM fiber. The step
//! therefore takes evm2's untracked account-info read through
//! `Evm::read_account_info_async`, the async-fiber counterpart of the
//! synchronous read: the same `state.account_info_untracked` call on the SAME
//! reusable fiber `transact_async` uses, so a cold read suspends the sequence
//! exactly as a cold transaction read does, and resolves to the same figure the
//! synchronous driver blocks for.
//!
//! Overlay semantics are unchanged by the fiber: the read consults the
//! accumulating accepted overlay first and only falls through to the database
//! for an account no earlier step loaded or moved, so a value-moving step
//! earlier in the sequence is visible to a later balance read on both drivers.
//! Nothing is seeded into a cache to make a read answerable.
//!
//! ## What the async verb refuses
//!
//! [`Evm2CaptureMode::Inspector`] capture is REFUSED
//! ([`Evm2SimError::AsyncInspectorUnsupported`]) before any step runs. The
//! composite capture inspector rides a step through a borrowed inspector, and
//! evm2's async entrypoints have no inspector counterpart — honoring the mode
//! here would silently mean "captured by another mechanism", which is exactly
//! the substitution the capture mode promises not to make. The diagnostic mode
//! stays on the synchronous verb.

use alloy::primitives::{Address, Bytes, U256};
use degenbot_pools::{
    decode_v2_reserves_word, decode_v3_slot0, ConcentratedLiquiditySlotLayout, V2ReservesParts,
    V3Slot0Parts, V2_RESERVES_SLOT,
};

use ::evm2::ethereum::RecoveredTxEnvelope;
use ::evm2::evm::{ExecutedTx, TxResult};
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
    /// The async sequence verb was asked for a capture mode evm2's async
    /// entrypoints cannot carry. `transact_async` dispatches through
    /// `transact`, which has no inspector counterpart, so an `Inspector`-mode
    /// sequence would have to capture by some other mechanism under the same
    /// name. Refused before any step runs.
    #[error(
        "async sequence cannot carry the {mode:?} capture mode: evm2 has no async inspector entrypoint"
    )]
    AsyncInspectorUnsupported {
        /// The refused capture mode.
        mode: Evm2CaptureMode,
    },
}

impl Evm2SimError {
    /// The refusing step's index (mirrors `SimError::step_index`).
    #[must_use]
    pub fn step_index(&self) -> Option<usize> {
        match self {
            Self::Sequence { index, .. } => Some(*index),
            Self::Database(_) | Self::Transact(_) | Self::AsyncInspectorUnsupported { .. } => None,
        }
    }
}

/// One settled step: the verdict, the gas burned, the top-level return data,
/// and the storage writes it produced (captured from the transaction's
/// account changes before the scratch was discarded).
#[derive(Debug)]
pub struct Evm2SimRun {
    /// How the step ended (the engine-neutral verdict shared with the revm
    /// session). A [`Evm2SimStep::ReadNativeBalance`] step always reports
    /// [`RunVerdict::Success`]: it executes no code and can neither revert nor
    /// halt.
    pub verdict: RunVerdict,
    /// The envelope's gas used; zero for a balance-read step.
    pub gas_used: u64,
    /// The call's return bytes, the revert payload, or empty for a halt and
    /// for a balance-read step.
    pub output: Bytes,
    /// Storage writes the run produced: `(address, slot, value)` per written
    /// slot whose value changed, in unspecified order. The settled result the
    /// discard contract removes from everywhere else.
    pub storage_writes: Vec<(Address, U256, U256)>,
    /// The native balance a [`Evm2SimStep::ReadNativeBalance`] step observed at
    /// its position in the sequence, read from the accumulating journaled state
    /// (an account the state layer holds no record of holds zero). `None` for
    /// every transaction step, which observes no balance of its own.
    pub native_balance: Option<U256>,
}

/// The capture a journaled sequence drains from its throwaway EVM's
/// composite inspector (the settlement bundle's byproducts — the revm
/// side's handle-drained trio plus the access list).
#[derive(Debug, Default)]
pub struct Evm2SequenceCapture {
    /// The call trace (revert attribution).
    pub trace: crate::sim::capture::CallTrace,
    /// The committed logs the captured steps emitted, in execution order — the
    /// event evidence a production observation reports (a reverted frame's logs
    /// are not committed, matching the swap capture's scope).
    pub logs: Vec<alloy::primitives::Log>,
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

/// The native balance a [`Evm2SimStep::ReadNativeBalance`] step observes in the
/// sequence's accumulating state.
///
/// Read untracked (a probe read buys no storage and enters no transaction
/// overlay); an account the state layer holds no record of holds zero, which is
/// the account's actual balance rather than an absent figure.
fn read_native_balance(
    evm: &mut Evm<'_, BaseEvmTypes>,
    account: &Address,
) -> Result<U256, Evm2SimError> {
    evm.state_mut()
        .account_info_untracked(account)
        .map(|info| info.map_or(U256::ZERO, |info| info.balance))
        .map_err(|err| Evm2SimError::Database(err.to_string()))
}

/// The same balance read on evm2's async fiber: the identical untracked
/// account-info peek, on the fiber `transact_async` uses, so a cold account's
/// database round trip suspends the sequence instead of failing outside a fiber.
/// Overlay precedence is `account_info_untracked`'s either way — the accepted
/// overlay answers first, the database only for an account no step has loaded or
/// moved.
async fn read_native_balance_async(
    evm: &mut Evm<'_, BaseEvmTypes>,
    account: &Address,
) -> Result<U256, Evm2SimError> {
    evm.read_account_info_async(*account)
        .await
        .map(|info| info.map_or(U256::ZERO, |info| info.balance))
        .map_err(|err| Evm2SimError::Database(err.to_string()))
}

/// The settled result a native-balance read step produces: no code runs, so no
/// gas is burned, no return data exists, and nothing is written — the observed
/// balance is the whole result.
const fn native_balance_run(balance: U256) -> Evm2SimRun {
    Evm2SimRun {
        verdict: RunVerdict::Success,
        gas_used: 0,
        output: Bytes::new(),
        storage_writes: Vec::new(),
        native_balance: Some(balance),
    }
}

/// One step of a journaled sequence (the revm-side `SimStep` analog):
/// `Transact` steps run bare; `Inspect` steps are the captured steps —
/// the settlement bundle's balance reads stay out of the execute()-only
/// access list (ADR-019 D3). The capture MECHANISM an `Inspect` step uses
/// is the runner's choice (see [`Evm2CaptureMode`]).
#[derive(Debug)]
pub enum Evm2SimStep {
    /// A bare step (no capture).
    Transact(RecoveredTxEnvelope),
    /// A captured step (capture rides just this step).
    Inspect(RecoveredTxEnvelope),
    /// A native-balance read at this position in the sequence: the declared
    /// account's own ETH balance, read from the accumulating journaled state
    /// (so a later step's value movement is visible to a later read). No
    /// transaction is built and nothing is spent: the read observes the account
    /// itself, never a contract called on its behalf.
    ReadNativeBalance(Address),
}

/// How a sequence's `Inspect` steps capture.
///
/// `Seam` is the production mechanism: the step runs as a plain
/// transaction (the JIT interpreter runner stays engaged — evm2's frame
/// dispatch is inspector-XOR-runner) and the capture is assembled
/// post-hoc from the state layer: slots from the executed transaction's
/// account changes (every slot loaded or written) and swaps from the
/// result's journal-scoped committed logs. The `CallTrace` and the
/// reverted-swaps diagnostic have no state-layer source; both come back
/// empty (diagnostic-only per the disposition recorded in the spine task).
///
/// `Inspector` is the diagnostic mechanism: the composite capture
/// inspector rides the step (`transact_with_inspector`), giving the full
/// trace + reverted-swaps forensics on the interpreted path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evm2CaptureMode {
    /// Post-hoc state-layer capture; the JIT runner stays engaged.
    Seam,
    /// The composite inspector rides the captured steps (interpreted).
    Inspector,
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
    /// The process-shared interpreter runner (the JIT spine), installed on
    /// each sequence's throwaway EVM. `None` = the stock interpreter.
    jit: Option<std::sync::Arc<dyn ::evm2::evm::InterpreterRunner<BaseEvmTypes>>>,
    /// The session's capture-learned hot-slot store: every completed sequence
    /// capture records its access list here, so the next anchor's fan-out can
    /// prewarm what the path derivation cannot reach (`crate::sim::prefetch`).
    /// `None` = this session teaches nothing (a lab EVM with no session
    /// owner).
    hot_slots: Option<std::sync::Arc<crate::sim::hot_slots::HotSlotStore>>,
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
        Self {
            evm,
            block_env,
            jit: None,
            hot_slots: None,
        }
    }

    /// Install the process-shared interpreter runner (the JIT spine) —
    /// sequences this session runs take the compiled path where the
    /// backend has programs, interpreting otherwise (fail-soft).
    #[must_use]
    pub fn with_jit(
        mut self,
        runner: Option<std::sync::Arc<dyn ::evm2::evm::InterpreterRunner<BaseEvmTypes>>>,
    ) -> Self {
        self.jit = runner;
        self
    }

    /// Install the session's capture-learned hot-slot store — the learning
    /// path: every sequence capture this session completes records its access
    /// list there (see [`Self::run_sequence`]).
    #[must_use]
    pub fn with_hot_slots(
        mut self,
        hot_slots: std::sync::Arc<crate::sim::hot_slots::HotSlotStore>,
    ) -> Self {
        self.hot_slots = Some(hot_slots);
        self
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
            native_balance: None,
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
    /// The synchronous driver of the shared accumulation
    /// `SequenceAccumulator` holds; [`Self::run_sequence_async`] is the
    /// coroutine driver of the same value (see the module doc).
    ///
    /// # Errors
    ///
    /// [`Evm2SimError::Sequence`] carrying the refusing step's index.
    pub fn run_sequence(
        &mut self,
        steps: &[Evm2SimStep],
        mode: Evm2CaptureMode,
    ) -> Result<Evm2SimSequence, Evm2SimError> {
        let mut acc = SequenceAccumulator::new(mode, steps.len());
        {
            let mut seq_evm = self.sequence_evm();
            for (index, step) in steps.iter().enumerate() {
                let (tx, captured) = match step {
                    Evm2SimStep::ReadNativeBalance(account) => {
                        observe_balance_step(&mut seq_evm, account, index, &mut acc)?;
                        continue;
                    }
                    Evm2SimStep::Transact(tx) => (tx, false),
                    Evm2SimStep::Inspect(tx) => (tx, true),
                };
                let settled = if captured && mode == Evm2CaptureMode::Inspector {
                    // `Inspector` mode: the composite capture inspector rides
                    // the captured steps as a BORROWED slot
                    // (`transact_with_inspector`) — the borrow ends with the
                    // call, so the handle comes straight back into `acc`.
                    seq_evm.transact_with_inspector(tx, &mut acc.inspected)
                } else {
                    seq_evm.transact(tx)
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
                acc.capture_slots(captured, &executed);
                acc.accept(captured, executed);
            }
        }
        let sequence = acc.finish();
        self.record_capture(&sequence.capture);
        Ok(sequence)
    }

    /// Run `steps` in order exactly as [`Self::run_sequence`] does, dispatching
    /// each transaction step through evm2's `transact_async` coroutine seam.
    ///
    /// This is the verb a session whose database stack bottoms out in evm2's
    /// `AsyncDb` adapter drives: a cold read that misses suspends the whole
    /// sequence (the returned future reports `Pending`) instead of blocking the
    /// task, and the sequence resumes in place when the read resolves. The
    /// settlement and capture accumulation is `SequenceAccumulator`'s — the
    /// one the synchronous driver feeds — so the two verbs differ in scheduling
    /// only: the same accepted writes, gas, output, verdicts, logs, swaps,
    /// access list, and step-indexed refusals.
    ///
    /// # Errors
    ///
    /// [`Evm2SimError::Sequence`] carrying the refusing step's index, and
    /// [`Evm2SimError::AsyncInspectorUnsupported`] when `mode` is
    /// [`Evm2CaptureMode::Inspector`] — refused before any step runs (see the
    /// module doc for why, and for how a bare balance read suspends).
    pub async fn run_sequence_async(
        &mut self,
        steps: &[Evm2SimStep],
        mode: Evm2CaptureMode,
    ) -> Result<Evm2SimSequence, Evm2SimError> {
        let sequence = {
            let mut seq_evm = self.sequence_evm();
            drive_sequence_async(&mut seq_evm, steps, mode).await?
        };
        self.record_capture(&sequence.capture);
        Ok(sequence)
    }

    /// Run `steps` in order on THIS session's EVM — the in-place coroutine
    /// driver: the same steps, the same accumulation, the same refusals as
    /// [`Self::run_sequence_async`], dispatched on the EVM the session was handed
    /// instead of on a throwaway built per call.
    ///
    /// The difference is what a sequence costs and where its writes land.
    /// [`Self::run_sequence_async`] builds a fresh EVM over the shared database
    /// handle for every call — engine configuration, block env, and fiber stack
    /// included — and drops it (and its accumulation) when the sequence ends.
    /// This verb runs `transact_async` and `read_account_info_async` on the
    /// caller's own EVM, so the sequence reuses that EVM's engine configuration,
    /// its block env, and its OWN fiber stack across steps and across sequences.
    ///
    /// # Reset ownership
    ///
    /// Because the steps commit into the caller's EVM, its accepted overlay
    /// carries the sequence's writes, journal, logs, and transient storage when
    /// the call returns. That is the trade: the caller owns the reset. A slot
    /// owner installs the next candidate's state (`Evm::set_database` → the
    /// whole accepted-state layer replaced), which is exactly the isolation
    /// `run_sequence_async` gets by dropping its throwaway — see
    /// [`AsyncFanoutSlot::install`](super::async_fanout::AsyncFanoutSlot::install),
    /// the caller that relies on it. A caller that keeps reading the EVM after
    /// the sequence (view reads, a second sequence without a reset) observes the
    /// candidate's writes rather than the block's state.
    ///
    /// # Errors
    ///
    /// [`Evm2SimError::Sequence`] carrying the refusing step's index, and
    /// [`Evm2SimError::AsyncInspectorUnsupported`] when `mode` is
    /// [`Evm2CaptureMode::Inspector`] — refused before any step runs.
    pub async fn run_sequence_async_in_place(
        &mut self,
        steps: &[Evm2SimStep],
        mode: Evm2CaptureMode,
    ) -> Result<Evm2SimSequence, Evm2SimError> {
        let sequence = drive_sequence_async(&mut *self.evm, steps, mode).await?;
        self.record_capture(&sequence.capture);
        Ok(sequence)
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

    /// The block env this session's sequences replay under — the simulated
    /// block's own facts (number, timestamp, base fee), so an observation can
    /// report the fee the transaction actually ran under instead of a
    /// caller-supplied copy.
    #[must_use]
    pub const fn block_env(&self) -> &::evm2::env::BlockEnvExt {
        &self.block_env
    }

    /// The raw evm2 EVM — the documented escape hatch (see the module doc).
    pub fn evm_mut(&mut self) -> &mut Evm<'a, BaseEvmTypes> {
        self.evm
    }

    /// Build the THROWAWAY EVM one sequence accumulates in — the shared DB by
    /// mutable handle (reads warm the shared stack; the accumulation dies with
    /// it), configured from the shared EVM's own `Version` so runtime feature
    /// relaxations (the frame-replay checks) carry over, under the session's
    /// block env and interpreter runner. Both sequence drivers build it here, so
    /// neither can run under a different engine configuration than the other.
    fn sequence_evm(&mut self) -> Evm<'_, BaseEvmTypes> {
        let spec = self.evm.spec_id();
        let exec = ExecutionConfig::<BaseEvmTypes>::for_spec_and_version(spec, *self.evm.version());
        let block_env = self.block_env;
        let jit = self.jit.clone();
        let mut seq_evm = Evm::new_with_execution_config(
            exec,
            spec,
            block_env,
            ::evm2::ethereum::ethereum_tx_registry(spec),
            self.evm.state_mut().initial_mut(),
            ::evm2::Precompiles::base(spec),
        );
        if let Some(runner) = jit {
            seq_evm.set_shared_interpreter_runner(runner);
        }
        seq_evm
    }

    /// The learning path: a completed capture's access list IS the exact
    /// `(address, slot)` set the sequence touched, so it is the prewarm's
    /// historical knowledge — recorded against the height the sequence's reads
    /// were pinned to (the session's replay block, which production sets to the
    /// fan-out's `sim_block`). The store keys on identity only: the next prewarm
    /// reads the VALUES from the node at its own anchor, so a drifted prediction
    /// costs one batch member, never a wrong read.
    fn record_capture(&self, capture: &Evm2SequenceCapture) {
        let Some(hot_slots) = &self.hot_slots else {
            return;
        };
        let anchor = u64::try_from(self.block_env.number).unwrap_or(u64::MAX);
        hot_slots.record_capture(anchor, &capture.access_list);
    }
}

/// The settlement + capture accumulation ONE ordered sequence performs — the
/// single implementation both drivers feed ([`Evm2SimSession::run_sequence`]
/// dispatches each step on the session's thread,
/// [`Evm2SimSession::run_sequence_async`] suspends it on evm2's coroutine seam).
/// A driver owns only how a step is dispatched; what a step settles — its
/// accepted writes, gas, output, verdict, and the logs/swaps/slots its capture
/// drains — is recorded here, once, for both.
struct SequenceAccumulator {
    /// The sequence's capture mechanism.
    mode: Evm2CaptureMode,
    /// The composite inspector `Inspector`-mode captured steps ride as a
    /// borrowed slot, drained at [`Self::finish`]. The async driver refuses
    /// `Inspector` mode and never fills it.
    inspected: crate::sim::evm2::inspectors::Evm2SimInspector,
    /// The steps' results, in step order.
    calls: Vec<Evm2SimRun>,
    /// The accumulated writes, merged in step order (a later step overwrites an
    /// earlier step's value for the same slot).
    writes: std::collections::BTreeMap<(Address, U256), U256>,
    /// The captured steps' committed logs, in execution order.
    logs: Vec<alloy::primitives::Log>,
    /// `Seam` capture: the slots the captured steps touched, read from the
    /// state layer.
    seam_slots: crate::sim::capture::CollectedSlots,
    /// `Seam` capture: the swaps decoded from the captured steps' committed
    /// logs.
    seam_swaps: Vec<crate::sim::capture::CapturedSwap>,
}

impl SequenceAccumulator {
    /// An empty accumulation for `steps` steps under the capture mechanism
    /// `mode`.
    fn new(mode: Evm2CaptureMode, steps: usize) -> Self {
        Self {
            mode,
            inspected: crate::sim::evm2::inspectors::Evm2SimInspector::default(),
            calls: Vec::with_capacity(steps),
            writes: std::collections::BTreeMap::new(),
            logs: Vec::new(),
            seam_slots: crate::sim::capture::CollectedSlots::default(),
            seam_swaps: Vec::new(),
        }
    }

    /// Record a [`Evm2SimStep::ReadNativeBalance`] step: the observed balance is
    /// the whole result (no code ran).
    fn observe_balance(&mut self, balance: U256) {
        self.calls.push(native_balance_run(balance));
    }

    /// Record what a settled step's capture reads BEFORE its state changes are
    /// accepted: `Seam` capture takes every slot the step loaded or wrote, from
    /// the state-layer overlay (the step hook's SLOAD/SSTORE coverage without
    /// the hook; storage-less touches are out of that domain). `Inspector`
    /// capture records its own hooks during the step, so it reads nothing here.
    fn capture_slots(&mut self, captured: bool, executed: &ExecutedTx<'_, '_, BaseEvmTypes>) {
        if !(captured && self.mode == Evm2CaptureMode::Seam) {
            return;
        }
        for changes in executed.account_changes() {
            if changes.storage.slots.is_empty() {
                continue;
            }
            for key in changes.storage.slots.keys() {
                self.seam_slots
                    .record(changes.address, alloy::primitives::B256::from(*key));
            }
        }
    }

    /// Accept the step into the accumulating overlay and record what it settled:
    /// the writes it produced, the committed logs (and, in `Seam` mode, the
    /// swaps decoded from them), and the per-step result. A step that reverted
    /// still commits — a revert's nonce/gas effects persist, matching the revm
    /// journal — and keeps the sequence running.
    fn accept(&mut self, captured: bool, executed: ExecutedTx<'_, '_, BaseEvmTypes>) {
        // The writes stream through a sink (`commit_with`) rather than
        // `account_changes`: the iterator's `impl Trait` over-captures the EVM
        // borrow under the Rust 2024 capture rules.
        let mut sink = StepWrites { writes: Vec::new() };
        let Ok(result) = executed.commit_with(&mut sink);
        let storage_writes = sink.writes;
        // `Seam` capture, post-resolve: the journal already scopes the result's
        // logs to committed frames (the swap buffer's LIFO notion), so decode
        // directly.
        if captured {
            self.logs.extend(result.logs.iter().cloned());
        }
        if captured && self.mode == Evm2CaptureMode::Seam {
            self.seam_swaps.extend(
                result
                    .logs
                    .iter()
                    .filter_map(crate::sim::capture::CapturedSwap::from_log),
            );
        }
        for (address, key, value) in &storage_writes {
            self.writes.insert((*address, *key), *value);
        }
        self.calls.push(Evm2SimRun {
            verdict: verdict_of(&result),
            gas_used: result.tx_gas_used(),
            output: result.output.clone(),
            storage_writes,
            native_balance: None,
        });
    }

    /// Assemble the settled sequence: the per-step results in order, the merged
    /// writes, and the drained capture.
    fn finish(self) -> Evm2SimSequence {
        let Self {
            mode,
            inspected,
            calls,
            writes,
            logs,
            seam_slots,
            seam_swaps,
        } = self;
        Evm2SimSequence {
            calls,
            storage_writes: writes
                .into_iter()
                .map(|((address, key), value)| (address, key, value))
                .collect(),
            capture: settle_capture(mode, inspected, logs, seam_slots, seam_swaps),
        }
    }
}

/// Assemble a finished sequence's capture from the accumulators its steps
/// filled: the inspector's trace/trace-scoped swaps in `Inspector` mode, the
/// state-layer slots and committed swaps in `Seam` mode. The committed logs are
/// captured in both modes — they are the event evidence a production
/// observation reports, not a diagnostic of one capture mechanism.
fn settle_capture(
    mode: Evm2CaptureMode,
    mut inspected: crate::sim::evm2::inspectors::Evm2SimInspector,
    logs: Vec<alloy::primitives::Log>,
    mut seam_slots: crate::sim::capture::CollectedSlots,
    seam_swaps: Vec<crate::sim::capture::CapturedSwap>,
) -> Evm2SequenceCapture {
    let log_count = logs.len();
    match mode {
        Evm2CaptureMode::Inspector => Evm2SequenceCapture {
            trace: inspected.take_trace(),
            logs,
            swaps: inspected.take_swaps(),
            reverted_swaps: inspected.take_reverted_swaps(),
            log_count: inspected.log_count(),
            access_list: inspected.take_access_list(),
        },
        Evm2CaptureMode::Seam => Evm2SequenceCapture {
            trace: crate::sim::capture::CallTrace::default(),
            logs,
            swaps: seam_swaps,
            reverted_swaps: Vec::new(),
            log_count,
            access_list: seam_slots.take_access_list(),
        },
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

/// The coroutine driver both async verbs feed: run `steps` in order on `evm`,
/// accumulating into one [`SequenceAccumulator`] (see the module doc).
///
/// The EVM is a parameter, not a field, because that is the only difference
/// between the two async verbs: one hands this function a throwaway EVM built
/// over the shared database handle, the other hands it the caller's own EVM. Both
/// therefore dispatch through the same `transact_async` /
/// `read_account_info_async` calls with the same accumulation, the same capture,
/// and the same step-indexed refusals — the fiber stack a sequence runs on is
/// whichever EVM's stack the caller supplied.
async fn drive_sequence_async(
    evm: &mut Evm<'_, BaseEvmTypes>,
    steps: &[Evm2SimStep],
    mode: Evm2CaptureMode,
) -> Result<Evm2SimSequence, Evm2SimError> {
    if mode == Evm2CaptureMode::Inspector {
        return Err(Evm2SimError::AsyncInspectorUnsupported { mode });
    }
    let mut acc = SequenceAccumulator::new(mode, steps.len());
    for (index, step) in steps.iter().enumerate() {
        let (tx, captured) = match step {
            Evm2SimStep::ReadNativeBalance(account) => {
                // A bare state read takes the fiber entrypoint too, so a cold
                // account suspends the sequence rather than refusing for want of
                // a transaction to suspend on.
                let balance = read_native_balance_async(evm, account)
                    .await
                    .map_err(|source| Evm2SimError::Sequence {
                        index,
                        source: Box::new(source),
                    })?;
                acc.observe_balance(balance);
                continue;
            }
            Evm2SimStep::Transact(tx) => (tx, false),
            Evm2SimStep::Inspect(tx) => (tx, true),
        };
        // `Inspector` capture is refused above, so every step takes the plain
        // async entrypoint; `Seam` capture reads the settled state layer
        // afterwards, exactly as on the synchronous driver.
        let executed = match evm.transact_async(tx).await {
            Ok(executed) => executed,
            Err(err) => {
                return Err(Evm2SimError::Sequence {
                    index,
                    source: Box::new(Evm2SimError::Transact(err.to_string())),
                });
            }
        };
        acc.capture_slots(captured, &executed);
        acc.accept(captured, executed);
    }
    Ok(acc.finish())
}

/// Read one [`Evm2SimStep::ReadNativeBalance`] step's balance out of the
/// sequence's accumulating state and record it, or report the refusal under the
/// step's own index. A balance-read step neither transacts nor captures: it
/// observes the account's own native balance in the accumulating overlay, so a
/// later read sees every earlier step's value movement. A database refusal is a
/// step refusal like any other — the read produced no figure, and the caller
/// must not receive a fabricated zero.
fn observe_balance_step(
    seq_evm: &mut Evm<'_, BaseEvmTypes>,
    account: &Address,
    index: usize,
    acc: &mut SequenceAccumulator,
) -> Result<(), Evm2SimError> {
    let balance =
        read_native_balance(seq_evm, account).map_err(|source| Evm2SimError::Sequence {
            index,
            source: Box::new(source),
        })?;
    acc.observe_balance(balance);
    Ok(())
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
