//! The planning workspace: a strategy-scoped scratch registry over a PRIVATE
//! [`BotState`].
//!
//! A scope owns everything a planning cycle needs: admit pools with EXPLICIT
//! typed state ([`ExplicitPoolState`] — from frame replay, RPC reads, or
//! anywhere), stage deterministic state overlays, declare cycles
//! ([`PlanningHop`]), and envelope-gate them — then drop the whole scope.
//! Nothing the scope mutates is visible outside it, and nothing outside is
//! visible inside: there is no canonical-registry write path here at all,
//! because the scope's `BotState` IS the registry for its lifetime and only
//! this value holds it.
//!
//! Register-with-explicit-state decision: [`BotState::register_v2_pool`] and
//! [`BotState::register_v3_pool`] are already explicit-state admissions —
//! the typed params + state carry the FULL state (reserves / slot0 /
//! liquidity / tick map), the seed stamp rides the same payload, and
//! neither writes outside its own instance nor wires event lifecycle (a
//! registered pool only joins event machinery when the pump routes events
//! into THAT `BotState` instance; this scope's instance is unreachable from
//! there). Growing a second parallel admission path on `BotState` would
//! drift from its spec-bound validation and genesis-journal discipline, so
//! the workspace adds NO `BotState` API: [`Workspace::register_with_state`]
//! stamps the planning `seed_block` over the state's `update_block` and
//! calls the canonical registrations. Strategies needing admission shapes
//! beyond [`ExplicitPoolState`]'s variants extend the enum, not `BotState`.
//!
//! Solve intake is the engine's own: [`Workspace::evaluate`] resolves hops
//! through the same per-family projections (`super::resolve`) and solves
//! with the offline gate deps, so a workspace verdict and an engine verdict
//! for identical state are one verdict.

use alloy::primitives::{aliases::U112, Address, U256};
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_pools::v2_state::RegisterV2PoolParams;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_pools::v3_state::{PoolTickCoverage, RegisterV3PoolParams};
use degenbot_pools::v4_state::{RegisterV4PoolParams, V4PoolKey};
use degenbot_pools::TickInfo;
use degenbot_solvers::mixed::{HopType, MixedPoolRef, ResolvedMixedPath, SolvePathResult};
use degenbot_solvers::profit_envelope::GateDeps;

use super::resolve::{resolve_hops, HopProjectionCache};
use super::BotState;

/// The pool identity an explicit-state admission rides on: address + token
/// pair (canonical order). Everything stateful travels separately in
/// [`ExplicitPoolState`] so identity and state cannot be conflated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanningPoolParams {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
}

/// Provenance of a staged tick map: which arm produced it. `Db` and `Chain`
/// seeds carry an invariant (state precedence and coverage semantics) and can
/// be minted only inside `bot_core`; `Journal` is exact-replay post-state
/// truth the backrun journal admission owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickMapSource {
    /// `TickMapDb::fetch_liquidity_map` supplied the complete map
    /// (`Tracked`).
    Db,
    /// The sparse Chain bootstrap supplied one bitmap word (`Sparse`).
    Chain,
    /// Replayed post-frame journal tick words: exact-replay truth, not a
    /// fabricated ladder.
    Journal,
}

impl TickMapSource {
    /// The stable JSONL label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Db => "Db",
            Self::Chain => "Chain",
            Self::Journal => "Journal",
        }
    }
}

/// A staged tick map plus the freshness stamp and provenance that must travel
/// with it. `#[non_exhaustive]` seals construction to this crate: an external
/// strategy cannot fabricate a seed (nor claim Db/Chain provenance) by struct
/// literal. The `db`/`chain` constructors are additionally crate-private
/// because Db/Chain provenance carries invariant-bearing state precedence —
/// only `bot_core` provisioning may claim it. `journal` is public: a replayed
/// journal's post-state words are exact-replay truth, and the backrun journal
/// admission path owns them.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TickMapSeed {
    /// The per-tick liquidity cells (raw values; layout-agnostic).
    pub ticks: hashbrown::HashMap<i32, TickInfo>,
    /// Bitmap words fetched with the tick rows. These are provenance, not a
    /// value derived from rows: a tracked map must carry the exact words that
    /// established its completeness.
    pub bitmaps: hashbrown::HashMap<i32, U256>,
    /// The coverage tag the workspace registers with.
    pub coverage: PoolTickCoverage,
    /// The block the map is exact at (the liquidity clock).
    pub seed_block: u64,
    /// Which arm produced the map.
    pub source: TickMapSource,
}

