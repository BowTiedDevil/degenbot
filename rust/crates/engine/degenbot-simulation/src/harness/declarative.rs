//! Declarative multi-hop fixture runner.
//!
//! The pre-`declarative` fixtures each hand-assembled a `PathInfo`, hand-
//! derived the per-hop amount chain, hand-wired the funding buffers and pool
//! approvals, and asserted only `executed(n)` — so every new grammar family
//! cost ~50 lines of near-identical, easy-to-get-wrong plumbing and proved
//! the encoder didn't *revert*, not that its amounts were *right*.
//!
//! This module collapses that plumbing into one line. Feed
//! [`Harness::run_chain`] an ordered list of [`Hop`]s (a protocol tag + src/
//! dst token + the pool) and `optimal_input`; it:
//!
//! - forward-traverses the hops via the same amount math the harness already
//!   proves (V2 `IntHopState`, V3/V4 `v3_amount_out`) to derive every
//!   intermediate amount and the terminal output,
//! - builds the production `PathInfo` (correct `zfo` from each pool's token0),
//! - applies the caller's capital posture ([`FixtureCapital`]) through the ONE
//!   [`CapitalOverridePlan`] the live probes compose, plus the operational
//!   fixture setup a swap needs (`approve_pair` on every V2 pool),
//! - executes, and reports [`ChainResult`]: the classified `outcome`, the
//!   `predicted_profit` (`out_terminal − optimal_input`), the *measured*
//!   executor WETH-balance delta, and the exact initial/final balance of every
//!   native, ERC-20, and PoolManager-claim asset the path touches.
//!
//! The measured delta is the assertion that matters: it proves the encoded
//! amounts actually moved the expected WETH, not merely that the payload
//! didn't revert. A family is then one table row (a few pool lines + one
//! `run_chain` call + one [`assert_profitable`] line) instead of a hand-
//! written test. `assert_profitable` is deliberately tolerant of ±1-wei
//! per-hop rounding (`getAmountIn` round-up, terminal V2 calc recompute) but
//! will catch a silently-wrong amount by orders of magnitude.
//!
//! # Capital is the posture's, never the runner's
//!
//! A run installs exactly what its [`FixtureCapital`] posture names, so the
//! three postures the live probe offers are the three a fixture can assert:
//!
//! - [`FixtureCapital::production`] — the default of every `run_chain*` entry —
//!   applies no override at all: whatever holding the fixture explicitly seeded
//!   is the one the executor executes against, untouched and uncleared.
//! - [`FixtureCapital::self_fund`] installs the stream's OWN declared entry
//!   capital and clears every other touched holding. The requirements are read
//!   from the built Plan of the very `EncodeContext`/`EncodeRequest` the
//!   fixture encodes (`encode_cmd_stream_capital`), so a fixture can never fund
//!   one asset while the stream declares another.
//! - [`FixtureCapital::zero_balance`] clears every touched holding, leaving only
//!   the path's own credit graph to fund a debit.
//!
//! No entry mints a universal WETH or per-intermediate-token buffer: a fixture
//! that wants a holding seeds it explicitly ([`Harness::fund`]) or asks for the
//! posture that declares it. Pool liquidity, WETH backing, and the V2 pair
//! approvals are fixture SETUP (how the fake pools can pay and pull), never
//! executor capital, and they stay in `Harness::{add_pool, add_v3_pool,
//! add_v4_pool, executor_approve_pair}`.
//!
//! Migrating a call site is therefore a statement about capital, not plumbing:
//! a row that used to lean on the runner's funding either seeds the holding it
//! means to test ([`Harness::run_chain`], the production default) or asks for
//! the self-funded posture ([`Harness::run_chain_capital`] with
//! [`FixtureCapital::self_fund`]), whose entry comes from the stream itself.
//!
//! # A raw payload declares its own requirements
//!
//! [`Harness::run_raw_payload`] drives bytes the composer did not produce, so
//! there is no built Plan to read a requirement from. Its `SelfFund` arm
//! therefore REQUIRES the caller's explicit [`CapitalRequirement`]s
//! ([`FixtureCapital::self_fund_declared`]) and refuses to guess an amount from
//! the bytes or the hop list; the production and zero-balance arms need no
//! guess because neither installs anything.
#![expect(clippy::cast_possible_wrap)]

use alloy::primitives::{Address, U256};

use degenbot_execution::{CapitalRequirement, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};

use degenbot_executor::composers::{
    encode_cmd_stream, encode_cmd_stream_capital, ComposerInputs, EncodeOptions, EncodeRequest,
};
use degenbot_executor::grammar_ledger::{FundingSource, InitialCapital, InsolventZeroCapital};
use degenbot_executor::grammar_shape::{derive_all_v2_detailed, derive_shape_detailed, Derive};

use crate::sim::capital::{
    CapitalAsset, CapitalLayouts, CapitalOverridePlan, CapitalPath, CapitalPlanError,
    CapitalTarget, ProbeCapital,
};

use super::{v3_amount_out, ExecOutcome, Harness, V2Pool, V3Pool, V4Pool};

/// A single path hop: the protocol-tagged pool + the src (input) and dst
/// (output) tokens. `src`/`dst` determine the hop's `zero_for_one` from the
/// pool's token0, and the amount computation from the pool's reserves/price.
#[derive(Debug, Clone, Copy)]
pub struct Hop {
    /// Input token of this hop (the previous hop's output, or WETH for hop 0).
    pub src: Address,
    /// Output token of this hop (the next hop's input, or WETH for the last).
    pub dst: Address,
    /// The pool the hop routes through, protocol-tagged.
    pub pool: HopPool,
}

