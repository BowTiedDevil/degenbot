//! Backrun connector-solve engine (epic DFYDYI, task B3): the
//! engine-backed path solver replacing the hand-composed mono-pool probe.
//!
//! Solves over a PRIVATE planning [`Workspace`] (the FORK-1 isolation: the
//! scope's scratch state never touches the mainline pump's registry), admits
//! pools with explicit typed state, declares two-hop paths, and evaluates
//! them with the solvers' envelope-gated mixed solve (V2-V2 = the closed-form
//! integer Mobius; no hand-composed pricing anywhere). Pool state arrives via
//! the frame pipeline's replay-extracted post-states (`admit_v2`,
//! `admit_v3_explicit`) or the RPC fetch ladders (`admit_v3_full`); discovery
//! over the DB connector index is the pipeline's job.

use alloy::primitives::{Address, B256, U256};
use degenbot_pathfinding::PoolKind;
use degenbot_pools::v3_state::{ClSlotLayout, PoolTickCoverage};
use degenbot_pools::{ConcentratedLiquidityVariant, Identity, ReservePairVariant, TickInfo};
use degenbot_solvers::mixed::SolvePathResult;

use degenbot_bot::bot_core::planning::{
    ExplicitPoolState, PlanningHop, PlanningPoolParams, Workspace,
};
use degenbot_bot::bot_core::pool_builder::builder::derive_hook_flags;

pub use degenbot_bot::bot_core::planning::PathReject;

/// One admitted V2 pool: identity + the LIVE reserves the caller fetched
/// (the adapter keeps this narrow; reserves come from `fetch_v2_reserves`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackrunV2Pool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: u128,
    pub reserve1: u128,
}

/// The standalone frame solver: the backrun lane's driver shell over a
/// planning [`Workspace`] (FORK-1 isolation — the scope's scratch state
/// never touches the mainline pump's registry). Admission, staging,
/// declaration, and evaluation delegate to the workspace; the lane owns the
/// RPC fetch ladders that PRODUCE the explicit state (slot0/tick bootstrap,
/// live reserves).
pub struct BackrunSolver {
    ws: Workspace,
}

impl BackrunSolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ws: Workspace::new(),
        }
    }

    /// The already-registered workspace pool id for `address`, if this
    /// frame's scope admitted it. Cycle sets overlap; a second cycle through
    /// the same cold connector must reuse the admitted pool, not re-admit
    /// it (`AlreadyRegistered` would otherwise drop the cycle).
    #[must_use]
    pub fn registered_pool_id(&self, address: &Address) -> Option<u64> {
        self.ws.pool_id_by_address(address)
    }

    /// Admit a V2 pool with the canonical 0.3% fee preset. Per-exchange fee
    /// variants enter with the CL/fee-lane work (B2 follow-up).
    ///
    /// # Errors
    ///
    /// Registration rejections (spec-bound reserve violations).
    pub fn admit_v2(&mut self, p: &BackrunV2Pool) -> Result<u64, String> {
        let Ok(reserve0) = p.reserve0.try_into() else {
            return Err(format!("reserve0 out of uint112: {}", p.reserve0));
        };
        let Ok(reserve1) = p.reserve1.try_into() else {
            return Err(format!("reserve1 out of uint112: {}", p.reserve1));
        };
        self.ws
            .register_with_state(
                PlanningPoolParams {
                    address: p.address,
                    token0: p.token0,
                    token1: p.token1,
                },
                ExplicitPoolState::V2 {
                    reserve0,
                    reserve1,
                    fee_token0: (997, 1000),
                    fee_token1: (997, 1000),
                },
                1,
            )
            .map_err(|e| format!("v2 admission failed: {e:?}"))
    }

    /// Declare a path from admitted pool ids + directions; returns its index.
    /// Hops referencing unknown pools make the path invalid for this frame
    /// (dropped loudly at resolve, not admitted lazily here).
    /// Declare a path from executable hop refs (the declare side of
    /// [`BackrunHopRef`] -- cycle order is hop order; the per-hop family
    /// carries the solver `HopType` so V2/V3 mixes resolve in one cycle).
    #[must_use]
    pub fn declare_hops(&mut self, hops: &[BackrunHopRef]) -> usize {
        let refs: Vec<PlanningHop> = hops
            .iter()
            .map(|h| PlanningHop {
                pool_id: h.pool_id,
                hop_type: h.family.hop_type(),
                zero_for_one: h.zfo,
            })
            .collect();
        self.ws.declare(&refs)
    }

    #[must_use]
    pub fn declare(&mut self, hops: &[(u64, bool)]) -> usize {
        let refs: Vec<PlanningHop> = hops
            .iter()
            .map(|(pool_id, zfo)| PlanningHop::v2(*pool_id, *zfo))
            .collect();
        self.ws.declare(&refs)
    }

    /// Envelope-gated solve of a declared path at `min_profit` (the gas
    /// floor + a safety margin). `None` = gate skipped or unsolvable.
    pub fn evaluate(&mut self, path_idx: usize, min_profit: U256) -> Option<SolvePathResult> {
        self.ws.evaluate(path_idx, min_profit)
    }

    /// The typed form of [`BackrunSolver::evaluate`]: the reject cause
    /// survives the `Option` collapse for the per-chain trace.
    ///
    /// # Errors
    ///
    /// [`PathReject`] carrying the workspace verdict verbatim.
    pub fn evaluate_verdict(
        &mut self,
        path_idx: usize,
        min_profit: U256,
    ) -> Result<SolvePathResult, PathReject> {
        self.ws.evaluate_verdict(path_idx, min_profit)
    }

    /// Diagnostic: the full evaluate path for one declared index -- resolve
    /// facts + the solve/gate verdict. Standalone + e2e observability only.
    #[must_use]
    pub fn resolve_debug(&mut self, path_idx: usize) -> String {
        self.ws.resolve_debug(path_idx)
    }

    #[must_use]
    pub fn path_count(&self) -> usize {
        self.ws.path_count()
    }
}