impl TickMapSeed {
    /// A Db-arm seed. Crate-private: Db provenance is minted only by the
    /// ingress.
    #[must_use]
    pub(crate) fn db(
        ticks: hashbrown::HashMap<i32, TickInfo>,
        bitmaps: hashbrown::HashMap<i32, U256>,
        coverage: PoolTickCoverage,
        block: u64,
    ) -> Self {
        Self {
            ticks,
            bitmaps,
            coverage,
            seed_block: block,
            source: TickMapSource::Db,
        }
    }

    /// A Chain-arm seed. Crate-private: a sparse ladder is minted only by the
    /// ingress chain bootstrap.
    #[must_use]
    pub(crate) fn chain(
        ticks: hashbrown::HashMap<i32, TickInfo>,
        bitmaps: hashbrown::HashMap<i32, U256>,
        coverage: PoolTickCoverage,
        block: u64,
    ) -> Self {
        Self {
            ticks,
            bitmaps,
            coverage,
            seed_block: block,
            source: TickMapSource::Chain,
        }
    }

    /// A Journal-arm seed: exact-replay post-state tick words. Public so the
    /// backrun journal admission path can mint it without an RPC ladder.
    #[must_use]
    pub fn journal(
        ticks: hashbrown::HashMap<i32, TickInfo>,
        coverage: PoolTickCoverage,
        block: u64,
    ) -> Self {
        Self {
            ticks,
            bitmaps: hashbrown::HashMap::new(),
            coverage,
            seed_block: block,
            source: TickMapSource::Journal,
        }
    }
}

/// The typed pool state an admission carries — the value the fetch layer
/// PRODUCES and the scope consumes verbatim. No event-driven fill-in exists:
/// whatever is in here is the entire state the pool solves from.
#[derive(Debug, Clone)]
pub enum ExplicitPoolState {
    /// Reserves + per-direction fee fractions, e.g. the canonical 0.3%
    /// preset `(997, 1000)`. Reserves are registry-validated against the
    /// on-chain `uint112` width.
    V2 {
        reserve0: U112,
        reserve1: U112,
        fee_token0: (u64, u64),
        fee_token1: (u64, u64),
    },
    /// Slot0 + active liquidity + a caller-fetched tick map with its
    /// coverage tag (frame replay or a sparse bootstrap; the scope never
    /// fetches or guesses missing words — projections fail loudly instead).
    V3 {
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        fee: u32,
        tick_spacing: i32,
        /// The staged tick map: ticks + coverage + freshness stamp +
        /// provenance. Db/Chain provenance is minted only by the ingress;
        /// Journal by the backrun replay admission.
        seed: TickMapSeed,
        /// The fork's storage-slot family — the layout the producer READ;
        /// registration stores it on the identity so every later slot-index
        /// consumer agrees. Never defaulted (VERIFY2 T4).
        slot_layout: ClSlotLayout,
    },
    /// V4 CL state with its manager-keyed identity: the shared
    /// [`PlanningPoolParams`] carries the generic `address` + token pair,
    /// which map to the `PoolManager` and `currency0`/`currency1`; the bytes32
    /// `pool_id` and the key's fee/spacing/hooks are V4-only and ride here.
    /// `pool_key.hooks` and `hook_flags` are the caller's verbatim values, not
    /// re-derived.
    V4 {
        pool_id: V4PoolId,
        fee: u32,
        tick_spacing: i32,
        hooks: Address,
        hook_flags: u16,
        protocol_fee: u32,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        /// The staged tick map. Its `seed_block` is the V4 liquidity clock.
        seed: TickMapSeed,
    },
}

/// Why an explicit-state admission was refused. The variants carry the
/// registry's typed rejection verbatim: `AlreadyRegistered` means the
/// address is admitted twice in this scope (bookkeeping), `SpecViolation`
/// means the state itself violates the on-chain invariants (bad data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanningAdmissionError {
    V2(::degenbot_pools::v2_state::RegisterV2PoolError),
    V3(::degenbot_pools::v3_state::RegisterV3PoolError),
    V4(::degenbot_pools::v4_state::RegisterV4PoolError),
}

