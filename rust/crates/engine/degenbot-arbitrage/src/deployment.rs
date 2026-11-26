//! The simulation composition boundary: block-wide facts beside the injected
//! execution deployment (ADR-098 D3).
//!
//! A production simulation crosses two different kinds of fact, and this module
//! keeps them apart:
//!
//! - [`SimulationBlock`] — the facts ONE simulated block contributes: the
//!   provider the cold-miss fallback reads through, the block environment, and
//!   the fee samples. Every candidate in a fan-out shares these.
//! - [`SimulationDeployment`] — the facts ONE execution deployment contributes:
//!   its operator-assigned [`degenbot_execution::DeploymentId`], its
//!   [`degenbot_execution::ExecutionAdapter`] (Encode / Probe / Assess), the
//!   account whose key sends the opportunity's transaction, its EXECUTION
//!   TARGET (the `to` of the transaction the adapter encodes), the gas limit
//!   that transaction may burn, any declared code override, and the settlement
//!   warmup it declares.
//!
//! Nothing here names a `cmd_executor`, a settlement token, a PoolManager, or a
//! command stream: a foreign contract's deployment declares no settlement
//! identity at all ([`SimulationDeployment::settlement`] is `None`) and crosses
//! the exact same simulation path as the built-in one (ADR-098 D6). The
//! built-in binding lives in [`crate::cmd_executor`] — on the built-in side of
//! the seam, never underneath it.
//!
//! # A session holds one or more deployments, in order
//!
//! [`DeploymentRegistry`] is the session's ordered set of deployments: one
//! built-in settlement deployment is a one-entry registry, and a heterogeneous
//! session registers more. The order is the fan-out order and therefore the
//! order every post-target record set merges in
//! ([`crate::dispatch_across_deployments`]).
//!
//! The target is a DEPLOYMENT fact, not a session-global stamp: the post-target
//! submit join reads the address off [`SimulationDeployment::target`], i.e. off
//! the deployment that encoded the transaction, so a foreign contract's `to` is
//! never overwritten by the settlement executor's.
//!
//! # Actual deployed code is the production authority
//!
//! [`SimulationDeployment::code`] is an EXPLICIT development override: declared
//! runtime installed at a declared address in the simulated block. `None` means
//! the sim executes the code the chain holds at the deployment's address. The
//! override never substitutes silently for a different deployed implementation,
//! and the submission side refuses live submission under it
//! (`ExecutorPolicy::inject_code_guard`).
//!
//! # Fee sizing is a deployment choice, not an adapter-type guess
//!
//! [`SimulationDeployment::fee_sizing`] names how the adapter's assessed gross
//! becomes the returned net. [`FeeSizing::AdapterOwned`] (the default) returns
//! the adapter's [`degenbot_execution::ExecutionResult`] unchanged — its priority fee, net,
//! threshold, and verdict are the deployment's answer, so a foreign adapter's
//! Fee decision is never overwritten. Only a deployment that explicitly opts
//! into [`FeeSizing::MarketAware`] is re-priced with the settlement strategy's
//! market-aware sizing; the built-in `cmd_executor` binding is the one caller
//! that does (ADR-098 D7 keeps operator fee policy above the seam, and the
//! built-in path is where that policy applies).

use std::collections::HashSet;
use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_execution::{
    CapitalRequirement, ComposeOptions, ComposerInputs, DeploymentId, ExecutionAdapter,
};
use degenbot_rpc::fees::BlockPriorityFees;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::{
    CapitalLayouts, CapitalOverridePlan, CapitalPath, CapitalPlanError, DeclaredCode, ProbeCapital,
    SettlementWarmup, SimulationOverrideParams,
};

use crate::simulator::SimulatePath;

