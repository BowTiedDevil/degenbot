//! The frame-replay seam — replay ONE externally-received signed tx through
//! a *scratch* EVM over the engine's layered DB and return a typed, settled
//! outcome (the primitive backrun/sandwich/liquidation lanes share).
//!
//! # The seam decision
//!
//! [`super::BlockSimHandle`] exposes ONE shared `&mut BlockEvm` — the
//! settlement-arbitrage strategy's 7-call fan relies on its journal and the
//! `transact_one` fan relies on (journaled-state accumulation across calls).
//! A foreign-tx frame replay must not pollute either. Rather than aliasing
//! the shared EVM (journal contamination + commit risk), the handle opens an
//! explicit seam — [`super::BlockSimHandle::scratch_evm()`] — that stacks a
//! **fresh instance of the same layered DB** (`AlloyDB` → `WrapDatabaseAsync`
//! → `BotStateDb` → [`CountingFrameDb`] → `WarmCodeCache` → `CacheDB`): the
//! cross-block `WarmCodeCacheInner` owner arc is shared (warm
//! bytecode/account caches carry over), the per-block layers are NOT aliased
//! (nothing a frame does can reach the shared engine's journal or read
//! caches). The strategy's shared view is the **strategy-private** view; this
//! is the first seam of that family — a second consumer beside `evm_mut`.
//!
//! State overrides are deliberately NOT applied on the scratch stack: a
//! foreign frame must execute against chain state, not the strategy's
//! executor funding / injected code.
//!
//! # Frame isolation (decision — in the open)
//!
//! Each frame runs in its own EVM instance with its own journal; a THROWAWAY
//! per-frame `CacheDB` layer over the ext was considered first and REJECTED:
//! it orphans every read-cache the frame populates (the layer is dropped),
//! so a warm replay re-forwards cold reads and the 0-RPC warm path can never
//! be met. The isolation the seam needs is structural, not a discarded
//! layer: revm's `transact` pipeline NEVER commits to the `Database` (the
//! journal covers the frame's writes and is CLEARED inside the pipeline —
//! `transact_one` → `finalize`), so a frame over `&mut ext` mutates only the
//! ext's read-caches and never its account/storage state. `ScratchEvm`
//! holds the ext exclusively (`replay(&mut self)`), no frame can interleave,
//! and a reverted frame surfaces zero partial state in
//! `ReplayOutcome::state` (revm rolled the journal back) — pinned by tests.
//!
//! # Cold-read accounting
//!
//! [`FrameRpcCounter`] lives INSIDE the scratch stack, below the persistent
//! ext `CacheDB` and below the cross-block `WarmCodeCache` (whose own cache
//! hits are not RPC). The frame's reads populate the persistent ext cache,
//! so a warm replay over a pre-touched set forwards ZERO reads to the cold
//! source — the soak budget depends on it.
//!
//! # Replay policy (each choice recorded here, not in call sites)
//!
//! - **Signer**: arrives as data ([`ReplayableTx::from`] — the feed/bin
//!   recovers it; replay never recovers).
//! - **Nonce**: validated against the layered view — the frame's own `nonce`
//!   field, faithfully checked. A stale pending tx surfaces as an error
//!   rather than replaying silently.
//! - **Balance**: relaxed (`cfg.disable_balance_check`) — a pending-tx
//!   caller may be under-funded at head; the frame simulates execution as if
//!   fee funding is available while staying faithful to execution semantics
//!   (value debits, reverts).
//! - **Base fee**: the projection ([`ScratchBlock::base_fee_next`]) is tried
//!   first; a `GasPriceLessThanBasefee` rejection retries ONCE with the
//!   base-fee check disabled and [`ReplayOutcome::base_fee_source`] records
//!   which path settled.
//! - **Block env**: number = the handle's pinned head + 1 (the frame targets
//!   the next block), timestamp = the handle's per-block timestamp (the same
//!   value the settlement sims run under, keeping V2 pool `_update`
//!   cumulative-sequence behavior identical).

// Solidity/EVM/rpc identifiers (SSTORE, CacheDB, TxEnv, AlloyDB, eth_call)
// are ubiquitous — match the degenbot-simulation convention.
#![expect(clippy::doc_markdown)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, Bytes, B256, U256};
use revm::context::TxEnv;
use revm::context_interface::result::{
    EVMError, ExecutionResult, InvalidTransaction, ResultAndState,
};
use revm::database_interface::{Database, DatabaseRef};
use revm::primitives::TxKind;
use revm::state::EvmState;
use revm::{ExecuteEvm, MainBuilder, MainContext};