impl Default for BackrunSolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a cold-connector V3 hop could not be raw-RPC admitted: the ladder
/// fetch (`admit_v3_full`) names the refused step, so the per-hop JSONL
/// trace distinguishes a network/spec failure from a registration refusal
/// (an `AlreadyRegistered` duplicate is a different beast from a dead
/// archive-node call and must not share one opaque label).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V3LadderReject {
    /// The `slot0`/`liquidity` eth_call failed.
    Slot0Fetch(String),
    /// A slot0/liquidity value did not fit its wire type.
    Slot0Width,
    /// A tick-bitmap word fetch failed.
    BitmapFetch { word: i16, detail: String },
    /// An initialized-tick fetch failed.
    TickFetch { tick: i32, detail: String },
    /// The workspace refused the fetched state (spec violation,
    /// `AlreadyRegistered`, ...).
    Register(String),
}

impl V3LadderReject {
    /// The stable JSONL `stage` label for the refused step.
    #[must_use]
    pub fn stage(&self) -> &'static str {
        match self {
            Self::Slot0Fetch(_) => "admit-v3-slot0-fetch",
            Self::Slot0Width => "admit-v3-slot0-width",
            Self::BitmapFetch { .. } => "admit-v3-bitmap-fetch",
            Self::TickFetch { .. } => "admit-v3-tick-fetch",
            Self::Register(_) => "admit-v3-register",
        }
    }

    /// The step's error detail (empty for width refusals, whose identity is
    /// the stage itself).
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Slot0Fetch(d)
            | Self::BitmapFetch { detail: d, .. }
            | Self::TickFetch { detail: d, .. }
            | Self::Register(d) => d.clone(),
            Self::Slot0Width => "slot0/liquidity value out of type range".into(),
        }
    }
}

/// One executable hop of a lane candidate (executor-composer input). Both
/// the declared solver key (`pool_id`) and the composer identity (`pool`)
/// ride together so a declared cycle and its executable form cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackrunHopRef {
    pub pool_id: u64,
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub zfo: bool,
    /// The hop's protocol family (V2 or V3 with its 1e6 fee). The declared
    /// `HopType` and the composer `HopInfo` both derive from this one tag.
    pub family: LaneFamily,
}

/// The lane hop's protocol family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneFamily {
    V2,
    /// Concentrated liquidity carrying the pool's 1e6-convention fee.
    V3 {
        fee: u32,
    },
    /// V4 concentrated liquidity: the `poolId`-keyed identity the executor's
    /// V4 swap commands need, plus the key's fee/spacing/hooks for the
    /// composer's path info. The lane's `BackrunHopRef.pool` is the
    /// `PoolManager` singleton address, not the poolId.
    V4 {
        fee: u32,
        pool_id: B256,
        tick_spacing: i32,
        hooks: Address,
    },
}

/// The family TAG of a [`LaneFamily`] — the lane vocabulary's projection onto
/// the V2/V3/V4 identities the discovery graph and the executor share
/// (ADR-059 D1). [`LaneFamily`] itself stays the lane's data-carrying payload;
/// only the tag projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaneFamilyTag {
    V2,
    V3,
    V4,
}

impl LaneFamilyTag {
    /// The solver hop engine this family tag dispatches to.
    #[must_use]
    pub const fn hop_type(self) -> degenbot_solvers::mixed::HopType {
        match self {
            Self::V2 => degenbot_solvers::mixed::HopType::V2,
            Self::V3 => degenbot_solvers::mixed::HopType::V3,
            Self::V4 => degenbot_solvers::mixed::HopType::V4,
        }
    }

    /// Project the pool taxonomy onto the lane tag (ADR-059 D1).
    ///
    /// The backrun arm is V2/V3/V4-only, so the projection LAGS the taxonomy:
    /// the balance-vector families and the Solidly-stable branch it does not
    /// admit project to `None` rather than to a false V2/V3 claim. A volatile
    /// `AerodromeV2` pair is the constant-product V2 lane; a stable one is
    /// Solidly, which the lane cannot express.
    #[must_use]
    pub fn from_identity(identity: &Identity) -> Option<Self> {
        match identity {
            Identity::ReservePair { variant, .. } => match variant {
                ReservePairVariant::UniswapV2
                | ReservePairVariant::AerodromeV2 { stable: false } => Some(Self::V2),
                ReservePairVariant::AerodromeV2 { stable: true } => None,
            },
            Identity::ConcentratedLiquidity { variant, .. } => match variant {
                ConcentratedLiquidityVariant::UniswapV3 => Some(Self::V3),
                ConcentratedLiquidityVariant::UniswapV4 => Some(Self::V4),
            },
            // The lane vocabulary is V2/V3/V4-only; balance-vector and binned
            // liquidity both LAG the taxonomy rather than a false claim (D8).
            Identity::BalanceVector { .. } | Identity::BinnedLiquidity { .. } => None,
        }
    }
}

