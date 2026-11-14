//! The capital posture of one simulated execution, and the override plan it
//! projects to.
//!
//! Three representations, and exactly one of each per session:
//!
//! - [`ProbeCapital`] — the MODE. It names what the session (or an explicit
//!   execution probe) assumes about the execution target's own capital:
//!   `Production` (the account's real balances — no override at all),
//!   `SelfFund` (the path's declared entry capital installed at the executor,
//!   with the self-funded stream encoding), or `ZeroBalance` (every asset the
//!   path touches stripped from the executor, with the in-path-flash encoding).
//!   It is the ONE capital value an operator, a driver, or the FFI names.
//! - [`CapitalOverridePlan`] — the PLAN. The same mode with the account it
//!   applies to BOUND and with every asset it covers RESOLVED to a concrete
//!   storage target and value. It is produced ONCE where a deployment composes
//!   for a fan-out (`degenbot_arbitrage::SimulationDeployment`), from the paths
//!   that fan-out can still hand to the EVM — a conservative union, never an
//!   exact projection (see [`ProbeCapital::override_plan`]) — and consumed by
//!   [`crate::SimulationOverrideParams`]. Binding the address at composition is
//!   what keeps a probe's capital on the executor it names in both deployment
//!   arms (declared code and deployed code), independent of whichever account a
//!   settlement warmup keyed.
//! - [`CapitalLayouts`] — the VERIFIED LAYOUT SOURCE the plan resolves against:
//!   an explicit, pinned list of token contracts whose `balanceOf` base slot is
//!   known, and PoolManager contracts whose ERC-6909 claim base slot is known.
//!
//! # One value, one projection
//!
//! [`ProbeCapital::override_plan`] is the whole mode→plan conversion, and it is
//! the only one: a deployment stores the mode, the override application reads
//! the plan, and nothing re-derives a "self-fund" or "zero-balance" boolean from
//! a neighbouring axis. The two encode-side projections a mode implies — the
//! stream's `funding` axis and the capital-validation policy
//! ([`ProbeCapital::initial_capital`]) — are derived from the SAME value at the
//! same composition point, so a session can never fund one posture, validate
//! another, and override a third.
//!
//! The modes are mutually exclusive BY CONSTRUCTION: they are variants, not a
//! pair of booleans, so "self-fund AND zero-balance" has no representation to
//! refuse.
//!
//! # Which assets, and how much
//!
//! The asset set is derived from the PATHS, never from a fixed token list:
//!
//! - `ZeroBalance` covers every asset the paths touch — the holder's native
//!   balance, every ERC-20 a hop names, and every PoolManager ERC-6909 claim a
//!   V4 hop's currencies name — each installed as `0`. A V4 path's native
//!   currency contributes the native balance and the claim whose id is
//!   `uint160(address(0))`; it never contributes an ERC-20 at a guessed slot,
//!   because the native currency has no ERC-20 balance to clear.
//! - `SelfFund` covers ONLY the external initial capital the deployment's own
//!   Encode declares for those paths — the built Plan's `SelfFund` seeds, read
//!   across the seam by
//!   [`degenbot_execution::ExecutionAdapter::capital_requirements`]. A path's
//!   entry amount is a per-family grammar fact, so the Plan is the authority;
//!   a touched-but-unrequired token is never granted a blanket balance, and a
//!   self-funding family (one that funds itself in-path) declares nothing and
//!   receives nothing.
//!
//! # Failing closed
//!
//! A plan can only be composed for an asset whose storage layout this build can
//! verify. [`CapitalLayouts`] is that verification: a token whose `balances`
//! mapping base is not on the list — and a PoolManager whose ERC-6909 mapping
//! base is not — refuses the whole plan ([`CapitalPlanError::UnverifiedLayout`])
//! BEFORE any EVM work, naming the asset. Nothing infers a slot from a token's
//! ABI, address, decimals, or a `balanceOf` call, and nothing scans storage: an
//! unverified token is never silently cleared (which would leave its balance in
//! place and call the probe saturated) and never silently funded.
//!
//! # Production
//!
//! `Production` names no holder and covers no asset, so the override application
//! writes nothing at all: a production simulation runs on the chain's own state.
//!
//! # An empty fan-out
//!
//! A batch the selection walk already answered in full — every candidate a
//! prepared decline — leaves no path to cover, and so no asset set to derive. Its
//! plan is [`ProbeCapital::no_write_plan`]: the declared mode and the account it
//! applies to preserved, and no write at all.
//! [`ProbeCapital::override_plan`] keeps answering [`CapitalPlanError::NoPaths`]
//! for an empty path list, so only a caller that OBSERVED an empty fan-out
//! composes that plan; a caller that merely forgot its paths still refuses.

use alloy::primitives::{address, Address, U256};

use degenbot_execution::{CapitalRequirement, HopInfo, PathInfo};
use degenbot_executor::composers::NATIVE_CURRENCY_ADDRESS;
use degenbot_executor::grammar_ledger::InitialCapital;
use degenbot_executor::{
    erc6909_id, mapping_slot, nested_mapping_slot, POOL_MANAGER_ERC6909_BALANCE_OF_SLOT,
    WETH9_BALANCE_OF_SLOT,
};

/// Canonical mainnet WETH9 — its `balanceOf` mapping base is
/// [`WETH9_BALANCE_OF_SLOT`].
pub const WETH9_MAINNET: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
/// Circle FiatToken V2's `balances` mapping base on Ethereum mainnet,
/// independently verified against the USDC/WETH 0.05% pool's live balance.
pub const USDC_MAINNET_BALANCE_SLOT: u64 = 9;
/// The mainnet USDC token named by the executor's deployment configuration.
pub const USDC_MAINNET: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
/// Wrapped BTC's `balances` mapping base on Ethereum mainnet, independently
/// verified against the WBTC/WETH 0.3% pool's live balance.
pub const WBTC_MAINNET_BALANCE_SLOT: u64 = 0;
/// The mainnet WBTC token.
pub const WBTC_MAINNET: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
/// Canonical mainnet Uniswap V4 `PoolManager` — its ERC-6909 `balanceOf` mapping
/// base is [`POOL_MANAGER_ERC6909_BALANCE_OF_SLOT`].
pub const POOL_MANAGER_MAINNET: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");

/// A verified ERC-20 balance layout: `token.balanceOf[holder]` lives at
/// `mapping_slot(balance_slot, holder)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Erc20Layout {
    /// The token contract whose storage the override writes.
    pub token: Address,
    /// The base slot of its `balances` mapping.
    pub balance_slot: u64,
}