/// A protocol-tagged pool reference for a [`Hop`].
#[derive(Debug, Clone, Copy)]
pub enum HopPool {
    V2(V2Pool),
    V3(V3Pool),
    V4(V4Pool),
}

impl HopPool {
    /// `zero_for_one` for a hop entering at `src`: true iff `src` is the
    /// pool's token0 (sorted for V2; call-ordered for V3/V4).
    fn zfo(&self, src: Address) -> bool {
        match self {
            HopPool::V2(p) => src == p.token0,
            HopPool::V3(p) => src == p.token0,
            HopPool::V4(p) => src == p.currency0,
        }
    }

    /// The V2 constant-product exact-out for `input_in` of a token into the
    /// pool at the seeded reserves — identical to `IntHopState::swap`
    /// (0.3% fee, `in*997/r` rounding down), inlined to avoid a lib-level dep
    /// on `degenbot-v2-math` (which the harness tests use via dev-deps).
    fn v2_amt_out(r_in: u128, r_out: u128, input_in: u128) -> u128 {
        let amp = U256::from(input_in) * U256::from(997u64);
        let num = amp * U256::from(r_out);
        let den = U256::from(r_in) * U256::from(1000u64) + amp;
        (num / den).to::<u128>()
    }

    /// The pool's output for `input_in` of `src` into `dst` — the same math
    /// the harness already proves (V2 constant-product, V3/V4 `v3_amount_out`).
    fn amount_out(&self, src: Address, dst: Address, input_in: u128) -> u128 {
        match self {
            HopPool::V2(p) => {
                let r_in = if src == p.token0 {
                    p.reserve0
                } else {
                    p.reserve1
                };
                let r_out = if dst == p.token0 {
                    p.reserve0
                } else {
                    p.reserve1
                };
                Self::v2_amt_out(r_in, r_out, input_in)
            }
            HopPool::V3(p) => {
                v3_amount_out(p.sqrt_price, p.liquidity, input_in, self.zfo(src), p.fee)
            }
            HopPool::V4(p) => {
                v3_amount_out(p.sqrt_price, p.liquidity, input_in, self.zfo(src), p.fee)
            }
        }
    }

    /// Into the executor's `HopInfo`, using `pool_manager` for V4.
    fn to_hop_info(self, pool_manager: Address, src: Address) -> HopInfo {
        match self {
            HopPool::V2(p) => HopInfo::V2(V2HopInfo {
                pool_address: p.pair,
                token0_address: p.token0,
                token1_address: p.token1,
                fee: 30,
                zfo: self.zfo(src),
            }),
            HopPool::V3(p) => HopInfo::V3(V3HopInfo {
                pool_address: p.pool,
                token0_address: p.token0,
                token1_address: p.token1,
                fee: p.fee,
                zfo: self.zfo(src),
            }),
            HopPool::V4(p) => HopInfo::V4(V4HopInfo {
                pool_manager_address: pool_manager,
                pool_id_hex: "0x0".into(),
                currency0_address: p.currency0,
                currency1_address: p.currency1,
                fee: p.fee,
                tick_spacing: p.tick_spacing,
                hook_address: Address::ZERO,
                zfo: self.zfo(src),
            }),
        }
    }
}

/// The fixture half of the live probe's [`ProbeCapital`] interface: which capital
/// posture a deterministic chain drive runs under, and — for a self-funded stream
/// whose payload the composer did not produce — where its requirements come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureCapital {
    posture: ProbeCapital,
    requirements: RequirementSource,
}

/// Where a `SelfFund` run's entry-capital requirements come from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RequirementSource {
    /// The built Plan behind the very request the fixture encodes.
    Plan,
    /// The caller's explicit declaration (a raw payload's only option).
    Declared(Vec<CapitalRequirement>),
}

impl FixtureCapital {
    /// Production: the executor's own holdings, no capital override at all.
    #[must_use]
    pub fn production() -> Self {
        Self {
            posture: ProbeCapital::Production,
            requirements: RequirementSource::Plan,
        }
    }

    /// Self-funded: the stream's declared entry capital installed, everything
    /// else the path touches cleared. The requirements are read from the built
    /// Plan of the same encode request the fixture drives.
    #[must_use]
    pub fn self_fund() -> Self {
        Self {
            posture: ProbeCapital::SelfFund,
            requirements: RequirementSource::Plan,
        }
    }

    /// Self-funded with CALLER-declared requirements — the only shape a raw
    /// payload can use, because its bytes carry no Plan to read an amount from.
    #[must_use]
    pub fn self_fund_declared(requirements: impl IntoIterator<Item = CapitalRequirement>) -> Self {
        Self {
            posture: ProbeCapital::SelfFund,
            requirements: RequirementSource::Declared(requirements.into_iter().collect()),
        }
    }

    /// Zero balance: every touched holding cleared, so only the path's own
    /// credit graph funds a debit.
    #[must_use]
    pub fn zero_balance() -> Self {
        Self {
            posture: ProbeCapital::ZeroBalance,
            requirements: RequirementSource::Plan,
        }
    }

    /// The posture this fixture drives.
    #[must_use]
    pub const fn posture(&self) -> ProbeCapital {
        self.posture
    }

    /// The requirement source this posture reads.
    const fn requirements(&self) -> &RequirementSource {
        &self.requirements
    }
}

/// Production is the default: a drive that says nothing about capital executes
/// against exactly the state its fixture seeded.
impl Default for FixtureCapital {
    fn default() -> Self {
        Self::production()
    }
}

