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
    Dispatcher, PathSuppression, ReceiptProbe, SubmissionLedger, SubmissionTarget, TxSigner,
};
use degenbot_substrate::nonce::NonceLane;
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
            inject_code: inject,
            inject_code_guard: inject,
        }
    }
}

/// The executor's submission policy: the ONE value answering whether the submit
/// lane may reach its nonce lease, signer, and broadcast arm.
///
/// It is converted ONCE, from the driver's run-mode stance
/// ([`ExecutorRuntime::dry_run`]), and no later reader re-derives it: the submit
/// leaf reads this value and nothing else decides whether the lane may lease a
/// nonce, sign, or broadcast. An execution probe never consults it, because a
/// probe has no submit lane to hold at all
/// ([`crate::probe::ProbeExecutor`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SubmissionPolicy {
    /// The production stance: the submit arm runs its whole choreography
    /// (mutual exclusion, nonce lease, sign, broadcast, monitor).
    #[default]
    Submitting,
    /// The non-submitting stance: every candidate stops at the dry-run guard,
    /// before the nonce lease and the signer, so nothing is leased, signed, or
    /// broadcast. A dry-run session's executor carries this policy.
    NonSubmitting,
}

impl SubmissionPolicy {
    /// Reads the driver's run-mode stance into the policy.
    #[must_use]
    pub const fn from_dry_run(dry_run: bool) -> Self {
        if dry_run {
            Self::NonSubmitting
        } else {
            Self::Submitting
        }
    }

    /// Whether the submit lane may lease a nonce, sign, and broadcast.
    #[must_use]
    pub const fn permits_submission(self) -> bool {
        matches!(self, Self::Submitting)
    }

    /// The `dispatch_and_submit` dry-run flag this policy maps to.
    #[must_use]
    pub const fn dry_run(self) -> bool {
        !self.permits_submission()
    }
}