/// The facts ONE simulated block contributes to every candidate simulated at it.
#[derive(Clone)]
pub struct SimulationBlock<'a> {
    /// The typed RPC provider (the in-process engine's cold-miss fallback DB).
    pub provider: &'a AlloyProvider,
    /// The base fee of the next block (wei/gas) — the fee floor the simulated
    /// transactions run under and the pricing half of the built-in fee sizing.
    pub base_fee_next: u128,
    /// The consumer's execution clock: executor targeting, the batch records,
    /// and the fan-out-trace row's block — never the chain-state read position.
    pub current_block: u64,
    /// The exact solve anchor these facts belong to (ADR-105 D10): the block
    /// identity the solver's state view named when it produced the rows, which
    /// is the block every simulation READ is pinned to.
    ///
    /// Distinct from [`Self::current_block`] on purpose. The consumer's clock
    /// may be one block ahead of the solve, and the two must never be read for
    /// each other: a state read tagged with the clock fetches pre-update state
    /// the solver never saw. `None` only for a work item whose solve resolved
    /// no exact identity (a cold-start/registration sweep, a probe), which then
    /// reads at the promoted block its rows name — an unfenced, cold memo.
    pub based_on: Option<degenbot_core::canonical_head::ChainAnchor>,
    /// The timestamp of `current_block` (the EVM's `block.timestamp`, so a V2
    /// pair's `_update()` computes a correct `timeElapsed`).
    pub block_timestamp: u64,
    /// The latest block priority-fee percentiles (the fee-sizing bounds).
    pub block_priority_fees: Option<BlockPriorityFees>,
}

impl std::fmt::Debug for SimulationBlock<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The provider has no `Debug`; its `rpc_url` is logged separately by the
        // driver.
        f.debug_struct("SimulationBlock")
            .field("rpc_url", &self.provider.rpc_url())
            .field("base_fee_next", &self.base_fee_next)
            .field("current_block", &self.current_block)
            .field("based_on", &self.based_on)
            .field("block_timestamp", &self.block_timestamp)
            .field("block_priority_fees", &self.block_priority_fees)
            .finish()
    }
}

/// One execution deployment: the injected adapter plus the deployment facts the
/// engine's state overrides and the opportunity's transaction need.
pub struct SimulationDeployment {
    deployment_id: DeploymentId,
    adapter: Arc<dyn ExecutionAdapter + Send + Sync>,
    owner: Address,
    target: Address,
    gas_limit: u64,
    code: Option<DeclaredCode>,
    settlement: Option<SettlementWarmup>,
    capital: ProbeCapital,
    capital_layouts: CapitalLayouts,
    fee_sizing: FeeSizing,
}

/// How a deployment turns its adapter's assessed gross figure into the net
/// profit the strategy reports.
///
/// The adapter's Assess carries Fee as its pricing half (ADR-025 D2), so a
/// deployment returns that answer verbatim unless it says otherwise. Nothing
/// here inspects the adapter's type: a foreign adapter and the built-in one are
/// indistinguishable to the seam, and only an explicit deployment choice
/// re-prices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeeSizing {
    /// The adapter owns Fee as well as Encode / Probe / Assess: its
    /// [`degenbot_execution::ExecutionResult`] reaches the caller unchanged — priority fee, net,
    /// threshold verdict and all. The default for every deployment.
    #[default]
    AdapterOwned,
    /// The deployment re-prices the adapter's gross figure with the settlement
    /// strategy's market-aware priority-fee sizing and the dispatch gas floor
    /// (`crate::simulator::price_gross`). Opted into by the built-in
    /// `cmd_executor` binding; a foreign deployment never inherits it.
    MarketAware,
}

impl std::fmt::Debug for SimulationDeployment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The adapter is user code with no `Debug` bound, so the derived fields
        // are named explicitly and the adapter is summarized by name.
        f.debug_struct("SimulationDeployment")
            .field("deployment_id", &self.deployment_id)
            .field("adapter", &"<ExecutionAdapter>")
            .field("owner", &self.owner)
            .field("target", &self.target)
            .field("gas_limit", &self.gas_limit)
            .field("code", &self.code)
            .field("settlement", &self.settlement)
            .field("capital", &self.capital)
            .field("capital_layouts", &self.capital_layouts)
            .field("fee_sizing", &self.fee_sizing)
            .finish()
    }
}