/// Why a deterministic chain drive did not reach a classified execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainError {
    /// The fixture could not set the run up — a harness problem, not a verdict.
    Setup(String),
    /// The posture's capital plan could not be composed: a covered asset has no
    /// layout this fixture declared, so the run fails closed before any EVM work.
    Capital(CapitalPlanError),
    /// The zero-capital stance found the stream needs capital the posture
    /// withholds: the typed deficit, raised BEFORE execution.
    InsolventZeroCapital(InsolventZeroCapital),
    /// No composer produces this shape (a routine no-path, not a verdict).
    Declined,
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Setup(why) => write!(f, "fixture setup: {why}"),
            Self::Capital(refusal) => write!(f, "capital plan: {refusal}"),
            Self::InsolventZeroCapital(deficit) => write!(
                f,
                "insolvent zero capital: needs {} of {} with {} available",
                deficit.required, deficit.asset, deficit.available
            ),
            Self::Declined => f.write_str("the composer has no producer for this shape"),
        }
    }
}

impl std::error::Error for ChainError {}

/// The exact balance of every touched asset at one capture point.
///
/// Rows keep the plan's stable first-touch order (native first, then each
/// ERC-20 contract, then each `(PoolManager, currency)` claim), so two snapshots
/// of one run describe the same assets in the same positions and a held asset is
/// never reduced to a WETH delta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapitalSnapshot {
    holder: Address,
    rows: Vec<(CapitalAsset, U256)>,
}

impl CapitalSnapshot {
    /// The account these balances belong to (the plan's holder: the executor).
    #[must_use]
    pub const fn holder(&self) -> Address {
        self.holder
    }

    /// Every touched asset with its exact balance, in first-touch order.
    #[must_use]
    pub fn rows(&self) -> &[(CapitalAsset, U256)] {
        &self.rows
    }

    /// The exact balance of `asset`.
    ///
    /// # Panics
    ///
    /// When `asset` is not one this run touched — a fixture asking about an
    /// asset the path never names is a broken assertion, not a zero balance.
    #[must_use]
    #[track_caller]
    pub fn balance_of(&self, asset: CapitalAsset) -> U256 {
        self.rows
            .iter()
            .find(|(seen, _)| *seen == asset)
            .map_or_else(
                || panic!("{asset} is not a touched asset of this run: {self:?}"),
                |(_, balance)| *balance,
            )
    }
}

/// The result of a [`Harness::run_chain`] drive: the classified outcome plus
/// the predicted and measured profit and the run's exact capital state.
#[derive(Debug)]
pub struct ChainResult {
    pub outcome: ExecOutcome,
    /// Per-hop outputs the drive committed; `hop_outputs[last]` is the terminal
    /// output.
    pub hop_outputs: Vec<u128>,
    /// `out_terminal − optimal_input`, the profit the amounts were encoded for.
    pub predicted_profit: i128,
    /// Measured executor WETH-balance delta after `execute`. Should be ≈
    /// `predicted_profit`; a large divergence means the encoded amounts moved
    /// the wrong WETH even though the payload didn't revert.
    pub actual_weth_delta: i128,
    /// Measured PoolManager ERC6909 WETH-balance delta
    /// (`PM.balanceOf(executor, weth)`) after `execute` — where the
    /// `Erc6909` capture mints the profit
    /// ([`assert_erc6909_capture`] is its assertion). 0 for
    /// streams the capture axis does not branch (every non-pure-V4 family:
    /// they reach the vault only via the on-chain `profit_mode` config, never
    /// the stream bytes).
    pub erc6909_delta: i128,
    /// The ONE path-aware plan this run composed and applied before execution.
    /// Empty for `Production`, which writes nothing.
    pub capital_plan: CapitalOverridePlan,
    /// Every touched asset's exact balance immediately before `execute` — after
    /// the plan, so a `SelfFund` run starts from its requirements and a
    /// `ZeroBalance` run from zeros.
    pub initial_capital: CapitalSnapshot,
    /// The same assets' exact balances immediately after `execute`.
    pub final_capital: CapitalSnapshot,
}

impl ChainResult {
    /// The measured movement of `asset` across `execute` (`final − initial`),
    /// signed: the economic delta of one holding.
    ///
    /// # Panics
    ///
    /// When `asset` is not a touched asset of this run.
    #[must_use]
    #[track_caller]
    pub fn capital_delta(&self, asset: CapitalAsset) -> i128 {
        self.final_capital.balance_of(asset).to::<u128>() as i128
            - self.initial_capital.balance_of(asset).to::<u128>() as i128
    }
}

/// How a drive obtains the payload it executes.
#[derive(Clone, Copy)]
enum Execution<'a> {
    /// Encode through the production intake — the SAME `EncodeRequest` the
    /// capital requirements are read from.
    Encoded,
    /// A caller-supplied raw stream and its `execute()` config.
    Raw { payload: &'a [u8], config: U256 },
}