/// One declared hop: the family-tagged solver key of a cycle step. Cycle
/// order = hop order; the family tag picks the projection, the pool id the
/// admitted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanningHop {
    pub pool_id: u64,
    pub hop_type: HopType,
    pub zero_for_one: bool,
}

impl PlanningHop {
    /// A V2 hop (the dominant lane family).
    #[must_use]
    pub const fn v2(pool_id: u64, zero_for_one: bool) -> Self {
        Self {
            pool_id,
            hop_type: HopType::V2,
            zero_for_one,
        }
    }
}

/// A declared path: hops in cycle order.
#[derive(Debug, Clone)]
struct DeclaredPath {
    hops: Vec<PlanningHop>,
}

/// Why [`Workspace::evaluate_verdict`] refused a declared path: the typed
/// `None` of the envelope-gated solve. The frame pipeline traces this
/// verbatim, so the per-chain dark half (declared but not solved) is legible
/// without re-deriving a cause from a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathReject {
    /// The declared index is out of range for this scope.
    NoSuchPath,
    /// Hop resolution left `deficits` hops unsatisfied: an unadmitted pool id
    /// or explicit state the projection could not use. `reasons` carries the
    /// per-hop [`crate::bot_core::resolve::MissingHopReason`] labels in hop
    /// order so the per-chain trace names the cause, not just a count.
    UnusablePoolState {
        deficits: usize,
        reasons: Vec<&'static str>,
        /// The responsible pools' addresses (join for the per-hop labels;
        /// the parity check cannot derive them from the declared chain).
        pools: Vec<String>,
    },
    /// The profit-envelope gate skipped the walk: `min_profit` sits above the
    /// path's rigorous profit bound.
    NoEnvelopeProfit,
    /// The solver walked the path and found no profitable input.
    Unsolved,
}

impl PathReject {
    /// The stable JSONL label (the offline-review contract).
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::NoSuchPath => "no_such_path",
            Self::UnusablePoolState { .. } => "unusable_pool_state",
            Self::NoEnvelopeProfit => "no_envelope_profit",
            Self::Unsolved => "unsolved",
        }
    }
}

/// The evaluate pipeline's verdict shapes, for observability + the gate.
enum EvalVerbose {
    NoSuchPath,
    Invalid(Vec<(&'static str, u64)>),
    GateSkipped,
    Unsolved,
    Solved(SolvePathResult),
}

/// The planning scope. Dropping it drops the registry, the projection memo,
/// and every declared path — the scope's whole footprint is this value.
pub struct Workspace {
    state: BotState,
    cache: HopProjectionCache,
    paths: Vec<DeclaredPath>,
}

impl Workspace {
    /// The already-registered workspace pool id for `address`, if this scope
    /// admitted it. A frame's cycle set can route through one pool in several
    /// cycles; re-admission would refuse (`AlreadyRegistered`) and the caller
    /// would wrongly drop the cycle — reuse the id instead.
    #[must_use]
    pub fn pool_id_by_address(&self, address: &Address) -> Option<u64> {
        self.state.pool_id_by_address(address)
    }

    /// A fresh, empty scope.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: BotState::new(),
            cache: HopProjectionCache::new(),
            paths: Vec::new(),
        }
    }