/// One externally-received signed transaction, recovered-signer-as-data.
/// Field set mirrors the backrun feed frame (`degenbot-rpc::backrun_feed`
/// `BackrunFeedEvent`) minus transport metadata (hash, arrival time,
/// access list) the replay does not consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayableTx {
    /// The recovered signer (data, not an act — the caller recovers).
    pub from: Address,
    /// `None` = contract creation.
    pub to: Option<Address>,
    pub value: U256,
    pub data: Bytes,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub nonce: u64,
}

/// How the frame settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayStatus {
    Success,
    Reverted,
    Halted,
}

/// Which base-fee path validated the frame (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseFeeSource {
    /// The projected next base fee ([`ScratchBlock::base_fee_next`])
    /// validated the frame.
    Projected,
    /// The projection disagreed; the frame settled under
    /// `cfg.disable_base_fee`.
    DisabledFallback,
}

/// The settled outcome of one frame replay.
#[derive(Debug)]
pub struct ReplayOutcome {
    pub status: ReplayStatus,
    /// The journal-settled post-state, filtered to touched accounts. Empty
    /// for a reverted frame (revm rolls the whole journal back) — the
    /// zero-partial-state-leak invariant, observably.
    pub state: EvmState,
    /// Touched accounts + the storage slots the frame accessed, sorted for
    /// deterministic consumption.
    pub touched: Vec<(Address, Vec<U256>)>,
    /// Cold reads the frame's attempts forwarded below the scratch ext's
    /// persistent cache (basic + storage). A warm replay over a pre-touched
    /// set is ZERO here — the soak budget depends on it.
    pub rpc_reads: u64,
    pub wall: Duration,
    pub base_fee_source: BaseFeeSource,
}

/// A frame that could not be replayed against the layered view. The
/// variants carry the reconstructable nonce/saturation evidence, so every
/// observe label downstream is truthful about WHICH class it belonged to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplayFrameError {
    /// The wire frame claims a nonce AHEAD of the parent state: pending
    /// predecessors sit between the snapshot and the frame (the dominant
    /// class on today's soak, resolved by gap-prefix replay).
    #[error("gap pending: frame nonce {claimed}, parent expects {expected}")]
    GapPending { claimed: u64, expected: u64 },
    /// The frame nonce is already consumed at the parent state: the block-
    /// boundary race the live feed accumulates. Nothing to rescue; final.
    #[error("already settled: frame nonce {frame} consumed at parent nonce {parent}")]
    AlreadySettled { frame: u64, parent: u64 },
    /// The envelope itself is unprocessable for replay (zero gas limit,
    /// malformed chain id, ...) — rejected at decode-downstream.
    #[error("envelope artifact: {raw}")]
    EnvelopeArtifact { raw: std::string::String },
    /// Anything else — RPC hydrate failure, validation, or an
    /// unexpected revm variant.
    #[error("replay failed: {raw}")]
    Other { raw: std::string::String },
}

/// The projected block env a scratch frame executes under. `base_fee_next`
/// is the projected next base fee in wei per gas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchBlock {
    pub number: u64,
    pub timestamp: u64,
    pub base_fee_next: u128,
}

/// Cold-read counter for a scratch stack's cold path — embed it via
/// [`CountingFrameDb`] and hand the same handle to
/// [`ScratchEvm::with_counter`].
#[derive(Debug, Default)]
pub struct FrameRpcCounter {
    basic: AtomicU64,
    storage: AtomicU64,
}

impl FrameRpcCounter {
    /// Total forwarded cold reads (basic + storage).
    #[must_use]
    pub fn reads(&self) -> u64 {
        self.basic.load(Ordering::Relaxed) + self.storage.load(Ordering::Relaxed)
    }
}

/// A scratch engine over a layered DB: replays ONE frame at a time onto a
/// throwaway per-frame DB layer (see the module doc). Constructed by
/// [`super::BlockSimHandle::scratch_evm`] in production, or directly over
/// any [`DatabaseRef`] ext in tests — pair the ext with a
/// [`CountingFrameDb`] + [`FrameRpcCounter`] when cold-read accounting
/// matters.
#[derive(Debug)]
pub struct ScratchEvm<Db> {
    ext: Db,
    block: ScratchBlock,
    counter: Arc<FrameRpcCounter>,
}

impl<Db: Database> ScratchEvm<Db> {
    /// A scratch whose cold reads are NOT accounted (ext carries no
    /// [`CountingFrameDb`] — `ReplayOutcome::rpc_reads` stays 0).
    #[must_use]
    pub fn new(ext: Db, block: ScratchBlock) -> Self {
        Self::with_counter(ext, block, Arc::new(FrameRpcCounter::default()))
    }