/// Project a posture onto the encoder options a fixture drives it with.
///
/// The SAME mode→axes projection the live binding applies
/// (`CmdExecutorBinding::opts`): `funding` is the STREAM axis a family branches
/// on, while `initial_capital` is the validation policy the ledger is evaluated
/// under. Both move together, because a self-funded STREAM under the zero-capital
/// POLICY is a different (and misleading) experiment: it would decline as
/// insolvent while the mode it claims to model runs a flash stream.
///
/// `Production` keeps the caller's baseline axes exactly as they are; the two
/// probe modes overwrite both capital axes, so a caller-supplied value can never
/// contradict the posture. Every non-capital axis (capture, batch, bribe) rides
/// through untouched, so a posture never changes a stream's shape.
fn capital_opts(posture: ProbeCapital, opts: EncodeOptions) -> EncodeOptions {
    match posture {
        ProbeCapital::Production => opts,
        ProbeCapital::SelfFund => EncodeOptions {
            funding: FundingSource::SelfFund,
            initial_capital: InitialCapital::Held,
            ..opts
        },
        ProbeCapital::ZeroBalance => EncodeOptions {
            funding: FundingSource::InPathFlash,
            initial_capital: InitialCapital::Zero,
            ..opts
        },
    }
}

/// Forward-traverse `hops` into the per-hop outputs plus the full-consumption
/// `consumed_inputs` (`[optimal_input, hop_outputs[0], …]`).
fn chain_amounts(hops: &[Hop], optimal_input: u128) -> (Vec<u128>, Vec<u128>) {
    let mut hop_outputs = Vec::with_capacity(hops.len());
    let mut consumed = optimal_input;
    for hop in hops {
        let out = hop.pool.amount_out(hop.src, hop.dst, consumed);
        hop_outputs.push(out);
        consumed = out;
    }
    let consumed_inputs: Vec<u128> = std::iter::once(optimal_input)
        .chain(hop_outputs.iter().copied())
        .take(hops.len())
        .collect();
    (hop_outputs, consumed_inputs)
}

impl Harness {
    /// The storage layouts this fixture's tokens and PoolManager are declared at.
    ///
    /// Every token the harness deploys is the `StubToken` stub (`balanceOf` at
    /// slot [`super::STUB_TOKEN_BALANCE_OF_SLOT`]) and the PoolManager is the
    /// stub whose ERC-6909 `balanceOf` sits at
    /// [`super::STUB_POOL_MANAGER_ERC6909_BALANCE_OF_SLOT`]. Neither is the
    /// canonical mainnet layout [`CapitalLayouts::verified`] pins, so the
    /// fixture declares its own explicitly instead of resolving a mainnet slot
    /// against a lab contract.
    #[must_use]
    pub fn capital_layouts(&self) -> CapitalLayouts {
        let mut layouts = CapitalLayouts::verified().with_manager(
            self.pool_manager,
            super::STUB_POOL_MANAGER_ERC6909_BALANCE_OF_SLOT,
        );
        for token in &self.tokens {
            layouts = layouts.with_token(*token, super::STUB_TOKEN_BALANCE_OF_SLOT);
        }
        layouts.with_token(self.weth, super::STUB_TOKEN_BALANCE_OF_SLOT)
    }

    /// Compose the ONE path-aware capital plan for `posture`.
    fn compose_plan(
        &self,
        posture: ProbeCapital,
        requirements: &[CapitalRequirement],
        path: &PathInfo,
    ) -> Result<CapitalOverridePlan, ChainError> {
        posture
            .override_plan(
                self.executor,
                &[CapitalPath { path, requirements }],
                &self.capital_layouts(),
            )
            .map_err(ChainError::Capital)
    }

    /// Every asset `path` touches, in the plan's stable first-touch order.
    ///
    /// The set is a fact about the PATH, not about the posture, and
    /// [`ProbeCapital::ZeroBalance`] is the projection that names it with no
    /// requirement set. Reading it here is what lets a `Production` run (whose
    /// plan writes nothing) still snapshot every holding the path touches.
    fn touched_assets(&self, path: &PathInfo) -> Result<Vec<CapitalAsset>, ChainError> {
        ProbeCapital::ZeroBalance
            .override_plan(
                self.executor,
                &[CapitalPath {
                    path,
                    requirements: &[],
                }],
                &self.capital_layouts(),
            )
            .map(|plan| plan.assets().collect())
            .map_err(ChainError::Capital)
    }

    /// Apply a composed plan: the native balance word through the account (code
    /// and nonce survive), every storage write through the fixture's slot seeder.
    ///
    /// A WETH `balanceOf` write installs a holding the token contract has no
    /// deposit backing for, so the same call credits the WETH stub that much
    /// native — the stub pays a later `WETH_WITHDRAW` out of its own balance,
    /// exactly as real WETH9 does. A clearing writes nothing back: the fixture
    /// never takes another holder's backing.
    fn apply_capital_plan(&mut self, plan: &CapitalOverridePlan) -> Result<(), ChainError> {
        let Some(holder) = plan.holder() else {
            return Ok(());
        };
        let mut storage_writes: Vec<(Address, Vec<(U256, U256)>)> = Vec::new();
        let mut weth_backing = U256::ZERO;
        for capital_write in plan.overrides() {
            match capital_write.target() {
                CapitalTarget::NativeBalance => {
                    self.set_native_balance(holder, capital_write.value());
                }
                CapitalTarget::Storage { contract, slot } => {
                    match storage_writes
                        .iter_mut()
                        .find(|(seen, _)| *seen == contract)
                    {
                        Some((_, slots)) => {
                            slots.push((slot, capital_write.value()));
                        }
                        None => {
                            storage_writes.push((contract, vec![(slot, capital_write.value())]));
                        }
                    }
                }
            }
            if capital_write.asset() == (CapitalAsset::Erc20 { token: self.weth }) {
                let held = self
                    .balance_of(self.weth, holder)
                    .map_err(ChainError::Setup)?;
                weth_backing += capital_write.value().saturating_sub(held);
            }
        }
        for (contract, slots) in storage_writes {
            crate::seed_slots(&mut self.evm, contract, &slots);
        }
        self.back_weth(weth_backing);
        Ok(())
    }

