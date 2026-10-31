//! The evm2 per-block shared-EVM handle — the twin of
//! `sim::evm::simulator::BlockSimHandle` (ADR-089 phase 4/5: the hard-swap
//! wiring, not a genericization layer; the revm handle retires at cutover).
//!
//! Same build inputs, same stack shape, same verbs: the layered DB
//! (`AlloyAsyncDb` → `BlockingAsyncDb` → `BotStateDb` → `WarmCodeCache` →
//! `CacheDB`), the state overrides applied ONCE, the block env pinned from
//! the pump's primitives, the nonce check disabled (each bundle call is
//! `eth_call`-shaped — parity with the node's lenient simulate), and the
//! composite capture inspector baked in. The frame-replay scratch stacks a
//! fresh counted instance of the same layers, lazily, once, and carries this
//! handle's interpreter runner into every frame EVM it builds.
//!
//! The engine spec is `SpecId::OSAKA` — the same fork revm's mainnet
//! production default pins (`SpecId::default` there is OSAKA too).

use std::sync::Arc;

use parking_lot::RwLock;

use ::evm2::env::BlockEnvExt;
use ::evm2::ethereum::ethereum_tx_registry;
use ::evm2::evm::{CacheDB, Db};
use ::evm2::{BaseEvmTypes, Evm, EvmFeatures, Precompiles, SpecId, Version};

use alloy::primitives::U256;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_substrate::SimAnchorOracle;

use super::alloy_async_db::AlloyAsyncDb;
use super::blocking_async_db::BlockingAsyncDb;
use super::bot_state_db::BotStateDb;
use super::frame_replay::{Evm2CountingFrameDb, Evm2ScratchEvm};
use super::inspectors::Evm2SimInspector;
use super::session::Evm2SimSession;
use super::state_override::apply_simulation_overrides;
use super::warm_code_cache::{WarmCodeCache, WarmCodeCacheInner};
use crate::sim::frame_replay::{FrameRpcCounter, ScratchBlock};
use crate::sim::SimulationOverrideParams;

/// The production engine spec (see the module doc).
pub const EVM2_PRODUCTION_SPEC: SpecId = SpecId::OSAKA;

/// The layered production DB stack behind the shared EVM.
pub type Evm2ProductionBlockDb<'a> =
    CacheDB<Db<WarmCodeCache<BotStateDb<'a, BlockingAsyncDb<'a>>>>>;

/// The scratch stack's layered ext (the counting bridge in the cold path).
pub type Evm2ScratchDb<'a> =
    CacheDB<Db<WarmCodeCache<Evm2CountingFrameDb<BotStateDb<'a, BlockingAsyncDb<'a>>>>>>;

/// Owns a per-block shared evm2 EVM for the in-process sim fan-out (the
/// revm-side `BlockSimHandle` contract — see its module docs for the serial
/// fan-out rationale, which carries over unchanged).
pub struct Evm2BlockSimHandle<'a> {
    /// The shared evm2 EVM with the composite inspector baked in. Borrowed
    /// for the scope of one [`Evm2SimSession`] at a time; never aliased —
    /// the fan-out is serial.
    evm: Evm<'a, BaseEvmTypes>,
    /// The block env the EVM was built with (sessions capture it for the
    /// journaled sequence's throwaway EVM — the EVM exposes no getter).
    block_env: BlockEnvExt,
    /// Build inputs retained ONLY to stack frame-replay scratch EVMs (the
    /// revm handle's same retention contract).
    provider: &'a AlloyProvider,
    base_fee_next: u128,
    current_block: u64,
    block_timestamp: u64,
    oracle: &'a dyn SimAnchorOracle,
    warm_cache: Arc<RwLock<WarmCodeCacheInner>>,
    storage_memo: Option<Arc<crate::sim::StorageMemo>>,
    verify_divergence: bool,
    /// The frame-replay scratch EVM, built lazily on the FIRST
    /// [`Self::scratch_evm`] call (same lifetime contract as the revm
    /// handle's).
    scratch: Option<Evm2ScratchEvm<Evm2ScratchDb<'a>>>,
    /// The process-shared interpreter runner (the JIT spine), threaded into
    /// every session this handle opens and every frame EVM its scratch
    /// builds.
    jit: Option<std::sync::Arc<dyn ::evm2::evm::InterpreterRunner<BaseEvmTypes>>>,
}

