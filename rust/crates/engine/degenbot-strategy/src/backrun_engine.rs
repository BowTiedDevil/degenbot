//! Backrun connector-solve engine (epic DFYDYI, task B3): the
//! engine-backed path solver replacing the hand-composed mono-pool probe.
//!
//! Solves over a PRIVATE planning [`Workspace`] (the FORK-1 isolation: the
//! scope's scratch state never touches the mainline pump's registry), admits
//! pools with explicit typed state, declares two-hop paths, and evaluates
//! them with the solvers' envelope-gated mixed solve (V2-V2 = the closed-form
//! integer Mobius; no hand-composed pricing anywhere). Pool state arrives via
//! the frame pipeline's replay-extracted post-states (`admit_v2`,
//! `admit_v3_replay`) or the ingress-routed cold-hop ladder
//! (`admit_v3_full`); discovery over the DB connector index is the pipeline's
//! job.

use alloy::primitives::{Address, B256, U256};
use degenbot_pathfinding::PoolKind;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_pools::{ConcentratedLiquidityVariant, Identity, ReservePairVariant, TickInfo};
use degenbot_solvers::mixed::SolvePathResult;

use degenbot_bot::bot_core::executor_hop::{V2FeePair, V2FeeRefusal, V2Fees};
use degenbot_bot::bot_core::planning::{
    ExplicitPoolState, PlanningHop, PlanningPoolParams, Workspace,
};
use degenbot_bot::bot_core::pool_ingress::{IngressV3Params, IngressV4Params, PoolIngress};

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
    /// Direction-specific discovered fees, including typed refusals.
    pub fees: V2FeePair,
}

/// Why a V2 pool could not enter solver admission.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum V2AdmissionError {
    #[error("discovered V2 fee is unusable: {0}")]
    Fee(#[from] V2FeeRefusal),
    #[error("reserve0 out of uint112: {0}")]
    Reserve0Width(u128),
    #[error("reserve1 out of uint112: {0}")]
    Reserve1Width(u128),
    #[error("V2 workspace registration failed: {0}")]
    Registration(String),
}

/// The standalone frame solver: the backrun lane's driver shell over a
/// planning [`Workspace`] (FORK-1 isolation — the scope's scratch state
/// never touches the mainline pump's registry). Admission, staging,
/// declaration, and evaluation delegate to the workspace; the lane owns the
/// slot0/liquidity reads and live reserves that PRODUCE the explicit state,
/// while the V3 tick map comes from the ingress (`Db → Chain`).
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

    /// Admit a V2 pool with the discovered directional fees. Fee refusal is
    /// resolved before any workspace mutation.
    ///
    /// # Errors
    ///
    /// Returns a typed fee/reserve refusal or the workspace registration
    /// rejection.
    pub fn admit_v2(&mut self, p: &BackrunV2Pool) -> Result<u64, V2AdmissionError> {
        let fees = p.fees.resolve()?;
        let Ok(reserve0) = p.reserve0.try_into() else {
            return Err(V2AdmissionError::Reserve0Width(p.reserve0));
        };
        let Ok(reserve1) = p.reserve1.try_into() else {
            return Err(V2AdmissionError::Reserve1Width(p.reserve1));
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
                    fee_token0: fees.token0.retained_fraction(),
                    fee_token1: fees.token1.retained_fraction(),
                },
                1,
            )
            .map_err(|e| V2AdmissionError::Registration(format!("{e:?}")))
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

/// Why a cold-connector V3 hop could not be admitted: the ingress decline
/// names the refused step (`slot0` fetch/width, the Db arm, the Chain-arm
/// tick-map bootstrap, or registration), so the per-hop JSONL trace
/// distinguishes a network/spec failure from a registration refusal (an
/// `AlreadyRegistered` duplicate is a different beast from a dead
/// archive-node call and must not share one opaque label).
pub use degenbot_bot::bot_core::pool_ingress::IngressDecline as V3LadderReject;

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
    V2 {
        fees: V2Fees,
    },
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
            Self::V2 { .. } => LaneFamilyTag::V2,
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
    /// The workspace declaration index that identifies this solved path.
    pub path_id: u64,
    pub hops: Vec<BackrunHopRef>,
    pub optimal_input: u128,
    pub hop_outputs: Vec<u128>,
    pub consumed_inputs: Vec<u128>,
    /// The solver's closed-form profit in the quote asset (wei).
    pub profit: u128,
}

