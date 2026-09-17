//! Sidecar connector-solve engine (epic DFYDYI, task B3): the standalone
//! engine-backed path solver replacing the hand-composed mono-pool probe.
//!
//! Solves over a PRIVATE planning [`Workspace`] (the FORK-1 isolation: the
//! scope's scratch state never touches the mainline pump's registry), admits
//! pools on demand with explicit typed state, stages the target's effect with
//! the exact `v2_post_target` overlay (applied through the same journal the
//! live path uses), declares two-hop paths, and evaluates them with the
//! solvers' envelope-gated mixed solve (V2-V2 = the closed-form integer
//! Mobius; no hand-composed pricing anywhere).
//!
//! Frame flow: `admit_v2` (affected + connectors) -> `stage_target_swap` ->
//! `declare` the 2-hop family -> `evaluate` with `min_profit = gas floor`.

use alloy::primitives::{Address, U256};
use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_pools::TickInfo;
use degenbot_solvers::mixed::SolvePathResult;

use crate::bot_core::planning::{ExplicitPoolState, PlanningHop, PlanningPoolParams, Workspace};

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

/// The standalone frame solver: the backrun lane's driver shell over a
/// planning [`Workspace`] (FORK-1 isolation — the scope's scratch state
/// never touches the mainline pump's registry). Admission, staging,
/// declaration, and evaluation delegate to the workspace; the lane owns the
/// RPC fetch ladders that PRODUCE the explicit state (slot0/tick bootstrap,
/// live reserves).
pub struct SidecarSolver {
    ws: Workspace,
}

impl SidecarSolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ws: Workspace::new(),
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
        self.ws
            .stage_v2_reserves(address, reserve0, reserve1, block);
    }

    /// Declare a path from admitted pool ids + directions; returns its index.
    /// Hops referencing unknown pools make the path invalid for this frame
    /// (dropped loudly at resolve, not admitted lazily here).
    /// Declare a path from executable hop refs (the declare side of
    /// [`SidecarHopRef`] -- cycle order is hop order; the per-hop family
    /// carries the solver `HopType` so V2/V3 mixes resolve in one cycle).
    #[must_use]
    pub fn declare_hops(&mut self, hops: &[SidecarHopRef]) -> usize {
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
    /// The hop's protocol family (V2 or V3 with its 1e6 fee). The declared
    /// `HopType` and the composer `HopInfo` both derive from this one tag.
    pub family: LaneFamily,
}

/// The lane's resolved frame: everything the guard ladder derives, so the
/// lane body stays a linear pipeline over checked inputs.
struct LaneFrame {
    pool_addr: Address,
    tok_id: u64,
    weth_id: u64,
    p_pool_id: u64,
    token0: Address,
    token1: Address,
    tok_is_t0: bool,
    amount_in: u128,
    in_is_t0: bool,
    /// The leg's live `(reserve_in, reserve_out)` view.
    r_view: (u128, u128),
    cap: usize,
    gas_floor_wei: U256,
}