impl<'a> Evm2BlockSimHandle<'a> {
    /// Build the per-block shared EVM (see the module doc). Returns `None`
    /// on a build failure (no ambient multi-threaded runtime for the
    /// blocking adapter); the caller tallies `rpc-failed` for every
    /// candidate in that case.
    #[expect(clippy::too_many_arguments)] // mirrors the revm handle's build
    #[must_use]
    pub fn build(
        provider: &'a AlloyProvider,
        base_fee_next: u128,
        current_block: u64,
        block_timestamp: u64,
        override_params: &SimulationOverrideParams,
        oracle: &'a dyn SimAnchorOracle,
        warm_cache: &Arc<RwLock<WarmCodeCacheInner>>,
        storage_memo: Option<&Arc<crate::sim::StorageMemo>>,
        verify_divergence: bool,
        jit: Option<std::sync::Arc<dyn ::evm2::evm::InterpreterRunner<BaseEvmTypes>>>,
    ) -> Option<Self> {
        let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(provider, current_block))?;
        let bot_state_db = BotStateDb::new(oracle, blocking)
            .with_storage_memo_opt(storage_memo)
            .with_divergence_probe(verify_divergence);
        let warm_code_cache =
            WarmCodeCache::with_owner(Arc::clone(warm_cache), current_block, bot_state_db);
        let mut cache_db = CacheDB::new(Db::new(warm_code_cache));
        // The same per-block-invariant overrides as the revm handle (owner
        // funding, executor code injection, warmup slots).
        apply_simulation_overrides(&mut cache_db, override_params);
        let block_env = BlockEnvExt {
            basefee: U256::from(base_fee_next),
            number: U256::from(current_block),
            timestamp: U256::from(block_timestamp),
            ..Default::default()
        };
        let mut version = Version::new(EVM2_PRODUCTION_SPEC);
        version.features.remove(EvmFeatures::NONCE_CHECK);
        let mut evm = Evm::new_with_execution_config(
            ::evm2::ExecutionConfig::for_spec_and_version(EVM2_PRODUCTION_SPEC, version),
            EVM2_PRODUCTION_SPEC,
            block_env,
            ethereum_tx_registry(EVM2_PRODUCTION_SPEC),
            cache_db,
            Precompiles::base(EVM2_PRODUCTION_SPEC),
        );
        evm.set_inspector(Evm2SimInspector::default());
        Some(Self {
            evm,
            block_env,
            provider,
            base_fee_next,
            current_block,
            block_timestamp,
            oracle,
            jit,
            warm_cache: Arc::clone(warm_cache),
            storage_memo: storage_memo.cloned(),
            verify_divergence,
            scratch: None,
        })
    }

    /// Open an [`Evm2SimSession`] over this block's shared EVM — the
    /// engine's one door to it (the revm handle's `session` contract).
    #[must_use]
    pub fn session(&mut self) -> Evm2SimSession<'_, 'a> {
        Evm2SimSession::with_block_env(&mut self.evm, self.block_env).with_jit(self.jit.clone())
    }

    /// The frame-replay seam: a scratch engine over a FRESHLY STACKED
    /// instance of the layered DB with the counting bridge in the cold
    /// path (the revm handle's `scratch_evm` contract, same lazy-once
    /// lifetime). State overrides are NOT applied — a foreign frame runs
    /// against chain state. The scratch carries this handle's interpreter
    /// runner, so a frame whose program the JIT backend holds replays
    /// compiled wherever the process has one.
    ///
    /// Returns `None` when no ambient multi-threaded runtime exists for
    /// the fresh blocking adapter.
    pub fn scratch_evm(&mut self) -> Option<&mut Evm2ScratchEvm<Evm2ScratchDb<'a>>> {
        if self.scratch.is_none() {
            let counter = Arc::new(FrameRpcCounter::default());
            self.scratch =
                BlockingAsyncDb::new(AlloyAsyncDb::new(self.provider, self.current_block)).map(
                    |blocking| {
                        let bot_state_db = BotStateDb::new(self.oracle, blocking)
                            .with_storage_memo_opt(self.storage_memo.as_ref())
                            .with_divergence_probe(self.verify_divergence);
                        let counted_cold_path =
                            Evm2CountingFrameDb::new(bot_state_db, Arc::clone(&counter));
                        let warm_code_cache = WarmCodeCache::with_owner(
                            Arc::clone(&self.warm_cache),
                            self.current_block,
                            counted_cold_path,
                        );
                        let ext = CacheDB::new(Db::new(warm_code_cache));
                        Evm2ScratchEvm::with_counter(
                            ext,
                            ScratchBlock {
                                number: self.current_block.saturating_add(1),
                                timestamp: self.block_timestamp,
                                base_fee_next: self.base_fee_next,
                            },
                            counter,
                        )
                        .with_jit(self.jit.clone())
                    },
                );
        }
        self.scratch.as_mut()
    }
}