impl SimulationDeployment {
    /// Compose a deployment from its operator-assigned identity, its adapter,
    /// and its declared facts.
    ///
    /// `deployment_id` is required: a deployment has no unnamed form, because
    /// every candidate it executes is keyed by that identity (ADR-098 D4). The
    /// caller that composes the session names it — degenbot never invents one.
    ///
    /// `target` is the deployment's execution target — the `to` of the
    /// transaction its adapter encodes. It is a deployment fact, so a
    /// heterogeneous fan-out stamps each target's own address instead of one
    /// session-global executor.
    ///
    /// `code` is the deployment's declared development override (see the module
    /// doc); `settlement` is what the deployment declares for WETH/PoolManager
    /// warmup — `None` for a contract with neither identity.
    ///
    /// The fee sizing starts at [`FeeSizing::AdapterOwned`]: the adapter's own
    /// `ExecutionResult` is the result until the deployment opts into
    /// market-aware re-pricing with [`Self::with_fee_sizing`].
    #[must_use]
    pub fn new(
        deployment_id: DeploymentId,
        adapter: Arc<dyn ExecutionAdapter + Send + Sync>,
        owner: Address,
        target: Address,
        gas_limit: u64,
        code: Option<DeclaredCode>,
        settlement: Option<SettlementWarmup>,
    ) -> Self {
        Self {
            deployment_id,
            adapter,
            owner,
            target,
            gas_limit,
            code,
            settlement,
            capital: ProbeCapital::Production,
            capital_layouts: CapitalLayouts::verified(),
            fee_sizing: FeeSizing::default(),
        }
    }

    /// Re-bind the deployment's fee sizing (an explicit deployment policy).
    #[must_use]
    pub const fn with_fee_sizing(mut self, fee_sizing: FeeSizing) -> Self {
        self.fee_sizing = fee_sizing;
        self
    }

    /// Declare this deployment's capital posture — the ONE capital value a
    /// session carries, projected per fan-out into the plan the overrides apply.
    ///
    /// The plan's holder is this deployment's execution target — the executor —
    /// never its owner, who only funds gas. The target is never inherited from
    /// the settlement warmup: the warmup's slot subject is a separate fact, and
    /// the plan names the account it funds or clears. `ProbeCapital::Production`
    /// declares no override at all, which is a deployment's default.
    ///
    /// Both probe arms name the executor in the declared-code and deployed-code
    /// cases alike: a declared-code build keeps the runtime its override
    /// installed and a deployed contract keeps the code the chain holds, so the
    /// capital never travels through a code override.
    #[must_use]
    pub const fn with_capital(mut self, capital: ProbeCapital) -> Self {
        self.capital = capital;
        self
    }

    /// Declare the **verified storage layouts** this deployment's probe overrides
    /// may resolve against.
    ///
    /// The default is the pinned production set
    /// ([`CapitalLayouts::verified`]). A fixture whose tokens and PoolManager
    /// live at lab addresses declares theirs here, explicitly — nothing infers a
    /// `balanceOf` slot from an ABI, an address, decimals, or a `balanceOf` call,
    /// and an undeclared layout refuses the probe rather than clearing nothing.
    #[must_use]
    pub fn with_capital_layouts(mut self, layouts: CapitalLayouts) -> Self {
        self.capital_layouts = layouts;
        self
    }

    /// The capital posture this deployment applies to its execution target.
    #[must_use]
    pub const fn capital(&self) -> ProbeCapital {
        self.capital
    }