    /// Admit a pool with EXPLICIT typed state. `seed_block` is the planning
    /// clock: it OVERWRITES the state's scalar `update_block` so every pool
    /// in the scope shares one timestamp reference regardless of where the
    /// state was fetched. (Tick-map entries keep their provenance stamps —
    /// only the scalar clock is scope-stamped.) The admission joins only
    /// this scope's registry; no other scope or the canonical state can see
    /// or be seen by it.
    ///
    /// # Errors
    ///
    /// Carries the registry's typed rejection verbatim — a duplicate address
    /// in this scope, or a spec-bound violation in the explicit state.
    pub fn register_with_state(
        &mut self,
        params: PlanningPoolParams,
        state: ExplicitPoolState,
        seed_block: u64,
    ) -> Result<u64, PlanningAdmissionError> {
        match state {
            ExplicitPoolState::V2 {
                reserve0,
                reserve1,
                fee_token0,
                fee_token1,
            } => self
                .state
                .register_v2_pool(&RegisterV2PoolParams {
                    address: params.address,
                    token0: params.token0,
                    token1: params.token1,
                    reserve0,
                    reserve1,
                    fee_token0,
                    fee_token1,
                    update_block: seed_block,
                    ..RegisterV2PoolParams::default()
                })
                .map_err(PlanningAdmissionError::V2),
            ExplicitPoolState::V3 {
                sqrt_price_x96,
                liquidity,
                tick,
                fee,
                tick_spacing,
                seed,
                slot_layout,
            } => self
                .state
                .register_v3_pool(&RegisterV3PoolParams {
                    address: params.address,
                    token0: params.token0,
                    token1: params.token1,
                    fee,
                    tick_spacing,
                    sqrt_price_x96,
                    liquidity,
                    tick,
                    tick_data: seed.ticks,
                    update_block: seed_block,
                    tick_data_block: None,
                    coverage: seed.coverage,
                    fetcher: None,
                    factory: Address::ZERO,
                    deployer: Address::ZERO,
                    init_hash: alloy::primitives::B256::ZERO,
                    slot_layout,
                })
                .map_err(PlanningAdmissionError::V3),
            ExplicitPoolState::V4 {
                pool_id,
                fee,
                tick_spacing,
                hooks,
                hook_flags,
                protocol_fee,
                sqrt_price_x96,
                liquidity,
                tick,
                seed,
            } => self
                .state
                .register_v4_pool(&RegisterV4PoolParams {
                    pool_manager: params.address,
                    pool_id,
                    pool_key: V4PoolKey {
                        currency0: params.token0,
                        currency1: params.token1,
                        fee,
                        tick_spacing,
                        hooks,
                    },
                    hook_flags,
                    protocol_fee,
                    sqrt_price_x96,
                    liquidity,
                    tick,
                    tick_data: seed.ticks,
                    update_block: seed_block,
                    tick_data_block: Some(seed.seed_block),
                    coverage: seed.coverage,
                    fetcher: None,
                })
                .map_err(PlanningAdmissionError::V4),
        }
    }

    /// Stage an explicit V2 state overlay onto an admitted pool (the target
    /// frame's post-swap reserves, or any replay/RPC proven state): the
    /// canonical live-path mutation, so journals and the state nonce behave
    /// exactly as the engine's. No-op when the address was never admitted or
    /// reserves exceed uint112 (lanes drop the frame silently; callers that
    /// require the overlay check [`Workspace::v2_reserves`]).
    pub fn stage_v2_reserves(
        &mut self,
        address: Address,
        reserve0: u128,
        reserve1: u128,
        block: u64,
    ) {
        let Ok(r0) = U112::try_from(reserve0) else {
            return;
        };
        let Ok(r1) = U112::try_from(reserve1) else {
            return;
        };
        let _ = self.state.apply_v2_sync(address, r0, r1, block);
    }

    /// The V2 reserves + update stamp the scope sees at `pool_id` — the
    /// scope's narrow observability window (grading + tests). `None` when
    /// the id is unadmitted or not a V2 pool.
    #[must_use]
    pub fn v2_reserves(&self, pool_id: u64) -> Option<(U256, U256, u64)> {
        self.state.v2_snapshot(pool_id)
    }

    /// Declare a cycle from family-tagged hops (cycle order = hop order).
    /// Hops referencing unadmitted pools stay invalid for this scope
    /// (dropped loudly at resolve, never admitted lazily). Returns the
    /// path's index for [`Workspace::evaluate`].
    pub fn declare(&mut self, hops: &[PlanningHop]) -> usize {
        self.paths.push(DeclaredPath {
            hops: hops.to_vec(),
        });
        self.paths.len() - 1
    }

    /// Envelope-gated solve of a declared path at `min_profit` (the gas
    /// floor + safety margin). `None` = gate skipped or unsolvable.
    pub fn evaluate(&mut self, path_idx: usize, min_profit: U256) -> Option<SolvePathResult> {
        self.evaluate_verdict(path_idx, min_profit).ok()
    }

    /// The typed form of [`Workspace::evaluate`]: the reject cause survives
    /// the `Option` collapse so callers can trace WHY a chain did not solve.
    ///
    /// # Errors
    ///
    /// [`PathReject`] carrying the `evaluate_verbose` verdict verbatim.
    pub fn evaluate_verdict(
        &mut self,
        path_idx: usize,
        min_profit: U256,
    ) -> Result<SolvePathResult, PathReject> {
        match self.evaluate_verbose(path_idx, min_profit) {
            EvalVerbose::Solved(r) => Ok(r),
            EvalVerbose::NoSuchPath => Err(PathReject::NoSuchPath),
            EvalVerbose::Invalid(deficits) => {
                let reasons = deficits.iter().map(|(label, _)| *label).collect();
                let pools = deficits
                    .iter()
                    .filter_map(|(_, id)| self.state.pool_address_of(*id))
                    .map(|a| format!("0x{}", alloy::hex::encode(a)))
                    .collect();
                Err(PathReject::UnusablePoolState {
                    deficits: deficits.len(),
                    reasons,
                    pools,
                })
            }
            EvalVerbose::GateSkipped => Err(PathReject::NoEnvelopeProfit),
            EvalVerbose::Unsolved => Err(PathReject::Unsolved),
        }
    }

