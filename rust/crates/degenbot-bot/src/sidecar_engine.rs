//! Sidecar connector-solve engine (epic DFYDYI, task B3): the standalone
//! engine-backed path solver replacing the hand-composed mono-pool probe.
//!
//! Owns a PRIVATE [`BotState`] (the FORK-1 isolation: nothing here touches the
//! mainline pump's state) + a CL hop-projection cache, admits V2 pools on
//! demand, stages the target's effect with the exact `v2_post_target` overlay
//! (applied through the same journal the live path uses), declares two-hop
//! paths, and evaluates them with the solvers' envelope-gated mixed solve
//! (V2-V2 = the closed-form integer Mobius; no hand-composed pricing
//! anywhere).
//!
//! Frame flow: `admit_v2` (affected + connectors) -> `stage_target_swap` ->
//! `declare` the 2-hop family -> `evaluate` with `min_profit = gas floor`.

use alloy::primitives::{Address, U256};
use degenbot_pools::v2_state::RegisterV2PoolParams;
use degenbot_solvers::mixed::{HopType, MixedPoolRef, ResolvedMixedPath, SolvePathResult};
use degenbot_solvers::profit_envelope::GateDeps;

use crate::bot_core::resolve::{resolve_hops, HopProjectionCache};
use crate::bot_core::BotState;

/// One admitted V2 pool: identity + the LIVE reserves the caller fetched
/// (the adapter keeps this narrow; reserves come from `fetch_v2_reserves`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarV2Pool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: u128,
    pub reserve1: u128,
}

/// A declared path: mixed pool refs in hop order.
#[derive(Debug, Clone)]
pub struct DeclaredPath {
    pub hops: Vec<MixedPoolRef>,
}

/// The standalone frame solver (private state; FORK-1 isolated).
pub struct SidecarSolver {
    state: BotState,
    cache: HopProjectionCache,
    paths: Vec<DeclaredPath>,
}

impl SidecarSolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: BotState::new(),
            cache: HopProjectionCache::new(),
            paths: Vec::new(),
        }
    }

    /// Admit a V2 pool with the canonical 0.3% fee preset. Per-exchange fee
    /// variants enter with the CL/fee-lane work (B2 follow-up).
    ///
    /// # Errors
    ///
    /// Registration rejections (spec-bound reserve violations).
    pub fn admit_v2(&mut self, p: &SidecarV2Pool) -> Result<u64, String> {
        let Ok(reserve0) = p.reserve0.try_into() else {
            return Err(format!("reserve0 out of uint112: {}", p.reserve0));
        };
        let Ok(reserve1) = p.reserve1.try_into() else {
            return Err(format!("reserve1 out of uint112: {}", p.reserve1));
        };
        let params = RegisterV2PoolParams {
            address: p.address,
            token0: p.token0,
            token1: p.token1,
            reserve0,
            reserve1,
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            update_block: 1,
            ..RegisterV2PoolParams::default()
        };
        self.state
            .register_v2_pool(&params)
            .map_err(|e| format!("v2 admission failed: {e:?}"))
    }

    /// Stage the target's effect onto an admitted pool: journal + apply the
    /// post-target reserves through the canonical live-path mutation (the
    /// staging is exact for constant-product via `v2_post_target` upstream).
    /// No-op when the address was never admitted or reserves exceed uint112.
    pub fn stage_target_swap(
        &mut self,
        address: Address,
        reserve0: u128,
        reserve1: u128,
        block: u64,
    ) {
        let Ok(r0) = reserve0.try_into() else { return };
        let Ok(r1) = reserve1.try_into() else { return };
        let _applied = self.state.apply_v2_sync(address, r0, r1, block);
    }

    /// Declare a path from admitted pool ids + directions; returns its index.
    /// Hops referencing unknown pools make the path invalid for this frame
    /// (dropped loudly at resolve, not admitted lazily here).
    #[must_use]
    pub fn declare(&mut self, hops: &[(u64, bool)]) -> usize {
        let refs = hops
            .iter()
            .map(|&(pool_id, zfo)| MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: pool_id,
                zero_for_one: zfo,
            })
            .collect();
        self.paths.push(DeclaredPath { hops: refs });
        self.paths.len() - 1
    }

    /// Envelope-gated solve of a declared path at `min_profit` (the gas
    /// floor + a safety margin). `None` = gate skipped or unsolvable.
    pub fn evaluate(&mut self, path_idx: usize, min_profit: U256) -> Option<SolvePathResult> {
        let refs = self.paths.get(path_idx)?.hops.clone();
        let mut resolved = ResolvedMixedPath::default();
        let deficits = resolve_hops(&self.state, &refs, &mut resolved, &self.cache, None, true);
        if !deficits.is_empty() || !resolved.valid {
            return None;
        }
        degenbot_solvers::mixed::solve_path_with_min_profit(
            &resolved,
            min_profit,
            &GateDeps::offline(),
        )
        .result
    }

    #[must_use]
    pub fn path_count(&self) -> usize {
        self.paths.len()
    }
}