    /// The path-aware capital plan this deployment applies to a fan-out over
    /// `paths`: every asset those paths touch (zero-balance) or every external
    /// entry their Encode declares (self-funded), each resolved to a concrete
    /// storage target and value.
    ///
    /// The requirements are the deployment's OWN Encode's answer, read from the
    /// Plan it builds
    /// ([`ExecutionAdapter::capital_requirements`]), because which family seeds
    /// an external entry is a grammar fact and a second, path-level rule would
    /// eventually fund one asset and declare another. Only the self-funded
    /// posture asks for them: `ZeroBalance` clears what the paths touch and
    /// discards any requirement, and `Production` composes no plan at all.
    ///
    /// A fan-out's several paths combine per asset by MAXIMUM, never by sum.
    /// Each candidate starts under the same fan-out bankroll but its stream
    /// spends only its own Plan's declared entry, so the shared simulated
    /// database has to cover the largest single entry — a sum would grant capital
    /// no single path declared.
    ///
    /// A fan-out with NOTHING left to cover — the selection walk already
    /// answered every candidate, so no prepared path reaches the EVM — composes
    /// [`ProbeCapital::no_write_plan`] instead: the deployment's own posture and
    /// its execution target preserved, and no capital write at all. An empty
    /// batch is therefore not a planning refusal; each of its candidates answers
    /// with the outcome its prepared encoding already decided, and no EVM
    /// transaction runs for any of them.
    ///
    /// # Errors
    ///
    /// [`CapitalPlanError::UnverifiedLayout`] when a covered asset's storage
    /// layout is not in [`Self::with_capital_layouts`]'s source, decided BEFORE
    /// any EVM work. [`CapitalPlanError::NoPaths`] never escapes this method: an
    /// empty path set is the empty fan-out answered above, and the strict refusal
    /// remains [`ProbeCapital::override_plan`]'s own guard against a caller that
    /// lost the paths it meant to name.
    pub fn capital_plan(
        &self,
        paths: &[SimulatePath],
    ) -> Result<CapitalOverridePlan, CapitalPlanError> {
        // An empty fan-out has no asset set to derive and nothing to fund or
        // clear: the posture and its holder travel, the writes do not.
        if paths.is_empty() {
            return Ok(self.capital.no_write_plan(self.target));
        }
        // Only `SelfFund` INSTALLS external entry capital, so only it asks its
        // adapter what that entry is: `ZeroBalance` clears what the paths touch
        // and discards any requirement, and `Production` composes no plan at
        // all. Asking in those postures would spend a grammar walk per path on
        // an answer nothing reads.
        let requirement_sets: Vec<Vec<CapitalRequirement>> =
            if self.capital == ProbeCapital::SelfFund {
                paths
                    .iter()
                    .map(|path| {
                        let hop_outputs = path.expected_outputs_vec();
                        let consumed_inputs = path.consumed_inputs_vec();
                        let inputs = ComposerInputs {
                            optimal_input: path.optimal_input,
                            hop_outputs: &hop_outputs,
                            consumed_inputs: &consumed_inputs,
                            opts: ComposeOptions,
                        };
                        self.adapter.capital_requirements(&path.path_info, &inputs)
                    })
                    .collect()
            } else {
                Vec::new()
            };
        let facts: Vec<CapitalPath<'_>> = paths
            .iter()
            .enumerate()
            .map(|(index, path)| CapitalPath {
                path: &path.path_info,
                // One plan per fan-out: the requirement sets are indexed by the
                // same path order, and a posture that declares none still keeps
                // every path (its touched assets are the whole plan).
                requirements: requirement_sets
                    .get(index)
                    .map_or(&[], std::vec::Vec::as_slice),
            })
            .collect();
        self.capital
            .override_plan(self.target, &facts, &self.capital_layouts)
    }

    /// This deployment's operator-assigned identity — the half of every
    /// [`degenbot_execution::CandidateId`] it executes.
    #[must_use]
    pub const fn deployment_id(&self) -> DeploymentId {
        self.deployment_id
    }

    /// How this deployment turns the assessed gross figure into net profit.
    #[must_use]
    pub const fn fee_sizing(&self) -> FeeSizing {
        self.fee_sizing
    }

    /// The injected Encode / Probe / Assess adapter.
    #[must_use]
    pub fn adapter(&self) -> &dyn ExecutionAdapter {
        self.adapter.as_ref()
    }

    /// The account whose key sends the opportunity's transaction (the `from`,
    /// and the account the engine funds in the simulated block).
    #[must_use]
    pub const fn owner(&self) -> Address {
        self.owner
    }

    /// The deployment's execution target: the `to` of the transaction its
    /// adapter encodes.
    ///
    /// The post-target join stamps THIS address onto the submit row. Reading it
    /// off the deployment (not off a session-global executor stamp) is what
    /// makes a heterogeneous fan-out route each candidate to its own contract.
    #[must_use]
    pub const fn target(&self) -> Address {
        self.target
    }

    /// The top-level gas limit the opportunity's transaction may burn.
    #[must_use]
    pub const fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    /// The declared development code override, if any.
    #[must_use]
    pub const fn code(&self) -> Option<&DeclaredCode> {
        self.code.as_ref()
    }