/// A verified PoolManager ERC-6909 claim layout:
/// `manager.balanceOf[holder][id]` lives at
/// `nested_mapping_slot(balance_slot, holder, id)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClaimLayout {
    /// The PoolManager contract whose storage the override writes.
    pub manager: Address,
    /// The base slot of its ERC-6909 `balanceOf` mapping.
    pub balance_slot: u64,
}

/// The **verified storage layouts** a capital plan may resolve against.
///
/// This is the whole point of the type: a probe override is a blind write to a
/// hashed storage slot, so the slot has to be KNOWN, not guessed. The default
/// ([`CapitalLayouts::verified`]) is the pinned production set — canonical
/// mainnet WETH9 (`balanceOf`@3), USDC (`balances`@9), WBTC (`balances`@0), and
/// the canonical mainnet PoolManager (ERC-6909 `balanceOf`@4). A fixture whose
/// tokens live at lab addresses declares its own layouts explicitly with
/// [`Self::with_token`] / [`Self::with_manager`]; nothing infers a layout from an
/// ABI, an address shape, decimals, or a `balanceOf` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapitalLayouts {
    tokens: Vec<Erc20Layout>,
    managers: Vec<ClaimLayout>,
}

/// The default source is the pinned production set ([`CapitalLayouts::verified`]):
/// a deployment that says nothing about layouts gets exactly the layouts this
/// build verified, never an empty source that would refuse every probe.
impl Default for CapitalLayouts {
    fn default() -> Self {
        Self::verified()
    }
}

impl CapitalLayouts {
    /// The pinned production set: the canonical mainnet tokens and PoolManager
    /// whose layouts this build has independently verified.
    #[must_use]
    pub fn verified() -> Self {
        Self {
            tokens: vec![
                Erc20Layout {
                    token: WETH9_MAINNET,
                    balance_slot: WETH9_BALANCE_OF_SLOT,
                },
                Erc20Layout {
                    token: USDC_MAINNET,
                    balance_slot: USDC_MAINNET_BALANCE_SLOT,
                },
                Erc20Layout {
                    token: WBTC_MAINNET,
                    balance_slot: WBTC_MAINNET_BALANCE_SLOT,
                },
            ],
            managers: vec![ClaimLayout {
                manager: POOL_MANAGER_MAINNET,
                balance_slot: POOL_MANAGER_ERC6909_BALANCE_OF_SLOT,
            }],
        }
    }

    /// Declare `token`'s verified `balances` base slot (a fixture's explicit
    /// layout, or a newly verified production token).
    ///
    /// **The LAST declaration for a token wins.** Redeclaring the same slot is
    /// idempotent (one entry, same answer); redeclaring a different slot REPLACES
    /// it, which is how a fixture deliberately overrides a slot the pinned set
    /// already names. Neither declaration is dropped silently: the later one is a
    /// later, equally explicit statement about the same contract, and the
    /// resolved target is always the one the caller last declared.
    #[must_use]
    pub fn with_token(mut self, token: Address, balance_slot: u64) -> Self {
        match self.tokens.iter_mut().find(|layout| layout.token == token) {
            Some(existing) => existing.balance_slot = balance_slot,
            None => self.tokens.push(Erc20Layout {
                token,
                balance_slot,
            }),
        }
        self
    }

    /// Declare `manager`'s verified ERC-6909 `balanceOf` base slot.
    ///
    /// **The LAST declaration for a manager wins**, exactly as
    /// [`Self::with_token`] states for a token: an idempotent redeclaration is
    /// harmless, and a differing one replaces the earlier slot rather than being
    /// ignored.
    #[must_use]
    pub fn with_manager(mut self, manager: Address, balance_slot: u64) -> Self {
        match self
            .managers
            .iter_mut()
            .find(|layout| layout.manager == manager)
        {
            Some(existing) => existing.balance_slot = balance_slot,
            None => self.managers.push(ClaimLayout {
                manager,
                balance_slot,
            }),
        }
        self
    }

    /// The verified layout for `token`, if this build has one.
    #[must_use]
    pub fn erc20(&self, token: Address) -> Option<Erc20Layout> {
        self.tokens
            .iter()
            .find(|layout| layout.token == token)
            .copied()
    }

    /// The verified claim layout for `manager`, if this build has one.
    #[must_use]
    pub fn claim(&self, manager: Address) -> Option<ClaimLayout> {
        self.managers
            .iter()
            .find(|layout| layout.manager == manager)
            .copied()
    }
}

/// One asset a path's capital is denominated in — the IDENTITY of a holding.
///
/// The holder is not part of it: one plan applies to exactly one account
/// ([`CapitalOverridePlan::holder`]). A claim carries its PoolManager and its
/// currency (the ERC-6909 id derives from the currency), so two claims on
/// different managers or different currencies are different assets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CapitalAsset {
    /// The holder's own native ETH balance.
    Native,
    /// An ERC-20 `balanceOf(holder)` holding.
    Erc20 {
        /// The token contract.
        token: Address,
    },
    /// A PoolManager ERC-6909 claim held by the holder.
    Erc6909Claim {
        /// The PoolManager holding the claim.
        manager: Address,
        /// The claim's currency (its id is `uint160(currency)`).
        currency: Address,
    },
}

impl std::fmt::Display for CapitalAsset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native => f.write_str("native ETH"),
            Self::Erc20 { token } => write!(f, "ERC-20 token {token}"),
            Self::Erc6909Claim { manager, currency } => write!(
                f,
                "PoolManager {manager} ERC-6909 claim of currency {currency}"
            ),
        }
    }
}

/// Where one asset's word lives in the simulated state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CapitalTarget {
    /// The account's own balance word (set through the account, so the code and
    /// nonce the simulated block reads survive).
    NativeBalance,
    /// A storage word in a contract.
    Storage {
        /// The contract whose storage is written.
        contract: Address,
        /// The resolved slot within it.
        slot: U256,
    },
}

/// One resolved capital override: the asset it covers, where that asset's word
/// lives, and the value installed there.
///
/// `value` is `0` for a clearing and the Plan's declared requirement for a
/// self-funded asset — never a blanket amount, and never a warmed slot's 1 wei.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CapitalOverride {
    asset: CapitalAsset,
    target: CapitalTarget,
    value: U256,
}

impl CapitalOverride {
    /// The asset this override covers.
    #[must_use]
    pub const fn asset(&self) -> CapitalAsset {
        self.asset
    }

    /// Where the asset's word lives.
    #[must_use]
    pub const fn target(&self) -> CapitalTarget {
        self.target
    }

    /// The value installed (`0` = cleared).
    #[must_use]
    pub const fn value(&self) -> U256 {
        self.value
    }
}