impl From<LaneFamilyTag> for PoolKind {
    /// Project the lane tag onto the discovery graph's pool kind (ADR-059 D1):
    /// a lane's declared anchor and the graph edge it is looked up by must not
    /// drift.
    fn from(tag: LaneFamilyTag) -> Self {
        match tag {
            LaneFamilyTag::V2 => Self::V2,
            LaneFamilyTag::V3 => Self::V3,
            LaneFamilyTag::V4 => Self::V4,
        }
    }
}

impl LaneFamily {
    /// The lane family's tag (ADR-059 D1) — the one way to reach
    /// [`LaneFamilyTag::hop_type`] and [`PoolKind`] from the data-carrying
    /// enum.
    #[must_use]
    pub const fn tag(self) -> LaneFamilyTag {
        match self {
            Self::V2 => LaneFamilyTag::V2,
            Self::V3 { .. } => LaneFamilyTag::V3,
            Self::V4 { .. } => LaneFamilyTag::V4,
        }
    }

    #[must_use]
    pub const fn hop_type(self) -> degenbot_solvers::mixed::HopType {
        self.tag().hop_type()
    }
}

/// The best executable path a lane found: everything the composer + exact
/// sim need (epic DFYDYI B4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneCandidate {
    pub hops: Vec<BackrunHopRef>,
    pub optimal_input: u128,
    pub hop_outputs: Vec<u128>,
    pub consumed_inputs: Vec<u128>,
    /// The solver's closed-form profit in the quote asset (wei).
    pub profit: u128,
}

impl BackrunSolver {
    /// Admit a V3 pool with EXPLICIT journal-provided state — the frame
    /// pipeline's no-RPC admission: post-target `slot0`/`liquidity` + the
    /// replayed per-tick words land here verbatim (`Sparse` coverage; the
    /// solver's projections fail loudly on missing words instead of guessing).
    /// `None` on a spec-bound registration rejection (bad replayed state).
    #[expect(
        clippy::too_many_arguments,
        reason = "explicit V3 state admission carries the full typed state"
    )]
    pub fn admit_v3_explicit(
        &mut self,
        address: Address,
        token0: Address,
        token1: Address,
        fee: u32,
        tick_spacing: i32,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        tick_data: hashbrown::HashMap<i32, TickInfo>,
        seed_block: u64,
        slot_layout: ClSlotLayout,
    ) -> Option<u64> {
        self.ws
            .register_with_state(
                PlanningPoolParams {
                    address,
                    token0,
                    token1,
                },
                ExplicitPoolState::V3 {
                    sqrt_price_x96,
                    liquidity,
                    tick,
                    fee,
                    tick_spacing,
                    tick_data,
                    coverage: PoolTickCoverage::Sparse,
                    slot_layout,
                },
                seed_block,
            )
            .ok()
    }

    /// Admit a V4 pool with EXPLICIT journal-provided state — the V4 twin of
    /// [`BackrunSolver::admit_v3_explicit`]: post-target `slot0`/`liquidity` +
    /// the replayed per-tick words land here verbatim (`Sparse` coverage; the
    /// solver's projections fail loudly on missing words instead of guessing).
    /// `None` on a spec-bound registration rejection (bad replayed state).
    ///
    /// `hook_flags` is derived from `hooks` (the hook contract address is the
    /// single source of truth for the low-16-bit mask). `protocol_fee` is
    /// admitted as `0`: the manager's live value is not consumed by backrun
    /// state math today — settlement's gate reads it from the manager settings,
    /// so a nonzero protocol fee would need that integration before live
    /// execution decisions.
    #[expect(
        clippy::too_many_arguments,
        reason = "explicit V4 state admission carries the full typed state"
    )]
    pub fn admit_v4_explicit(
        &mut self,
        manager: Address,
        token0: Address,
        token1: Address,
        pool_id: B256,
        fee: u32,
        tick_spacing: i32,
        hooks: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        tick_data: hashbrown::HashMap<i32, TickInfo>,
        seed_block: u64,
    ) -> Option<u64> {
        self.ws
            .register_with_state(
                PlanningPoolParams {
                    address: manager,
                    token0,
                    token1,
                },
                ExplicitPoolState::V4 {
                    pool_id: pool_id.0,
                    fee,
                    tick_spacing,
                    hooks,
                    hook_flags: derive_hook_flags(hooks),
                    protocol_fee: 0,
                    sqrt_price_x96,
                    liquidity,
                    tick,
                    tick_data,
                    coverage: PoolTickCoverage::Sparse,
                    tick_data_block: None,
                },
                seed_block,
            )
            .ok()
    }

    /// Register a V3 pool at HEAD (via the connector edge's shared ladder
    /// logic), optionally OVERRIDING `sqrt_price_x96` with the post-target
    /// staged price (the V3-affected-pool path). Args are decoded-wire
    /// semantics: `fee` in the v3 tier convention, `tick_spacing` from the
    /// pool's immutables.
    #[expect(
        clippy::too_many_arguments,
        reason = "pool identity + staged override describe distinct layers"
    )]
    pub async fn admit_v3_full(
        &mut self,
        provider: &degenbot_rpc::provider::AlloyProvider,
        address: Address,
        token0: Address,
        token1: Address,
        fee: u32,
        tick_spacing: i32,
        sqrt_override: Option<U256>,
        head: u64,
        slot_layout: ClSlotLayout,
    ) -> Result<u64, V3LadderReject> {
        use degenbot_rpc::abi::{fetch_tick_bitmap, fetch_tick_data, fetch_v3_slot0_liquidity};

        let (sqrt_price_x96, tick, liquidity) = fetch_v3_slot0_liquidity(provider, &address, None)
            .await
            .map_err(|e| V3LadderReject::Slot0Fetch(e.to_string()))?;
        let liquidity = u128::try_from(liquidity).map_err(|_| V3LadderReject::Slot0Width)?;

        // Sparse coverage: the current word +- 1 (a 3-word window around the
        // active range; the exact-sim oracle is the truth bar behind this).
        let tick_i32 = i32::try_from(tick).map_err(|_| V3LadderReject::Slot0Width)?;
        let (word, _) = degenbot_math::cl::liquidity_mapping::get_tick_word_and_bit_position(
            tick_i32,
            tick_spacing,
        );
        let word_i16 = i16::try_from(word).map_err(|_| V3LadderReject::Slot0Width)?;
        let mut tick_data: hashbrown::HashMap<i32, TickInfo> = hashbrown::HashMap::new();
        for w in [
            word_i16.saturating_sub(1),
            word_i16,
            word_i16.saturating_add(1),
        ] {
            let bitmap = fetch_tick_bitmap(provider, &address, w, None)
                .await
                .map_err(|e| V3LadderReject::BitmapFetch {
                    word: w,
                    detail: e.to_string(),
                })?;
            for bit in 0..256usize {
                if !bitmap.bit(bit) {
                    continue;
                }
                let active_tick = ((i32::from(w) << 8)
                    + i32::try_from(bit).map_err(|_| V3LadderReject::BitmapFetch {
                        word: w,
                        detail: "bit index out of i32".into(),
                    })?)
                    * tick_spacing;
                let (gross, net) = fetch_tick_data(provider, &address, active_tick, None)
                    .await
                    .map_err(|e| V3LadderReject::TickFetch {
                        tick: active_tick,
                        detail: e.to_string(),
                    })?;
                tick_data.insert(
                    active_tick,
                    TickInfo {
                        liquidity_gross: gross,
                        liquidity_net: net,
                        block: head,
                    },
                );
            }
        }

        self.ws
            .register_with_state(
                PlanningPoolParams {
                    address,
                    token0,
                    token1,
                },
                ExplicitPoolState::V3 {
                    sqrt_price_x96: sqrt_override.unwrap_or(sqrt_price_x96),
                    liquidity,
                    tick: tick_i32,
                    fee,
                    tick_spacing,
                    tick_data,
                    coverage: PoolTickCoverage::Sparse,
                    slot_layout,
                },
                head,
            )
            .map_err(|e| V3LadderReject::Register(format!("{e:?}")))
    }
}