    /// The settlement warmup this deployment declares, if any.
    ///
    /// The built-in binding keys the slots it carries on this deployment's
    /// [`Self::target`] (the executor) in both deployment arms; nothing in the
    /// type enforces that subject. Whatever account they name, they stay a
    /// settlement-warmup fact rather than a capital target: the capital plan
    /// ([`Self::with_capital`]) names its own target and never inherits this
    /// one.
    ///
    /// `None` is a complete answer: a foreign contract with no WETH or
    /// PoolManager identity declares none, and the fetch/prewarm lanes derive
    /// only the words its paths name.
    #[must_use]
    pub const fn settlement(&self) -> Option<SettlementWarmup> {
        self.settlement
    }

    /// Project the engine's state-override parameters for a fan-out over `paths`.
    ///
    /// The capital half is composed HERE — once per fan-out — so one path-aware
    /// plan travels from this composition through every override the engine
    /// applies.
    ///
    /// The path set is a CONSERVATIVE union, not an exact projection of what the
    /// EVM will run: a candidate whose encoding the selection walk already
    /// declined is filtered cheaply (it never reaches the EVM), but an
    /// unselected or not-yet-encoded candidate is INCLUDED, because the plan is
    /// composed before every per-candidate encoding decision is final. One asset
    /// among them whose layout this build cannot verify therefore refuses the
    /// whole fan-out — before any EVM work — rather than simulating a path whose
    /// balance the probe cannot account for.
    ///
    /// # Errors
    ///
    /// [`CapitalPlanError`] when the plan cannot be composed; see
    /// [`Self::capital_plan`]. The refusal is returned before the engine builds
    /// any EVM state, so an unverified asset stops the fan-out rather than
    /// simulating it. An EMPTY path set is not such a case: it composes the
    /// posture's no-write plan, so `override_params(&[])` answers with this
    /// deployment's owner, code, settlement, and posture intact and no capital
    /// write.
    pub fn override_params(
        &self,
        paths: &[SimulatePath],
    ) -> Result<SimulationOverrideParams, CapitalPlanError> {
        Ok(SimulationOverrideParams {
            owner: self.owner,
            code: self.code.clone(),
            settlement: self.settlement,
            capital: self.capital_plan(paths)?,
        })
    }
}

/// The simulation context the canonical production entry drives: one simulated
/// block plus the deployment executing its opportunities.
#[derive(Debug)]
pub struct SimulateContext<'a> {
    /// The block-wide facts.
    pub block: SimulationBlock<'a>,
    /// The injected execution deployment.
    pub deployment: &'a SimulationDeployment,
}

/// One session's ordered execution deployments: one or more, each named by a
/// distinct [`DeploymentId`].
///
/// Explicit all-target fan-out iterates this registry in order (ADR-098 D4):
/// every solved path becomes one candidate per entry, so the order is both the
/// fan-out order and the merge order of every post-target record set
/// ([`crate::dispatch_across_deployments`]).
///
/// Two facts are composition errors, refused where the session's configuration
/// is assembled rather than discovered per block:
///
/// - **empty** — a session with no deployment has no target to encode against
///   and no join to stamp;
/// - **duplicate identity** — two entries claiming one [`DeploymentId`] make
///   every post-target row ambiguous, so the registry cannot be truthful about
///   which deployment produced it.
///
/// The signer/owner check ([`Self::validate_signing_authority`]) is the third
/// composition error, and it is CONDITIONAL on the session's submission stance:
/// a deployment whose owner is not the account a SUBMITTING session's signer
/// holds would be simulated from — and submitted as — an address the session
/// cannot sign for, so construction refuses it. A NON-SUBMITTING executor (a
/// dry run) never leases a nonce, signs, or broadcasts, so it validates no
/// signing authority and may simulate a deployment owner it does not hold; that
/// owner stays the transaction's simulated `from`. A probe construction
/// validates its own binding's owner unconditionally.
#[derive(Debug)]
pub struct DeploymentRegistry {
    entries: Vec<Arc<SimulationDeployment>>,
}

