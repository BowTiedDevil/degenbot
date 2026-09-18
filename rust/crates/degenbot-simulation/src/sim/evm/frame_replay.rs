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
//! # Sequences (the commit-accumulating overlay)
//!
//! A frame whose parent state has unmined predecessors cannot replay against
//! that parent ([`ReplayFrameError::GapPending`]): the predecessors sit
//! between the snapshot and the frame. [`ScratchEvm::replay_sequence`] runs
//! each hydrated predecessor through the SAME pipeline as a plain frame, over
//! a local overlay DB (`SequenceDb`) that reads through to the ext and merges
//! each step's `ResultAndState` into itself; the frame then replays over the
//! accumulated view. The overlay is the ONLY writer — the ext receives reads,
//! never writes — so the isolation contract above holds unchanged and the
//! overlay (with everything it accumulated) drops when the call returns. A
//! predecessor failure surfaces as [`SequenceReplayError::Predecessor`] (the
//! frame never ran, so its quarantine life cannot be resolved on that
//! evidence); only the frame's own failure is [`SequenceReplayError::Frame`].
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

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, Bytes, B256, U256};
use revm::bytecode::Bytecode;
use revm::context::TxEnv;
use revm::context_interface::result::{
    EVMError, ExecutionResult, InvalidTransaction, ResultAndState,
};
use revm::database::CacheDB;
use revm::database_interface::{Database, DatabaseCommit, DatabaseRef};
use revm::primitives::TxKind;
use revm::state::{AccountInfo, EvmState};
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

/// How one predecessor in a sequence settled. A predecessor occupies its
/// nonce even when it reverted or halted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredStatus {
    Success,
    Reverted,
    Halted,
    /// The parent state had already consumed the nonce — stale pool evidence.
    /// The predecessor's effects are already in the parent state, so it
    /// commits nothing and the sequence continues.
    AlreadyMined,
}

impl PredStatus {
    /// The trace label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Reverted => "reverted",
            Self::Halted => "halted",
            Self::AlreadyMined => "already_mined",
        }
    }
}

impl From<ReplayStatus> for PredStatus {
    fn from(status: ReplayStatus) -> Self {
        match status {
            ReplayStatus::Success => Self::Success,
            ReplayStatus::Reverted => Self::Reverted,
            ReplayStatus::Halted => Self::Halted,
        }
    }
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

/// The settled outcome of a sequence replay: each predecessor's status in
/// ascending prefix order, plus the frame's typed outcome over the
/// accumulated view.
#[derive(Debug)]
pub struct SequenceOutcome {
    pub predecessors: Vec<PredStatus>,
    /// Each predecessor's transaction gas used, in ascending prefix order.
    /// `AlreadyMined` predecessors commit nothing and read as `0`.
    pub predecessor_gas: Vec<u64>,
    pub frame: ReplayOutcome,
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
    /// The transaction is structurally unprocessable as sent (zero gas
    /// limit, intrinsic cost above gas limit, malformed chain id, ...) — no
    /// chain state could ever admit it.
    #[error("malformed transaction: {raw}")]
    MalformedTransaction { raw: std::string::String },
    /// Anything else — RPC hydrate failure, validation, or an
    /// unexpected revm variant.
    #[error("replay failed: {raw}")]
    Other { raw: std::string::String },
    /// The projected base fee rejected the tx AND the disabled-base-fee
    /// retry rejected it too: no chain state admits this gas price now, but
    /// the owner may replace it with a repriced tx — transient, never
    /// malformed-terminal and never structural-permanent.
    #[error("mispriced transaction: {raw}")]
    Mispriced { raw: std::string::String },
}

/// A sequence replay's typed failure, split by which side of the sequence
/// aborted. The rescue router must never mistake a predecessor's death for
/// the frame's: a predecessor that cannot run as fetched is pool-pred lane
/// evidence (the owner may replace it), not proof the frame is dead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SequenceReplayError {
    /// A prefix predecessor failed before the frame ran. The frame is
    /// untouched — its quarantine life cannot be resolved on this evidence.
    #[error("sequence predecessor at nonce {nonce} failed: {source}")]
    Predecessor {
        /// The offending predecessor's claimed nonce.
        nonce: u64,
        /// The predecessor's typed failure class.
        #[source]
        source: ReplayFrameError,
    },
    /// The frame itself failed after the prefix settled cleanly.
    #[error("frame: {0}")]
    Frame(ReplayFrameError),
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
        let (settled, base_fee_source) =
            run_frame_with_fallback(&mut self.ext, &self.block, &tx.tx_env())?;
        Ok(outcome(
            settled,
            self.counter.reads().saturating_sub(reads_before),
            started.elapsed(),
            base_fee_source,
        ))
    }

    /// Replay a hydrated predecessor prefix, ascending, then `frame` over the
    /// accumulated view (see the module doc's sequence section).
    ///
    /// Each predecessor runs through the SAME pipeline and error taxonomy as
    /// [`Self::replay`]. A reverted or halted predecessor is not a failure:
    /// it occupies its nonce, so its settled state is committed and the
    /// sequence continues. A predecessor whose nonce the parent state already
    /// consumed ([`ReplayFrameError::AlreadySettled`]) is
    /// [`PredStatus::AlreadyMined`] — its effects are already in the parent
    /// state and it commits nothing. Every other predecessor error surfaces as
    /// [`SequenceReplayError::Predecessor`] with the offending nonce, so the
    /// caller routes it without ever resolving the frame.
    ///
    /// # Errors
    ///
    /// [`SequenceReplayError::Predecessor`] aborts the sequence before the
    /// frame runs; [`SequenceReplayError::Frame`] is the frame's own failure
    /// over the accumulated view.
    pub fn replay_sequence(
        &mut self,
        prefix: &[ReplayableTx],
        frame: &ReplayableTx,
    ) -> Result<SequenceOutcome, SequenceReplayError> {
        let started = Instant::now();
        let reads_before = self.counter.reads();
        let mut overlay = SequenceDb::new(&mut self.ext);
        let mut predecessors = Vec::with_capacity(prefix.len());
        let mut predecessor_gas = Vec::with_capacity(prefix.len());
        for predecessor in prefix {
            match run_frame_with_fallback(&mut overlay, &self.block, &predecessor.tx_env()) {
                Ok((settled, _source)) => {
                    predecessors.push(execution_status(&settled.result).into());
                    predecessor_gas.push(settled.result.tx_gas_used());
                    overlay.commit(settled.state);
                }
                Err(ReplayFrameError::AlreadySettled { .. }) => {
                    predecessors.push(PredStatus::AlreadyMined);
                    predecessor_gas.push(0);
                }
                Err(source) => {
                    return Err(SequenceReplayError::Predecessor {
                        nonce: predecessor.nonce,
                        source,
                    });
                }
            }
        }
        let (settled, base_fee_source) =
            run_frame_with_fallback(&mut overlay, &self.block, &frame.tx_env())
                .map_err(SequenceReplayError::Frame)?;
        Ok(SequenceOutcome {
            predecessors,
            predecessor_gas,
            frame: outcome(
                settled,
                self.counter.reads().saturating_sub(reads_before),
                started.elapsed(),
                base_fee_source,
            ),
        })
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
// The sequence overlay (commit-accumulating; see the module doc)
// ─────────────────────────────────────────────────────────────────────────

/// The commit-accumulating overlay of a [`ScratchEvm::replay_sequence`]: a
/// read-through [`CacheDB`] layered over the scratch ext. Reads consult the
/// overlay and, on a miss, fall through [`WarmingExt`] to the ext's MUTABLE
/// [`Database`] path — the SAME cold path a plain frame warms — so the
/// ext's per-block read-caches accumulate during a sequence exactly as they
/// do for a plain frame and [`FrameRpcCounter`] counts the same misses. A
/// step's `ResultAndState` is merged into the overlay between steps. The ext
/// receives reads only — never a write — and the overlay drops when the call
/// returns.
struct SequenceDb<'a, Db> {
    overlay: CacheDB<WarmingExt<'a, Db>>,
}