/// Why [`compose_candidate`] refused a solved candidate: the typed `None` of
/// the composer. The frame pipeline traces [`ComposeReject::label`] so a
/// solved-but-uncomposable frame names the exact encoding seam it died at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeReject {
    /// Fewer than two hops, or the per-hop amount vectors do not align with
    /// the hop list.
    UnsupportedHopShape,
    /// A `V2_SWAP_COMPACT` amount reaches the uint96 wire width.
    AmountExceedsUint96,
    /// The command-stream encoder rejected the path.
    StreamEncodingFailed,
    /// The `execute(commands, config)` call could not be ABI-encoded.
    ExecuteEncodingFailed,
    /// V4 hops in one candidate name different `PoolManager` singletons; the
    /// session's `EncodeContext` carries one manager, so a mixed path has no
    /// single sentinel to resolve.
    MixedPoolManagers,
}

impl ComposeReject {
    /// The stable JSONL label (the offline-review contract).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::UnsupportedHopShape => "unsupported_hop_shape",
            Self::AmountExceedsUint96 => "amount_exceeds_uint96",
            Self::StreamEncodingFailed => "encoding_failed:cmd_stream",
            Self::ExecuteEncodingFailed => "encoding_failed:execute_call",
            Self::MixedPoolManagers => "mixed_pool_managers",
        }
    }
}