/// One path's capital facts: the hops whose assets it touches, and the external
/// initial capital its deployment's Encode declares for it.
///
/// Both halves come from the composition point, where the deployment still has
/// its adapter: the requirements are the built Plan's `SelfFund` seeds
/// ([`degenbot_execution::ExecutionAdapter::capital_requirements`]), never a
/// re-derivation of a grammar rule from the hop list.
#[derive(Clone, Copy, Debug)]
pub struct CapitalPath<'a> {
    /// The path whose assets this simulation touches.
    pub path: &'a PathInfo,
    /// The external initial capital the deployment's Encode declares for it.
    pub requirements: &'a [CapitalRequirement],
}

/// Why a capital override plan could not be composed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapitalPlanError {
    /// A non-production plan was asked for with no path at all: there is no
    /// asset set to fund or clear, and substituting a fixed one would install
    /// capital no path asked for.
    NoPaths,
    /// The plan needs an asset whose storage layout this build cannot verify, so
    /// it cannot be cleared or funded and must not be left alone either.
    UnverifiedLayout {
        /// The asset the plan could not resolve a target for.
        asset: CapitalAsset,
    },
}

impl CapitalPlanError {
    /// The asset this refusal names, when it names one.
    #[must_use]
    pub const fn asset(&self) -> Option<CapitalAsset> {
        match self {
            Self::NoPaths => None,
            Self::UnverifiedLayout { asset } => Some(*asset),
        }
    }
}

impl std::fmt::Display for CapitalPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPaths => f.write_str(
                "a probe capital plan needs at least one path: its asset set is derived from the \
                 paths a fan-out simulates",
            ),
            Self::UnverifiedLayout { asset } => write!(
                f,
                "no verified storage layout for {asset}: this build cannot override its balance, \
                 so the probe refuses rather than leaving the holding in place"
            ),
        }
    }
}

impl std::error::Error for CapitalPlanError {}

/// The capital posture one session — or one explicit execution probe — assumes
/// for its execution target.
///
/// Ordinary solver candidates always use `Production`; the two probe modes are
/// explicit operator/driver choices and exclude each other by construction.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub enum ProbeCapital {
    /// The deployed contract's own capital: production encoding with the
    /// account's real balances, and no capital override at all.
    #[default]
    Production,
    /// The paths' declared entry capital installed at the executor, with the
    /// self-funded stream encoding.
    SelfFund,
    /// Every asset the paths touch stripped from the executor, with the
    /// in-path-flash stream encoding.
    ZeroBalance,
}

impl ProbeCapital {
    /// The accepted spellings, in refusal-message order.
    ///
    /// One vocabulary for the operator wire, the Python construction boundary,
    /// and the `PyO3` boundary's validation, so a mode can never be spelled one
    /// way in a driver and another in the FFI.
    pub const NAMES: &'static [&'static str] = &["production", "self_fund", "zero_balance"];

    /// Parse a mode from its spelling; `None` for anything else.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "production" => Some(Self::Production),
            "self_fund" => Some(Self::SelfFund),
            "zero_balance" => Some(Self::ZeroBalance),
            _ => None,
        }
    }

    /// This mode's spelling ([`Self::NAMES`] entry).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::SelfFund => "self_fund",
            Self::ZeroBalance => "zero_balance",
        }
    }

    /// The **capital-validation policy** this mode declares — the ONE
    /// mode→policy projection, read where the encode policy is composed.
    ///
    /// `Production` and `SelfFund` are both ordinary validation
    /// ([`InitialCapital::Held`]): the stream's `SelfFund` seed names capital the
    /// executor genuinely holds, which is what makes a losing self-funded stream
    /// execute to its economic receipt instead of being refused.
    /// `ZeroBalance` is the foundation's zero-capital policy
    /// ([`InitialCapital::Zero`]): the executor holds nothing, so only the path's
    /// own credit graph can fund a debit and a stream that still needs external
    /// capital reports a typed deficit.
    ///
    /// This is a *validation* stance and never a stream axis: the mode's
    /// `funding` axis is projected separately beside it, and a stream the policy
    /// finds solvent encodes exactly the production bytes.
    #[must_use]
    pub const fn initial_capital(self) -> InitialCapital {
        match self {
            Self::Production | Self::SelfFund => InitialCapital::Held,
            Self::ZeroBalance => InitialCapital::Zero,
        }
    }

    /// Bind the account this mode applies to and the assets the paths it is
    /// handed touch — the ONE mode→plan projection.
    ///
    /// `holder` is the deployment's execution target (the executor): the account
    /// the probe funds or clears, never the owner, who only funds gas.
    ///
    /// `paths` is composed by the fan-out from the candidates it can still hand
    /// to the EVM, which is a CONSERVATIVE union rather than an exact projection:
    /// a prepared decline is filtered out, while an unselected or not-yet-encoded
    /// candidate is still included. A plan may therefore cover a path a later
    /// encoding decision drops — the safe direction, because a plan that misses a
    /// touched asset would leave a balance the probe claims to withhold.
    ///
    /// Both probe modes name the **complete deduplicated asset set** of the
    /// paths they cover — native balance, every ERC-20 a hop names, and every
    /// PoolManager claim a V4 currency names — so no touched holding is left
    /// with whatever balance the chain happened to hold:
    ///
    /// - `ZeroBalance` installs `0` for every one of them;
    /// - `SelfFund` installs the declared requirement where the path declares
    ///   one, and `0` for every other touched holding. A self-funding family
    ///   (one whose stream funds itself in-path) therefore declares no capital
    ///   and still clears what it touches.
    ///
    /// # Errors
    ///
    /// [`CapitalPlanError::NoPaths`] when a probe mode is asked for with no path,
    /// and [`CapitalPlanError::UnverifiedLayout`] when a covered asset has no
    /// layout in `layouts` — the fail-closed answer, raised here so no EVM work
    /// happens first. A touched holding with no verified layout refuses BOTH
    /// modes: leaving it alone would retain a balance the probe claims to model.
    pub fn override_plan(
        self,
        holder: Address,
        paths: &[CapitalPath<'_>],
        layouts: &CapitalLayouts,
    ) -> Result<CapitalOverridePlan, CapitalPlanError> {
        if self == Self::Production {
            return Ok(CapitalOverridePlan::default());
        }
        if paths.is_empty() {
            return Err(CapitalPlanError::NoPaths);
        }
        let holder_word = U256::from_be_bytes(holder.into_word().into());
        let mut assets: Vec<CapitalAsset> = Vec::new();
        for facts in paths {
            touched_assets(facts.path, &mut assets);
        }
        let required = self == Self::SelfFund;
        let mut installed: Vec<(CapitalAsset, u128)> = Vec::new();
        if required {
            // The shared simulated state is one database: the installed value
            // must cover the largest single requirement. A sum would grant
            // capital no single path asked for.
            for facts in paths {
                for requirement in facts.requirements {
                    let asset = asset_of_currency(requirement.currency);
                    match installed.iter_mut().find(|(seen, _)| *seen == asset) {
                        Some((_, amount)) => *amount = (*amount).max(requirement.amount),
                        None => installed.push((asset, requirement.amount)),
                    }
                    // A declared requirement is capital whether or not a hop
                    // names its currency: install it even when nothing touches
                    // it, and never twice.
                    if !assets.contains(&asset) {
                        assets.push(asset);
                    }
                }
            }
        }
        let mut overrides: Vec<CapitalOverride> = Vec::with_capacity(assets.len());
        for asset in assets {
            let target = resolve(asset, holder_word, layouts)?;
            let value = installed
                .iter()
                .find(|(seen, _)| *seen == asset)
                .map_or(U256::ZERO, |(_, amount)| U256::from(*amount));
            overrides.push(CapitalOverride {
                asset,
                target,
                value,
            });
        }
        Ok(CapitalOverridePlan {
            mode: self,
            writes: Some(CapitalWrites { holder, overrides }),
        })
    }

    /// The plan this posture carries when its fan-out has **no path left to
    /// cover**: the mode and the account it applies to are preserved, and the
    /// write set is empty.
    ///
    /// This is the answer for a batch the selection walk already answered in
    /// full — every candidate prepared as a decline, so nothing reaches the EVM
    /// and no asset needs funding or clearing. The posture still travels: a
    /// reader of [`CapitalOverridePlan::mode`] sees the mode the deployment
    /// declared, and the plan still names the executor it applies to, so an empty
    /// fan-out is never silently reported as `Production`.
    ///
    /// It is deliberately NOT [`Self::override_plan`] with an empty path list.
    /// That call keeps answering [`CapitalPlanError::NoPaths`], because a caller
    /// that hands over paths and receives no plan cannot tell "nothing is left to
    /// cover" from "the path list was forgotten". Only the caller that observed
    /// the empty fan-out can tell the two apart, and
    /// `SimulationDeployment::capital_plan` is where it says so.
    ///
    /// `Production` keeps its own empty plan — no account and no write — so the
    /// ordinary posture is unchanged.
    #[must_use]
    pub const fn no_write_plan(self, holder: Address) -> CapitalOverridePlan {
        let writes = match self {
            Self::Production => None,
            Self::SelfFund | Self::ZeroBalance => Some(CapitalWrites {
                holder,
                overrides: Vec::new(),
            }),
        };
        CapitalOverridePlan { mode: self, writes }
    }
}

