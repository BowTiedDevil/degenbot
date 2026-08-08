//! The in-process simulation engine — the per-block shared EVM handle.
//!
//! ADR-019 D4/D7 (decision R — Rust-canonical + strategy/engine separation):
//! this module owns the **engine** — the revm EVM handle (`BlockSimHandle`),
//! the layered DB stack (`BlockEvm` / `ProductionBlockDb`), the
//! `ArcDynProviderEthereum` provider newtype, and the block-env wiring + the
//! state-override application. The **strategy** (the 7-call pre/post-balance
//! bundle, `compute_priority_fee`, `decode_balance`, `SimResult`,
//! `dispatch_profitable_results`, `SimulateContext`, `SimulatePath`,
//! `FailBuckets`, the calldata builders) relocated to the
//! `degenbot-backrun-strategy` crate, which drives the borrowed `&mut evm`
//! the engine exposes via [`BlockSimHandle::evm_mut`].
//!
//! The engine stays generic + thin: it never names `SimulateContext` (a
//! strategy type) — [`BlockSimHandle::build`] takes the block-env primitives
//! (`provider`, `base_fee_next`, `current_block`, `block_timestamp`) + a
//! projected `&SimulationOverrideParams` directly. Multiple searcher
//! strategies (the backrun bundle today; sandwich/JIT-L/liquidation later) can
//! drive the same engine with their own `SimulateContext`-equivalent config.
//!
//! # Per-block shared-EVM handle (Tier 1, `V5HCR5`)
//!
//! Retires the per-path `simulate_in_process` (which rebuilt the full
//! `CacheDB`+EVM stack per call). The per-block handle is built ONCE per block
//! (by the strategy's `dispatch_profitable_results`) and shared (as `&mut`)
//! across the serial fan-out. The measured shape: the trigger path pays the
//! cold RPCs, the fan-out hits the warmed `CacheDB` at ~1 µs p50 (~50× faster
//! than the per-path fresh-`CacheDB` config A — benchmark
//! `examples/rpc_cache_fanout.rs`).
//!
//! # Correctness — shared `CacheDB` does NOT leak execute() SSTOREs
//!
//! revm 41 splits journalling into `transact_one` (accumulate to the journal) →
//! `finalize` (return the `State` + CLEAR the journal; does NOT commit to the
//! DB) → `commit` (write a `State` to the DB). The strategy's
//! `simulate_path_on_evm` calls `transact_one` + `finalize` only — it NEVER
//! calls `commit`. So execute()'s SSTOREs live in the per-path `State`
//! returned by `finalize` (then discarded), NOT in the shared `CacheDB`. The
//! `CacheDB` accumulates only READ caches (account info, bytecode, storage
//! slots read during balanceOf/execute), which is exactly the latency win +
//! never a stale-write hazard. (revm-handler-41/src/api.rs:44-110 is the
//! source of truth on this split.)

// Solidity/EVM identifiers (WETH9, PoolManager, ERC6909, cacheMD, databaseRef)
// are ubiquitous here — match the degenbot-simulation convention.
#![allow(clippy::doc_markdown)]

use alloy::eips::BlockId;
use alloy::network::Ethereum;
use alloy::primitives::U256;
use alloy::providers::{Provider, RootProvider};
use degenbot_bot::bot_core::BotState;
use parking_lot::RwLock;
use revm::database::CacheDB;
use revm::database_interface::WrapDatabaseAsync;
use std::sync::Arc;

use super::state_override::SimulationOverrideParams;
use revm::{MainBuilder, MainContext};

// ─────────────────────────────────────────────────────────────────────────
// The type-erased provider newtype (bridges `Arc<dyn Provider>` → `Provider`)
// ─────────────────────────────────────────────────────────────────────────

/// A `Provider<Ethereum>` newtype over `Arc<dyn Provider<Ethereum>>`, so the
/// type-erased provider from [`AlloyProvider::provider_arc`](degenbot_rpc::provider::AlloyProvider::provider_arc)
/// satisfies `P: Provider<Ethereum>` for [`revm::database::AlloyDB::new`].
///
/// Alloy's `Provider` trait carries `#[auto_impl::auto_impl(&, &mut, Rc, Arc,
/// Box)]`, but the generated impl is `impl<T: Provider + Sized> Provider for
/// Arc<T>` — it does **not** cover `?Sized` trait objects, so
/// `Arc<dyn Provider<Ethereum>>` itself does not satisfy `P: Provider`. This
/// newtype bridges the gap: it stores the `Arc<dyn Provider>` (always `Sized`)
/// and impls `Provider` by delegating `root()` — the one non-default `Provider`
/// method. Every other `Provider` method has a default impl routed through
/// `root()`.
#[derive(Clone)]
pub struct ArcDynProviderEthereum(Arc<dyn Provider<Ethereum>>);