/// The composed candidate bundle: the executor's `execute(commands, config)`
/// calldata, ready for the exact-sim oracle + submission (DFYDYI B4).
///
/// Funding = in-path flash (the leading pool's swap callback extends entry
/// credit, repaid by the path), capture = executor custody, and the bid is
/// the executor's native coinbase bribe: config `bribe_bips` on the TRUE
/// profit delta (check mode 1 = WETH+ETH), recipient 0 = coinbase, with
/// WETH auto-unwrap built into the bribe payout.
///
/// # Errors
///
/// [`ComposeReject`] naming the encoding seam that refused the candidate.
pub fn compose_candidate(
    candidate: &LaneCandidate,
    executor: Address,
    weth: Address,
    bribe_bips: u16,
) -> Result<alloy::primitives::Bytes, ComposeReject> {
    use degenbot_executor::composers::{
        encode_cmd_stream, encode_execute_call, EncodeContext, EncodeOptions, EncodeRequest,
        HopInfo, V2HopInfo, V3HopInfo, V4HopInfo,
    };
    use degenbot_executor::grammar_ledger::{Bribe, FundingSource, ProfitCapture};

    if candidate.hops.len() < 2
        || candidate.hop_outputs.len() != candidate.hops.len()
        || candidate.consumed_inputs.len() != candidate.hops.len()
    {
        return Err(ComposeReject::UnsupportedHopShape);
    }
    // V2_SWAP_COMPACT carries uint96 amounts.
    let u96_max = u128::from(u64::MAX) * 0x1_0000_0000 + 0xFFFF_FFFF;
    let too_big = |v: u128| v >= u96_max;
    if too_big(candidate.optimal_input)
        || candidate.hop_outputs.iter().any(|&v| too_big(v))
        || candidate.consumed_inputs.iter().any(|&v| too_big(v))
    {
        return Err(ComposeReject::AmountExceedsUint96);
    }

    let mut hops = Vec::with_capacity(candidate.hops.len());
    // The session's `EncodeContext` holds one PoolManager; source it from the
    // candidate's V4 hops rather than a hardcoded deployment so a non-mainnet
    // manager resolves its own sentinel. All V4 hops must agree (single-species
    // is the documented scope); V2/V3-only candidates never read it.
    let mut pool_manager: Option<Address> = None;
    for h in &candidate.hops {
        let hop = match h.family {
            LaneFamily::V2 => HopInfo::V2(V2HopInfo {
                pool_address: h.pool,
                token0_address: h.token0,
                token1_address: h.token1,
                fee: 30,
                zfo: h.zfo,
            }),
            LaneFamily::V3 { fee } => HopInfo::V3(V3HopInfo {
                pool_address: h.pool,
                token0_address: h.token0,
                token1_address: h.token1,
                fee,
                zfo: h.zfo,
            }),
            LaneFamily::V4 {
                fee,
                pool_id,
                tick_spacing,
                hooks,
            } => {
                if pool_manager.is_some_and(|manager| manager != h.pool) {
                    return Err(ComposeReject::MixedPoolManagers);
                }
                pool_manager = Some(h.pool);
                HopInfo::V4(V4HopInfo {
                    pool_manager_address: h.pool,
                    pool_id_hex: format!("0x{}", alloy::hex::encode(pool_id)),
                    currency0_address: h.token0,
                    currency1_address: h.token1,
                    fee,
                    tick_spacing,
                    hook_address: hooks,
                    zfo: h.zfo,
                })
            }
        };
        hops.push(hop);
    }
    let req = EncodeRequest::new(
        degenbot_executor::composers::PathInfo::new(hops),
        candidate.optimal_input,
        candidate.hop_outputs.clone(),
        candidate.consumed_inputs.clone(),
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: false,
            funding: FundingSource::InPathFlash,
            capture: ProfitCapture::Custody,
            bribe: Bribe::None,
        },
    );
    // V2/V3-only streams never read the manager (no V4 sentinel/table entry),
    // so the canonical mainnet PoolManager keeps that context well-formed.
    let ctx = EncodeContext::new(
        executor,
        pool_manager.unwrap_or(alloy::primitives::address!(
            "000000000004444c5dc75cb358380d2e3de08a90"
        )),
        weth,
    );
    let commands = encode_cmd_stream(&ctx, &req).ok_or(ComposeReject::StreamEncodingFailed)?;
    // check_mode 1 (WETH+ETH true-delta check) + coinbase bribe bips.
    let config = (U256::from(bribe_bips) << 8) | U256::from(1u8);
    encode_execute_call(executor, &commands, config)
        .map(|call| alloy::primitives::Bytes::from(call.data.clone()))
        .map_err(|_| ComposeReject::ExecuteEncodingFailed)
}

/// [`compose_candidate`] as the historical `Option` shape.
#[must_use]
pub fn build_candidate_calldata(
    candidate: &LaneCandidate,
    executor: Address,
    weth: Address,
    bribe_bips: u16,
) -> Option<alloy::primitives::Bytes> {
    compose_candidate(candidate, executor, weth, bribe_bips).ok()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "golden-reference tests: admitted fixtures are spec-valid by construction"
)]
mod tests {
    use super::*;
    use alloy::primitives::{address, aliases::U112};

    /// The ladder reject is legible: each stage maps to a distinct JSONL
    /// `stage` label so a failed hop admission names the refused step
    /// (RPC fetch vs spec/width vs registration) instead of one lumped
    /// opaque label.
    #[test]
    fn ladder_reject_stages_report_the_refused_step() {
        assert_eq!(
            V3LadderReject::Slot0Fetch("timeout".into()).stage(),
            "admit-v3-slot0-fetch"
        );
        assert_eq!(V3LadderReject::Slot0Width.stage(), "admit-v3-slot0-width");
        assert_eq!(
            V3LadderReject::BitmapFetch {
                word: -1,
                detail: "revert".into()
            }
            .stage(),
            "admit-v3-bitmap-fetch"
        );
        assert_eq!(
            V3LadderReject::TickFetch {
                tick: 60,
                detail: "revert".into()
            }
            .stage(),
            "admit-v3-tick-fetch"
        );
        let reg = V3LadderReject::Register("AlreadyRegistered { 0xb001 }".into());
        assert_eq!(reg.stage(), "admit-v3-register");
        assert!(
            reg.detail().contains("AlreadyRegistered"),
            "detail survives"
        );
    }

    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const TOK: Address = address!("0000000000000000000000000000000000000aa1");
    /// P: the affected pool (the target swapped WETH -> TOK here).
    const P: Address = address!("000000000000000000000000000000000000b001");
    /// Q: the DB-discovered connector pool (cheaper TOK than staged P).
    const Q: Address = address!("000000000000000000000000000000000000b002");

