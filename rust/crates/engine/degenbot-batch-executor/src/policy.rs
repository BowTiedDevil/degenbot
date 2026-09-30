//! The verdict-to-executor conversion: the ONE typed boundary that turns
//! the resolved values (`degenbot_config::BotConfig`) into the executor's
//! policy values, owning every default and clamp (ADR-065 spirit: the
//! verdict is the single configuration authority — the executor receives
//! policy values, never choreography, and no driver keeps a twin clamp).
//!
//! Placement: this crate owns [`crate::executor::ExecutorConfig`], and
//! `degenbot-config` is a foundation crate (it depends only on the TOML
//! stack), so the engine-crate dependency direction is the existing
//! foundation←engine edge — no cycle, and both drivers (the `PyO3` shell and
//! the settlement example) reach the boundary through the umbrella.

use std::sync::{Arc, Mutex};

use degenbot_arbitrage::{FeeOnTransferRegistry, PoolDivergence};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::{
    Dispatcher, NonceLane, PathSuppression, ReceiptProbe, SubmissionTarget, TxSigner,
};
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use parking_lot::RwLock as ParkingRwLock;

/// The verdict-named executor policy: every value the resolved config
/// declares, converted ONCE by [`From`] with every default and clamp
/// applied here — the conversion is the only clamp owner, so a driver-side
/// or FFI-side floor would be a twin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutorPolicy {
    /// The in-flight sim bound: `simulation.pipeline_concurrency`, floored
    /// at 1 (a cap of zero sims would wedge the pipeline, not serialize it).
    pub sim_concurrency: usize,
    /// The thin-margin floor: `dispatch.min_profit_margin_bps` verbatim
    /// (unsigned end to end — a magnitude, so there is no clamp to own).
    pub min_profit_margin_bps: u64,
    /// The ERC6909 encode axis: `dispatch.erc6909_profit` (the v4-batch
    /// axis has no declared key yet, so it stays a runtime value).
    pub erc6909_profit: bool,
    /// The sim-injection stance: `simulation.inject_executor_code`.
    pub inject_code: bool,
    /// The submit-side guard: the SAME inject stance — the injected contract
    /// does not exist on-chain, so live submission is unsafe while injection
    /// is on. One declared key, two guards, one owner.
    pub inject_code_guard: bool,
}

impl From<&degenbot_config::BotConfig> for ExecutorPolicy {
    fn from(verdict: &degenbot_config::BotConfig) -> Self {
        let inject = verdict.simulation.inject_executor_code;
        Self {
            sim_concurrency: verdict.simulation.pipeline_concurrency.max(1),
            min_profit_margin_bps: verdict.dispatch.min_profit_margin_bps,
            erc6909_profit: verdict.dispatch.erc6909_profit,
            inject_code: inject,
            inject_code_guard: inject,
        }
    }
}

/// The injected runtime handles: everything the resolved verdict CANNOT name
/// (the session arcs, identity addresses, relay posture, the driver's
/// run-mode stance, and the values with no declared key yet). A driver
/// constructs this beside the verdict and hands both to
/// [`crate::executor::ExecutorConfig::from_verdict`]; nothing else assembles
/// the config.
pub struct ExecutorRuntime {
    /// The per-batch sim cap (a plain count; `0` = uncapped, clamped here at
    /// the boundary so the drop happens once, where the dropped rows are
    /// known).
    pub max_candidates: usize,
    /// The v4-batch encode axis (no declared key yet — a runtime value).
    pub use_v4_batch: bool,
    /// The driver's run-mode stance (the live-submission skip; a CLI fact,
    /// not a declared key).
    pub dry_run: bool,
    /// The path resolver (the engine registry projection).
    pub resolver: Arc<dyn crate::assembly::PathResolver>,
    /// The cross-block suppression registry.
    pub suppression: Arc<Mutex<PathSuppression>>,
    /// The per-pool solver-divergence memo.
    pub divergence: Arc<Mutex<PoolDivergence>>,
    /// The per-token fee-on-transfer registry.
    pub fot: Arc<Mutex<FeeOnTransferRegistry>>,
    /// The typed RPC provider (the sim leaf's cold-miss fallback DB).
    pub provider: Arc<AlloyProvider>,
    /// The operator key's address (the `execute()` `from`).
    pub executor_owner: alloy::primitives::Address,
    /// The `cmd_executor` contract address (the `execute()` target + join).
    pub executor_address: alloy::primitives::Address,
    /// WETH9 contract address.
    pub weth_address: alloy::primitives::Address,
    /// The `Uniswap V4 PoolManager` contract address.
    pub pool_manager_address: alloy::primitives::Address,
    /// Multicall3 contract address.
    pub multicall3_address: alloy::primitives::Address,
    /// The injected executor address (used when the inject stance is on).
    pub injected_address: Option<alloy::primitives::Address>,
    /// The executor runtime bytecode.
    pub runtime_bytecode: alloy::primitives::Bytes,
    /// The simulation warmup slots.
    pub warmup: degenbot_executor::WarmupSlots,
    /// The engine's shared state owner (`None` only for empty-input
    /// callers).
    pub bot_state: Option<Arc<StateLock<BotState>>>,
    /// The cross-block warm-code cache.
    pub warm_cache: Option<Arc<ParkingRwLock<degenbot_simulation::WarmCodeCacheInner>>>,
    /// The coordination state (pool mutual exclusion, monitors, block clock).
    pub dispatcher: Arc<Mutex<Dispatcher>>,
    /// The operator key holder (constructed ONCE; the key never leaves Rust).
    pub signer: Arc<TxSigner>,
    /// The receipt probe the spawned monitors poll.
    pub probe: Arc<dyn ReceiptProbe + Send + Sync>,
    /// The host-minted sign-time nonce lane.
    pub nonce_lane: Arc<NonceLane>,
    /// Additional broadcast providers fanned out alongside the read provider.
    pub extra_broadcast: Vec<Arc<AlloyProvider>>,
    /// Where the signed transaction is sent (the relay posture value).
    pub target: SubmissionTarget,
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests drive known-valid fixtures and assert on the outcomes"
)]
mod tests {
    use super::*;