impl Default for SidecarSolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Observe-lane grade for one frame's connector solve (the B3/B4 seam).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LaneGrade {
    pub connectors: usize,
    pub paths_evaluated: usize,
    pub best_profit: U256,
    pub best_input: U256,
}

/// Per-frame lane inputs (bundled to keep the lane signature narrow).
pub struct ConnectorLaneCtx<'a> {
    pub index: &'a crate::sidecar_paths::V2ConnectorIndex,
    /// Frame token addresses -> DB token ids (the bin owns the cache).
    pub ids: &'a std::collections::HashMap<Address, u64>,
    pub leg: &'a degenbot_decoders::target_classifier::SwapLeg,
    /// The affected pair's live `getReserves` in PAIR order (token0, token1).
    pub pair_reserves: (u128, u128),
    pub cap: usize,
    pub gas_floor_wei: U256,
}

impl SidecarSolver {
    /// The frame's connector lane: stage the affected pairing, admit DB
    /// connectors at live reserves, declare + evaluate the 2-hop family,
    /// return the grade. Orientation is derived from V2 canonical token
    /// order (token0 = min-address), never assumed. The exact-sim oracle
    /// still gates anything this labels profitable (B4 wires the calldata).
    pub async fn run_connector_lane(
        &mut self,
        provider: &degenbot_rpc::provider::AlloyProvider,
        ctx: &ConnectorLaneCtx<'_>,
    ) -> LaneGrade {
        use degenbot_decoders::target_classifier::PoolProtocol;

        let leg = ctx.leg;
        let ids = ctx.ids;
        let cap = ctx.cap;
        let gas_floor_wei = ctx.gas_floor_wei;
        let pair_reserves = ctx.pair_reserves;
        let index = ctx.index;
        let mut grade = LaneGrade::default();
        if leg.protocol != PoolProtocol::V2 || leg.hops != 0 {
            return grade;
        }
        let Some(pool_addr) = leg.pool else {
            return grade;
        };
        let (Some(t_in), Some(t_out)) = (leg.token_in, leg.token_out) else {
            return grade;
        };
        let Some(amount_in_u256) = leg.amount_in else {
            return grade;
        };
        let Ok(amount_in) = u128::try_from(amount_in_u256) else {
            return grade;
        };
        if amount_in == 0 {
            return grade;
        }
        let weth = crate::sidecar_solve::weth();
        let (Some(&weth_id), Some(&t_in_id), Some(&t_out_id)) =
            (ids.get(&weth), ids.get(&t_in), ids.get(&t_out))
        else {
            return grade;
        };
        // WETH-denominated 2-hop family only (the mono-pool lane's domain).
        let (tok, tok_id, _target_bought_tok) = if weth_id == t_in_id {
            (t_out, t_out_id, true)
        } else if weth_id == t_out_id {
            (t_in, t_in_id, false)
        } else {
            return grade;
        };
        let Some(p_edge) = index.edge_by_address(pool_addr) else {
            return grade;
        };
        let (t0_id, t1_id) = (p_edge.token0_id, p_edge.token1_id);
        if !((t0_id == tok_id && t1_id == weth_id) || (t0_id == weth_id && t1_id == tok_id)) {
            return grade;
        }

        // V2 canonical order: token0 = the smaller address.
        let (token0, token1) = if tok < weth { (tok, weth) } else { (weth, tok) };
        let tok_is_t0 = tok == token0;

        // Stage the target's exact effect on P (in the leg's (in, out) view,
        // mapped back to the pair's canonical order); both drift directions
        // are declared per connector -- the target's own direction alone
        // does not fix which side of P dislocated against the connector, and
        // the envelope gate prunes the fee-drain direction for free.
        let in_is_t0 = leg.token_in == Some(token0);
        let (r_in, r_out) = if in_is_t0 {
            (pair_reserves.0, pair_reserves.1)
        } else {
            (pair_reserves.1, pair_reserves.0)
        };
        let Some((p_id, p_weth_side_zfo, p_tok_side_zfo)) = self.stage_affected_pair(
            (pool_addr, token0, token1),
            tok_is_t0,
            amount_in,
            in_is_t0,
            (r_in, r_out),
        ) else {
            return grade;
        };

        let cands = index.connectors(tok_id, weth_id, p_edge.pool_id, cap);
        grade.connectors = cands.len();
        for (c_edge, _tok_flag) in cands {
            let Some((cr0, cr1)) =
                crate::sidecar_solve::fetch_v2_reserves(provider, c_edge.address).await
            else {
                continue;
            };
            let Ok(c_id) = self.admit_v2(&SidecarV2Pool {
                address: c_edge.address,
                token0,
                token1,
                reserve0: cr0,
                reserve1: cr1,
            }) else {
                continue;
            };
            // Connector drives the opposite leg of the cycle.
            // Direction 1: buy TOK on the staged P, sell to the connector.
            let idx1 = self.declare(&[(p_id, p_weth_side_zfo), (c_id, tok_is_t0)]);
            // Direction 2: buy TOK on the connector, sell into staged P.
            let idx2 = self.declare(&[(c_id, !tok_is_t0), (p_id, p_tok_side_zfo)]);
            for idx in [idx1, idx2] {
                if let Some(res) = self.evaluate(idx, gas_floor_wei) {
                    grade.paths_evaluated += 1;
                    if res.profit > grade.best_profit {
                        grade.best_profit = res.profit;
                        grade.best_input = res.optimal_input;
                    }
                }
            }
        }
        grade
    }