    /// A scratch whose cold reads are counted through the shared handle.
    #[must_use]
    pub fn with_counter(ext: Db, block: ScratchBlock, counter: Arc<FrameRpcCounter>) -> Self {
        Self {
            ext,
            block,
            counter,
        }
    }

    /// The layered ext this scratch serves frames over. A settlement-style
    /// `transact_one` engine can be driven over the SAME ext (revm's
    /// blanket `Database for &mut D`) to observe the shared-layer view —
    /// the mutual-isolation test drives exactly that. Frame writes never
    /// land here (the per-frame layer is discarded, never committed).
    #[must_use]
    pub fn ext_mut(&mut self) -> &mut Db {
        &mut self.ext
    }

    /// The layered ext as a SHARED read view: connector-state reads the
    /// admission layer serves from the same chain view the frames replay
    /// over (the scratch's read-caches answer; cold misses count in the
    /// shared counter). See [`read_view_word`].
    #[must_use]
    pub fn ext(&self) -> &Db {
        &self.ext
    }

    /// Replay ONE frame: transact it through the full pipeline in a frame
    /// EVM over the ext (which finalizes — the journal is cleared inside
    /// the pipeline and nothing is committed; see the isolation decision
    /// in the module doc).
    ///
    /// # Errors
    ///
    /// An invalid frame (stale nonce, cold-miss RPC failure) surfaces as
    /// [`ReplayFrameError`]; a base-fee-projection rejection falls back once
    /// (see the module doc) before giving up.
    pub fn replay(&mut self, tx: &ReplayableTx) -> Result<ReplayOutcome, ReplayFrameError> {
        let started = Instant::now();
        let reads_before = self.counter.reads();
        let tx_env = tx.tx_env();

        match run_frame(&mut self.ext, &self.block, &tx_env, false) {
            Ok(settled) => Ok(outcome(
                settled,
                self.counter.reads().saturating_sub(reads_before),
                started.elapsed(),
                BaseFeeSource::Projected,
            )),
            Err(FrameAbort::BaseFeeRejected) => {
                run_frame(&mut self.ext, &self.block, &tx_env, true)
                    .map(|settled| {
                        outcome(
                            settled,
                            self.counter.reads().saturating_sub(reads_before),
                            started.elapsed(),
                            BaseFeeSource::DisabledFallback,
                        )
                    })
                    .map_err(|abort| match abort {
                        FrameAbort::OtherError(e) => e,
                        FrameAbort::BaseFeeRejected => ReplayFrameError::Other {
                            raw: abort.to_string(),
                        },
                    })
            }
            Err(FrameAbort::OtherError(e)) => Err(e),
        }
    }
}

/// Read ONE word through a layered [`DatabaseRef`] view — the admission
/// layer's read for connector state from the SAME chain view the frames
/// replay over (scratch read-cache first, cold misses forwarded to the
/// layered DB's cold source). `None` on a read failure (transport error);
/// a zero word reads through as `Some(U256::ZERO)` — the caller decides
/// whether zero is a usable pool state (the journal never fabricates).
#[must_use]
pub fn read_view_word<Ext: DatabaseRef>(ext: &Ext, address: Address, slot: U256) -> Option<U256> {
    ext.storage_ref(address, slot).ok()
}