    /// Read one capital asset's exact balance for `holder`.
    fn asset_balance(&mut self, holder: Address, asset: CapitalAsset) -> Result<U256, ChainError> {
        match asset {
            CapitalAsset::Native => self.native_balance_of(holder),
            CapitalAsset::Erc20 { token } => self.balance_of(token, holder),
            CapitalAsset::Erc6909Claim { manager, currency } => {
                self.erc6909_balance_of(manager, holder, currency)
            }
        }
        .map_err(ChainError::Setup)
    }

    /// Capture `assets`' exact balances for `holder`.
    fn capital_snapshot(
        &mut self,
        holder: Address,
        assets: &[CapitalAsset],
    ) -> Result<CapitalSnapshot, ChainError> {
        let mut rows = Vec::with_capacity(assets.len());
        for asset in assets {
            rows.push((*asset, self.asset_balance(holder, *asset)?));
        }
        Ok(CapitalSnapshot { holder, rows })
    }

    /// Encode `request` through the production intake.
    fn encode_request(&self, request: &EncodeRequest) -> Result<Vec<u8>, ChainError> {
        match encode_cmd_stream(&self.encode_context(), request) {
            Some(bytes) => Ok(bytes),
            None => Err(self.classify_decline(request)),
        }
    }

    /// Name WHY the production intake declined `request`.
    ///
    /// `encode_cmd_stream`'s `Option` seam folds a typed zero-capital deficit
    /// into `None` (a routine answer, not a bug), and that typed answer is
    /// exactly what a zero-balance fixture has to read back. The routing below
    /// mirrors the intake's own (all-V2 with ≥2 hops → the Plan + validator
    /// path, otherwise the shape walker); the `*_detailed` entries are the only
    /// place the `Derive` survives.
    fn classify_decline(&self, request: &EncodeRequest) -> ChainError {
        let context = self.encode_context();
        let inputs = ComposerInputs {
            executor_address: context.executor,
            pool_manager_address: context.pool_manager,
            weth_address: context.weth,
            optimal_input: request.optimal_input,
            hop_outputs: request.hop_outputs.as_slice(),
            consumed_inputs: request.consumed_inputs.as_slice(),
            opts: request.opts,
        };
        let derive = if request.path.hops.len() >= 2
            && request
                .path
                .hops
                .iter()
                .all(|hop| matches!(hop, HopInfo::V2(_)))
        {
            derive_all_v2_detailed(&request.path, &inputs)
        } else {
            derive_shape_detailed(&request.path, &inputs)
        };
        match derive {
            Derive::InsolventZeroCapital(deficit) => ChainError::InsolventZeroCapital(deficit),
            // A `Rejected` derivation is fatal inside the intake itself (ADR-030)
            // and can never be observed from here.
            _ => ChainError::Declined,
        }
    }

    /// Drive one chain: compose and apply its capital posture, execute, and
    /// report the classified outcome with exact before/after capital snapshots.
    fn drive_chain(
        &mut self,
        hops: &[Hop],
        optimal_input: u128,
        hop_outputs: &[u128],
        consumed_inputs: &[u128],
        gas: u64,
        opts: EncodeOptions,
        capital: &FixtureCapital,
        execution: Execution<'_>,
    ) -> Result<ChainResult, ChainError> {
        let n = hops.len();
        if n < 1 {
            return Err(ChainError::Setup(
                "a chain drive needs >=1 hops".to_string(),
            ));
        }
        if hop_outputs.len() != n || consumed_inputs.len() != n {
            return Err(ChainError::Setup(format!(
                "per-hop arrays need {n} entries ({} outputs, {} consumed)",
                hop_outputs.len(),
                consumed_inputs.len()
            )));
        }
        let predicted_profit = hop_outputs[n - 1] as i128 - optimal_input as i128;
        let path = PathInfo::new(
            hops.iter()
                .map(|hop| hop.pool.to_hop_info(self.pool_manager, hop.src))
                .collect(),
        );

        // 1. The capital-adjusted intake: the posture moves the two capital
        //    axes and leaves every non-capital axis the caller declared alone.
        let capital_opts = capital_opts(capital.posture(), opts);
        let request = EncodeRequest::new(
            path.clone(),
            optimal_input,
            hop_outputs.to_vec(),
            consumed_inputs.to_vec(),
            capital_opts,
        );

        // 2. The entry capital a self-funded posture installs. A raw payload has
        //    no Plan behind it, so its requirements must be declared — the
        //    fixture never guesses an amount from bytes or hop count.
        let requirements: Vec<CapitalRequirement> = match capital.requirements() {
            RequirementSource::Declared(declared) => declared.clone(),
            RequirementSource::Plan => match capital.posture() {
                ProbeCapital::SelfFund if matches!(execution, Execution::Raw { .. }) => {
                    return Err(ChainError::Setup(
                        "a raw payload's SelfFund requirements must be declared \
                         (FixtureCapital::self_fund_declared): its bytes carry no Plan \
                         to read an amount from"
                            .to_string(),
                    ));
                }
                ProbeCapital::SelfFund => {
                    encode_cmd_stream_capital(&self.encode_context(), &request)
                }
                ProbeCapital::Production | ProbeCapital::ZeroBalance => Vec::new(),
            },
        };

        // 3. The ONE path-aware plan for this posture.
        let plan = self.compose_plan(capital.posture(), &requirements, &path)?;
        let holder = plan.holder().unwrap_or(self.executor);

        // 4. The assets the run snapshots: the path's touched set plus any
        //    requirement-only asset the plan names.
        let mut assets = self.touched_assets(&path)?;
        for asset in plan.assets() {
            if !assets.contains(&asset) {
                assets.push(asset);
            }
        }

        // 5. The payload, resolved BEFORE any state change: a typed capital
        //    refusal leaves the fixture exactly as its setup left it.
        let (payload, config) = match execution {
            Execution::Encoded => (
                self.encode_request(&request)?,
                super::production_config(capital_opts).map_err(ChainError::Setup)?,
            ),
            Execution::Raw { payload, config } => (payload.to_vec(), config),
        };

        // 6. Operational fixture setup (approvals, not capital), then the plan —
        //    authoritative last — then the run.
        for hop in hops {
            if let HopPool::V2(pool) = &hop.pool {
                self.executor_approve_pair(*pool)
                    .map_err(ChainError::Setup)?;
            }
        }
        self.apply_capital_plan(&plan)?;

        let initial_capital = self.capital_snapshot(holder, &assets)?;
        let weth_before = self
            .balance_of(self.weth, holder)
            .map_err(ChainError::Setup)?
            .to::<u128>();
        let claim_before = self
            .erc6909_balance_of(self.pool_manager, holder, self.weth)
            .map_err(ChainError::Setup)?
            .to::<u128>();

        let outcome = self
            .execute_payload_config(&payload, gas, config)
            .map_err(ChainError::Setup)?;

        let weth_after = self
            .balance_of(self.weth, holder)
            .map_err(ChainError::Setup)?
            .to::<u128>();
        let claim_after = self
            .erc6909_balance_of(self.pool_manager, holder, self.weth)
            .map_err(ChainError::Setup)?
            .to::<u128>();
        let final_capital = self.capital_snapshot(holder, &assets)?;

        Ok(ChainResult {
            outcome,
            hop_outputs: hop_outputs.to_vec(),
            predicted_profit,
            actual_weth_delta: weth_after as i128 - weth_before as i128,
            erc6909_delta: claim_after as i128 - claim_before as i128,
            capital_plan: plan,
            initial_capital,
            final_capital,
        })
    }