    use degenbot_submission::SubmissionResult;
    use std::future::Future;
    use std::pin::Pin;

    /// A resolver that never resolves (the conversion test never assembles).
    struct EmptyResolver;

    impl crate::assembly::PathResolver for EmptyResolver {
        fn resolve(&self, _path_id: u64) -> Option<degenbot_executor::composers::PathInfo> {
            None
        }
    }

    /// A probe that never reports a receipt (unused under the fixture's
    /// uncapped, dry-run-free construction — the test never submits).
    struct NoopProbe;

    impl ReceiptProbe for NoopProbe {
        fn receipt_found(
            &self,
            _tx_hash: alloy::primitives::B256,
        ) -> Pin<Box<dyn Future<Output = SubmissionResult<bool>> + Send + '_>> {
            Box::pin(async { Ok(false) })
        }
    }

    /// The declared defaults, converted: the executor policy equals the
    /// verdict's own declared defaults (no second default invented here).
    #[test]
    fn the_declared_defaults_convert_verbatim() {
        let verdict = degenbot_config::BotConfig::default();
        let policy = ExecutorPolicy::from(&verdict);
        assert_eq!(
            policy.sim_concurrency, verdict.simulation.pipeline_concurrency,
            "the cap is the verdict's declared default, not a second default"
        );
        assert_eq!(
            policy.min_profit_margin_bps,
            verdict.dispatch.min_profit_margin_bps
        );
        assert!(!policy.erc6909_profit);
        assert!(!policy.inject_code);
        assert!(!policy.inject_code_guard);
    }

    /// The ONE clamp owner: a zero cap converts to the 1 floor HERE, so no
    /// driver (Python or Rust) carries a twin floor.
    #[test]
    fn a_zero_cap_floors_at_one_in_the_conversion() {
        let mut verdict = degenbot_config::BotConfig::default();
        verdict.simulation.pipeline_concurrency = 0;
        assert_eq!(ExecutorPolicy::from(&verdict).sim_concurrency, 1);
    }

    /// Every verdict-named knob lands on the executor value the verdict
    /// names — the resolution-oracle parity property, pinned Rust-side.
    #[test]
    fn every_verdict_knob_lands_on_the_named_executor_value() {
        let mut verdict = degenbot_config::BotConfig::default();
        verdict.simulation.pipeline_concurrency = 5;
        verdict.dispatch.min_profit_margin_bps = 30;
        verdict.dispatch.erc6909_profit = true;
        verdict.simulation.inject_executor_code = true;
        let policy = ExecutorPolicy::from(&verdict);
        assert_eq!(policy.sim_concurrency, 5);
        assert_eq!(policy.min_profit_margin_bps, 30);
        assert!(policy.erc6909_profit);
        assert!(policy.inject_code, "the sim stance follows the verdict");
        assert!(
            policy.inject_code_guard,
            "the submit guard follows the SAME verdict key"
        );
    }

    /// `from_verdict` composes the policy conversion with the runtime: the
    /// uncapped sentinel and the injected stances land where the boundary
    /// owns them.
    #[tokio::test]
    async fn from_verdict_composes_policy_and_runtime() {
        let verdict = degenbot_config::BotConfig::default();
        let provider = Arc::new(
            AlloyProvider::new("http://127.0.0.1:1", 0)
                .await
                .expect("a lazy provider constructs offline"),
        );
        let executor_address = alloy::primitives::Address::ZERO;
        let runtime = ExecutorRuntime {
            max_candidates: 0,
            use_v4_batch: false,
            dry_run: false,
            resolver: Arc::new(EmptyResolver),
            suppression: Arc::new(Mutex::new(PathSuppression::new())),
            divergence: Arc::new(Mutex::new(PoolDivergence::new())),
            fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
            provider,
            executor_owner: alloy::primitives::Address::ZERO,
            executor_address,
            weth_address: alloy::primitives::Address::ZERO,
            pool_manager_address: alloy::primitives::Address::ZERO,
            multicall3_address: alloy::primitives::Address::ZERO,
            injected_address: None,
            runtime_bytecode: alloy::primitives::Bytes::new(),
            warmup: degenbot_executor::compute_simulation_warmup_slots(
                executor_address,
                alloy::primitives::Address::ZERO,
            ),
            bot_state: None,
            warm_cache: None,
            dispatcher: Arc::new(Mutex::new(Dispatcher::for_block(0))),
            signer: Arc::new(
                TxSigner::from_key_hex(
                    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
                    1,
                )
                .expect("the anvil key is valid"),
            ),
            probe: Arc::new(NoopProbe),
            nonce_lane: Arc::new(NonceLane::new(
                Arc::new(degenbot_substrate::nonce::NonceAuthority::new(0)),
                Arc::new(degenbot_submission::SubmissionLedger::new()),
                "settlement",
            )),
            extra_broadcast: Vec::new(),
            target: SubmissionTarget::Public,
        };
        let config = crate::executor::ExecutorConfig::from_verdict(&verdict, runtime);
        assert_eq!(config.sim_concurrency, 8);
        assert_eq!(config.max_candidates, usize::MAX, "0 = uncapped");
        assert_eq!(config.min_profit_margin_bps, 0);
        assert!(!config.dry_run);
    }
}