/// The asset a Plan requirement's currency denotes: the holder's native balance
/// for the V4 native currency, an ERC-20 holding otherwise.
fn asset_of_currency(currency: Address) -> CapitalAsset {
    if currency == NATIVE_CURRENCY_ADDRESS {
        CapitalAsset::Native
    } else {
        CapitalAsset::Erc20 { token: currency }
    }
}

/// Push every asset `path` touches onto `assets`, deduplicated in first-touch
/// order.
///
/// The native balance is always first: every execution spends gas, and a V4
/// path settles native through the PoolManager. An ERC-20 appears once per
/// contract however many hops cross it, and a claim appears once per
/// (PoolManager, currency) pair. A V4 hop's native currency contributes the
/// native balance and its claim — never an ERC-20 at a guessed slot.
fn touched_assets(path: &PathInfo, assets: &mut Vec<CapitalAsset>) {
    fn push(assets: &mut Vec<CapitalAsset>, asset: CapitalAsset) {
        if !assets.contains(&asset) {
            assets.push(asset);
        }
    }

    push(assets, CapitalAsset::Native);
    for hop in &path.hops {
        for (currency, pool_manager) in hop_currencies(hop) {
            if currency == NATIVE_CURRENCY_ADDRESS {
                push(assets, CapitalAsset::Native);
            } else {
                push(assets, CapitalAsset::Erc20 { token: currency });
            }
            if let Some(manager) = pool_manager {
                push(assets, CapitalAsset::Erc6909Claim { manager, currency });
            }
        }
    }
}

/// A hop's two currencies, each with the PoolManager that holds a claim on it
/// (`None` for a V2/V3 pool, which holds no ERC-6909 balance).
fn hop_currencies(hop: &HopInfo) -> [(Address, Option<Address>); 2] {
    fn pool_tokens(token0: Address, token1: Address) -> [(Address, Option<Address>); 2] {
        [(token0, None), (token1, None)]
    }
    match hop {
        HopInfo::V2(hop) => pool_tokens(hop.token0_address, hop.token1_address),
        HopInfo::V3(hop) => pool_tokens(hop.token0_address, hop.token1_address),
        HopInfo::V4(hop) => [
            (hop.currency0_address, Some(hop.pool_manager_address)),
            (hop.currency1_address, Some(hop.pool_manager_address)),
        ],
    }
}

/// Resolve one asset's storage target through the verified layout source.
fn resolve(
    asset: CapitalAsset,
    holder_word: U256,
    layouts: &CapitalLayouts,
) -> Result<CapitalTarget, CapitalPlanError> {
    match asset {
        CapitalAsset::Native => Ok(CapitalTarget::NativeBalance),
        CapitalAsset::Erc20 { token } => layouts
            .erc20(token)
            .map(|layout| CapitalTarget::Storage {
                contract: layout.token,
                slot: mapping_slot(U256::from(layout.balance_slot), holder_word),
            })
            .ok_or(CapitalPlanError::UnverifiedLayout { asset }),
        CapitalAsset::Erc6909Claim { manager, currency } => layouts
            .claim(manager)
            .map(|layout| CapitalTarget::Storage {
                contract: layout.manager,
                slot: nested_mapping_slot(
                    U256::from(layout.balance_slot),
                    holder_word,
                    erc6909_id(currency),
                ),
            })
            .ok_or(CapitalPlanError::UnverifiedLayout { asset }),
    }
}

/// A [`ProbeCapital`] mode with the account and the assets it applies to
/// resolved: the simulation-override plan a fan-out carries.
///
/// A deployment produces it once per fan-out (from its own execution target and
/// the paths that fan-out can still hand to the EVM), and the override
/// application reads it: `Production` declares nothing, the two probe arms name
/// the one account whose capital they install or strip plus every resolved write.
/// The write set and the account are ONE value, so a plan that names writes but
/// no account to apply them to is unrepresentable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapitalOverridePlan {
    mode: ProbeCapital,
    writes: Option<CapitalWrites>,
}

/// A plan's bound account together with the resolved writes it applies.
///
/// The two halves are ONE value because an override names an account: a write
/// with no holder to apply it to has no representation here, so the override
/// application reads one `Option` and never has to decide what to do with a
/// half-populated plan in either build profile.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CapitalWrites {
    holder: Address,
    overrides: Vec<CapitalOverride>,
}