    /// Declaratively drive an ordered hop chain: derive amounts, build the
    /// `PathInfo`, apply the `Production` capital posture (no override at all),
    /// execute, and report the outcome plus predicted/measured profit.
    ///
    /// The executor executes against exactly the state the fixture seeded; this
    /// entry mints nothing.
    ///
    /// # Errors
    ///
    /// [`ChainError`] when the fixture cannot set the run up or the composer has
    /// no producer for the shape.
    pub fn run_chain(
        &mut self,
        hops: &[Hop],
        optimal_input: u128,
        gas: u64,
    ) -> Result<ChainResult, ChainError> {
        self.run_chain_capital(
            hops,
            optimal_input,
            gas,
            EncodeOptions::default(),
            &FixtureCapital::production(),
        )
    }

    /// Variant of [`Self::run_chain`] with explicit [`EncodeOptions`]
    /// (funding axis, capture, bribe) under the `Production` capital posture.
    ///
    /// A test that drives a NON-default stream axis — e.g. a self-funded stream
    /// whose losing path must still EXECUTE — pairs the axis with the matching
    /// capital posture ([`Self::run_chain_capital`]); the options alone install
    /// no capital.
    ///
    /// # Errors
    ///
    /// [`ChainError`] as [`Self::run_chain`].
    pub fn run_chain_with_opts(
        &mut self,
        hops: &[Hop],
        optimal_input: u128,
        gas: u64,
        opts: EncodeOptions,
    ) -> Result<ChainResult, ChainError> {
        self.run_chain_capital(
            hops,
            optimal_input,
            gas,
            opts,
            &FixtureCapital::production(),
        )
    }

    /// Drive a chain under an explicit capital posture.
    ///
    /// The posture's plan is composed from the path, the Plan-derived (or
    /// declared) requirements, and this fixture's token/PoolManager layouts, and
    /// applied before execution. `Production` applies nothing.
    ///
    /// # Errors
    ///
    /// [`ChainError::Capital`] when a touched asset has no declared layout,
    /// [`ChainError::InsolventZeroCapital`] when the zero-capital stance answers
    /// the path before execution, and [`ChainError::Setup`] for a fixture
    /// failure.
    pub fn run_chain_capital(
        &mut self,
        hops: &[Hop],
        optimal_input: u128,
        gas: u64,
        opts: EncodeOptions,
        capital: &FixtureCapital,
    ) -> Result<ChainResult, ChainError> {
        let (hop_outputs, consumed_inputs) = chain_amounts(hops, optimal_input);
        self.drive_chain(
            hops,
            optimal_input,
            &hop_outputs,
            &consumed_inputs,
            gas,
            opts,
            capital,
            Execution::Encoded,
        )
    }

    /// ADR-033 (D7) variant of [`Self::run_chain_with_opts`] with CALLER-supplied
    /// per-hop amounts: `hop_outputs` (the predicted per-hop outputs) and
    /// `consumed_inputs` (the committed per-hop inputs — the shape the
    /// production solver commits after `clamp_concentrated_liquidity_hop_capacity`).
    /// Runs under the `Production` posture: no capital is installed.
    ///
    /// # Errors
    ///
    /// [`ChainError`] as [`Self::run_chain`].
    pub fn run_chain_with_consumed(
        &mut self,
        hops: &[Hop],
        optimal_input: u128,
        hop_outputs: &[u128],
        consumed_inputs: &[u128],
        gas: u64,
        opts: EncodeOptions,
    ) -> Result<ChainResult, ChainError> {
        self.run_chain_with_consumed_capital(
            hops,
            optimal_input,
            hop_outputs,
            consumed_inputs,
            gas,
            opts,
            &FixtureCapital::production(),
        )
    }