impl BackrunSolver {
    /// Admit a replayed V3 post-state through the policy-enforcing ingress.
    /// The ingress owns Db/Chain staging, overlay merge, bitmap provenance,
    /// verification, and registration; this method only carries the typed
    /// replay facts and the workspace boundary.
    ///
    /// # Errors
    ///
    /// Returns the ingress's typed decline without registering a partial seed.
    #[expect(
        clippy::too_many_arguments,
        reason = "replay identity + post-state + ingress describe distinct layers"
    )]
    pub async fn admit_v3_replay(
        &mut self,
        address: Address,
        token0: Address,
        token1: Address,
        fee: u32,
        tick_spacing: i32,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        overlay: hashbrown::HashMap<i32, TickInfo>,
        seed_block: u64,
        slot_layout: ClSlotLayout,
        ingress: &PoolIngress,
    ) -> Result<u64, V3LadderReject> {
        ingress
            .admit_v3_replay(
                &mut self.ws,
                IngressV3Params {
                    address,
                    token0,
                    token1,
                    fee,
                    tick_spacing,
                    sqrt_price_x96,
                    liquidity,
                    tick,
                    slot_layout,
                },
                overlay,
                seed_block,
            )
            .await
    }

    /// Admit a replayed V4 post-state through the policy-enforcing ingress.
    /// The ingress owns Db→Chain staging, Db-to-head backfill, bitmap merge,
    /// verification, and registration; this method carries only typed replay
    /// facts and the workspace boundary.
    ///
    /// # Errors
    ///
    /// Returns the ingress's typed decline without registering a partial seed.
    #[expect(
        clippy::too_many_arguments,
        reason = "replay identity + post-state + ingress describe distinct layers"
    )]
    pub async fn admit_v4_replay(
        &mut self,
        manager: Address,
        state_view: Option<Address>,
        token0: Address,
        token1: Address,
        pool_id: B256,
        fee: u32,
        tick_spacing: i32,
        hooks: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        overlay: hashbrown::HashMap<i32, TickInfo>,
        seed_block: u64,
        ingress: &PoolIngress,
    ) -> Result<u64, V3LadderReject> {
        ingress
            .admit_v4_replay(
                &mut self.ws,
                IngressV4Params {
                    manager,
                    state_view,
                    pool_id,
                    token0,
                    token1,
                    fee,
                    tick_spacing,
                    hooks,
                    sqrt_price_x96,
                    liquidity,
                    tick,
                },
                overlay,
                seed_block,
            )
            .await
    }

    /// Register a V3 pool at HEAD through the ingress, optionally OVERRIDING
    /// `sqrt_price_x96` with the post-target staged price (the
    /// V3-affected-pool path). The slot0/liquidity scalars are read here
    /// through the caller's `slot_layout`; the tick map is the ingress's
    /// `Db → Chain` responsibility — this lane never reads a tick word.
    ///
    /// # Errors
    ///
    /// [`IngressDecline`] naming the refused staging or registration step.
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
        ingress: &PoolIngress,
    ) -> Result<u64, V3LadderReject> {
        let (sqrt_price_x96, tick, liquidity) =
            degenbot_rpc::abi::fetch_v3_slot0_liquidity(provider, &address, None)
                .await
                .map_err(|e| V3LadderReject::Slot0Fetch(e.to_string()))?;
        let liquidity = u128::try_from(liquidity).map_err(|_| V3LadderReject::Slot0Width)?;
        let tick_i32 = i32::try_from(tick).map_err(|_| V3LadderReject::Slot0Width)?;
        ingress
            .admit_v3_verified(
                &mut self.ws,
                IngressV3Params {
                    address,
                    token0,
                    token1,
                    fee,
                    tick_spacing,
                    sqrt_price_x96: sqrt_override.unwrap_or(sqrt_price_x96),
                    liquidity,
                    tick: tick_i32,
                    slot_layout,
                },
                head,
            )
            .await
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "golden-reference tests: admitted fixtures are spec-valid by construction"
)]
mod tests {
    use super::*;
    use crate::backrun_strategy::backrun_encode_options;
    use crate::cmd_executor_adapter::{CmdExecutorAdapter, CmdExecutorDecline, CmdExecutorOutcome};
    use crate::execution_context::ExecutionContext;
    use crate::project_candidate;
    use alloy::primitives::{address, aliases::U112};
    use degenbot_execution::{solve_result::HopDescriptor, SolveResult};

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
            V3LadderReject::TickMapFetch("revert".into()).stage(),
            "admit-v3-tick-map-fetch"
        );
        assert_eq!(
            V3LadderReject::Db("database is locked".into()).stage(),
            "admit-v3-db"
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

    fn v2_fees() -> V2Fees {
        V2FeePair::from_discovered(Some(3), Some(3), Some(1_000))
            .resolve()
            .expect("valid fixture fee")
    }

    #[test]
    fn v2_admission_typed_refuses_missing_and_unrepresentable_fees_before_registration() {
        let mut solver = BackrunSolver::new();
        let result = solver.admit_v2(&BackrunV2Pool {
            address: P,
            token0: TOK,
            token1: WETH,
            reserve0: 500_000,
            reserve1: 1_000,
            fees: degenbot_bot::bot_core::executor_hop::V2FeePair::from_discovered(
                Some(3),
                Some(3),
                Some(0),
            ),
        });
        assert_eq!(
            result,
            Err(V2AdmissionError::Fee(
                degenbot_bot::bot_core::executor_hop::V2FeeRefusal::ZeroDenominator
            ))
        );
        let missing = solver.admit_v2(&BackrunV2Pool {
            address: Q,
            token0: TOK,
            token1: WETH,
            reserve0: 200_000,
            reserve1: 900,
            fees: V2FeePair::missing(),
        });
        assert_eq!(missing, Err(V2AdmissionError::Fee(V2FeeRefusal::Missing)));
        assert_eq!(
            solver.registered_pool_id(&P),
            None,
            "the refused fee must not enter the workspace"
        );
        assert_eq!(solver.registered_pool_id(&Q), None);
    }

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
            path_id: 17,
            hops: vec![
                BackrunHopRef {
                    pool_id: 1,
                    pool: P,
                    token0,
                    token1: WETH,
                    zfo: false,
                    family: LaneFamily::V2 { fees: v2_fees() },
                },
                BackrunHopRef {
                    pool_id: 2,
                    pool: Q,
                    token0,
                    token1: WETH,
                    zfo: true,
                    family: LaneFamily::V2 { fees: v2_fees() },
                },
            ],
            // The golden solve's numbers, via a scoped solver eval:
            optimal_input: 123,
            hop_outputs: vec![5_893_000, 1_235], // tok out + weth back (shape only)
            consumed_inputs: vec![123, 5_892_315],
            profit: 55,
        };
        let (path, result) = project_candidate(&candidate);
        let outcome = CmdExecutorAdapter::new(ExecutionContext::new(
            P,
            address!("000000000004444c5dc75cb358380d2e3de08a90"),
            WETH,
        ))
        .compose(&path, &result, backrun_encode_options(1_000));
        let CmdExecutorOutcome::Encoded(cd) = outcome else {
            panic!("golden candidate encodes")
        };
        assert_eq!(&cd[0..4], &EXECUTE_SELECTOR[..]);
        // config = (1000 << 8) | 1 rides the head of the ABI tail; just
        // sanity the total size bounds (selector + 0x40 + words + bytes).
        assert!(cd.len() > 4 + 32 * 3 + 64);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the parity test keeps both public encoder pipelines visible"
    )]
    #[test]
    fn settlement_and_backrun_project_v2_to_the_same_executor_bytes() {
        use crate::execution_context::ExecutionContext;
        use degenbot_bot::bot_core::{BotState, RegisterV2PoolParams};
        use degenbot_solvers::mixed::{HopType, MixedPoolRef};

        let mut core = BotState::new();
        let p_id = core
            .register_v2_pool(&RegisterV2PoolParams {
                address: P,
                token0: TOK,
                token1: WETH,
                reserve0: U112::from(500_000),
                reserve1: U112::from(1_000),
                fee_token0: (997, 1_000),
                fee_token1: (997, 1_000),
                ..Default::default()
            })
            .expect("registers settlement P");
        let q_id = core
            .register_v2_pool(&RegisterV2PoolParams {
                address: Q,
                token0: TOK,
                token1: WETH,
                reserve0: U112::from(200_000),
                reserve1: U112::from(900),
                fee_token0: (997, 1_000),
                fee_token1: (997, 1_000),
                ..Default::default()
            })
            .expect("registers settlement Q");
        let settlement_path = degenbot_bot::arb_engine::path_info::build_path_info(
            &core,
            &[
                MixedPoolRef {
                    hop_type: HopType::V2,
                    pool_key: p_id,
                    zero_for_one: false,
                },
                MixedPoolRef {
                    hop_type: HopType::V2,
                    pool_key: q_id,
                    zero_for_one: true,
                },
            ],
        )
        .expect("settlement projection succeeds");
        let amounts = (123, vec![5_893_000, 1_235], vec![123, 5_892_315]);
        let settlement_result = SolveResult {
            path_id: 17,
            hop_count: settlement_path.hops.len(),
            optimal_input: U256::from(amounts.0),
            hop_outputs: amounts.1.iter().copied().map(U256::from).collect(),
            consumed_inputs: amounts.2.iter().copied().map(U256::from).collect(),
            net_profit: U256::from(55),
            hop_descriptors: settlement_path
                .hops
                .iter()
                .map(HopDescriptor::from_hop_info)
                .collect(),
        };
        let adapter = CmdExecutorAdapter::new(ExecutionContext::new(
            P,
            address!("000000000004444c5dc75cb358380d2e3de08a90"),
            WETH,
        ));
        let CmdExecutorOutcome::Encoded(settlement_call) = adapter.compose(
            &settlement_path,
            &settlement_result,
            backrun_encode_options(1_000),
        ) else {
            panic!("settlement execute call encodes")
        };

        let candidate = LaneCandidate {
            path_id: 17,
            hops: vec![
                BackrunHopRef {
                    pool_id: p_id,
                    pool: P,
                    token0: TOK,
                    token1: WETH,
                    zfo: false,
                    family: LaneFamily::V2 { fees: v2_fees() },
                },
                BackrunHopRef {
                    pool_id: q_id,
                    pool: Q,
                    token0: TOK,
                    token1: WETH,
                    zfo: true,
                    family: LaneFamily::V2 { fees: v2_fees() },
                },
            ],
            optimal_input: amounts.0,
            hop_outputs: amounts.1,
            consumed_inputs: amounts.2,
            profit: 55,
        };
        let (backrun_path, backrun_result) = project_candidate(&candidate);
        let CmdExecutorOutcome::Encoded(backrun_call) = adapter.compose(
            &backrun_path,
            &backrun_result,
            backrun_encode_options(1_000),
        ) else {
            panic!("backrun execute call encodes")
        };

        assert_eq!(settlement_call, backrun_call);
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
            path_id: 17,
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
                    family: LaneFamily::V2 { fees: v2_fees() },
                },
            ],
            optimal_input: 1_000_000_000_000_000_000,
            hop_outputs: vec![1_000_000_000_000_000_000, 1_000_000_000_000_000_000],
            consumed_inputs: vec![1_000_000_000_000_000_000, 1_000_000_000_000_000_000],
            profit: 1_000_000_000_000_000_000,
        };
        let (path, result) = project_candidate(&candidate);
        let outcome = CmdExecutorAdapter::new(ExecutionContext::new(P, V4_MANAGER, WETH)).compose(
            &path,
            &result,
            backrun_encode_options(1_000),
        );
        let CmdExecutorOutcome::Encoded(cd) = outcome else {
            panic!("V4 lane encodes")
        };
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
            path_id: 17,
            hops: vec![
                v4_hop(1, address!("000000000000000000000000000000000000c0fe")),
                v4_hop(2, address!("000000000000000000000000000000000000dead")),
            ],
            optimal_input: 1_000,
            hop_outputs: vec![990, 1_010],
            consumed_inputs: vec![1_000, 990],
            profit: 10,
        };
        let (path, result) = project_candidate(&candidate);
        let context = ExecutionContext::new(
            P,
            address!("000000000000000000000000000000000000c0fe"),
            WETH,
        );
        assert_eq!(
            CmdExecutorAdapter::new(context)
                .compose(&path, &result, backrun_encode_options(1_000),),
            CmdExecutorOutcome::Declined(CmdExecutorDecline::MixedPoolManagers)
        );
    }

    #[test]
    fn candidate_rejects_misaligned_hops() {
        let candidate = LaneCandidate {
            path_id: 17,
            hops: vec![
                BackrunHopRef {
                    pool_id: 1,
                    pool: P,
                    token0: TOK,
                    token1: WETH,
                    zfo: false,
                    family: LaneFamily::V2 { fees: v2_fees() },
                },
                BackrunHopRef {
                    pool_id: 2,
                    pool: Q,
                    token0: TOK,
                    token1: WETH,
                    zfo: true,
                    family: LaneFamily::V2 { fees: v2_fees() },
                },
            ],
            optimal_input: 1,
            hop_outputs: vec![1],
            consumed_inputs: vec![1],
            profit: 1,
        };
        let (path, result) = project_candidate(&candidate);
        assert_eq!(
            CmdExecutorAdapter::new(ExecutionContext::new(
                P,
                address!("000000000004444c5dc75cb358380d2e3de08a90"),
                WETH,
            ))
            .compose(&path, &result, backrun_encode_options(1_000)),
            CmdExecutorOutcome::Declined(CmdExecutorDecline::UnsupportedHopShape)
        );
    }

    #[test]
    fn compose_reject_names_amount_overflow() {
        let candidate = LaneCandidate {
            path_id: 17,
            hops: vec![
                BackrunHopRef {
                    pool_id: 1,
                    pool: P,
                    token0: TOK,
                    token1: WETH,
                    zfo: false,
                    family: LaneFamily::V2 { fees: v2_fees() },
                },
                BackrunHopRef {
                    pool_id: 2,
                    pool: Q,
                    token0: TOK,
                    token1: WETH,
                    zfo: true,
                    family: LaneFamily::V2 { fees: v2_fees() },
                },
            ],
            optimal_input: u128::MAX,
            hop_outputs: vec![1, 1],
            consumed_inputs: vec![1, 1],
            profit: 1,
        };
        let (path, result) = project_candidate(&candidate);
        assert_eq!(
            CmdExecutorAdapter::new(ExecutionContext::new(
                P,
                address!("000000000004444c5dc75cb358380d2e3de08a90"),
                WETH,
            ))
            .compose(&path, &result, backrun_encode_options(1_000)),
            CmdExecutorOutcome::Declined(CmdExecutorDecline::AmountExceedsUint96)
        );
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod taxonomy_tests {
    //! ADR-059 D1 parity: the lane vocabulary projects from the pool taxonomy,
    //! and the lane tag drives both the solver hop engine and the discovery
    //! graph kind. The lagging (unsupported) legs are pinned here too.
    use super::{LaneFamily, LaneFamilyTag};
    use alloy::primitives::{Address, B256};
    use degenbot_bot::bot_core::executor_hop::{V2FeePair, V2Fees};
    use degenbot_pathfinding::PoolKind;
    use degenbot_pools::{
        BalanceVectorVariant, BinnedLiquidityVariant, ConcentratedLiquidityVariant, Identity,
        ReservePairVariant,
    };
    use degenbot_solvers::mixed::HopType;

    fn v2_fees() -> V2Fees {
        V2FeePair::from_discovered(Some(3), Some(3), Some(1_000))
            .resolve()
            .expect("valid fixture fee")
    }

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
        assert_eq!(LaneFamily::V2 { fees: v2_fees() }.tag(), LaneFamilyTag::V2);
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