impl CapitalOverridePlan {
    /// The mode this plan was projected from, without its account.
    #[must_use]
    pub const fn mode(&self) -> ProbeCapital {
        self.mode
    }

    /// The account this plan funds or clears; `None` for `Production`.
    #[must_use]
    pub fn holder(&self) -> Option<Address> {
        self.writes.as_ref().map(|writes| writes.holder)
    }

    /// Every resolved override, in asset order: a `ZeroBalance` plan's clearings
    /// or a `SelfFund` plan's installations.
    #[must_use]
    pub fn overrides(&self) -> &[CapitalOverride] {
        self.writes
            .as_ref()
            .map_or(&[], |writes| writes.overrides.as_slice())
    }

    /// The account and the writes it applies, as ONE value: `None` exactly when
    /// the plan writes nothing (always the case for `Production`).
    #[must_use]
    pub(crate) fn writes(&self) -> Option<(Address, &[CapitalOverride])> {
        self.writes
            .as_ref()
            .map(|writes| (writes.holder, writes.overrides.as_slice()))
    }

    /// Every asset this plan names, once each.
    pub fn assets(&self) -> impl Iterator<Item = CapitalAsset> + '_ {
        self.overrides()
            .iter()
            .map(|override_row| override_row.asset)
    }

    /// Whether this plan writes nothing at all (always true for `Production`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.overrides().is_empty()
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)] // fixtures read their own composed plans
mod tests {
    use super::*;

    use alloy::primitives::address;
    use degenbot_execution::{V2HopInfo, V3HopInfo, V4HopInfo};

    const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");
    const LAB_TOKEN: Address = address!("00000000000000000000000000000000000000bb");
    const LAB_MANAGER: Address = address!("00000000000000000000000000000000000000cc");