/// Why a session's deployment registry was refused at composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentRegistryError {
    /// No entry: a session with nothing to execute has no target to encode
    /// against and no join to stamp.
    Empty,
    /// Two entries claim one operator-assigned identity.
    DuplicateId {
        /// The identity that appears twice.
        deployment_id: DeploymentId,
    },
    /// A SUBMITTING session's deployment declares an owner its signer does not
    /// hold.
    OwnerNotSigner {
        /// The deployment whose owner disagrees with the signer.
        deployment_id: DeploymentId,
        /// The owner the deployment declares (the transaction's `from`).
        owner: Address,
        /// The account the session's signer holds.
        authority: Address,
    },
}

impl std::fmt::Display for DeploymentRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str(
                "the execution deployment registry is empty: a session executes at one or more \
                 deployments",
            ),
            Self::DuplicateId { deployment_id } => write!(
                f,
                "deployment {deployment_id} is registered twice: a post-target row keys on \
                 (path, deployment), so two entries with one identity cannot be told apart"
            ),
            Self::OwnerNotSigner {
                deployment_id,
                owner,
                authority,
            } => write!(
                f,
                "deployment {deployment_id} declares owner {owner}, but the session signer holds \
                 {authority}: the session cannot sign for an account it does not hold"
            ),
        }
    }
}

impl std::error::Error for DeploymentRegistryError {}

impl DeploymentRegistry {
    /// Register the session's deployments, in fan-out order.
    ///
    /// # Errors
    ///
    /// [`DeploymentRegistryError::Empty`] when `entries` is empty;
    /// [`DeploymentRegistryError::DuplicateId`] when two entries name one
    /// identity.
    pub fn new(entries: Vec<Arc<SimulationDeployment>>) -> Result<Self, DeploymentRegistryError> {
        if entries.is_empty() {
            return Err(DeploymentRegistryError::Empty);
        }
        let mut seen: HashSet<DeploymentId> = HashSet::with_capacity(entries.len());
        for entry in &entries {
            if !seen.insert(entry.deployment_id()) {
                return Err(DeploymentRegistryError::DuplicateId {
                    deployment_id: entry.deployment_id(),
                });
            }
        }
        Ok(Self { entries })
    }

    /// Every registered deployment, in fan-out order.
    #[must_use]
    pub fn entries(&self) -> &[Arc<SimulationDeployment>] {
        &self.entries
    }

    /// How many deployments the session executes at (never zero by
    /// construction, so there is no `is_empty` predicate to mirror it).
    #[must_use]
    #[expect(
        clippy::len_without_is_empty,
        reason = "a registry refuses an empty entry set at construction, so `is_empty` would be a constant"
    )]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Every registered identity, in fan-out order.
    pub fn ids(&self) -> impl Iterator<Item = DeploymentId> + '_ {
        self.entries.iter().map(|entry| entry.deployment_id())
    }

    /// The deployment registered under `deployment_id`, if any.
    #[must_use]
    pub fn get(&self, deployment_id: DeploymentId) -> Option<&Arc<SimulationDeployment>> {
        self.entries
            .iter()
            .find(|entry| entry.deployment_id() == deployment_id)
    }

    /// Refuse a registry that declares an owner the session's signer cannot
    /// sign for.
    ///
    /// The transaction's `from` is the deployment's owner and the bytes are
    /// signed with the session signer's key, so on a lane that submits the two
    /// must be the same account. A heterogeneous session where two deployments
    /// share the operator account satisfies this; one that names a foreign
    /// account does not, and is refused at composition instead of simulated and
    /// submitted incorrectly.
    ///
    /// The check belongs to the constructions that may SIGN: a non-submitting
    /// executor never leases a nonce, signs, or broadcasts, so it offers no
    /// authority and does not call this — a dry run therefore simulates the
    /// deployed artifact's owner while holding only a throwaway key, and that
    /// owner stays the simulated `from`. A probe construction always calls it
    /// with its binding's owner.
    ///
    /// # Errors
    ///
    /// [`DeploymentRegistryError::OwnerNotSigner`] naming the first deployment
    /// whose owner disagrees, in registry order.
    pub fn validate_signing_authority(
        &self,
        authority: Address,
    ) -> Result<(), DeploymentRegistryError> {
        for entry in &self.entries {
            if entry.owner() != authority {
                return Err(DeploymentRegistryError::OwnerNotSigner {
                    deployment_id: entry.deployment_id(),
                    owner: entry.owner(),
                    authority,
                });
            }
        }
        Ok(())
    }
}