    /// [`Self::run_chain_with_consumed`] under an explicit capital posture.
    ///
    /// # Errors
    ///
    /// [`ChainError`] as [`Self::run_chain_capital`].
    pub fn run_chain_with_consumed_capital(
        &mut self,
        hops: &[Hop],
        optimal_input: u128,
        hop_outputs: &[u128],
        consumed_inputs: &[u128],
        gas: u64,
        opts: EncodeOptions,
        capital: &FixtureCapital,
    ) -> Result<ChainResult, ChainError> {
        self.drive_chain(
            hops,
            optimal_input,
            hop_outputs,
            consumed_inputs,
            gas,
            opts,
            capital,
            Execution::Encoded,
        )
    }

    /// Build the production `PathInfo`, forward-traversed `hop_outputs`, and
    /// `consumed_inputs = [optimal_input, hop_outputs[0], …]` for a hop chain —
    /// everything the derivations/composers need, exposed publicly for the
    /// ShapeClass derivation spike to drive raw payloads. Uses the same
    /// amount math as [`Self::run_chain`].
    pub fn path_and_amounts(
        &mut self,
        hops: &[Hop],
        optimal_input: u128,
    ) -> (PathInfo, Vec<u128>, Vec<u128>) {
        let (hop_outputs, consumed_inputs) = chain_amounts(hops, optimal_input);
        let path_hops: Vec<HopInfo> = hops
            .iter()
            .map(|hop| hop.pool.to_hop_info(self.pool_manager, hop.src))
            .collect();
        (PathInfo::new(path_hops), hop_outputs, consumed_inputs)
    }

    /// Like [`Self::run_chain`] but drives an **explicit** payload instead of
    /// re-encoding via the production composer, under the `Production` posture.
    /// Used by the ShapeClass derivation spike to inject rule-driven bytes into
    /// the runtime matrix; `payload` is the raw `execute()` command stream.
    ///
    /// # Errors
    ///
    /// [`ChainError`] as [`Self::run_chain`].
    pub fn run_raw_payload(
        &mut self,
        hops: &[Hop],
        payload: &[u8],
        optimal_input: u128,
        gas: u64,
    ) -> Result<ChainResult, ChainError> {
        self.run_raw_payload_config(hops, payload, optimal_input, gas, U256::from(1u8))
    }

    /// Run an explicit payload under the `Production` posture with a
    /// caller-selected measured balance domain.
    ///
    /// # Errors
    ///
    /// [`ChainError`] as [`Self::run_chain`].
    pub fn run_raw_payload_config(
        &mut self,
        hops: &[Hop],
        payload: &[u8],
        optimal_input: u128,
        gas: u64,
        config: U256,
    ) -> Result<ChainResult, ChainError> {
        self.run_raw_payload_capital(
            hops,
            payload,
            optimal_input,
            gas,
            config,
            &FixtureCapital::production(),
        )
    }

    /// Run an explicit payload under an explicit capital posture.
    ///
    /// The payload's bytes cannot be introspected, so a `SelfFund` posture must
    /// carry its requirements explicitly
    /// ([`FixtureCapital::self_fund_declared`]); the fixture refuses to guess an
    /// amount from the bytes or the hop list, and never falls back to an
    /// unconditional grant.
    ///
    /// # Errors
    ///
    /// [`ChainError::Setup`] for an undeclared `SelfFund` requirement set, plus
    /// every [`ChainError`] of [`Self::run_chain_capital`].
    pub fn run_raw_payload_capital(
        &mut self,
        hops: &[Hop],
        payload: &[u8],
        optimal_input: u128,
        gas: u64,
        config: U256,
        capital: &FixtureCapital,
    ) -> Result<ChainResult, ChainError> {
        let (hop_outputs, consumed_inputs) = chain_amounts(hops, optimal_input);
        self.drive_chain(
            hops,
            optimal_input,
            &hop_outputs,
            &consumed_inputs,
            gas,
            EncodeOptions::default(),
            capital,
            Execution::Raw { payload, config },
        )
    }
}

/// Assert a [`ChainResult`] is a genuine profitable execution: it reached every
/// pool, the measured WETH delta is positive, and it matches the predicted
/// profit within a small tolerance (covering ±1-wei per-hop rounding so a
/// legitimate path isn't flaky, while a silently-wrong amount — off by orders
/// of magnitude — still fails loudly).
#[track_caller]
pub fn assert_profitable(result: &ChainResult, expected_swaps: usize, label: &str) {
    assert!(
        result.outcome.executed(expected_swaps),
        "[{label}] payload must execute (reach {expected_swaps} pools): {:?}",
        result.outcome
    );
    assert!(
        result.actual_weth_delta > 0,
        "[{label}] expected a profitable (positive) WETH delta, got {result:?}"
    );
    // Tolerance: 0.1% of the predicted magnitude, floored at 64 wei — plenty to
    // absorb ±1-wei-per-hop rounding without masking a real mis-encoding.
    let tol = (result.predicted_profit.abs() / 1000).max(64);
    assert!(
        (result.actual_weth_delta - result.predicted_profit).abs() <= tol,
        "[{label}] measured WETH delta {} diverges from predicted {} (tol {}): {:?}",
        result.actual_weth_delta,
        result.predicted_profit,
        tol,
        result
    );
}

