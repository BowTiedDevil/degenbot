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
use degenbot_pools::v2_state::RegisterV2PoolParams;
use degenbot_pools::v3_state::{PoolTickCoverage, RegisterV3PoolParams};
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
        tick_data: hashbrown::HashMap<i32, TickInfo>,
        coverage: PoolTickCoverage,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathReject {
    /// The declared index is out of range for this scope.
    NoSuchPath,
    /// Hop resolution left `deficits` hops unsatisfied: an unadmitted pool id
    /// or explicit state the projection could not use.
    UnusablePoolState { deficits: usize },
    /// The profit-envelope gate skipped the walk: `min_profit` sits above the
    /// path's rigorous profit bound.
    NoEnvelopeProfit,
    /// The solver walked the path and found no profitable input.
    Unsolved,
}

impl PathReject {
    /// The stable JSONL label (the offline-review contract).
    #[must_use]
    pub const fn label(self) -> &'static str {
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
    Invalid(usize),
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
                tick_data,
                coverage,
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
                    tick_data,
                    update_block: seed_block,
                    tick_data_block: None,
                    coverage,
                    ..RegisterV3PoolParams::default()
                })
                .map_err(PlanningAdmissionError::V3),
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
            EvalVerbose::Invalid(deficits) => Err(PathReject::UnusablePoolState { deficits }),
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
            EvalVerbose::Invalid(deficits) => format!("invalid deficits={deficits}"),
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
            return EvalVerbose::Invalid(deficits.len());
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
    use alloy::primitives::address;

    const TOK: Address = address!("0000000000000000000000000000000000000aa1");
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    /// P: the affected pool (the target swapped WETH -> TOK here).
    const P: Address = address!("000000000000000000000000000000000000b001");
    /// Q: the external connector pool (cheaper TOK than staged P).
    const Q: Address = address!("000000000000000000000000000000000000b002");
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
            w.evaluate_verdict(bad, U256::ZERO)
                .err()
                .map(PathReject::label),
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
}