/// Run the lane's guard ladder over the frame ctx: protocol/direction/
/// amount/index checks, canonical pair order, and the leg->pair reserve
/// mapping. `None` = the frame is outside the WETH-denominated 2-hop lane.
fn resolve_lane_frame(ctx: &ConnectorLaneCtx<'_>) -> Option<LaneFrame> {
    use degenbot_decoders::target_classifier::PoolProtocol;

    let leg = ctx.leg;
    if leg.protocol != PoolProtocol::V2 || leg.hops != 0 {
        return None;
    }
    let pool_addr = leg.pool?;
    let t_in = leg.token_in?;
    let t_out = leg.token_out?;
    let amount_in = u128::try_from(leg.amount_in?).ok()?;
    if amount_in == 0 {
        return None;
    }
    let weth = crate::sidecar_solve::weth();
    let weth_id = *ctx.ids.get(&weth)?;
    let t_in_id = *ctx.ids.get(&t_in)?;
    let t_out_id = *ctx.ids.get(&t_out)?;
    // WETH-denominated 2-hop family only (the mono-pool lane's domain).
    let (tok, tok_id) = if weth_id == t_in_id {
        (t_out, t_out_id)
    } else if weth_id == t_out_id {
        (t_in, t_in_id)
    } else {
        return None;
    };
    let p_edge = ctx.index.edge_by_address(pool_addr)?;
    let (t0_id, t1_id) = (p_edge.token0_id, p_edge.token1_id);
    if !((t0_id == tok_id && t1_id == weth_id) || (t0_id == weth_id && t1_id == tok_id)) {
        return None;
    }

    // V2 canonical order: token0 = the smaller address.
    let (token0, token1) = if tok < weth { (tok, weth) } else { (weth, tok) };
    let tok_is_t0 = tok == token0;
    let in_is_t0 = leg.token_in == Some(token0);
    let r_view = if in_is_t0 {
        ctx.pair_reserves
    } else {
        (ctx.pair_reserves.1, ctx.pair_reserves.0)
    };
    Some(LaneFrame {
        pool_addr,
        tok_id,
        weth_id,
        p_pool_id: p_edge.pool_id,
        token0,
        token1,
        tok_is_t0,
        amount_in,
        in_is_t0,
        r_view,
        cap: ctx.cap,
        gas_floor_wei: ctx.gas_floor_wei,
    })
}

/// The lane hop's protocol family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneFamily {
    V2,
    /// Concentrated liquidity carrying the pool's 1e6-convention fee.
    V3 {
        fee: u32,
    },
}

impl LaneFamily {
    #[must_use]
    pub const fn hop_type(self) -> degenbot_solvers::mixed::HopType {
        match self {
            Self::V2 => degenbot_solvers::mixed::HopType::V2,
            Self::V3 { .. } => degenbot_solvers::mixed::HopType::V3,
        }
    }
}

/// Build both cycles of a (staged P, connector C) pairing. Hops are the
/// executable form themselves -- declaration and composition share these
/// objects, so orientation cannot drift between the two surfaces.
fn connector_cycles(
    p: (u64, Address),
    c: (u64, Address),
    tokens: (Address, Address),
    tok_is_t0: bool,
    c_family: LaneFamily,
    p_family: LaneFamily,
) -> Vec<Vec<SidecarHopRef>> {
    let hop = |pool_id: u64, pool: Address, zfo: bool, family: LaneFamily| SidecarHopRef {
        pool_id,
        pool,
        token0: tokens.0,
        token1: tokens.1,
        zfo,
        family,
    };
    // P: WETH -> USDC (token1 -> token0) and P: USDC -> WETH (token0 -> token1).
    // Cycle 1: buy USDC on the market connector, sell into the staged P.
    // Cycle 2: buy USDC on the staged P, sell into the connector. The
    // envelope gate prunes the fee-drain direction per pair for free.
    let buy_on_c = hop(c.0, c.1, !tok_is_t0, c_family);
    let sell_into_p = hop(p.0, p.1, tok_is_t0, p_family);
    let buy_on_p = hop(p.0, p.1, !tok_is_t0, p_family);
    let sell_into_c = hop(c.0, c.1, tok_is_t0, c_family);
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
/// Per-frame inputs for the V3-affected-pool lane (the B1 follow-up). The
/// pool resolution + post-target staging happened upstream (bin owner).
pub struct V3LaneCtx<'a> {
    pub index: &'a crate::sidecar_paths::V2ConnectorIndex,
    /// DB ids for the affected pool's tokens: `tok_id` = the token that the
    /// target's swap MOVES INTO the pool's quoting side (`token_in`),
    /// `other_id` = the token out. Connectors are those sharing either.
    pub tok_id: u64,
    pub other_id: u64,
    /// The affected pool's DB cluster id + address.
    pub p_pool_id: u64,
    pub p_id: u64,
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub tok_is_t0: bool,
    pub fee: u32,
    pub head: u64,
    pub cap: usize,
    pub gas_floor_wei: U256,
}