    /// Admit a V2 fixture into a workspace scope with the canonical 0.3%
    /// fee preset (the lane's admission params, inlined per fixture).
    fn admitted_v2(s: &mut Workspace, address_: Address, r0: u128, r1: u128) -> u64 {
        s.register_with_state(
            PlanningPoolParams {
                address: address_,
                token0: TOK,
                token1: WETH,
            },
            ExplicitPoolState::V2 {
                reserve0: U112::from(r0),
                reserve1: U112::from(r1),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
            },
            1,
        )
        .expect("admits")
    }

    fn declared_cycle(s: &mut Workspace, p_id: u64, q_id: u64, p_zfo: bool, q_zfo: bool) -> usize {
        s.declare(&[PlanningHop::v2(p_id, p_zfo), PlanningHop::v2(q_id, q_zfo)])
    }

    /// The golden 2-hop frame (values hand-derived):
    /// Golden frame values (hand-derived): staged reserves `(476_259, 1_050)`
    /// after the target swaps 50 WETH into the pair; connector live reserves
    /// `(200_000, 900)`. Optimal integer 2-hop: input ~123 WETH, profit 56 wei
    /// (plateau 55..56); equal-reserves control: max profit 0.
    fn staged_frame() -> (Workspace, usize) {
        let mut s = Workspace::new();
        let p_id = admitted_v2(&mut s, P, 500_000, 1_000);
        let q_id = admitted_v2(&mut s, Q, 200_000, 900);
        // The target's exact effect: 50 WETH in on P.
        s.stage_v2_reserves(P, 476_259, 1_050, 2);
        // Cycle: WETH -> TOK at staged P (token1->token0, zfo=false),
        // TOK -> WETH at Q (token0->token1, zfo=true).
        let idx = declared_cycle(&mut s, p_id, q_id, false, true);
        (s, idx)
    }

    #[test]
    fn two_hop_exceeds_floor_and_matches_reference() {
        let (mut s, idx) = staged_frame();
        let res = s.evaluate(idx, U256::ZERO).expect("solvable");
        assert!(res.optimal_input > U256::ZERO);
        assert!(
            res.profit >= U256::from(55u8) && res.profit <= U256::from(56u8),
            "profit {} outside the golden plateau [55,56]",
            res.profit
        );
        assert!(
            res.optimal_input > U256::from(100u8) && res.optimal_input < U256::from(150u8),
            "optimal input {} far from the golden 123",
            res.optimal_input
        );
    }

    #[test]
    fn envelope_gate_skips_below_floor() {
        let (mut s, idx) = staged_frame();
        // Floor far above every feasible profit (bounded by the staged WETH
        // reserve, ~1e3): the envelope bound at that floor is definitive ->
        // skipped WITHOUT a walk. (The bound is sound-but-loose, so a floor
        // just above the observed optimum is not guaranteed to skip.)
        assert!(s.evaluate(idx, U256::from(1_000_000u64)).is_none());
    }

    #[test]
    fn equal_price_connector_yields_no_profit() {
        let mut s = Workspace::new();
        let p_id = admitted_v2(&mut s, P, 500_000, 1_000);
        s.stage_v2_reserves(P, 476_259, 1_050, 2);
        // Q priced identically to staged P: no arbitrage room (fees drain).
        let q_id = admitted_v2(&mut s, Q, 476_259, 1_050);
        let idx = declared_cycle(&mut s, p_id, q_id, false, true);
        let res = s.evaluate(idx, U256::ZERO);
        assert!(
            res.as_ref().is_none_or(|r| r.profit.is_zero()),
            "expected no profit, got {res:?}"
        );
    }

    #[test]
    fn orientation_flipped_path_hurts() {
        let (mut s, _) = staged_frame();
        // Deliberately wrong directions: the cycle drains on fees both ways.
        let idx = s.declare(&[PlanningHop::v2(1, true), PlanningHop::v2(2, false)]);
        let res = s.evaluate(idx, U256::ZERO);
        assert!(
            res.as_ref().is_none_or(|r| r.profit.is_zero()),
            "flipped cycle must not profit: {res:?}"
        );
    }

    #[test]
    fn unknown_pool_path_is_dropped() {
        let mut s = Workspace::new();
        let idx = s.declare(&[PlanningHop::v2(1, false), PlanningHop::v2(2, true)]);
        assert!(s.evaluate(idx, U256::ZERO).is_none());
    }

    #[test]
    fn golden_candidate_composes_to_execute_calldata() {
        use degenbot_executor::composers::EXECUTE_SELECTOR;
        // canonical order: TOK (0x..aa1) < WETH -> token0 = TOK
        let token0 = TOK;
        let candidate = LaneCandidate {
            hops: vec![
                BackrunHopRef {
                    pool_id: 1,
                    pool: P,
                    token0,
                    token1: WETH,
                    zfo: false,
                    family: LaneFamily::V2,
                },
                BackrunHopRef {
                    pool_id: 2,
                    pool: Q,
                    token0,
                    token1: WETH,
                    zfo: true,
                    family: LaneFamily::V2,
                },
            ],
            // The golden solve's numbers, via a scoped solver eval:
            optimal_input: 123,
            hop_outputs: vec![5_893_000, 1_235], // tok out + weth back (shape only)
            consumed_inputs: vec![123, 5_892_315],
            profit: 55,
        };
        let cd = build_candidate_calldata(&candidate, P, WETH, 1000).expect("composes");
        assert_eq!(&cd[0..4], &EXECUTE_SELECTOR[..]);
        // config = (1000 << 8) | 1 rides the head of the ABI tail; just
        // sanity the total size bounds (selector + 0x40 + words + bytes).
        assert!(cd.len() > 4 + 32 * 3 + 64);
    }