    /// Diagnostic: the full evaluate path for one declared index — resolve
    /// facts + solve/gate verdict. Observability only.
    #[must_use]
    pub fn resolve_debug(&mut self, path_idx: usize) -> String {
        match self.evaluate_verbose(path_idx, U256::ZERO) {
            EvalVerbose::NoSuchPath => String::from("no-such-path"),
            EvalVerbose::Invalid(deficits) => {
                format!("invalid deficits={} reasons={deficits:?}", deficits.len())
            }
            EvalVerbose::GateSkipped => String::from("gate-skipped"),
            EvalVerbose::Unsolved => String::from("unsolved (solver returned None)"),
            EvalVerbose::Solved(r) => format!(
                "solved: input={} profit={} hops_out={:?}",
                r.optimal_input, r.profit, r.hop_outputs
            ),
        }
    }

    /// Number of declared paths in the scope.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.paths.len()
    }

    /// The full evaluate verdict for one declared index. Private: the
    /// tri-state split matters only to `evaluate`/`resolve_debug`.
    fn evaluate_verbose(&mut self, path_idx: usize, min_profit: U256) -> EvalVerbose {
        let Some(hops) = self.paths.get(path_idx).cloned() else {
            return EvalVerbose::NoSuchPath;
        };
        let refs: Vec<MixedPoolRef> = hops
            .hops
            .iter()
            .map(|h| MixedPoolRef {
                hop_type: h.hop_type,
                pool_key: h.pool_id,
                zero_for_one: h.zero_for_one,
            })
            .collect();
        let mut resolved = ResolvedMixedPath::default();
        let deficits = resolve_hops(&self.state, &refs, &mut resolved, &self.cache, None, true);
        if !deficits.is_empty() || !resolved.valid {
            return EvalVerbose::Invalid(
                deficits
                    .iter()
                    .map(|d| (d.reason.short_label(), d.pool_key))
                    .collect(),
            );
        }

        let outcome = degenbot_solvers::mixed::solve_path_with_min_profit(
            &resolved,
            min_profit,
            &GateDeps::offline(),
        );
        match outcome.result {
            Some(r) => EvalVerbose::Solved(r),
            // Gate-skipped vs solver-None are indistinguishable at this
            // seam; the walk stats tell them apart (a gate skip reports no
            // walk steps at a non-zero bound).
            None if outcome.stats.sims == 0 => EvalVerbose::GateSkipped,
            None => EvalVerbose::Unsolved,
        }
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "planning-scope fixtures are spec-valid by construction"
)]
mod tests {
    use super::*;
    use alloy::primitives::{address, U128};

    const TOK: Address = address!("0000000000000000000000000000000000000aa1");
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    /// P: the affected pool (the target swapped WETH -> TOK here).
    const P: Address = address!("000000000000000000000000000000000000b001");
    /// Q: the external connector pool (cheaper TOK than staged P).
    const Q: Address = address!("000000000000000000000000000000000000b002");
    /// The V4 `PoolManager` (the V4 "pool address") + its bytes32 pool id.
    const PM: Address = address!("000000000000000000000000000000000000c001");
    const V4_POOL_ID: [u8; 32] = [0xab; 32];
    const SEED: u64 = 1;

    fn admitted_v2(w: &mut Workspace, addr: Address, r0: u128, r1: u128) -> u64 {
        w.register_with_state(
            PlanningPoolParams {
                address: addr,
                token0: TOK,
                token1: WETH,
            },
            ExplicitPoolState::V2 {
                reserve0: U112::from(r0),
                reserve1: U112::from(r1),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
            },
            SEED,
        )
        .expect("admits")
    }