/// The session's capital posture for its execution target, re-exported from the
/// simulation crate that also owns the override plan it projects to.
///
/// One value drives every projection of a capital posture — the stream's
/// `funding` axis, the capital-validation policy the built Plan's ledger is
/// evaluated under (`initial_capital`), and the deployment's simulation override
/// plan — so a driver never names a `self_fund` / `zero_balance` boolean pair.
/// See [`degenbot_simulation::ProbeCapital`]: `Production` installs and strips
/// nothing, `SelfFund` installs the known asset set at the executor with the
/// self-funded encoding and ordinary validation, and `ZeroBalance` strips that
/// set with the in-path-flash encoding and the zero-capital policy (so an
/// unfundable stream reports the ledger's typed deficit). The two probe modes
/// exclude each other by construction.
pub use degenbot_simulation::ProbeCapital;

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
    /// Capital posture for an explicit dry-run execution probe.
    pub probe_capital: ProbeCapital,
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
    /// The built-in settlement deployment binding: the executor / WETH /
    /// `PoolManager` identities, the owner, and the encode policy. The warmup
    /// math, the runtime-artifact selection, and the execution target are the
    /// binding's.
    pub binding: degenbot_arbitrage::CmdExecutorBinding,
    /// The session's ADDITIONAL execution deployments, registered after the
    /// built-in one and in this order.
    ///
    /// Empty is the one-entry session every existing driver composes: the
    /// built-in settlement deployment alone, with unchanged behavior. A
    /// heterogeneous session names its other deployments here — each one a
    /// frozen [`degenbot_arbitrage::SimulationDeployment`] carrying its own
    /// adapter, owner, execution target, and code stance — and every solved
    /// path is then bound to all of them (explicit all-target fan-out).
    ///
    /// The composition boundary validates the whole registry: identities must
    /// be distinct (never the built-in id) and — when the executor submits —
    /// every owner must be the account the configured signer holds. A
    /// non-submitting (dry-run) executor validates no signing authority, so it
    /// may simulate an owner it does not hold.
    pub additional_deployments: Vec<Arc<degenbot_arbitrage::SimulationDeployment>>,
    /// How a solved path chooses among those deployments
    /// ([`degenbot_arbitrage::TargetSelection`]).
    ///
    /// A driver's choice, not a declared verdict key: `FirstCompatible` (the
    /// value's default) offers each path the registry in order and executes at
    /// the first deployment that accepts composition; `AllTargets` offers every
    /// path to every deployment. A one-entry registry — the built-in session —
    /// selects the same deployment under either, so nothing changes for a
    /// driver that names no additional deployment.
    pub target_selection: degenbot_arbitrage::TargetSelection,
    /// The declared-code override address (used when the inject stance is on).
    ///
    /// Must be the executor address the built-in adapter encodes its
    /// transaction to: the override installs the runtime at that address, so a
    /// mismatch is a construction error, not a second target.
    pub injected_address: Option<alloy::primitives::Address>,
    /// The executor runtime bytecode (installed by the declared override).
    pub runtime_bytecode: alloy::primitives::Bytes,
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
    pub nonce_lane: Arc<NonceLane<SubmissionLedger>>,
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
        fn resolve(&self, _path_id: u64) -> Option<degenbot_execution::PathInfo> {
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
        verdict.simulation.inject_executor_code = true;
        let policy = ExecutorPolicy::from(&verdict);
        assert_eq!(policy.sim_concurrency, 5);
        assert_eq!(policy.min_profit_margin_bps, 30);
        assert!(policy.inject_code, "the sim stance follows the verdict");
        assert!(
            policy.inject_code_guard,
            "the submit guard follows the SAME verdict key"
        );
    }

    /// The fixture operator key: one account is both the session signer and the
    /// deployments' owner, which is what the composition boundary requires.
    const ANVIL_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    /// A runtime over the built-in settlement deployment (plus `additional`),
    /// signed by the fixture account and pointed at the built-in executor.
    async fn runtime(
        additional: Vec<Arc<degenbot_arbitrage::SimulationDeployment>>,
    ) -> ExecutorRuntime {
        let signer = TxSigner::from_key_hex(ANVIL_KEY, 1).expect("the anvil key is valid");
        let owner = signer.address();
        ExecutorRuntime {
            max_candidates: 0,
            use_v4_batch: false,
            probe_capital: ProbeCapital::Production,
            dry_run: false,
            resolver: Arc::new(EmptyResolver),
            suppression: Arc::new(Mutex::new(PathSuppression::new())),
            divergence: Arc::new(Mutex::new(PoolDivergence::new())),
            fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
            provider: Arc::new(
                AlloyProvider::new("http://127.0.0.1:1", 0)
                    .await
                    .expect("a lazy provider constructs offline"),
            ),
            binding: degenbot_arbitrage::CmdExecutorBinding::new(
                owner,
                BUILTIN_EXECUTOR,
                alloy::primitives::Address::ZERO,
                alloy::primitives::Address::ZERO,
                degenbot_executor::composers::EncodeOptions::default(),
            ),
            additional_deployments: additional,
            target_selection: degenbot_arbitrage::TargetSelection::default(),
            injected_address: None,
            runtime_bytecode: alloy::primitives::Bytes::new(),
            bot_state: None,
            warm_cache: None,
            dispatcher: Arc::new(Mutex::new(Dispatcher::for_block(0))),
            signer: Arc::new(signer),
            probe: Arc::new(NoopProbe),
            nonce_lane: Arc::new(
                Arc::new(degenbot_substrate::nonce::NonceAuthority::new(0)).mint_lane(
                    "settlement",
                    Arc::new(degenbot_submission::SubmissionLedger::new()),
                    degenbot_substrate::nonce::LanePolicy::default(),
                ),
            ),
            extra_broadcast: Vec::new(),
            target: SubmissionTarget::Public,
        }
    }

    /// The built-in fixture executor address (the deployment's target).
    const BUILTIN_EXECUTOR: alloy::primitives::Address =
        alloy::primitives::Address::repeat_byte(0xEE);

    /// `from_verdict` composes the policy conversion with the runtime: the
    /// uncapped sentinel and the injected stances land where the boundary
    /// owns them.
    #[tokio::test]
    async fn from_verdict_composes_policy_and_runtime() {
        let verdict = degenbot_config::BotConfig::default();
        let config =
            crate::executor::ExecutorConfig::from_verdict(&verdict, runtime(Vec::new()).await)
                .expect("the built-in session composes a one-entry registry");
        assert_eq!(config.sim_concurrency, 8);
        assert_eq!(config.sim.max_candidates, usize::MAX, "0 = uncapped");
        assert_eq!(config.sim.min_profit_margin_bps, 0);
        assert_eq!(
            config.submission,
            SubmissionPolicy::Submitting,
            "the live run mode converts to the submitting policy"
        );
        assert_eq!(
            config.sim.deployments.len(),
            1,
            "the built-in session is the one-entry registry"
        );
        assert_eq!(
            config.sim.deployments.ids().collect::<Vec<_>>(),
            vec![crate::executor::ExecutorConfig::BUILTIN_DEPLOYMENT_ID]
        );
        assert_eq!(
            config
                .sim
                .deployments
                .get(crate::executor::ExecutorConfig::BUILTIN_DEPLOYMENT_ID)
                .map(|deployment| deployment.target()),
            Some(BUILTIN_EXECUTOR),
            "the built-in deployment executes at the executor it encodes to"
        );
    }

    /// The run mode DECIDES the submission policy, and that one value is what
    /// the submit leaf and the probe entry read: a dry-run executor cannot lease
    /// a nonce, sign, or broadcast.
    #[tokio::test]
    async fn the_run_mode_decides_the_submission_policy() {
        let verdict = degenbot_config::BotConfig::default();

        let mut dry_runtime = runtime(Vec::new()).await;
        dry_runtime.dry_run = true;
        let dry = crate::executor::ExecutorConfig::from_verdict(&verdict, dry_runtime)
            .expect("the built-in session composes a one-entry registry");
        assert_eq!(dry.submission, SubmissionPolicy::NonSubmitting);
        assert!(!dry.submission.permits_submission());
        assert!(
            dry.submission.dry_run(),
            "the submit lane's dry-run flag reads the policy, never a second stance"
        );

        let live =
            crate::executor::ExecutorConfig::from_verdict(&verdict, runtime(Vec::new()).await)
                .expect("the built-in session composes a one-entry registry");
        assert_eq!(live.submission, SubmissionPolicy::Submitting);
        assert!(live.submission.permits_submission());
        assert!(!live.submission.dry_run());
    }

    /// The target-selection policy rides the runtime verbatim into the config:
    /// the default is the ordered first-compatible walk, and a driver's explicit
    /// fan-out reaches the fan-out unchanged.
    #[tokio::test]
    async fn the_target_selection_policy_reaches_the_config_verbatim() {
        let verdict = degenbot_config::BotConfig::default();
        let mut runtime = runtime(Vec::new()).await;
        assert_eq!(
            runtime.target_selection,
            degenbot_arbitrage::TargetSelection::FirstCompatible,
            "the fixture takes the default"
        );
        runtime.target_selection = degenbot_arbitrage::TargetSelection::AllTargets;
        let config = crate::executor::ExecutorConfig::from_verdict(&verdict, runtime)
            .expect("the built-in session composes a one-entry registry");
        assert_eq!(
            config.sim.target_selection,
            degenbot_arbitrage::TargetSelection::AllTargets
        );
    }

    /// A heterogeneous session appends its own deployments AFTER the built-in
    /// one, and that order is the fan-out order.
    #[tokio::test]
    async fn additional_deployments_follow_the_builtin_in_order() {
        let verdict = degenbot_config::BotConfig::default();
        let runtime = runtime(Vec::new()).await;
        let owner = runtime.signer.address();
        let binding = degenbot_arbitrage::CmdExecutorBinding::new(
            owner,
            BUILTIN_EXECUTOR,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            degenbot_executor::composers::EncodeOptions::default(),
        );
        let first = degenbot_execution::DeploymentId::new(7);
        let second = degenbot_execution::DeploymentId::new(9);
        let mut runtime = runtime;
        runtime.additional_deployments = vec![
            Arc::new(binding.deployment(first)),
            Arc::new(binding.deployment(second)),
        ];
        let config = crate::executor::ExecutorConfig::from_verdict(&verdict, runtime)
            .expect("distinct identities compose a three-entry registry");
        assert_eq!(
            config.sim.deployments.ids().collect::<Vec<_>>(),
            vec![
                crate::executor::ExecutorConfig::BUILTIN_DEPLOYMENT_ID,
                first,
                second
            ],
            "the built-in deployment leads, then the runtime's order"
        );
    }

    /// Two registered deployments MAY share one signing account (the supported
    /// heterogeneous case): the authority check is about the account, not about
    /// distinctness.
    #[tokio::test]
    async fn same_author_deployments_compose() {
        let verdict = degenbot_config::BotConfig::default();
        let runtime = runtime(Vec::new()).await;
        let owner = runtime.signer.address();
        let binding = degenbot_arbitrage::CmdExecutorBinding::new(
            owner,
            BUILTIN_EXECUTOR,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            degenbot_executor::composers::EncodeOptions::default(),
        );
        let mut runtime = runtime;
        runtime.additional_deployments = vec![Arc::new(
            binding.deployment(degenbot_execution::DeploymentId::new(2)),
        )];
        let config = crate::executor::ExecutorConfig::from_verdict(&verdict, runtime)
            .expect("both deployments name the session's signing account");
        assert_eq!(config.sim.deployments.len(), 2);
        assert!(config
            .sim
            .deployments
            .validate_signing_authority(owner)
            .is_ok());
    }

    /// A deployment reusing the built-in identity is refused at composition:
    /// `from_verdict` returns the typed registry error rather than registering
    /// two rows that one `CandidateId` cannot tell apart.
    #[tokio::test]
    async fn a_duplicate_identity_is_refused_at_composition() {
        let runtime = runtime(Vec::new()).await;
        let owner = runtime.signer.address();
        let binding = degenbot_arbitrage::CmdExecutorBinding::new(
            owner,
            BUILTIN_EXECUTOR,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            degenbot_executor::composers::EncodeOptions::default(),
        );
        let mut runtime = runtime;
        runtime.additional_deployments = vec![Arc::new(
            binding.deployment(crate::executor::ExecutorConfig::BUILTIN_DEPLOYMENT_ID),
        )];
        let error = crate::executor::ExecutorConfig::from_verdict(
            &degenbot_config::BotConfig::default(),
            runtime,
        )
        .err()
        .expect("two deployments cannot share one identity");
        assert_eq!(
            error,
            degenbot_arbitrage::DeploymentRegistryError::DuplicateId {
                deployment_id: crate::executor::ExecutorConfig::BUILTIN_DEPLOYMENT_ID,
            }
        );
    }

    /// A deployment whose owner is not the signing account is refused at
    /// composition: the session cannot sign a transaction from an account it
    /// does not hold.
    #[tokio::test]
    async fn a_foreign_owner_is_refused_at_composition() {
        let stranger = alloy::primitives::Address::repeat_byte(0x11);
        let binding = degenbot_arbitrage::CmdExecutorBinding::new(
            stranger,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            degenbot_executor::composers::EncodeOptions::default(),
        );
        let mut runtime = runtime(Vec::new()).await;
        runtime.additional_deployments = vec![Arc::new(
            binding.deployment(degenbot_execution::DeploymentId::new(2)),
        )];
        let authority = runtime.signer.address();
        let error = crate::executor::ExecutorConfig::from_verdict(
            &degenbot_config::BotConfig::default(),
            runtime,
        )
        .err()
        .expect("a foreign owner cannot be signed for");
        assert_eq!(
            error,
            degenbot_arbitrage::DeploymentRegistryError::OwnerNotSigner {
                deployment_id: degenbot_execution::DeploymentId::new(2),
                owner: stranger,
                authority,
            }
        );
    }

    /// A NON-SUBMITTING (dry-run) executor simulates an owner it does not hold.
    ///
    /// A session holds only the throwaway dry-run key, while the deployed
    /// artifact bakes the real owner as the transaction's `from`. Nothing is
    /// leased, signed, or broadcast on this lane, so there is no signing
    /// authority to validate — and the simulated `from` stays the real
    /// deployment owner rather than the throwaway signer.
    #[tokio::test]
    async fn a_non_submitting_executor_simulates_a_foreign_owner() {
        let stranger = alloy::primitives::Address::repeat_byte(0x11);
        let mut runtime = runtime(Vec::new()).await;
        let signer = runtime.signer.address();
        assert_ne!(
            stranger, signer,
            "the fixture owner is foreign to the signer"
        );
        runtime.dry_run = true;
        runtime.binding = degenbot_arbitrage::CmdExecutorBinding::new(
            stranger,
            BUILTIN_EXECUTOR,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            degenbot_executor::composers::EncodeOptions::default(),
        );
        let config = crate::executor::ExecutorConfig::from_verdict(
            &degenbot_config::BotConfig::default(),
            runtime,
        )
        .expect("a non-submitting executor has no signing authority to validate");
        assert_eq!(config.submission, SubmissionPolicy::NonSubmitting);
        let deployment = config
            .sim
            .deployments
            .get(crate::executor::ExecutorConfig::BUILTIN_DEPLOYMENT_ID)
            .expect("the built-in deployment is the registry's first entry");
        let overrides = deployment
            .override_params(&[])
            .expect("an empty path set composes the posture's no-write plan");
        assert_eq!(
            overrides.owner, stranger,
            "the simulated `from` is the deployed owner, never the throwaway signer"
        );
    }

    /// The SAME mismatch on a SUBMITTING executor is still refused: this lane
    /// signs with the session key and sends from the deployment's owner, so the
    /// two must be one account.
    #[tokio::test]
    async fn a_submitting_executor_refuses_a_foreign_owner() {
        let stranger = alloy::primitives::Address::repeat_byte(0x11);
        let mut runtime = runtime(Vec::new()).await;
        let authority = runtime.signer.address();
        runtime.dry_run = false;
        runtime.binding = degenbot_arbitrage::CmdExecutorBinding::new(
            stranger,
            BUILTIN_EXECUTOR,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            degenbot_executor::composers::EncodeOptions::default(),
        );
        let error = crate::executor::ExecutorConfig::from_verdict(
            &degenbot_config::BotConfig::default(),
            runtime,
        )
        .err()
        .expect("a submitting executor cannot sign for an account it does not hold");
        assert_eq!(
            error,
            degenbot_arbitrage::DeploymentRegistryError::OwnerNotSigner {
                deployment_id: crate::executor::ExecutorConfig::BUILTIN_DEPLOYMENT_ID,
                owner: stranger,
                authority,
            }
        );
    }

    /// The composition boundary refuses an empty registry loudly — a session
    /// with nothing to execute has no target to encode against.
    #[test]
    fn an_empty_registry_is_refused() {
        let error = degenbot_arbitrage::DeploymentRegistry::new(Vec::new())
            .expect_err("an empty registry has no lane to run");
        assert_eq!(error, degenbot_arbitrage::DeploymentRegistryError::Empty);
    }

    /// The composition boundary refuses a duplicate identity loudly: two
    /// entries with one `DeploymentId` make every post-target row ambiguous.
    #[test]
    fn a_duplicate_identity_is_refused() {
        let binding = degenbot_arbitrage::CmdExecutorBinding::new(
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            degenbot_executor::composers::EncodeOptions::default(),
        );
        let id = crate::executor::ExecutorConfig::BUILTIN_DEPLOYMENT_ID;
        let error = degenbot_arbitrage::DeploymentRegistry::new(vec![
            Arc::new(binding.deployment(id)),
            Arc::new(binding.deployment(id)),
        ])
        .expect_err("two deployments cannot share one identity");
        assert_eq!(
            error,
            degenbot_arbitrage::DeploymentRegistryError::DuplicateId { deployment_id: id }
        );
    }

    /// The composition boundary refuses an owner the session signer does not
    /// hold: the transaction would be simulated from — and submitted as — an
    /// account the session cannot sign for.
    #[test]
    fn an_owner_the_signer_cannot_sign_for_is_refused() {
        let signer = TxSigner::from_key_hex(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            1,
        )
        .expect("the anvil key is valid");
        let owner = signer.address();
        let stranger = alloy::primitives::Address::repeat_byte(0x11);
        let binding = degenbot_arbitrage::CmdExecutorBinding::new(
            stranger,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            alloy::primitives::Address::ZERO,
            degenbot_executor::composers::EncodeOptions::default(),
        );
        let registry = degenbot_arbitrage::DeploymentRegistry::new(vec![Arc::new(
            binding.deployment(crate::executor::ExecutorConfig::BUILTIN_DEPLOYMENT_ID),
        )])
        .expect("one entry is a valid registry");
        let error = registry
            .validate_signing_authority(owner)
            .expect_err("a foreign owner cannot be signed for");
        assert_eq!(
            error,
            degenbot_arbitrage::DeploymentRegistryError::OwnerNotSigner {
                deployment_id: crate::executor::ExecutorConfig::BUILTIN_DEPLOYMENT_ID,
                owner: stranger,
                authority: owner,
            }
        );
    }
}