    /// A single V4→V2 lane candidate composes to `execute` calldata: the V4
    /// arm builds a real `V4HopInfo` (not the interim refusal) and the stream
    /// opens with a `V4_UNLOCK` wrapping a `V4_SWAP_COMPACT`.
    ///
    /// The V4 lead must consume WETH (or native) and the V2 tail return it —
    /// the only v4v2 cycle the shape admits — so the terminal is WETH and the
    /// path needs no leading self-fund byte.
    #[test]
    fn golden_v4_lane_composes_to_execute_calldata() {
        use degenbot_executor::composers::EXECUTE_SELECTOR;
        use degenbot_executor::encoders::{
            BEGIN_EXECUTION, CMD_SET_ADDRESS, CMD_V4_SWAP_COMPACT, CMD_V4_UNLOCK,
        };
        // A non-canonical manager: the composer must source the session's
        // PoolManager from the V4 hop, not the hardcoded mainnet const.
        const V4_MANAGER: Address = address!("000000000000000000000000000000000000c0fe");
        let candidate = LaneCandidate {
            hops: vec![
                BackrunHopRef {
                    pool_id: 1,
                    pool: V4_MANAGER,
                    token0: TOK,
                    token1: WETH,
                    // input currency1=WETH, output currency0=TOK
                    zfo: false,
                    family: LaneFamily::V4 {
                        fee: 500,
                        pool_id: B256::new([0xab; 32]),
                        tick_spacing: 10,
                        hooks: Address::ZERO,
                    },
                },
                BackrunHopRef {
                    pool_id: 2,
                    pool: Q,
                    token0: TOK,
                    token1: WETH,
                    // input token0=TOK, output token1=WETH
                    zfo: true,
                    family: LaneFamily::V2,
                },
            ],
            optimal_input: 1_000_000_000_000_000_000,
            hop_outputs: vec![1_000_000_000_000_000_000, 1_000_000_000_000_000_000],
            consumed_inputs: vec![1_000_000_000_000_000_000, 1_000_000_000_000_000_000],
            profit: 1_000_000_000_000_000_000,
        };
        let cd = compose_candidate(&candidate, P, WETH, 1000).expect("a V4 lane composes");
        assert_eq!(&cd[0..4], &EXECUTE_SELECTOR[..]);
        // Decode the `execute(bytes,uint256)` ABI tail: head slot 0 is the
        // offset (0x40) to the `bytes`, so the length word starts at 4+0x40.
        let mut len_word = [0u8; 32];
        len_word.copy_from_slice(&cd[68..100]);
        let cmds_len = usize::try_from(u64::try_from(U256::from_be_bytes(len_word)).expect("fits"))
            .expect("fits usize");
        let commands = &cd[100..100 + cmds_len];
        // Skip the address-table preprocessing (`SET_ADDRESS` × N) to the
        // `BEGIN_EXECUTION` separator, then read the first command opcode.
        let mut at = 0;
        while commands[at] == CMD_SET_ADDRESS {
            at += 21;
        }
        assert_eq!(commands[at], BEGIN_EXECUTION, "preprocessing ends at 0xFF");
        at += 1;
        // Ledger-only `SelfFund` emits no byte, so a V4-led path opens its
        // PoolManager unlock first.
        assert_eq!(
            commands[at], CMD_V4_UNLOCK,
            "the V4-led path opens its PoolManager unlock first"
        );
        assert_eq!(
            commands[at + 2],
            CMD_V4_SWAP_COMPACT,
            "the unlock's first inner command is the V4 swap"
        );
    }