    /// Two initialized ticks straddling the current tick — enough for a
    /// non-empty walk in either direction.
    fn v4_ticks() -> hashbrown::HashMap<i32, TickInfo> {
        let mut t = hashbrown::HashMap::new();
        t.insert(
            120,
            TickInfo {
                liquidity_gross: U128::from(10_000),
                liquidity_net: 5_000i128,
                block: 0,
            },
        );
        t.insert(
            -120,
            TickInfo {
                liquidity_gross: U128::from(8_000),
                liquidity_net: -4_000i128,
                block: 0,
            },
        );
        t
    }

    /// Admit a V4 pool at tick 0 with caller-supplied tick data (the sandbox
    /// fetches nothing: `Sparse` coverage carries exactly this map).
    fn admitted_v4(w: &mut Workspace, liquidity: u128, protocol_fee: u32) -> u64 {
        w.register_with_state(
            PlanningPoolParams {
                address: PM,
                token0: TOK,
                token1: WETH,
            },
            ExplicitPoolState::V4 {
                pool_id: V4_POOL_ID,
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
                hook_flags: 0,
                protocol_fee,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity,
                tick: 0,
                seed: TickMapSeed::journal(v4_ticks(), PoolTickCoverage::Sparse, SEED),
            },
            SEED,
        )
        .expect("admits")
    }

    /// The scope's V2 cycle for pools `p`, `q`: WETH -> TOK at P (zfo=false),
    /// TOK -> WETH at Q (zfo=true).
    fn golden_cycle(p: u64, q: u64) -> Vec<PlanningHop> {
        vec![PlanningHop::v2(p, false), PlanningHop::v2(q, true)]
    }