    fn v2(pool: Address, token0: Address, token1: Address) -> HopInfo {
        HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address: token0,
            token1_address: token1,
            fee: 30,
            zfo: true,
        })
    }

    fn v3(pool: Address, token0: Address, token1: Address) -> HopInfo {
        HopInfo::V3(V3HopInfo {
            pool_address: pool,
            token0_address: token0,
            token1_address: token1,
            fee: 500,
            zfo: true,
        })
    }

    fn v4(pool_id: &str, currency0: Address, currency1: Address) -> HopInfo {
        HopInfo::V4(V4HopInfo {
            pool_manager_address: LAB_MANAGER,
            pool_id_hex: pool_id.to_string(),
            currency0_address: currency0,
            currency1_address: currency1,
            fee: 500,
            tick_spacing: 10,
            hook_address: Address::ZERO,
            zfo: true,
        })
    }

    fn layouts() -> CapitalLayouts {
        CapitalLayouts::verified()
            .with_token(LAB_TOKEN, 7)
            .with_manager(LAB_MANAGER, 4)
    }

    fn requirement(currency: Address, amount: u128) -> Vec<CapitalRequirement> {
        vec![CapitalRequirement { currency, amount }]
    }

    /// The deduplicated asset set of a path that crosses WETH twice and a V4 pool
    /// once: native, WETH, USDC, and the pool's two claims — each named once.
    #[test]
    fn the_zero_balance_plan_covers_every_touched_asset_once() {
        let path = PathInfo::new(vec![
            v2(Address::from([0x11; 20]), WETH9_MAINNET, USDC_MAINNET),
            v4("0x01", USDC_MAINNET, NATIVE_CURRENCY_ADDRESS),
            v3(Address::from([0x12; 20]), USDC_MAINNET, WETH9_MAINNET),
        ]);
        let plan = ProbeCapital::ZeroBalance
            .override_plan(
                EXECUTOR,
                &[CapitalPath {
                    path: &path,
                    requirements: &[],
                }],
                &layouts(),
            )
            .expect("every touched asset has a verified layout");
        assert_eq!(plan.mode(), ProbeCapital::ZeroBalance);
        assert_eq!(plan.holder(), Some(EXECUTOR));
        let assets: Vec<CapitalAsset> = plan.assets().collect();
        assert_eq!(
            assets,
            vec![
                CapitalAsset::Native,
                CapitalAsset::Erc20 {
                    token: WETH9_MAINNET
                },
                CapitalAsset::Erc20 {
                    token: USDC_MAINNET
                },
                CapitalAsset::Erc6909Claim {
                    manager: LAB_MANAGER,
                    currency: USDC_MAINNET
                },
                CapitalAsset::Erc6909Claim {
                    manager: LAB_MANAGER,
                    currency: NATIVE_CURRENCY_ADDRESS
                },
            ],
            "native first, then each token and claim exactly once"
        );
        assert!(
            plan.overrides()
                .iter()
                .all(|entry| entry.value() == U256::ZERO),
            "a zero-balance plan clears, never funds"
        );
        let weth = plan
            .overrides()
            .iter()
            .find(|entry| {
                entry.asset()
                    == CapitalAsset::Erc20 {
                        token: WETH9_MAINNET,
                    }
            })
            .expect("the WETH holding is named");
        assert_eq!(
            weth.target(),
            CapitalTarget::Storage {
                contract: WETH9_MAINNET,
                slot: mapping_slot(
                    U256::from(WETH9_BALANCE_OF_SLOT),
                    U256::from_be_bytes(EXECUTOR.into_word().into()),
                ),
            },
            "the target is the token's verified balanceOf word at the holder"
        );
        let claim = plan
            .overrides()
            .iter()
            .find(|entry| {
                entry.asset()
                    == CapitalAsset::Erc6909Claim {
                        manager: LAB_MANAGER,
                        currency: NATIVE_CURRENCY_ADDRESS,
                    }
            })
            .expect("the native claim is named");
        assert_eq!(
            claim.target(),
            CapitalTarget::Storage {
                contract: LAB_MANAGER,
                slot: nested_mapping_slot(
                    U256::from(POOL_MANAGER_ERC6909_BALANCE_OF_SLOT),
                    U256::from_be_bytes(EXECUTOR.into_word().into()),
                    U256::ZERO,
                ),
            },
            "the claim's slot is the manager's ERC-6909 word at the native id"
        );
    }

    /// A self-funded plan installs the declared requirement and CLEARS every
    /// other touched holding — no blanket USDC grant, and no retained balance
    /// the probe never declared.
    #[test]
    fn the_self_fund_plan_installs_the_requirement_and_clears_the_rest() {
        let path = PathInfo::new(vec![
            v2(Address::from([0x11; 20]), WETH9_MAINNET, USDC_MAINNET),
            v2(Address::from([0x12; 20]), USDC_MAINNET, WETH9_MAINNET),
        ]);
        let requirements = requirement(WETH9_MAINNET, 1_000_000);
        let plan = ProbeCapital::SelfFund
            .override_plan(
                EXECUTOR,
                &[CapitalPath {
                    path: &path,
                    requirements: &requirements,
                }],
                &layouts(),
            )
            .expect("the declared entry has a verified layout");
        assert_eq!(
            plan.assets().collect::<Vec<_>>(),
            vec![
                CapitalAsset::Native,
                CapitalAsset::Erc20 {
                    token: WETH9_MAINNET
                },
                CapitalAsset::Erc20 {
                    token: USDC_MAINNET
                },
            ],
            "the whole touched set is named once, the unrequired USDC included"
        );
        let weth = &plan.overrides()[1];
        assert_eq!(
            weth.asset(),
            CapitalAsset::Erc20 {
                token: WETH9_MAINNET
            }
        );
        assert_eq!(weth.value(), U256::from(1_000_000u64));
        assert_eq!(
            weth.target(),
            CapitalTarget::Storage {
                contract: WETH9_MAINNET,
                slot: mapping_slot(
                    U256::from(WETH9_BALANCE_OF_SLOT),
                    U256::from_be_bytes(EXECUTOR.into_word().into()),
                ),
            }
        );
        for entry in [&plan.overrides()[0], &plan.overrides()[2]] {
            assert_eq!(
                entry.value(),
                U256::ZERO,
                "{} is touched but never required, so it is cleared",
                entry.asset()
            );
        }
    }

    /// A **non-WETH V4 claim** is cleared by a self-funded plan too: it is
    /// touched, unrequired, and must not keep a balance.
    #[test]
    fn a_self_fund_plan_clears_a_non_weth_claim() {
        let path = PathInfo::new(vec![v4("0x01", USDC_MAINNET, WETH9_MAINNET)]);
        let requirements = requirement(WETH9_MAINNET, 1_000_000);
        let plan = ProbeCapital::SelfFund
            .override_plan(
                EXECUTOR,
                &[CapitalPath {
                    path: &path,
                    requirements: &requirements,
                }],
                &layouts(),
            )
            .expect("both currencies and the manager are verified");
        let claim = |currency| CapitalAsset::Erc6909Claim {
            manager: LAB_MANAGER,
            currency,
        };
        assert_eq!(
            plan.overrides()
                .iter()
                .map(|entry| (entry.asset(), entry.value()))
                .collect::<Vec<_>>(),
            vec![
                (CapitalAsset::Native, U256::ZERO),
                (
                    CapitalAsset::Erc20 {
                        token: USDC_MAINNET
                    },
                    U256::ZERO
                ),
                (claim(USDC_MAINNET), U256::ZERO),
                (
                    CapitalAsset::Erc20 {
                        token: WETH9_MAINNET
                    },
                    U256::from(1_000_000u64)
                ),
                (claim(WETH9_MAINNET), U256::ZERO),
            ],
            "the requirement is installed; every other touched holding, both claims included, is cleared"
        );
    }

    /// Two paths sharing the shared simulated database install the LARGEST
    /// single requirement, never their sum.
    #[test]
    fn the_self_fund_plan_installs_the_largest_requirement() {
        let path_a = PathInfo::new(vec![v2(
            Address::from([0x11; 20]),
            WETH9_MAINNET,
            USDC_MAINNET,
        )]);
        let path_b = PathInfo::new(vec![v2(
            Address::from([0x12; 20]),
            WETH9_MAINNET,
            USDC_MAINNET,
        )]);
        let small = requirement(WETH9_MAINNET, 1_000);
        let large = requirement(WETH9_MAINNET, 9_000);
        let plan = ProbeCapital::SelfFund
            .override_plan(
                EXECUTOR,
                &[
                    CapitalPath {
                        path: &path_a,
                        requirements: &small,
                    },
                    CapitalPath {
                        path: &path_b,
                        requirements: &large,
                    },
                ],
                &layouts(),
            )
            .expect("the entry has a verified layout");
        // The WETH entry takes the largest single requirement; the other
        // touched holding is cleared, not granted.
        assert_eq!(
            plan.overrides()
                .iter()
                .map(|entry| (entry.asset(), entry.value()))
                .collect::<Vec<_>>(),
            vec![
                (CapitalAsset::Native, U256::ZERO),
                (
                    CapitalAsset::Erc20 {
                        token: WETH9_MAINNET
                    },
                    U256::from(9_000u64)
                ),
                (
                    CapitalAsset::Erc20 {
                        token: USDC_MAINNET
                    },
                    U256::ZERO
                ),
            ]
        );
    }

    /// A self-funding family declares nothing, so a self-funded probe grants
    /// nothing — and still clears what the path touches, so no holding keeps a
    /// balance the probe never declared.
    #[test]
    fn a_self_funded_path_that_declares_nothing_grants_nothing_and_still_clears() {
        let path = PathInfo::new(vec![v2(
            Address::from([0x11; 20]),
            WETH9_MAINNET,
            USDC_MAINNET,
        )]);
        let plan = ProbeCapital::SelfFund
            .override_plan(
                EXECUTOR,
                &[CapitalPath {
                    path: &path,
                    requirements: &[],
                }],
                &layouts(),
            )
            .expect("an empty requirement set needs no layout");
        assert_eq!(plan.holder(), Some(EXECUTOR));
        assert_eq!(
            plan.overrides()
                .iter()
                .map(|entry| (entry.asset(), entry.value()))
                .collect::<Vec<_>>(),
            vec![
                (CapitalAsset::Native, U256::ZERO),
                (
                    CapitalAsset::Erc20 {
                        token: WETH9_MAINNET
                    },
                    U256::ZERO
                ),
                (
                    CapitalAsset::Erc20 {
                        token: USDC_MAINNET
                    },
                    U256::ZERO
                ),
            ],
            "nothing is granted, and every touched holding is cleared"
        );
    }

    /// An unverified token refuses the whole plan and names the asset — before
    /// any EVM work, and never as a zero-balance claim over a token it cannot
    /// clear.
    #[test]
    fn an_unverified_layout_refuses_and_names_the_asset() {
        let unknown = address!("00000000000000000000000000000000000000dd");
        let path = PathInfo::new(vec![v2(Address::from([0x11; 20]), WETH9_MAINNET, unknown)]);
        let refusal = ProbeCapital::ZeroBalance
            .override_plan(
                EXECUTOR,
                &[CapitalPath {
                    path: &path,
                    requirements: &[],
                }],
                &layouts(),
            )
            .expect_err("an unknown token layout is refused");
        assert_eq!(
            refusal,
            CapitalPlanError::UnverifiedLayout {
                asset: CapitalAsset::Erc20 { token: unknown }
            }
        );
        assert_eq!(
            refusal.asset(),
            Some(CapitalAsset::Erc20 { token: unknown })
        );
        assert!(
            refusal.to_string().contains("no verified storage layout"),
            "the refusal names the gap: {refusal}"
        );
        assert!(
            refusal
                .to_string()
                .contains("0x00000000000000000000000000000000000000dd"),
            "the refusal names the asset: {refusal}"
        );
        // The same token as a SELF-FUND requirement refuses identically.
        let requirement = requirement(unknown, 5);
        assert_eq!(
            ProbeCapital::SelfFund
                .override_plan(
                    EXECUTOR,
                    &[CapitalPath {
                        path: &path,
                        requirements: &requirement,
                    }],
                    &CapitalLayouts::verified(),
                )
                .expect_err("an unknown requirement layout is refused"),
            CapitalPlanError::UnverifiedLayout {
                asset: CapitalAsset::Erc20 { token: unknown }
            }
        );
    }

    /// A touched token with an unverified layout refuses a self-funded plan too,
    /// even though nothing requires capital in it: leaving the holding alone is
    /// exactly the silent retained balance the refusals exist to prevent.
    #[test]
    fn an_unverified_touched_token_refuses_a_self_funded_plan() {
        let unknown = address!("00000000000000000000000000000000000000ee");
        let path = PathInfo::new(vec![
            v2(Address::from([0x11; 20]), WETH9_MAINNET, unknown),
            v2(Address::from([0x12; 20]), unknown, WETH9_MAINNET),
        ]);
        let requirements = requirement(WETH9_MAINNET, 1_000);
        assert_eq!(
            ProbeCapital::SelfFund
                .override_plan(
                    EXECUTOR,
                    &[CapitalPath {
                        path: &path,
                        requirements: &requirements,
                    }],
                    &layouts(),
                )
                .expect_err("the unrequired token's layout is unverified"),
            CapitalPlanError::UnverifiedLayout {
                asset: CapitalAsset::Erc20 { token: unknown }
            }
        );
    }

    /// A probe mode with no path has no asset set to derive; production needs no
    /// path at all because it names no asset.
    #[test]
    fn a_probe_mode_needs_a_path_and_production_needs_none() {
        for mode in [ProbeCapital::SelfFund, ProbeCapital::ZeroBalance] {
            assert_eq!(
                mode.override_plan(EXECUTOR, &[], &layouts())
                    .expect_err("no path is no asset set"),
                CapitalPlanError::NoPaths
            );
        }
        assert_eq!(
            ProbeCapital::Production
                .override_plan(EXECUTOR, &[], &layouts())
                .expect("production declares nothing"),
            CapitalOverridePlan::default()
        );
        assert_eq!(CapitalOverridePlan::default().holder(), None);
        assert!(CapitalOverridePlan::default().is_empty());
    }

    /// A fan-out the selection walk answered in full leaves no path to cover: the
    /// posture's plan keeps its mode and its holder and writes nothing, so an
    /// empty batch is never mistaken for `Production` and never funds or clears an
    /// asset no path asked for.
    #[test]
    fn an_empty_probe_plan_keeps_its_posture_and_writes_nothing() {
        for mode in [ProbeCapital::SelfFund, ProbeCapital::ZeroBalance] {
            let plan = mode.no_write_plan(EXECUTOR);
            assert_eq!(plan.mode(), mode, "the declared posture survives");
            assert_eq!(
                plan.holder(),
                Some(EXECUTOR),
                "the plan still names the executor it applies to"
            );
            assert_eq!(
                plan.writes(),
                Some((EXECUTOR, &[][..])),
                "a bound account with no write at all"
            );
            assert!(plan.overrides().is_empty(), "nothing is funded or cleared");
            assert!(plan.is_empty());
        }
        let production = ProbeCapital::Production.no_write_plan(EXECUTOR);
        assert_eq!(production, CapitalOverridePlan::default());
        assert_eq!(
            production.holder(),
            None,
            "production names no account to apply anything to"
        );
    }

    /// A V4 hop's native currency is the native balance plus a claim, never an
    /// ERC-20 at a guessed slot.
    #[test]
    fn a_v4_native_currency_is_never_an_erc20() {
        let path = PathInfo::new(vec![v4("0x01", NATIVE_CURRENCY_ADDRESS, WETH9_MAINNET)]);
        let plan = ProbeCapital::ZeroBalance
            .override_plan(
                EXECUTOR,
                &[CapitalPath {
                    path: &path,
                    requirements: &[],
                }],
                &layouts(),
            )
            .expect("native and WETH are both resolved");
        let assets: Vec<CapitalAsset> = plan.assets().collect();
        assert_eq!(
            assets,
            vec![
                CapitalAsset::Native,
                CapitalAsset::Erc6909Claim {
                    manager: LAB_MANAGER,
                    currency: NATIVE_CURRENCY_ADDRESS
                },
                CapitalAsset::Erc20 {
                    token: WETH9_MAINNET
                },
                CapitalAsset::Erc6909Claim {
                    manager: LAB_MANAGER,
                    currency: WETH9_MAINNET
                },
            ]
        );
        assert!(
            !assets.contains(&CapitalAsset::Erc20 {
                token: NATIVE_CURRENCY_ADDRESS
            }),
            "the native currency has no ERC-20 balance"
        );
    }

    /// Every mode round-trips through its spelling, and the zero mode is the only
    /// zero-capital policy.
    #[test]
    fn the_modes_round_trip_and_only_zero_balance_withholds_capital() {
        assert_eq!(ProbeCapital::NAMES.len(), 3);
        for mode in [
            ProbeCapital::Production,
            ProbeCapital::SelfFund,
            ProbeCapital::ZeroBalance,
        ] {
            assert!(ProbeCapital::NAMES.contains(&mode.name()));
            assert_eq!(ProbeCapital::from_name(mode.name()), Some(mode));
        }
        assert_eq!(ProbeCapital::from_name("self-funded"), None);
        assert_eq!(ProbeCapital::from_name("self_fund "), None);
        assert_eq!(ProbeCapital::from_name(""), None);
        assert_eq!(
            ProbeCapital::Production.initial_capital(),
            InitialCapital::Held
        );
        assert_eq!(
            ProbeCapital::SelfFund.initial_capital(),
            InitialCapital::Held,
            "a self-funded stream declares held entry capital"
        );
        assert_eq!(
            ProbeCapital::ZeroBalance.initial_capital(),
            InitialCapital::Zero
        );
    }

    /// The verified source is explicit and narrow: the three pinned mainnet
    /// tokens plus the canonical PoolManager, and nothing else.
    #[test]
    fn the_verified_layouts_are_the_pinned_set() {
        let verified = CapitalLayouts::verified();
        assert_eq!(
            verified
                .erc20(WETH9_MAINNET)
                .map(|layout| layout.balance_slot),
            Some(WETH9_BALANCE_OF_SLOT)
        );
        assert_eq!(
            verified
                .erc20(USDC_MAINNET)
                .map(|layout| layout.balance_slot),
            Some(USDC_MAINNET_BALANCE_SLOT)
        );
        assert_eq!(
            verified
                .erc20(WBTC_MAINNET)
                .map(|layout| layout.balance_slot),
            Some(WBTC_MAINNET_BALANCE_SLOT)
        );
        assert_eq!(
            verified
                .claim(POOL_MANAGER_MAINNET)
                .map(|layout| layout.balance_slot),
            Some(POOL_MANAGER_ERC6909_BALANCE_OF_SLOT)
        );
        assert_eq!(verified.erc20(LAB_TOKEN), None, "a lab token is not pinned");
        assert_eq!(verified.claim(LAB_MANAGER), None);
        assert_eq!(
            CapitalLayouts::default(),
            CapitalLayouts::verified(),
            "the production source is the default"
        );
        let extended = verified.clone().with_token(LAB_TOKEN, 7);
        assert_eq!(
            extended.erc20(LAB_TOKEN).map(|layout| layout.balance_slot),
            Some(7),
            "a fixture declares its own layout explicitly"
        );
        assert_eq!(
            verified.erc20(LAB_TOKEN),
            None,
            "declaring one does not mutate the source it came from"
        );
    }

    /// A SECOND declaration for one token REPLACES the first: the last one wins,
    /// so a fixture can deliberately re-pin a contract the pinned set already
    /// names, and the resolved target is the newly declared slot. Redeclaring
    /// the same slot stays harmless — one entry, the same answer.
    #[test]
    fn a_conflicting_token_declaration_wins_and_the_plan_uses_the_new_slot() {
        let path = PathInfo::new(vec![v2(
            Address::from([0x11; 20]),
            WETH9_MAINNET,
            USDC_MAINNET,
        )]);
        let layouts = CapitalLayouts::verified()
            // The pinned USDC slot is 9; the fixture re-pins the same contract
            // twice, so the LAST declaration (99) is the one that resolves.
            .with_token(USDC_MAINNET, 42)
            .with_token(USDC_MAINNET, 99);
        assert_eq!(
            layouts
                .erc20(USDC_MAINNET)
                .map(|layout| layout.balance_slot),
            Some(99),
            "the later declaration wins, and the earlier one is not kept"
        );

        let holder_word = U256::from_be_bytes(EXECUTOR.into_word().into());
        let plan = ProbeCapital::ZeroBalance
            .override_plan(
                EXECUTOR,
                &[CapitalPath {
                    path: &path,
                    requirements: &[],
                }],
                &layouts,
            )
            .expect("the re-pinned token still has a verified layout");
        let usdc = plan
            .overrides()
            .iter()
            .find(|row| {
                row.asset()
                    == CapitalAsset::Erc20 {
                        token: USDC_MAINNET,
                    }
            })
            .expect("the USDC holding is named exactly once");
        assert_eq!(
            usdc.target(),
            CapitalTarget::Storage {
                contract: USDC_MAINNET,
                slot: mapping_slot(U256::from(99u64), holder_word),
            },
            "the plan writes the slot the caller declared LAST"
        );
        assert_eq!(
            plan.assets()
                .filter(|asset| *asset
                    == CapitalAsset::Erc20 {
                        token: USDC_MAINNET
                    })
                .count(),
            1,
            "a re-declaration never names the asset twice"
        );

        // Idempotent redeclaration: the same slot twice is one entry and the
        // same answer, so a fixture may declare its layout unconditionally.
        let idempotent = CapitalLayouts::verified()
            .with_token(LAB_TOKEN, 7)
            .with_token(LAB_TOKEN, 7);
        assert_eq!(
            idempotent
                .erc20(LAB_TOKEN)
                .map(|layout| layout.balance_slot),
            Some(7)
        );
    }

    /// The management side of the same contract: a second declaration for one
    /// PoolManager replaces the first, and the resolved claim target is the
    /// newly declared slot.
    #[test]
    fn a_conflicting_manager_declaration_wins_and_the_plan_uses_the_new_slot() {
        let path = PathInfo::new(vec![HopInfo::V4(V4HopInfo {
            pool_manager_address: POOL_MANAGER_MAINNET,
            pool_id_hex: "0x01".to_string(),
            currency0_address: USDC_MAINNET,
            currency1_address: WETH9_MAINNET,
            fee: 500,
            tick_spacing: 10,
            hook_address: Address::ZERO,
            zfo: true,
        })]);
        let layouts = CapitalLayouts::verified()
            // The pinned mainnet PoolManager base is 4; the fixture re-pins it.
            .with_manager(POOL_MANAGER_MAINNET, 8)
            .with_manager(POOL_MANAGER_MAINNET, 11);
        assert_eq!(
            layouts
                .claim(POOL_MANAGER_MAINNET)
                .map(|layout| layout.balance_slot),
            Some(11),
            "the later declaration wins, and the earlier one is not kept"
        );

        let holder_word = U256::from_be_bytes(EXECUTOR.into_word().into());
        let plan = ProbeCapital::ZeroBalance
            .override_plan(
                EXECUTOR,
                &[CapitalPath {
                    path: &path,
                    requirements: &[],
                }],
                &layouts,
            )
            .expect("the re-pinned manager still has a verified layout");
        let claim = plan
            .overrides()
            .iter()
            .find(|row| {
                row.asset()
                    == CapitalAsset::Erc6909Claim {
                        manager: POOL_MANAGER_MAINNET,
                        currency: WETH9_MAINNET,
                    }
            })
            .expect("the WETH claim on the pinned manager is named exactly once");
        assert_eq!(
            claim.target(),
            CapitalTarget::Storage {
                contract: POOL_MANAGER_MAINNET,
                slot: nested_mapping_slot(
                    U256::from(11u64),
                    holder_word,
                    erc6909_id(WETH9_MAINNET),
                ),
            },
            "the claim word is derived from the slot the caller declared LAST"
        );

        // Idempotent redeclaration stays harmless here too.
        let idempotent = CapitalLayouts::verified()
            .with_manager(LAB_MANAGER, 4)
            .with_manager(LAB_MANAGER, 4);
        assert_eq!(
            idempotent
                .claim(LAB_MANAGER)
                .map(|layout| layout.balance_slot),
            Some(4)
        );
    }
}