    /// V4 hops naming different `PoolManager` singletons have no single
    /// sentinel to resolve, so the candidate is refused loudly.
    #[test]
    fn mixed_pool_managers_reject_loudly() {
        let v4_hop = |pool_id: u8, manager: Address| BackrunHopRef {
            pool_id: u64::from(pool_id),
            pool: manager,
            token0: TOK,
            token1: WETH,
            zfo: false,
            family: LaneFamily::V4 {
                fee: 500,
                pool_id: B256::new([pool_id; 32]),
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
        };
        let candidate = LaneCandidate {
            hops: vec![
                v4_hop(1, address!("000000000000000000000000000000000000c0fe")),
                v4_hop(2, address!("000000000000000000000000000000000000dead")),
            ],
            optimal_input: 1_000,
            hop_outputs: vec![990, 1_010],
            consumed_inputs: vec![1_000, 990],
            profit: 10,
        };
        assert_eq!(
            compose_candidate(&candidate, P, WETH, 1000),
            Err(ComposeReject::MixedPoolManagers)
        );
    }

    #[test]
    fn candidate_rejects_misaligned_hops() {
        let candidate = LaneCandidate {
            hops: vec![
                BackrunHopRef {
                    pool_id: 1,
                    pool: P,
                    token0: TOK,
                    token1: WETH,
                    zfo: false,
                    family: LaneFamily::V2,
                },
                BackrunHopRef {
                    pool_id: 2,
                    pool: Q,
                    token0: TOK,
                    token1: WETH,
                    zfo: true,
                    family: LaneFamily::V2,
                },
            ],
            optimal_input: 1,
            hop_outputs: vec![1],
            consumed_inputs: vec![1],
            profit: 1,
        };
        assert!(build_candidate_calldata(&candidate, P, WETH, 1000).is_none());
        assert_eq!(
            compose_candidate(&candidate, P, WETH, 1000),
            Err(ComposeReject::UnsupportedHopShape)
        );
    }

    #[test]
    fn compose_reject_names_amount_overflow() {
        let candidate = LaneCandidate {
            hops: vec![
                BackrunHopRef {
                    pool_id: 1,
                    pool: P,
                    token0: TOK,
                    token1: WETH,
                    zfo: false,
                    family: LaneFamily::V2,
                },
                BackrunHopRef {
                    pool_id: 2,
                    pool: Q,
                    token0: TOK,
                    token1: WETH,
                    zfo: true,
                    family: LaneFamily::V2,
                },
            ],
            optimal_input: u128::MAX,
            hop_outputs: vec![1, 1],
            consumed_inputs: vec![1, 1],
            profit: 1,
        };
        assert_eq!(
            compose_candidate(&candidate, P, WETH, 1000).err(),
            Some(ComposeReject::AmountExceedsUint96)
        );
    }
}

#[cfg(test)]
mod taxonomy_tests {
    //! ADR-059 D1 parity: the lane vocabulary projects from the pool taxonomy,
    //! and the lane tag drives both the solver hop engine and the discovery
    //! graph kind. The lagging (unsupported) legs are pinned here too.
    use super::{LaneFamily, LaneFamilyTag};
    use alloy::primitives::{Address, B256};
    use degenbot_pathfinding::PoolKind;
    use degenbot_pools::{
        BalanceVectorVariant, BinnedLiquidityVariant, ConcentratedLiquidityVariant, Identity,
        ReservePairVariant,
    };
    use degenbot_solvers::mixed::HopType;

    fn reserve_pair(variant: ReservePairVariant) -> Identity {
        Identity::ReservePair { variant, dex: None }
    }

    fn concentrated_liquidity(variant: ConcentratedLiquidityVariant) -> Identity {
        Identity::ConcentratedLiquidity { variant, dex: None }
    }

    fn balance_vector(variant: BalanceVectorVariant) -> Identity {
        Identity::BalanceVector { variant, dex: None }
    }

    #[test]
    fn tag_covers_every_data_carrying_lane_family() {
        assert_eq!(LaneFamily::V2.tag(), LaneFamilyTag::V2);
        assert_eq!(LaneFamily::V3 { fee: 500 }.tag(), LaneFamilyTag::V3);
        assert_eq!(
            LaneFamily::V4 {
                fee: 500,
                pool_id: B256::ZERO,
                tick_spacing: 10,
                hooks: Address::ZERO,
            }
            .tag(),
            LaneFamilyTag::V4
        );
    }

    #[test]
    fn lane_tag_drives_the_solver_engine_and_graph_kind() {
        assert_eq!(LaneFamilyTag::V2.hop_type(), HopType::V2);
        assert_eq!(LaneFamilyTag::V3.hop_type(), HopType::V3);
        assert_eq!(LaneFamilyTag::V4.hop_type(), HopType::V4);
        assert_eq!(PoolKind::from(LaneFamilyTag::V2), PoolKind::V2);
        assert_eq!(PoolKind::from(LaneFamilyTag::V3), PoolKind::V3);
        assert_eq!(PoolKind::from(LaneFamilyTag::V4), PoolKind::V4);
    }

    #[test]
    fn identity_projection_admits_only_the_lanes_the_arm_can_express() {
        assert_eq!(
            LaneFamilyTag::from_identity(&reserve_pair(ReservePairVariant::UniswapV2)),
            Some(LaneFamilyTag::V2)
        );
        assert_eq!(
            LaneFamilyTag::from_identity(&reserve_pair(ReservePairVariant::AerodromeV2 {
                stable: false
            })),
            Some(LaneFamilyTag::V2)
        );
        assert_eq!(
            LaneFamilyTag::from_identity(&reserve_pair(ReservePairVariant::AerodromeV2 {
                stable: true
            })),
            None
        );
        assert_eq!(
            LaneFamilyTag::from_identity(&concentrated_liquidity(
                ConcentratedLiquidityVariant::UniswapV3
            )),
            Some(LaneFamilyTag::V3)
        );
        assert_eq!(
            LaneFamilyTag::from_identity(&concentrated_liquidity(
                ConcentratedLiquidityVariant::UniswapV4
            )),
            Some(LaneFamilyTag::V4)
        );
        assert_eq!(
            LaneFamilyTag::from_identity(&balance_vector(BalanceVectorVariant::Curve)),
            None
        );
        assert_eq!(
            LaneFamilyTag::from_identity(&balance_vector(BalanceVectorVariant::BalancerWeighted)),
            None
        );
        assert_eq!(
            LaneFamilyTag::from_identity(&balance_vector(BalanceVectorVariant::BalancerStable)),
            None
        );
        assert_eq!(
            LaneFamilyTag::from_identity(&Identity::BinnedLiquidity {
                variant: BinnedLiquidityVariant::Lfj,
                dex: None,
            }),
            None,
            "the lane vocabulary is V2/V3/V4-only: binned liquidity must lag",
        );
    }
}