pub struct ConnectorLaneCtx<'a> {
    pub index: &'a crate::sidecar_paths::V2ConnectorIndex,
    /// Frame token addresses -> DB token ids (the bin owns the cache).
    pub ids: &'a std::collections::HashMap<Address, u64>,
    pub leg: &'a degenbot_decoders::target_classifier::SwapLeg,
    /// Affected-pool family (V2 staged via the lean overlay; V3 staged via
    /// the in-range post-target sqrtP — the B1 follow-up).
    pub p_family: LaneFamily,
    /// PRE-STAGED affected pool (the V3 path registers it before the lane
    /// runs, with the post-target sqrtPriceX96): `(pool_id, tok_is_t0)`.
    /// `None` = let the lane stage the V2 overlay itself.
    pub staged_p: Option<(u64, bool)>,
    /// The affected pair's live `getReserves` in PAIR order (token0, token1).
    pub pair_reserves: (u128, u128),
    /// The head block (the V3 admission's seed stamp).
    pub head: u64,
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
        let Some(frame) = resolve_lane_frame(ctx) else {
            return LaneGrade::default();
        };

        let staged_result = if let Some((id, tok_is_t0)) = ctx.staged_p {
            Some((id, !tok_is_t0, tok_is_t0))
        } else {
            self.stage_affected_pair(
                (frame.pool_addr, frame.token0, frame.token1),
                frame.tok_is_t0,
                frame.amount_in,
                frame.in_is_t0,
                frame.r_view,
            )
        };
        let Some((p_id, _pw, _pt)) = staged_result else {
            return LaneGrade::default();
        };

        self.fan_connectors(
            provider,
            ctx.index,
            frame.tok_id,
            frame.weth_id,
            frame.p_pool_id,
            p_id,
            frame.pool_addr,
            (frame.token0, frame.token1),
            frame.tok_is_t0,
            ctx.p_family,
            ctx.head,
            frame.cap,
            frame.gas_floor_wei,
        )
        .await
    }

    /// The shared connector fan: admit live V2 + V3 connectors for the staged
    /// affected pool `p_id`, declare both drift cycles per connector, evaluate,
    /// return the best. Used by the V2 overlay lane AND the V3 staged lane.
    #[expect(
        clippy::too_many_arguments,
        reason = "the fan takes a full frame descriptor"
    )]
    async fn fan_connectors(
        &mut self,
        provider: &degenbot_rpc::provider::AlloyProvider,
        index: &crate::sidecar_paths::V2ConnectorIndex,
        tok_id: u64,
        weth_id: u64,
        p_pool_id: u64,
        p_id: u64,
        pool_p: Address,
        tokens: (Address, Address),
        tok_is_t0: bool,
        p_family: LaneFamily,
        head: u64,
        cap: usize,
        gas_floor_wei: U256,
    ) -> LaneGrade {
        let mut grade = LaneGrade::default();

        // Spot connectors (V2): live reserves, same two-cycle shape.
        let cands = index.connectors(tok_id, weth_id, p_pool_id, cap).await;
        grade.connectors = cands.len();
        for (c_edge, _tok_flag) in cands {
            let Some(c_id) = self
                .admit_connector(provider, c_edge.address, tokens.0, tokens.1)
                .await
            else {
                grade.admit_failures += 1;
                continue;
            };
            let cycles = connector_cycles(
                (p_id, pool_p),
                (c_id, c_edge.address),
                tokens,
                tok_is_t0,
                LaneFamily::V2,
                p_family,
            );
            self.eval_cycles(&mut grade, gas_floor_wei, cycles);
        }

        // CL connectors (B2-CL): the same two-cycle shape with a V3 second
        // hop. Admission costs 3-6 RPC round trips per pool (bounded by
        // cap); the exact-sim oracle remains the truth bar.
        let v3cands = index.v3_connectors(tok_id, weth_id, p_pool_id, cap).await;
        grade.connectors += v3cands.len();
        for (v3_edge, _tok_flag) in v3cands {
            let Some(c_id) = self
                .admit_v3_connector(provider, v3_edge, tokens.0, tokens.1, head)
                .await
            else {
                grade.admit_failures += 1;
                continue;
            };
            let cycles = connector_cycles(
                (p_id, pool_p),
                (c_id, v3_edge.address),
                tokens,
                tok_is_t0,
                LaneFamily::V3 { fee: v3_edge.fee },
                p_family,
            );
            self.eval_cycles(&mut grade, gas_floor_wei, cycles);
        }
        grade
    }

    /// The V3-affected-pool lane (the B1 follow-up): the router-side V3 pool was
    /// already resolved + staged (post-target sqrtPriceX96 registered) by the
    /// caller; this fans connectors across BOTH pool tokens and grades the
    /// mixed-family cycles.
    pub async fn run_v3_target_lane(
        &mut self,
        provider: &degenbot_rpc::provider::AlloyProvider,
        ctx: &V3LaneCtx<'_>,
    ) -> LaneGrade {
        self.fan_connectors(
            provider,
            ctx.index,
            ctx.tok_id,
            ctx.other_id,
            ctx.p_pool_id,
            ctx.p_id,
            ctx.pool,
            (ctx.token0, ctx.token1),
            ctx.tok_is_t0,
            LaneFamily::V3 { fee: ctx.fee },
            ctx.head,
            ctx.cap,
            ctx.gas_floor_wei,
        )
        .await
    }

    /// Admit a V3 connector    /// Admit a V3 connector at HEAD state: slot0 + liquidity + a Sparse tick
    /// bootstrap (current word +- 1) via the provider-level V3 probes.
    /// `None` on any fetch/spec failure (the connector is skipped).
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

    async fn admit_v3_connector(
        &mut self,
        provider: &degenbot_rpc::provider::AlloyProvider,
        edge: &crate::sidecar_paths::V3Edge,
        token0: Address,
        token1: Address,
        head: u64,
    ) -> Option<u64> {
        self.admit_v3_full(
            provider,
            edge.address,
            token0,
            token1,
            edge.fee,
            edge.tick_spacing,
            None,
            head,
        )
        .await
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
    ) -> Option<u64> {
        use degenbot_rpc::abi::{fetch_tick_bitmap, fetch_tick_data, fetch_v3_slot0_liquidity};

        let (sqrt_price_x96, tick, liquidity) = fetch_v3_slot0_liquidity(provider, &address, None)
            .await
            .ok()?;
        let liquidity = u128::try_from(liquidity).ok()?;

        // Sparse coverage: the current word +- 1 (a 3-word window around the
        // active range; the exact-sim oracle is the truth bar behind this).
        let tick_i32 = i32::try_from(tick).ok()?;
        let (word, _) = degenbot_math::cl::liquidity_mapping::get_tick_word_and_bit_position(
            tick_i32,
            tick_spacing,
        );
        let word_i16 = i16::try_from(word).ok()?;
        let mut tick_data: hashbrown::HashMap<i32, TickInfo> = hashbrown::HashMap::new();
        for w in [
            word_i16.saturating_sub(1),
            word_i16,
            word_i16.saturating_add(1),
        ] {
            let bitmap = fetch_tick_bitmap(provider, &address, w, None).await.ok()?;
            for bit in 0..256usize {
                if !bitmap.bit(bit) {
                    continue;
                }
                let active_tick = ((i32::from(w) << 8) + i32::try_from(bit).ok()?) * tick_spacing;
                let (gross, net) = fetch_tick_data(provider, &address, active_tick, None)
                    .await
                    .ok()?;
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
                },
                head,
            )
            .ok()
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
        HopInfo, V2HopInfo, V3HopInfo,
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
        .map(|h| match h.family {
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
#[expect(
    clippy::expect_used,
    reason = "golden-reference tests: admitted fixtures are spec-valid by construction"
)]
mod tests {
    use super::*;
    use alloy::primitives::{address, aliases::U112};

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
        // The target's exact effect (v2_post_target: 50 WETH in on P).
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
                SidecarHopRef {
                    pool_id: 1,
                    pool: P,
                    token0,
                    token1: WETH,
                    zfo: false,
                    family: LaneFamily::V2,
                },
                SidecarHopRef {
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
                    family: LaneFamily::V2,
                },
                SidecarHopRef {
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
    }
}