impl ReplayableTx {
    /// The revm tx env: EIP-1559 semantics (the builder derives
    /// `tx_type = EIP-1559` from the priority fee being `Some`), with
    /// `gas_price` carrying `max_fee_per_gas` (revm's unified field).
    fn tx_env(&self) -> TxEnv {
        let kind = match self.to {
            Some(to) => TxKind::Call(to),
            None => TxKind::Create,
        };
        {
            #[expect(clippy::expect_used)] // well-formed by field construction
            let env = TxEnv::builder()
                .caller(self.from)
                .kind(kind)
                .data(self.data.clone())
                .value(self.value)
                .gas_limit(self.gas_limit)
                .nonce(self.nonce)
                .gas_price(self.max_fee_per_gas)
                .gas_priority_fee(Some(self.max_priority_fee_per_gas))
                .build()
                .expect("ReplayableTx fields always build a valid TxEnv");
            env
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The counting layer (embeds FrameRpcCounter at the stack's cold path)
// ─────────────────────────────────────────────────────────────────────────

/// The cold-path bridge of a scratch stack: counts forwarded reads and
/// otherwise forwards untouched. Place it below the persistent ext `CacheDB`
/// and below the cross-block `WarmCodeCache` (whose cache hits are not RPC)
/// — see the module doc.
#[derive(Debug)]
pub struct CountingFrameDb<ExtDb> {
    inner: ExtDb,
    counter: Arc<FrameRpcCounter>,
}

impl<ExtDb> CountingFrameDb<ExtDb> {
    /// Wrap `inner`'s cold reads with `counter`.
    #[must_use]
    pub fn new(inner: ExtDb, counter: Arc<FrameRpcCounter>) -> Self {
        Self { inner, counter }
    }
}

impl<ExtDb: DatabaseRef> DatabaseRef for CountingFrameDb<ExtDb> {
    type Error = ExtDb::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        self.counter.basic.fetch_add(1, Ordering::Relaxed);
        self.inner.basic_ref(address)
    }

    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.counter.storage.fetch_add(1, Ordering::Relaxed);
        self.inner.storage_ref(address, index)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        self.inner.code_by_hash_ref(code_hash)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.inner.block_hash_ref(number)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The per-frame layer + execution
// ─────────────────────────────────────────────────────────────────────────

type FrameEvm<'a, Db> = revm::MainnetEvm<revm::handler::MainnetContext<&'a mut Db>>;

/// One attempt: build a frame EVM over the ext and transact the frame
/// through the full pipeline (transact_one → finalize — the journal is
/// cleared inside the pipeline and nothing is committed, so the frame
/// touches only the ext's read-caches).
fn run_frame<Db: Database>(
    ext: &mut Db,
    block: &ScratchBlock,
    tx_env: &TxEnv,
    disable_base_fee: bool,
) -> Result<ResultAndState, FrameAbort> {
    let mut ctx = revm::context::Context::mainnet();
    ctx.cfg.disable_balance_check = true;
    ctx.cfg.disable_base_fee = disable_base_fee;
    let mut evm: FrameEvm<'_, Db> = ctx.with_db(ext).build_mainnet();
    evm.ctx.modify_block(|b| {
        b.basefee = u64::try_from(block.base_fee_next).unwrap_or(u64::MAX);
        b.number = U256::from(block.number);
        b.timestamp = U256::from(block.timestamp);
    });
    evm.transact(tx_env.clone()).map_err(|err| match err {
        EVMError::Transaction(InvalidTransaction::GasPriceLessThanBasefee) => {
            FrameAbort::BaseFeeRejected
        }
        EVMError::Transaction(InvalidTransaction::NonceTooHigh { tx, state }) => {
            FrameAbort::OtherError(ReplayFrameError::GapPending {
                claimed: tx,
                expected: state,
            })
        }
        EVMError::Transaction(InvalidTransaction::NonceTooLow { tx, state }) => {
            FrameAbort::OtherError(ReplayFrameError::AlreadySettled {
                frame: tx,
                parent: state,
            })
        }
        EVMError::Transaction(InvalidTransaction::CallGasCostMoreThanGasLimit {
            initial_gas,
            gas_limit,
        }) => FrameAbort::OtherError(ReplayFrameError::EnvelopeArtifact {
            raw: format!("call gas cost ({initial_gas}) exceeds the gas limit ({gas_limit})"),
        }),
        other => FrameAbort::OtherError(ReplayFrameError::Other {
            raw: other.to_string(),
        }),
    })
}

enum FrameAbort {
    BaseFeeRejected,
    OtherError(ReplayFrameError),
}

impl std::fmt::Display for FrameAbort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BaseFeeRejected => {
                write!(f, "base-fee projection rejected (fallback also failed)")
            }
            Self::OtherError(e) => write!(f, "{e}"),
        }
    }
}

/// Settle a `ResultAndState` into the typed [`ReplayOutcome`] (status,
/// touched-filtered state, deterministic touched-slot set).
fn outcome(
    settled: ResultAndState,
    rpc_reads: u64,
    wall: Duration,
    base_fee_source: BaseFeeSource,
) -> ReplayOutcome {
    let status = match settled.result {
        ExecutionResult::Success { .. } => ReplayStatus::Success,
        ExecutionResult::Revert { .. } => ReplayStatus::Reverted,
        ExecutionResult::Halt { .. } => ReplayStatus::Halted,
    };
    let state: EvmState = settled
        .state
        .into_iter()
        .filter(|(_, account)| account.is_touched())
        .collect();
    let mut touched: Vec<(Address, Vec<U256>)> = state
        .iter()
        .map(|(address, account)| {
            let mut slots: Vec<U256> = account.storage.keys().copied().collect();
            slots.sort_unstable();
            (*address, slots)
        })
        .collect();
    touched.sort_unstable_by_key(|(address, _)| *address);
    ReplayOutcome {
        status,
        state,
        touched,
        rpc_reads,
        wall,
        base_fee_source,
    }
}
