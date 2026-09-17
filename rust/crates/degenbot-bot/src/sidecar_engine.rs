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

/// The evaluate pipeline's tri-state verdict plus the solved result.
enum EvalVerbose {
    NoSuchPath,
    Invalid(usize),
    GateSkipped,
    Unsolved,
    Solved(SolvePathResult),
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
    /// Declare a path from executable hop refs (the declare side of
    /// [`SidecarHopRef`] -- cycle order is hop order).
    #[must_use]
    pub fn declare_hops(&mut self, hops: &[SidecarHopRef]) -> usize {
        let refs = hops
            .iter()
            .map(|h| MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: h.pool_id,
                zero_for_one: h.zfo,
            })
            .collect();
        self.paths.push(DeclaredPath { hops: refs });
        self.paths.len() - 1
    }

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

    /// Diagnostic: the full evaluate path for one declared index -- resolve
    /// facts + the solve/gate verdict. Standalone + e2e observability only.
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

    /// Envelope-gated solve of a declared path at `min_profit` (the gas
    /// floor + a safety margin). `None` = gate skipped or unsolvable.
    pub fn evaluate(&mut self, path_idx: usize, min_profit: U256) -> Option<SolvePathResult> {
        match self.evaluate_verbose(path_idx, min_profit) {
            EvalVerbose::Solved(r) => Some(r),
            _ => None,
        }
    }

    /// The evaluate pipeline with a tri-state verdict for observability.
    fn evaluate_verbose(&mut self, path_idx: usize, min_profit: U256) -> EvalVerbose {
        let Some(refs) = self.paths.get(path_idx).map(|p| p.hops.clone()) else {
            return EvalVerbose::NoSuchPath;
        };
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

/// One executable hop of a lane candidate (executor-composer input). Both
/// the declared solver key (`pool_id`) and the composer identity (`pool`)
/// ride together so a declared cycle and its executable form cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarHopRef {
    pub pool_id: u64,
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub zfo: bool,
}

/// Build both cycles of a (staged P, connector C) pairing. Hops are the
/// executable form themselves -- declaration and composition share these
/// objects, so orientation cannot drift between the two surfaces.
fn connector_cycles(
    p_id: u64,
    pool_p: Address,
    c_id: u64,
    pool_c: Address,
    tokens: (Address, Address),
    tok_is_t0: bool,
) -> Vec<Vec<SidecarHopRef>> {
    let hop = |pool_id: u64, pool: Address, zfo: bool| SidecarHopRef {
        pool_id,
        pool,
        token0: tokens.0,
        token1: tokens.1,
        zfo,
    };
    // P: WETH -> USDC (token1 -> token0) and P: USDC -> WETH (token0 -> token1).
    // Cycle 1: buy USDC on the market connector, sell into the staged P.
    // Cycle 2: buy USDC on the staged P, sell into the connector. The
    // envelope gate prunes the fee-drain direction per pair for free.
    let buy_on_c = hop(c_id, pool_c, !tok_is_t0);
    let sell_into_p = hop(p_id, pool_p, tok_is_t0);
    let buy_on_p = hop(p_id, pool_p, !tok_is_t0);
    let sell_into_c = hop(c_id, pool_c, tok_is_t0);
    vec![vec![buy_on_c, sell_into_p], vec![buy_on_p, sell_into_c]]
}

/// The best executable path a lane found: everything the composer + exact
/// sim need (epic DFYDYI B4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneCandidate {
    pub hops: Vec<SidecarHopRef>,
    pub optimal_input: u128,
    pub hop_outputs: Vec<u128>,
    pub consumed_inputs: Vec<u128>,
    /// The solver's closed-form profit in the quote asset (wei).
    pub profit: u128,
}