/// Assert a [`ChainResult`] is a genuine **ERC6909-vault capture** (the
/// `capture = ProfitCapture::Erc6909` encode axis): the path executed, the measured
/// PoolManager ERC6909 WETH delta — the **contract-computed** side of the
/// profit assertion, read via `PM.balanceOf(executor, weth)` — is positive
/// and matches the predicted profit within the same 0.1% tolerance as
/// [`assert_profitable`] (±1-wei-per-hop rounding), and the profit did NOT
/// also remain in the executor's custody WETH (a capture that regressed to
/// custody, or a double count, would leave the WETH delta ≈ the profit).
///
/// This is the oracle half of the `profit_mode=2` on-chain assert: the
/// contract floors `PM.balanceOf` across `execute` (`after >= before`), and
/// this assert pins the **magnitude** the floor alone cannot see.
#[track_caller]
pub fn assert_erc6909_capture(result: &ChainResult, expected_swaps: usize, label: &str) {
    assert!(
        result.outcome.executed(expected_swaps),
        "[{label}] payload must execute (reach {expected_swaps} pools): {:?}",
        result.outcome
    );
    assert!(
        result.erc6909_delta > 0,
        "[{label}] expected an ERC6909 vault capture (positive PM balance delta), got {result:?}"
    );
    let tol = (result.predicted_profit.abs() / 1000).max(64);
    assert!(
        (result.erc6909_delta - result.predicted_profit).abs() <= tol,
        "[{label}] ERC6909 delta {} diverges from predicted {} (tol {}): {:?}",
        result.erc6909_delta,
        result.predicted_profit,
        tol,
        result
    );
    assert!(
        result.actual_weth_delta <= tol,
        "[{label}] custody WETH delta {} must not also carry the profit (tol {}): {:?}",
        result.actual_weth_delta,
        tol,
        result
    );
}

/// Assert the applied capital plan is exactly the state the executor started
/// from: every asset the plan names holds the value it installed, in plan order.
///
/// This is the fixture-side statement of "the posture installed what it
/// declared": a missing write, a holding the plan failed to clear, or a
/// leftover universal grant all fail it. Under `SelfFund` the required assets
/// hold the Plan's amounts and every other touched asset holds zero; under
/// `ZeroBalance` every row is zero.
///
/// # Panics
///
/// When the plan writes nothing (a `Production` run, whose seeded state is the
/// fixture's own), or when the snapshot and the plan disagree.
#[track_caller]
pub fn assert_applied_capital(result: &ChainResult, label: &str) {
    assert!(
        !result.capital_plan.is_empty(),
        "[{label}] a probe posture must apply a plan; production writes nothing"
    );
    let expected: Vec<(CapitalAsset, U256)> = result
        .capital_plan
        .overrides()
        .iter()
        .map(|capital_write| (capital_write.asset(), capital_write.value()))
        .collect();
    assert_eq!(
        result.initial_capital.rows(),
        expected.as_slice(),
        "[{label}] the executor's initial state must be exactly the applied plan"
    );
    assert_eq!(
        result.initial_capital.holder(),
        result.final_capital.holder(),
        "[{label}] both snapshots describe the same account"
    );
}

/// Assert every touched holding starts at exactly zero (the zero-balance
/// posture), naming the first holding that does not.
#[track_caller]
pub fn assert_zero_capital(result: &ChainResult, label: &str) {
    for (asset, balance) in result.initial_capital.rows() {
        assert!(
            balance.is_zero(),
            "[{label}] {asset} starts at {balance}, not zero: {:?}",
            result.initial_capital.rows()
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use degenbot_executor::grammar_ledger::{Bribe, ProfitCapture};

    /// The fixture's mode→axes projection is the live binding's, read off ONE
    /// posture: both capital axes move together, and every non-capital axis the
    /// caller declared rides through untouched.
    ///
    /// The zero-balance arm is the one that matters. A self-funded STREAM under
    /// the zero-capital POLICY is not that mode: the stream declares entry
    /// capital the policy withholds, so it would decline as insolvent while the
    /// flash stream the mode actually models executes. The posture therefore
    /// overwrites `funding`, not only `initial_capital`.
    #[test]
    fn capital_opts_projects_both_capital_axes_and_keeps_the_rest() {
        let baseline = EncodeOptions {
            funding: FundingSource::SelfFund,
            initial_capital: InitialCapital::Held,
            capture: ProfitCapture::Erc6909,
            use_v4_batch: true,
            bribe: Bribe::Some {
                bips: 25,
                recipient_idx: 1,
            },
        };

        let production = capital_opts(ProbeCapital::Production, baseline);
        assert_eq!(
            production.funding,
            FundingSource::SelfFund,
            "production keeps the caller's own axes"
        );
        assert_eq!(production.initial_capital, InitialCapital::Held);

        let self_fund = capital_opts(ProbeCapital::SelfFund, baseline);
        assert_eq!(self_fund.funding, FundingSource::SelfFund);
        assert_eq!(self_fund.initial_capital, InitialCapital::Held);

        let zero = capital_opts(ProbeCapital::ZeroBalance, baseline);
        assert_eq!(
            zero.funding,
            FundingSource::InPathFlash,
            "the zero posture drives the flash stream, never the caller's self-funded one"
        );
        assert_eq!(zero.initial_capital, InitialCapital::Zero);

        for projected in [production, self_fund, zero] {
            assert_eq!(projected.capture, baseline.capture);
            assert_eq!(projected.use_v4_batch, baseline.use_v4_batch);
            assert_eq!(projected.bribe, baseline.bribe);
        }
    }
}