impl Provider<Ethereum> for ArcDynProviderEthereum {
    fn root(&self) -> &RootProvider<Ethereum> {
        self.0.root()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The per-block shared-EVM handle (Tier 1, task `V5HCR5`)
// ─────────────────────────────────────────────────────────────────────────

/// The production DB stack backing a per-block [`BlockEvm`] —
/// `CacheDB<WarmCodeCache<BotStateDb<WrapDatabaseAsync<AlloyDB<...>>>>>`.
/// The `WarmCodeCache` layer (cross-block bytecode + account-existence, per-
/// entry TTL'd) sits between the per-block `CacheDB` and the `BotStateDb`
/// storage-forwarding seam; the `AlloyDB` cold-miss fallback (RPC) sits at the
/// bottom (see [`super::warm_code_cache`]).
pub type ProductionBlockDb<'a> = CacheDB<
    super::WarmCodeCache<
        super::BotStateDb<
            'a,
            WrapDatabaseAsync<revm::database::AlloyDB<Ethereum, ArcDynProviderEthereum>>,
        >,
    >,
>;

/// The concrete per-block EVM type held by [`BlockSimHandle`] — revm's
/// [`revm::MainnetEvm`] over the production [`ProductionBlockDb`] stack. The
/// inspector type parameter is [`super::inspectors::SimInspector`] (a nested
/// tuple `(AccessListCollector, (CallTraceInspector,
/// SwapEventCaptureInspector))` — ADR-019 D3 + ergo epic 63I7WJ) — baked in so
/// the strategy's `simulate_path_on_evm` can attach it to `execute()`'s
/// `inspect_one` run and drain the access list + call trace + swap events.
/// revm's blanket `Inspector` impl covers 2-tuples only, so the three-way
/// composition is nested (`AccessListCollector` is `L`, the
/// `CallTraceInspector`/`SwapEventCaptureInspector` pair is `R`).
pub type BlockEvm<'a> = revm::MainnetEvm<
    revm::handler::MainnetContext<ProductionBlockDb<'a>>,
    super::inspectors::SimInspector,
>;

/// Owns a per-block shared EVM (one `CacheDB` + revm `Context`, state overrides
/// applied once) for the in-process sim fan-out (Tier 1, `V5HCR5`).
///
/// Built ONCE per block by [`BlockSimHandle::build`] (the strategy's
/// `dispatch_profitable_results` calls it), then each candidate path is
/// simulated SERIALLY by the strategy's `simulate_path_on_evm` over the shared
/// `&mut evm` exposed by [`BlockSimHandle::evm_mut`]. Per-path isolation is
/// revm's `finalize()` (clears the journal between paths — execute()'s
/// SSTOREs live in the per-path `State` returned by `finalize`, never
/// committed to the shared `CacheDB`; see the module-level note above). The
/// `CacheDB` + EVM drop naturally at end of block — no cross-block caching in
/// Tier 1, so memory is bounded by one block's working set.
///
/// Serial (not `buffer_unordered`) because a shared `&mut evm` can't be held
/// across `'static`+`Send` futures. The benchmark's config B (serial-warm,
/// ~1 µs p50) beats config A (parallel-cold, ~590 µs p50) by ~60× — losing
/// concurrency is a NET WIN because the per-path RPC cold-miss dwarfs the
/// per-path EVM execution.
///
/// The state overrides — owner ETH funding, executor code injection, warmup
/// slots, WETH balance — are all per-block-invariant, so applying them once at
/// [`BlockSimHandle::build`] is semantically identical to applying them
/// per-path.
pub struct BlockSimHandle<'a> {
    /// The shared revm EVM. `&mut`-borrowed per path via [`evm_mut`](Self::evm_mut)
    /// by the strategy; never aliased — the fan-out is serial.
    evm: BlockEvm<'a>,
}

impl<'a> BlockSimHandle<'a> {
    /// Build the per-block shared EVM: the layered DB (`AlloyDB` →
    /// `WrapDatabaseAsync` → `BotStateDb` → `WarmCodeCache` → `CacheDB`), the
    /// state overrides applied ONCE (every override is per-block-invariant),
    /// and the revm `Context` pinned to the block env. Returns `None` on a
    /// build failure (no ambient multi-threaded runtime for
    /// `WrapDatabaseAsync`, or an override-application error); the caller
    /// tallies `rpc-failed` for every candidate in that case.
    ///
    /// The block-env primitives (`provider`, `base_fee_next`, `current_block`,
    /// `block_timestamp`) + the projected [`SimulationOverrideParams`] are
    /// handed in directly — the engine never names the strategy's
    /// `SimulateContext` (ADR-019 D7, decision R — strategy/engine separation).
    ///
    /// `warm_cache` is the cross-block persistent bytecode + account-existence
    /// layer ([`super::WarmCodeCache`]); it persists across blocks via the
    /// engine owner's `Arc<RwLock<WarmCodeCacheInner>>` (cloned into the
    /// per-block `WarmCodeCache` wrapper value here — only the inner map
    /// survives across blocks). The per-block `CacheDB` layered above still
    /// drops at end of block (overrides + mutable storage are per-block).
    ///
    /// `WrapDatabaseAsync::new` returns `None` if there is no runtime or the
    /// runtime is `CurrentThread`. The pump drives the dispatch through
    /// `pyo3_async_runtimes` (`Builder::new_multi_thread()`), so the `None`
    /// path is unreachable in production — `None` means the dispatch host lost
    /// its runtime.
    #[must_use]
    pub fn build(
        provider: &degenbot_rpc::provider::AlloyProvider,
        base_fee_next: u128,
        current_block: u64,
        block_timestamp: u64,
        override_params: &SimulationOverrideParams,
        bot_state: &'a BotState,
        warm_cache: &Arc<RwLock<super::WarmCodeCacheInner>>,
    ) -> Option<Self> {
        // `AlloyDB::new` requires `P: Provider<Ethereum>` by value; the
        // type-erased `Arc<dyn Provider>` from `provider_arc()` does NOT satisfy
        // it (Alloy's auto-impl covers `Arc<T: Provider + Sized>`, not
        // `?Sized` trait objects). [`ArcDynProviderEthereum`] bridges the gap.
        let alloy_db = revm::database::AlloyDB::new(
            ArcDynProviderEthereum(provider.provider_arc()),
            BlockId::Number(current_block.into()),
        );
        let Some(wrap_db) = WrapDatabaseAsync::new(alloy_db) else {
            tracing::warn!(
                "BlockSimHandle: no ambient multi-threaded tokio runtime — \
                 WrapDatabaseAsync unavailable; block sim disabled"
            );
            return None;
        };
        let bot_state_db = super::BotStateDb::new_with_code_probe(
            bot_state,
            wrap_db,
            provider.rpc_url(),
            current_block,
        );
        let warm_code_cache =
            super::WarmCodeCache::with_owner(Arc::clone(warm_cache), current_block, bot_state_db);
        let mut cache_db = CacheDB::new(warm_code_cache);
        // Apply the state-override adaptor (owner funding, executor code
        // injection, warmup slots) over the layered DB — the same overrides the
        // retired `eth_simulateV1` `stateOverrides` carried.
        if let Err(err) =
            super::state_override::apply_simulation_overrides(&mut cache_db, override_params)
        {
            // The override adaptor only fails on an override-application
            // error (e.g. a warmup-slot write to an account the DB refused).
            // The override-params are operator config, so a failure here is a
            // wiring error, not a per-path revert — tally `rpc-failed` for
            // every candidate (the whole block's sim is dead).
            tracing::warn!(%err, "BlockSimHandle: state-override application failed");
            return None;
        }
        let mut revm_ctx = revm::context::Context::mainnet();
        // `disable_nonce_check`: the 7 calls share ONE owner; `eth_simulateV1`
        // does NOT bump the caller's nonce per call (each entry is an
        // `eth_call`-shaped read), so revm's per-tx nonce floor would reject
        // calls [1..6]. Disable it — parity with the node's lenient simulate.
        revm_ctx.cfg.disable_nonce_check = true;
        let mut evm = revm_ctx
            .with_db(cache_db)
            .build_mainnet_with_inspector(super::inspectors::SimInspector::default());
        evm.ctx.modify_block(|block| {
            block.basefee = u64::try_from(base_fee_next).unwrap_or(u64::MAX);
            block.number = U256::from(current_block);
            // The block timestamp, threaded from the pump's block header. The
            // default `timestamp = 1` causes V2 pair `_update` to overflow
            // `price0CumulativeLast` in Solidity 0.8+ forks (Camelot/Aerodrome),
            // reverting every swap — the root cause of the in-process-evm
            // parity gap (XPPMQG).
            block.timestamp = U256::from(block_timestamp);
        });
        Some(Self { evm })
    }

    /// Borrow the shared per-block EVM mutably — the strategy's
    /// `simulate_path_on_evm` drives this `&mut evm` per candidate path. The
    /// fan-out is serial, so the borrow is never aliased. Per-path isolation
    /// is revm's `finalize()` (the strategy clears the journal between paths
    /// via the `finalize()` calls inside `simulate_path_on_evm` —
    /// execute()'s SSTOREs stay in the per-path `State`, never committed to
    /// the shared `CacheDB`).
    #[must_use]
    pub fn evm_mut(&mut self) -> &mut BlockEvm<'a> {
        &mut self.evm
    }
}