/// Observe-lane grade for one frame's connector solve (the B3/B4 seam).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaneGrade {
    pub connectors: usize,
    pub paths_evaluated: usize,
    /// Connectors whose live-reserve admission failed (skipped for frame).
    pub admit_failures: usize,
    /// Direction-pairs declared (both drift directions count).
    pub paths_declared: usize,
    pub best_profit: U256,
    pub best_input: U256,
    /// The best candidate when one cleared the gas floor.
    pub best: Option<LaneCandidate>,
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
        let Some((p_id, _p_weth_side_zfo, _p_tok_side_zfo)) = self.stage_affected_pair(
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
            let Some(c_id) = self
                .admit_connector(provider, c_edge.address, token0, token1)
                .await
            else {
                grade.admit_failures += 1;
                continue;
            };
            // Both cycles of the (staged P, connector C) pairing, best kept.
            // Hops are built explicitly per cycle so the executable form and
            // the declared form are the SAME objects (identity cannot drift)
            // -- the earlier (p-side zfo, c-side zfo) tuple encoding
            // mis-assigned a column once; explicit hops cannot mis-encode.
            let cycles = connector_cycles(
                p_id,
                pool_addr,
                c_id,
                c_edge.address,
                (token0, token1),
                tok_is_t0,
            );
            self.eval_cycles(&mut grade, gas_floor_wei, cycles);
        }
        grade
    }

    /// Declare each candidate cycle (hop lists in traversal order), evaluate
    /// envelope-gated, and keep the best result on `grade`.
    fn eval_cycles(
        &mut self,
        grade: &mut LaneGrade,
        gas_floor_wei: U256,
        cycles: Vec<Vec<SidecarHopRef>>,
    ) {
        for hops in cycles {
            let idx = self.declare_hops(&hops);
            grade.paths_declared += 1;
            let Some(res) = self.evaluate(idx, gas_floor_wei) else {
                continue;
            };
            grade.paths_evaluated += 1;
            if res.profit <= grade.best_profit {
                continue;
            }
            grade.best_profit = res.profit;
            grade.best_input = res.optimal_input;
            grade.best = Some(LaneCandidate {
                hops,
                optimal_input: res.optimal_input.to::<u128>(),
                hop_outputs: res.hop_outputs.iter().map(|v| v.to::<u128>()).collect(),
                consumed_inputs: res.consumed_inputs.iter().map(|v| v.to::<u128>()).collect(),
                profit: res.profit.to::<u128>(),
            });
        }
    }

    /// Admit a connector pool at its live reserves. `None` on fetch or
    /// uint112-overflow failure (the connector is skipped for the frame).
    async fn admit_connector(
        &mut self,
        provider: &degenbot_rpc::provider::AlloyProvider,
        address: Address,
        token0: Address,
        token1: Address,
    ) -> Option<u64> {
        let (r0, r1) = crate::sidecar_solve::fetch_v2_reserves(provider, address).await?;
        self.admit_v2(&SidecarV2Pool {
            address,
            token0,
            token1,
            reserve0: r0,
            reserve1: r1,
        })
        .ok()
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

/// The composed candidate bundle: the executor's `execute(commands, config)`
/// calldata, ready for the exact-sim oracle + submission (DFYDYI B4).
///
/// Funding = in-path flash (the leading pool's swap callback extends entry
/// credit, repaid by the path), capture = executor custody, and the bid is
/// the executor's native coinbase bribe: config `bribe_bips` on the TRUE
/// profit delta (check mode 1 = WETH+ETH), recipient 0 = coinbase, with
/// WETH auto-unwrap built into the bribe payout.
///
/// `None` when the composer rejects the shape (unsupported family / uint96
/// overflow on a hop amount) -- the frame then falls back to observe.
#[must_use]
pub fn build_candidate_calldata(
    candidate: &LaneCandidate,
    executor: Address,
    weth: Address,
    bribe_bips: u16,
) -> Option<alloy::primitives::Bytes> {
    use degenbot_executor::composers::{
        encode_cmd_stream, encode_execute_call, EncodeContext, EncodeOptions, EncodeRequest,
        HopInfo, V2HopInfo,
    };
    use degenbot_executor::grammar_ledger::{Bribe, FundingSource, ProfitCapture};

    if candidate.hops.len() < 2
        || candidate.hop_outputs.len() != candidate.hops.len()
        || candidate.consumed_inputs.len() != candidate.hops.len()
    {
        return None;
    }
    // V2_SWAP_COMPACT carries uint96 amounts.
    let u96_max = u128::from(u64::MAX) * 0x1_0000_0000 + 0xFFFF_FFFF;
    let too_big = |v: u128| v >= u96_max;
    if too_big(candidate.optimal_input)
        || candidate.hop_outputs.iter().any(|&v| too_big(v))
        || candidate.consumed_inputs.iter().any(|&v| too_big(v))
    {
        return None;
    }

    let hops = candidate
        .hops
        .iter()
        .map(|h| {
            HopInfo::V2(V2HopInfo {
                pool_address: h.pool,
                token0_address: h.token0,
                token1_address: h.token1,
                fee: 30,
                zfo: h.zfo,
            })
        })
        .collect();
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
    // V4-only context field: irrelevant for all-V2 streams; the canonical
    // mainnet PoolManager keeps the context well-formed.
    let ctx = EncodeContext::new(
        executor,
        alloy::primitives::address!("000000000004444c5dc75cb358380d2e3de08a90"),
        weth,
    );
    let commands = encode_cmd_stream(&ctx, &req)?;
    // check_mode 1 (WETH+ETH true-delta check) + coinbase bribe bips.
    let config = (U256::from(bribe_bips) << 8) | U256::from(1u8);
    encode_execute_call(executor, &commands, config)
        .ok()
        .map(|call| alloy::primitives::Bytes::from(call.data.clone()))
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

    #[test]
    fn golden_candidate_composes_to_execute_calldata() {
        use degenbot_executor::composers::EXECUTE_SELECTOR;
        // canonical order: TOK (0x..aa1) < WETH -> token0 = TOK
        let token0 = TOK;
        let candidate = LaneCandidate {
            hops: vec![
                SidecarHopRef {
                    pool_id: 1,
                    pool: P,
                    token0,
                    token1: WETH,
                    zfo: false,
                },
                SidecarHopRef {
                    pool_id: 2,
                    pool: Q,
                    token0,
                    token1: WETH,
                    zfo: true,
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

    #[test]
    fn candidate_rejects_misaligned_hops() {
        let candidate = LaneCandidate {
            hops: vec![
                SidecarHopRef {
                    pool_id: 1,
                    pool: P,
                    token0: TOK,
                    token1: WETH,
                    zfo: false,
                },
                SidecarHopRef {
                    pool_id: 2,
                    pool: Q,
                    token0: TOK,
                    token1: WETH,
                    zfo: true,
                },
            ],
            optimal_input: 1,
            hop_outputs: vec![1],
            consumed_inputs: vec![1],
            profit: 1,
        };
        assert!(build_candidate_calldata(&candidate, P, WETH, 1000).is_none());
    }
}