impl<'a, Db: Database> SequenceDb<'a, Db> {
    fn new(ext: &'a mut Db) -> Self {
        Self {
            overlay: CacheDB::new(WarmingExt {
                ext: RefCell::new(ext),
            }),
        }
    }

    /// Merge one step's settled state into the overlay. `CacheDB`'s commit
    /// applies the whole touched `Account` entry (replace nonce/balance/code,
    /// merge storage, clear on create/selfdestruct) — the post-journal
    /// snapshot revm already derived, never re-derived here.
    fn commit(&mut self, state: EvmState) {
        self.overlay.commit(state);
    }
}

impl<Db: Database> Database for SequenceDb<'_, Db> {
    type Error = Db::Error;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.overlay.basic(address)
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.overlay.code_by_hash(code_hash)
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.overlay.storage(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.overlay.block_hash(number)
    }
}

/// The overlay's inner ext as a [`DatabaseRef`] that forwards every miss to
/// the ext's MUTABLE [`Database`] methods. `CacheDB` only ever calls the
/// `*_ref` methods on its inner, and the `&mut Db` blanket `DatabaseRef` impl
/// forwards straight to `Db::*_ref`, which skips the cache inserts; interior
/// mutability is what lets a sequence miss warm the same read-caches a plain
/// replay does.
struct WarmingExt<'a, Db> {
    ext: RefCell<&'a mut Db>,
}

impl<Db: Database> DatabaseRef for WarmingExt<'_, Db> {
    type Error = Db::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.ext.borrow_mut().basic(address)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.ext.borrow_mut().code_by_hash(code_hash)
    }

    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.ext.borrow_mut().storage(address, index)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.ext.borrow_mut().block_hash(number)
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
        }) => FrameAbort::OtherError(ReplayFrameError::MalformedTransaction {
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

/// Run one tx through [`run_frame`] with the seam's projection ritual: the
/// projected base fee first, the `disable_base_fee` retry once on a
/// `GasPriceLessThanBasefee` rejection. Returns which path settled.
fn run_frame_with_fallback<Db: Database>(
    db: &mut Db,
    block: &ScratchBlock,
    tx_env: &TxEnv,
) -> Result<(ResultAndState, BaseFeeSource), ReplayFrameError> {
    match run_frame(db, block, tx_env, false) {
        Ok(settled) => Ok((settled, BaseFeeSource::Projected)),
        Err(FrameAbort::BaseFeeRejected) => run_frame(db, block, tx_env, true)
            .map(|settled| (settled, BaseFeeSource::DisabledFallback))
            .map_err(|abort| match abort {
                FrameAbort::OtherError(e) => e,
                FrameAbort::BaseFeeRejected => ReplayFrameError::Mispriced {
                    raw: abort.to_string(),
                },
            }),
        Err(FrameAbort::OtherError(e)) => Err(e),
    }
}

/// The seam's execution-status taxonomy for a settled result.
fn execution_status(result: &ExecutionResult) -> ReplayStatus {
    match result {
        ExecutionResult::Success { .. } => ReplayStatus::Success,
        ExecutionResult::Revert { .. } => ReplayStatus::Reverted,
        ExecutionResult::Halt { .. } => ReplayStatus::Halted,
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
    let status = execution_status(&settled.result);
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