    /// Stage the affected pair for a frame: exact `v2_post_target` overlay on
    /// the leg's (in, out) reserves, mapped back to canonical pair order, then
    /// admission. Returns `(pool_id, zfo for the P WETH-side hop, zfo for the
    /// P TOK-side hop)`.
    fn stage_affected_pair(
        &mut self,
        pair: (Address, Address, Address),
        tok_is_t0: bool,
        amount_in: u128,
        in_is_t0: bool,
        reserves: (u128, u128),
    ) -> Option<(u64, bool, bool)> {
        use crate::bot_core::post_target::{v2_post_target, V2FeeParams};
        let fee = V2FeeParams {
            gamma_numer: 997,
            fee_denom: 1000,
        };
        let overlay = v2_post_target(reserves.0, reserves.1, fee, amount_in).ok()?;
        let (staged_r0, staged_r1) = if in_is_t0 {
            (overlay.new_reserve_in, overlay.new_reserve_out)
        } else {
            (overlay.new_reserve_out, overlay.new_reserve_in)
        };
        let p_id = self
            .admit_v2(&SidecarV2Pool {
                address: pair.0,
                token0: pair.1,
                token1: pair.2,
                reserve0: staged_r0,
                reserve1: staged_r1,
            })
            .ok()?;
        Some((p_id, !tok_is_t0, tok_is_t0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const TOK: Address = address!("0000000000000000000000000000000000000aa1");
    /// P: the affected pool (the target swapped WETH -> TOK here).
    const P: Address = address!("000000000000000000000000000000000000b001");
    /// Q: the DB-discovered connector pool (cheaper TOK than staged P).
    const Q: Address = address!("000000000000000000000000000000000000b002");

    fn pool_conf(
        address_: Address,
        token0: Address,
        token1: Address,
        r0: u128,
        r1: u128,
    ) -> SidecarV2Pool {
        SidecarV2Pool {
            address: address_,
            token0,
            token1,
            reserve0: r0,
            reserve1: r1,
        }
    }

    /// The golden 2-hop frame (values hand-derived):
    /// P (TOK,WETH) live (500_000, 1_000); the target swaps 50 WETH -> TOK;
    /// exact staged reserves (476_259, 1_050). Q (TOK,WETH) live (200_000, 900).
    /// Optimal integer 2-hop: input ~123 WETH, profit 56 wei (plateau 55..56);
    /// equal-reserves control: max profit 0.
    fn staged_frame() -> (SidecarSolver, usize) {
        let mut s = SidecarSolver::new();
        let p_id = s
            .admit_v2(&pool_conf(P, TOK, WETH, 500_000, 1_000))
            .expect("P admits");
        let q_id = s
            .admit_v2(&pool_conf(Q, TOK, WETH, 200_000, 900))
            .expect("Q admits");
        // The target's exact effect (v2_post_target: 50 WETH in on P).
        s.stage_target_swap(P, 476_259, 1_050, 2);
        // Cycle: WETH -> TOK at staged P (token1->token0, zfo=false),
        // TOK -> WETH at Q (token0->token1, zfo=true).
        let idx = s.declare(&[(p_id, false), (q_id, true)]);
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
        let mut s = SidecarSolver::new();
        let p_id = s
            .admit_v2(&pool_conf(P, TOK, WETH, 500_000, 1_000))
            .expect("P");
        s.stage_target_swap(P, 476_259, 1_050, 2);
        // Q priced identically to staged P: no arbitrage room (fees drain).
        let q_id = s
            .admit_v2(&pool_conf(Q, TOK, WETH, 476_259, 1_050))
            .expect("Q");
        let idx = s.declare(&[(p_id, false), (q_id, true)]);
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
        let p_id = 1;
        let q_id = 2;
        let idx = s.declare(&[(p_id, true), (q_id, false)]);
        let res = s.evaluate(idx, U256::ZERO);
        assert!(
            res.as_ref().is_none_or(|r| r.profit.is_zero()),
            "flipped cycle must not profit: {res:?}"
        );
    }

    #[test]
    fn unknown_pool_path_is_dropped() {
        let mut s = SidecarSolver::new();
        let idx = s.declare(&[(1, false), (2, true)]);
        assert!(s.evaluate(idx, U256::ZERO).is_none());
    }
}