    /// Behavioral parity through the Workspace's own public surface (the
    /// golden 2-hop frame): P pre-staged with `500_000 / 1_000` wei-reserves, target swaps 50
    /// WETH in -> `(476_259, 1_050)`; live connector Q `(200_000, 900)` sold
    /// against it. Hand-derived: optimal input ~123 WETH, profit 56 wei
    /// (plateau 55..56).
    #[test]
    fn workspace_v2_cycle_matches_golden_reference() {
        let mut w = Workspace::new();
        let p_id = admitted_v2(&mut w, P, 500_000, 1_000);
        let q_id = admitted_v2(&mut w, Q, 200_000, 900);
        // Registration rides BotState id semantics (1-based).
        assert_eq!((p_id, q_id), (1, 2), "admission key order");

        // The seed_block stamp overwrites the state's update_block.
        let (_, _, stamp) = w.v2_reserves(p_id).expect("P readable");
        assert_eq!(stamp, SEED, "the planning clock stamps admission");

        w.stage_v2_reserves(P, 476_259, 1_050, 2);
        let idx = w.declare(&golden_cycle(p_id, q_id));
        let res = w.evaluate(idx, U256::ZERO).expect("solvable");
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

    /// The envelope gate: a floor far above every feasible profit (bounded
    /// by the staged WETH reserve, ~1e3) skips WITHOUT a walk.
    #[test]
    fn workspace_gate_skips_below_floor() {
        let mut w = Workspace::new();
        let p_id = admitted_v2(&mut w, P, 500_000, 1_000);
        let q_id = admitted_v2(&mut w, Q, 200_000, 900);
        w.stage_v2_reserves(P, 476_259, 1_050, 2);
        let idx = w.declare(&golden_cycle(p_id, q_id));
        assert!(w.evaluate(idx, U256::from(1_000_000u64)).is_none());
    }

    /// The unusable-state reject carries per-hop deficit labels, not just a
    /// count: the per-chain trace names WHY each hop could not project, so
    /// a declared-but-unsolved chain is legible without re-deriving a cause.
    #[test]
    #[expect(clippy::panic)]
    fn evaluate_verdict_carries_deficit_reasons() {
        let mut w = Workspace::new();
        let p_id = admitted_v2(&mut w, P, 500_000, 1_000);
        let bad = w.declare(&golden_cycle(p_id, 999));
        match w.evaluate_verdict(bad, U256::ZERO) {
            Err(PathReject::UnusablePoolState {
                deficits,
                reasons,
                pools,
            }) => {
                assert_eq!(deficits, 1, "one dead hop in a two-hop cycle");
                assert_eq!(reasons, vec!["missing_state"]);
                assert!(pools.is_empty(), "an unadmitted id has no address");
            }
            other => panic!("expected unusable_pool_state with reasons, got {other:?}"),
        }
    }

    /// Overlapping cycle sets must reuse a scope pool instead of re-admitting
    /// it: the id lookup survives a register and back-solves the funnel's
    /// `AlreadyRegistered` cycle-drop.
    #[test]
    fn workspace_pool_id_lookup_survives_registration() {
        let mut w = Workspace::new();
        let p_id = admitted_v2(&mut w, P, 500_000, 1_000);
        assert_eq!(w.pool_id_by_address(&P), Some(p_id));
        assert_eq!(w.pool_id_by_address(&Address::new([0xEE; 20])), None);
    }

    /// The typed reject surface: the gate skip and the unresolvable-path
    /// verdict keep distinct causes for the per-chain trace.
    #[test]
    fn evaluate_verdict_names_the_reject_cause() {
        let mut w = Workspace::new();
        let p_id = admitted_v2(&mut w, P, 500_000, 1_000);
        let q_id = admitted_v2(&mut w, Q, 200_000, 900);
        w.stage_v2_reserves(P, 476_259, 1_050, 2);

        let gated = w.declare(&golden_cycle(p_id, q_id));
        assert_eq!(
            w.evaluate_verdict(gated, U256::from(1_000_000u64)).err(),
            Some(PathReject::NoEnvelopeProfit)
        );

        // A hop naming an unadmitted pool leaves deficits: usable-state reject.
        let bad = w.declare(&golden_cycle(p_id, 999));
        assert_eq!(
            w.evaluate_verdict(bad, U256::ZERO).err().map(|r| r.label()),
            Some("unusable_pool_state")
        );
    }

    /// Isolation: two scopes diverge the SAME pool address (both get pool
    /// id 0 — each scope numbers its own registry), and neither observes
    /// the other's state through any surface.
    #[test]
    fn divergent_workspaces_for_the_same_pool_do_not_observe_each_other() {
        let mut scope_a = Workspace::new();
        let mut scope_b = Workspace::new();

        let a_id = admitted_v2(&mut scope_a, P, 500_000, 1_000);
        let b_id = admitted_v2(&mut scope_b, P, 9_000_000, 7_000);
        assert_eq!(a_id, b_id, "each scope numbers its own registry from 0");

        // Divergent staging on the same address.
        scope_a.stage_v2_reserves(P, 476_259, 1_050, 2);
        scope_b.stage_v2_reserves(P, 4_000_000, 9_800, 2);

        let (a_r0, a_r1, _) = scope_a.v2_reserves(a_id).expect("A sees its own");
        let (b_r0, b_r1, _) = scope_b.v2_reserves(b_id).expect("B sees its own");
        assert_eq!((a_r0, a_r1), (U256::from(476_259u64), U256::from(1_050u64)));
        assert_eq!(
            (b_r0, b_r1),
            (U256::from(4_000_000u64), U256::from(9_800u64))
        );
    }

    /// Isolation at the verdict: identical declared cycles against the two
    /// divergent states grade differently — the cheap-connector cycle
    /// profits in A; B's divergent state makes the same cycle drain.
    #[test]
    fn divergent_workspaces_grade_the_same_cycle_differently() {
        let build = |staged: (u128, u128), connector: (u128, u128)| {
            let mut w = Workspace::new();
            let p_id = admitted_v2(&mut w, P, staged.0, staged.1);
            let q_id = admitted_v2(&mut w, Q, connector.0, connector.1);
            let idx = w.declare(&golden_cycle(p_id, q_id));
            (w, idx)
        };

        // A: staged P below connector Q — the golden arbitrage.
        let (mut a, a_idx) = build((476_259, 1_050), (200_000, 900));
        assert!(a.evaluate(a_idx, U256::ZERO).is_some(), "A profits");

        // B: same declared cycle, divergent state — staged P matches Q's
        // price, so the cycle drains on fees (max profit 0, nothing to take).
        let (mut b, b_idx) = build((476_259, 1_050), (476_259, 1_050));
        let b_res = b.evaluate(b_idx, U256::ZERO);
        assert!(
            b_res.is_none_or(|r| r.profit.is_zero()),
            "B does not profit"
        );
    }

    /// Scope leak: pool ids are scope-local AND the scope's writes never
    /// touch a coexisting canonical `BotState` — admission, staging, declare,
    /// evaluate, and drop all leave the outside registry bit-identical.
    #[test]
    fn workspace_scope_never_touches_a_coexisting_canonical_state() {
        let mut canonical = BotState::new();
        let canonical_id = canonical
            .register_v2_pool(&::degenbot_pools::v2_state::RegisterV2PoolParams {
                address: P,
                token0: TOK,
                token1: WETH,
                reserve0: U112::from(313_131u64),
                reserve1: U112::from(212_121u64),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                update_block: 7,
                ..::degenbot_pools::v2_state::RegisterV2PoolParams::default()
            })
            .expect("canonical P");
        let canonical_before = canonical.v2_snapshot(canonical_id);
        let count_before = canonical.v2_pool_count();

        {
            let mut w = Workspace::new();
            let p_id = admitted_v2(&mut w, P, 500_000, 1_000);
            w.stage_v2_reserves(P, 476_259, 1_050, 2);
            let q_id = admitted_v2(&mut w, Q, 200_000, 900);
            let idx = w.declare(&golden_cycle(p_id, q_id));
            let _ = w.evaluate(idx, U256::ZERO);
            // The scope's ids never index the canonical registry.
            assert_ne!(
                canonical.v2_snapshot(p_id),
                Some((U256::from(476_259u64), U256::from(1_050u64), 2))
            );
            assert_ne!(
                canonical.v2_snapshot(q_id),
                Some((U256::from(200_000u64), U256::from(900u64), SEED))
            );
        } // scope dropped

        assert_eq!(canonical.v2_pool_count(), count_before, "no pool leaked in");
        assert_eq!(
            canonical.v2_snapshot(canonical_id),
            canonical_before,
            "canonical state untouched"
        );
    }

    /// V4 admission is explicit-state: the caller's tick map, coverage,
    /// protocol fee, and hook flags land verbatim; the planning clock stamps
    /// the scalar block and the liquidity clock defaults to it.
    #[test]
    fn workspace_admits_v4_pool_with_explicit_state() {
        let mut w = Workspace::new();
        let id = admitted_v4(&mut w, 1_000_000_000, 1_234);
        let pool = w.state.get_v4_pool(id).expect("V4 state admitted");
        assert_eq!(pool.sqrt_price_x96, U256::from(1u128) << 96);
        assert_eq!(pool.liquidity, 1_000_000_000);
        assert_eq!(pool.tick, 0);
        assert_eq!(pool.protocol_fee, 1_234, "protocol fee carried verbatim");
        assert_eq!(pool.coverage, PoolTickCoverage::Sparse);
        assert_eq!(
            pool.update_block, SEED,
            "the planning clock stamps admission"
        );
        assert_eq!(
            pool.tick_data_block, SEED,
            "tick_data_block defaults to the seed block"
        );
        assert_eq!(
            pool.tick_data.len(),
            2,
            "the caller's tick map, nothing more"
        );

        let identity = w.state.get_v4_identity(id).expect("V4 identity");
        assert_eq!(identity.pool_manager, PM);
        assert_eq!(identity.pool_id, V4_POOL_ID);
        assert_eq!(identity.pool_key.tick_spacing, 10);
        assert_eq!(identity.pool_key.fee, 500);
        assert_eq!(identity.pool_key.hooks, Address::ZERO);
    }

    /// A declared V4 + V2 cycle must reach the solver: admission and V4
    /// projection both succeed, so the verdict is a solve or an economics
    /// miss — never an unusable-pool-state reject.
    #[test]
    fn workspace_v4_cycle_reaches_the_solver() {
        let mut w = Workspace::new();
        let v4_id = admitted_v4(&mut w, 1_000_000_000, 0);
        let p_id = admitted_v2(&mut w, P, 500_000, 1_000);
        let idx = w.declare(&[
            PlanningHop {
                pool_id: v4_id,
                hop_type: HopType::V4,
                zero_for_one: true,
            },
            PlanningHop::v2(p_id, false),
        ]);
        // The golden V4 + V2 cycle solves through the sandbox: a V4 admission
        // or projection failure would surface as `UnusablePoolState`, so an
        // exact solved result is the anti-regression pin for both.
        let solved = w
            .evaluate_verdict(idx, U256::ZERO)
            .expect("V4 admission + projection reach the solver");
        assert_eq!(solved.optimal_input, U256::from(21_394u64));
        assert_eq!(solved.profit, U256::from(456_202u64));
        assert_eq!(
            solved.hop_outputs,
            vec![U256::from(21_382u64), U256::from(477_596u64)]
        );
    }
}
