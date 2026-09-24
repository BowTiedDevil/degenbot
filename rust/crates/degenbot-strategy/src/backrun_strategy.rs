//! The backrun strategy — the first [`PendingTxReaction`] implementation.
//!
//! Given a pending transaction's recovered pool post-states, [`BackrunStrategy`]
//! admits the pools that settle through a supported quote, walks the anchored
//! connector graph for WETH-closing cycles, solves them against the fresh
//! workspace, and prices the best candidate into the wallet-true bid the
//! driver simulates and submits.

use std::collections::HashSet;

use crate::backrun::{decide, BackrunConfig, Decision};
use crate::backrun_engine::{
    project_candidate_for_cmd_executor, BackrunHopRef, BackrunSolver, BackrunV2Pool, LaneCandidate,
    LaneFamily, PathReject,
};
use crate::cmd_executor_adapter::{CmdExecutorAdapter, CmdExecutorOutcome};
use alloy::primitives::{address, Address, U256};
use degenbot_bot::connector_index::V2ConnectorIndex;
use degenbot_decoders::target_class::TargetClass;
use degenbot_executor::composers::{EncodeContext, EncodeOptions};
use degenbot_executor::encoders::V4_FEE_ENCODER_MAX;
use degenbot_executor::grammar_ledger::{Bribe, FundingSource, ProfitCapture};
use degenbot_pathfinding::PoolKind;
use degenbot_pools::{slot_layout, TickInfo};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost,
};
use degenbot_simulation::sim::evm::{read_view_word, ScratchDb, ScratchEvm};
use hashbrown::HashMap as HbMap;

use crate::anchored_dfs::{resolve_hop, AnchorPool, DfsCycle, ResolvedHop, NON_WETH_CYCLE};
use crate::frame_pipeline::{honest_observe, trace_jsonl, BidEconomics, PipelineConfig};
use crate::market_context::MarketContext;
use crate::pending_tx::{ComposedIntent, Decided, PendingTxReaction, V3TickWindow};

/// The canonical mainnet WETH address — the base (settlement) quote.
pub const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
/// The canonical mainnet USDC address — a supported per-frame quote.
pub const USDC: Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
/// The canonical mainnet USDT address — a supported per-frame quote.
pub const USDT: Address = address!("dac17f958d2ee523a2206206994597c13d831ec7");
/// The per-frame quote set: every affected pool may settle its drift cycle
/// in any of these, selected by the touched token's edge degree. Outside
/// the set, a pool keeps its truthful no-quote skip.
pub const SUPPORTED_QUOTES: [Address; 3] = [WETH, USDC, USDT];

/// The composed bundle's bid economics: what the builder gets and the bips
/// the on-chain config word speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetBid {
    /// The executor config's bribe bips for THIS bundle: bips of the true
    /// profit delta paid on-chain to the block builder (0 = keep all).
    pub bribe_bips: u16,
    /// The resulting on-chain bribe, `bribe_bips` of gross.
    pub bid_wei: U256,
    /// What lands in executor custody if the bundle lands.
    pub keep_wei: U256,
}

/// The wallet's keep over the gas burn: gas plus 5% margin.
fn wallet_keep(wallet_gas_cost_wei: u128) -> u128 {
    wallet_gas_cost_wei.saturating_mul(105) / 100
}

/// Bid sizing from the wallet's truth: the wallet funds ONLY the bundle's
/// gas — the on-chain bribe is drawn from flash proceeds inside the tx and
/// the un-bribed remainder parks in executor custody (the owner recovers
/// it). Viable only when the solved gross covers `wallet_gas_cost_wei`
/// plus margin; the bribe then takes everything the wallet does not need
/// to keep, capped by `max_bribe_bips` (competitiveness ceiling) and
/// `bid_cap_wei` (per-bundle cap). `None` = the wallet cannot break even —
/// no bid.
///
/// # Panics
///
/// Never: the bips cap keeps the `u16` conversion in range.
#[must_use]
pub fn net_bid(
    gross: u128,
    wallet_gas_cost_wei: u128,
    max_bribe_bips: u16,
    bid_cap_wei: u128,
) -> Option<NetBid> {
    let keep_needed = wallet_keep(wallet_gas_cost_wei);
    if gross <= keep_needed {
        return None;
    }
    let bid_target = (gross - keep_needed).min(bid_cap_wei);
    let bips_cap = u128::from(max_bribe_bips.min(10_000));
    let raw_bips = bid_target.checked_mul(10_000)? / gross;
    let bips = u16::try_from(raw_bips.min(bips_cap)).unwrap_or(u16::MAX);
    let bid_wei = U256::from(bips) * U256::from(gross) / U256::from(10_000u16);
    Some(NetBid {
        bribe_bips: bips,
        bid_wei,
        keep_wei: U256::from(gross) - bid_wei,
    })
}

/// The built-in command policy used at the strategy ceiling and again at the
/// wallet-true net-bid share.
#[must_use]
pub const fn backrun_encode_options(bribe_bips: u16) -> EncodeOptions {
    EncodeOptions {
        erc6909_profit: false,
        use_v4_batch: false,
        funding: FundingSource::InPathFlash,
        capture: ProfitCapture::Custody,
        bribe: Bribe::Some {
            bips: bribe_bips,
            recipient_idx: 0,
        },
    }
}

#[expect(
    clippy::panic,
    reason = "a ledger rejection is a process-fatal invariant failure"
)]
fn cmd_executor_bytes(
    outcome: CmdExecutorOutcome,
    trace_tx: &str,
) -> Option<alloy::primitives::Bytes> {
    match outcome {
        CmdExecutorOutcome::Encoded(bytes) => Some(bytes),
        CmdExecutorOutcome::Declined(decline) => {
            trace_jsonl(
                "composed",
                serde_json::json!({
                    "tx": trace_tx,
                    "composed": false,
                    "reason": decline.label(),
                }),
            );
            None
        }
        CmdExecutorOutcome::Rejected(rejection) => {
            panic!("cmd_executor ledger validation rejected a derived plan: {rejection:?}")
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Workspace admission (frame-scoped scope; replayed state verbatim)
// ─────────────────────────────────────────────────────────────────────────

/// One settlement quote an admitted pool supports: the quote's identity +
/// DB ids for the fan's `(token, quote)` connector lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AffectedQuote {
    pub quote: Address,
    pub quote_id: u64,
    /// The pool's OTHER token — the discovery fan's token side.
    pub tok_id: u64,
}

/// An affected pool admitted into THIS frame's scope with its replayed
/// post-target state.
#[derive(Debug, Clone)]
pub struct AffectedPool {
    pub address: Address,
    /// The scope-local workspace id (hop references; dies with the frame).
    pub workspace_pool_id: u64,
    /// The connector-index pool id (discovery exclusion).
    pub index_pool_id: u64,
    pub token0: Address,
    pub token1: Address,
    /// The supported quotes the pool trades (a subset of
    /// [`SUPPORTED_QUOTES`]; empty keeps the truthful no-quote skip). The
    /// discovery fan picks per frame by edge degree.
    pub quotes: Vec<AffectedQuote>,
    pub family: LaneFamily,
}

/// The supported-quote orientations of an admitted pool: one entry per
/// [`SUPPORTED_QUOTES`] member the pool trades, with the other side's DB id
/// for the fan. Unjoined ids drop that orientation (no id, no fan).
fn quote_orientations(rt: &MarketContext, token0: Address, token1: Address) -> Vec<AffectedQuote> {
    let mut out = Vec::new();
    for (quote, other) in [(token0, token1), (token1, token0)] {
        if !SUPPORTED_QUOTES.contains(&quote) {
            continue;
        }
        let Some(quote_id) = rt.token_id(quote) else {
            continue;
        };
        let Some(tok_id) = rt.token_id(other) else {
            continue;
        };
        out.push(AffectedQuote {
            quote,
            quote_id,
            tok_id,
        });
    }
    out
}

/// Admit every typed extracted post-state into the frame's fresh workspace
/// scope. Returns the affected pools that BOTH admitted AND trade WETH.
#[must_use]
pub fn admit_extracted(
    rt: &MarketContext,
    solver: &mut BackrunSolver,
    states: &[PoolPostState],
    seed_block: u64,
    trace_tx: &str,
    tick_window: Option<&dyn V3TickWindow>,
) -> Vec<AffectedPool> {
    degenbot_core::runtime::get_runtime().block_on(admit_extracted_verified(
        rt,
        solver,
        states,
        seed_block,
        trace_tx,
        tick_window,
    ))
}

#[expect(
    clippy::too_many_lines,
    reason = "the V2/V3/V4 admission arms read top-to-bottom per family"
)]
#[must_use]
pub async fn admit_extracted_verified(
    rt: &MarketContext,
    solver: &mut BackrunSolver,
    states: &[PoolPostState],
    seed_block: u64,
    trace_tx: &str,
    _tick_window: Option<&dyn V3TickWindow>,
) -> Vec<AffectedPool> {
    let mut out = Vec::new();
    let Some(idx) = rt.index() else {
        return out;
    };
    let skip = |pool: Address, stage: &'static str| {
        trace_jsonl(
            "extract_skip",
            serde_json::json!({
                "tx": trace_tx,
                "pool": format!("0x{}", alloy::hex::encode(pool)),
                "stage": stage,
            }),
        );
    };
    for st in states {
        let PoolPostKind::Typed(tp) = &st.kind else {
            // A V4 post-state in a frame that also carried typed states: the
            // V4 half is observed but not admitted (no V4 lane yet). Record
            // the skip so a mixed frame is never silently empty-armed on the
            // V4 side.
            skip(st.address, "v4-half-unobserved");
            continue;
        };
        match &st.family {
            PoolFamily::V2Pair => {
                let TypedPoolPost::V2 { reserves } = tp else {
                    continue;
                };
                let Some(edge) = idx.edge_by_address(st.address) else {
                    continue;
                };
                let (Some(token0), Some(token1)) =
                    (rt.token_addr(edge.token0_id), rt.token_addr(edge.token1_id))
                else {
                    skip(st.address, "token-join");
                    continue;
                };
                let (r0, r1) = (
                    u112_to_u128(reserves.reserve0),
                    u112_to_u128(reserves.reserve1),
                );
                let Ok(fees) = edge.fees.resolve() else {
                    skip(st.address, "v2-fee");
                    continue;
                };
                let Ok(p_id) = solver.admit_v2(&BackrunV2Pool {
                    address: st.address,
                    token0,
                    token1,
                    reserve0: r0,
                    reserve1: r1,
                    fees: degenbot_bot::bot_core::executor_hop::V2FeePair::new(
                        fees.token0,
                        fees.token1,
                    ),
                }) else {
                    skip(st.address, "v2-admit");
                    continue;
                };
                let quotes = quote_orientations(rt, token0, token1);
                if quotes.is_empty() {
                    skip(st.address, "no-supported-quote");
                } else {
                    out.push(AffectedPool {
                        address: st.address,
                        workspace_pool_id: p_id,
                        index_pool_id: edge.pool_id,
                        token0,
                        token1,
                        quotes,
                        family: LaneFamily::V2 { fees },
                    });
                }
            }
            PoolFamily::V3 {
                layout,
                tick_spacing,
                ..
            } => {
                let TypedPoolPost::V3 {
                    sqrt_price_x96,
                    tick,
                    liquidity,
                    touched_ticks,
                } = tp
                else {
                    continue;
                };
                let spacing = *tick_spacing;
                let Some(edge) = idx.v3_edge_by_address(st.address) else {
                    skip(st.address, "v3-edge");
                    continue;
                };
                let (Some(token0), Some(token1)) =
                    (rt.token_addr(edge.token0_id), rt.token_addr(edge.token1_id))
                else {
                    skip(st.address, "token-join");
                    continue;
                };
                // Nothing is fabricated: an incomplete slot0/liquidity set
                // cannot stage (the scope never fills in missing words).
                let (Some(sqrt), Some(tk), Some(liq)) = (*sqrt_price_x96, *tick, *liquidity) else {
                    skip(st.address, "incomplete-slot0-liquidity");
                    continue;
                };
                let mut tick_data = HbMap::default();
                for t in touched_ticks {
                    tick_data.insert(
                        t.tick,
                        TickInfo {
                            liquidity_gross: alloy::primitives::aliases::U128::from(
                                t.liquidity_gross,
                            ),
                            liquidity_net: t.liquidity_net,
                            block: seed_block,
                        },
                    );
                }
                // The replayed journal carries only ticks the target CROSSED;
                // a shallow swap touches none, leaving a map too sparse to
                // build a solver range sequence (every chain anchored on it
                // then rejects `unusable_pool_state`, deficits=1). The ingress
                // stages the rest with `Db → Chain` precedence — the complete
                // per-tick map the pump maintains, else the sparse bootstrap.
                // Replayed touched ticks win (they are the post-frame facts).
                let crossed = touched_ticks.len();
                trace_jsonl(
                    "anchor_stage",
                    serde_json::json!({
                        "tx": trace_tx,
                        "pool": format!("0x{}", alloy::hex::encode(st.address)),
                        "stage": "window-enter",
                    }),
                );
                let p_id = match solver
                    .admit_v3_replay(
                        st.address,
                        token0,
                        token1,
                        edge.fee,
                        spacing,
                        sqrt,
                        liq,
                        tk,
                        tick_data,
                        seed_block,
                        *layout,
                        rt.ingress(),
                    )
                    .await
                {
                    Ok(p_id) => p_id,
                    Err(decline) => {
                        trace_admit_fail(trace_tx, st.address, decline.stage(), &decline.detail());
                        skip(st.address, "v3-ingress");
                        continue;
                    }
                };
                trace_jsonl(
                    "anchor_ticks",
                    serde_json::json!({
                        "tx": trace_tx,
                        "pool": format!("0x{}", alloy::hex::encode(st.address)),
                        "layout": format!("{layout:?}"),
                        "spacing": spacing,
                        "tick": tk,
                        "liquidity": liq.to_string(),
                        "crossed": crossed,
                        "source": "ingress",
                    }),
                );
                let quotes = quote_orientations(rt, token0, token1);
                if quotes.is_empty() {
                    skip(st.address, "no-supported-quote");
                } else {
                    out.push(AffectedPool {
                        address: st.address,
                        workspace_pool_id: p_id,
                        index_pool_id: edge.pool_id,
                        token0,
                        token1,
                        quotes,
                        family: LaneFamily::V3 { fee: edge.fee },
                    });
                }
            }
            PoolFamily::V4PoolManager { .. } => {
                let TypedPoolPost::V4 {
                    pool_id,
                    sqrt_price_x96,
                    tick,
                    liquidity,
                    touched_ticks,
                } = tp
                else {
                    continue;
                };
                // The index edge carries the key's fee/spacing/hooks and the
                // manager-keyed DB identity; a typed post with no roster edge
                // has no lane (the manager's touched set may name a pool the
                // connector never loaded).
                let Some(edge) = idx.v4_edge_by_pool_hash(*pool_id) else {
                    skip(st.address, "v4-edge");
                    continue;
                };
                // The executor encodes the V4 fee in 2 bytes; a fee past that
                // bound can never compose, so refuse at admission (the gate
                // says no, rather than the composer failing later).
                if edge.fee >= V4_FEE_ENCODER_MAX {
                    skip(st.address, "v4-fee-encoder-overflow");
                    continue;
                }
                // Nothing is fabricated: an incomplete slot0/liquidity set
                // cannot stage (the scope never fills in missing words).
                let (Some(sqrt), Some(tk), Some(liq)) = (*sqrt_price_x96, *tick, *liquidity) else {
                    skip(st.address, "incomplete-slot0-liquidity");
                    continue;
                };
                let mut tick_data = HbMap::default();
                for t in touched_ticks {
                    tick_data.insert(
                        t.tick,
                        TickInfo {
                            liquidity_gross: alloy::primitives::aliases::U128::from(
                                t.liquidity_gross,
                            ),
                            liquidity_net: t.liquidity_net,
                            block: seed_block,
                        },
                    );
                }
                let p_id = match solver
                    .admit_v4_replay(
                        st.address,
                        edge.state_view,
                        edge.token0,
                        edge.token1,
                        *pool_id,
                        edge.fee,
                        edge.tick_spacing,
                        edge.hooks,
                        sqrt,
                        liq,
                        tk,
                        tick_data,
                        seed_block,
                        rt.ingress(),
                    )
                    .await
                {
                    Ok(pool_id) => pool_id,
                    Err(decline) => {
                        trace_admit_fail(trace_tx, st.address, decline.stage(), &decline.detail());
                        skip(st.address, "v4-ingress");
                        continue;
                    }
                };
                let quotes = quote_orientations(rt, edge.token0, edge.token1);
                if quotes.is_empty() {
                    skip(st.address, "no-supported-quote");
                } else {
                    out.push(AffectedPool {
                        address: st.address,
                        workspace_pool_id: p_id,
                        index_pool_id: edge.db_pool_id,
                        token0: edge.token0,
                        token1: edge.token1,
                        quotes,
                        family: LaneFamily::V4 {
                            fee: edge.fee,
                            pool_id: *pool_id,
                            tick_spacing: edge.tick_spacing,
                            hooks: edge.hooks,
                        },
                    });
                }
            }
        }
    }
    out
}

/// U112 → u128 (lossless; the workspace re-validates the uint112 class).
fn u112_to_u128(v: alloy::primitives::aliases::U112) -> u128 {
    u128::try_from(U256::from(v)).unwrap_or(0)
}
// ─────────────────────────────────────────────────────────────────────────
// Same-chain-view reads (connector state; raw-RPC fallback only)
// ─────────────────────────────────────────────────────────────────────────

/// V2 reserves read through the frame-replay chain view (slot 8). Zero
/// reserves read through as `None` (a pool with zero reserves is not usable
/// state — the caller falls back or skips).
fn view_v2_reserves(
    scratch: &mut ScratchEvm<ScratchDb<'_>>,
    pool: Address,
) -> Option<(u128, u128)> {
    let word = read_view_word(
        scratch.ext(),
        pool,
        U256::from(slot_layout::V2_RESERVES_SLOT),
    )?;
    let parts = slot_layout::decode_v2_reserves_word(word);
    (parts.reserve0 != alloy::primitives::aliases::U112::ZERO
        || parts.reserve1 != alloy::primitives::aliases::U112::ZERO)
        .then(|| {
            (
                u128::try_from(U256::from(parts.reserve0)).unwrap_or(0),
                u128::try_from(U256::from(parts.reserve1)).unwrap_or(0),
            )
        })
}

// ─────────────────────────────────────────────────────────────────────────
// Discovery + solve
// ─────────────────────────────────────────────────────────────────────────

/// One declared chain's solve verdict — hop pools + the typed reject — for
/// the JSONL `solve` event. `evaluated` means the envelope gate let the solve
/// run and it returned a result (even one below the incumbent); `profit_wei`
/// then carries that value so sub-incumbent outcomes stay legible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainOutcome {
    /// The hop pool addresses in traversal order.
    pub pools: Vec<Address>,
    /// Whether [`BackrunSolver::evaluate_verdict`] returned `Ok`.
    pub evaluated: bool,
    /// The solved profit in wei when evaluated.
    pub profit_wei: Option<u128>,
    /// The typed reject when not evaluated.
    pub reject: Option<PathReject>,
}

/// Solve stats for the JSONL trace + the composed candidate.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SolveStats {
    /// The anchored walker's WETH-entry chains declared / envelope-evaluated
    /// (any depth — 2-hop parity cycles included).
    pub dfs_declared: usize,
    pub dfs_evaluated: usize,
    pub best: Option<LaneCandidate>,
    /// A non-WETH quote fan had connectors but no priceable WETH
    /// normalization lane, so its candidates were refused (the frame's
    /// truthful `non_base_quote` observation when nothing else composed).
    pub non_base_quote_dropped: bool,
    /// The per-chain outcomes behind `dfs_declared` / `dfs_evaluated`.
    pub chains: Vec<ChainOutcome>,
}

/// Evaluate the anchored walker's depth-3 chains through the SAME
/// declare/evaluate/envelope gate as the fans (wei floor vs wei profit),
/// keeping the best. The chains arrive anchor-first in traversal order;
/// selection across fans, quotes, and walker chains still compares wei
/// downstream — this function only bundles the walker surface's counts.
pub fn solve_dfs_chains(
    solver: &mut BackrunSolver,
    chains: &[Vec<BackrunHopRef>],
    gas_floor_wei: U256,
) -> SolveStats {
    let mut stats = SolveStats::default();
    for chain in chains {
        let pools: Vec<Address> = chain.iter().map(|h| h.pool).collect();
        let idx = solver.declare_hops(chain);
        stats.dfs_declared += 1;
        let res = match solver.evaluate_verdict(idx, gas_floor_wei) {
            Ok(res) => res,
            Err(reject) => {
                stats.chains.push(ChainOutcome {
                    pools,
                    evaluated: false,
                    profit_wei: None,
                    reject: Some(reject),
                });
                continue;
            }
        };
        stats.dfs_evaluated += 1;
        let profit = res.profit.to::<u128>();
        stats.chains.push(ChainOutcome {
            pools,
            evaluated: true,
            profit_wei: Some(profit),
            reject: None,
        });
        if stats.best.as_ref().is_some_and(|b| b.profit >= profit) {
            continue;
        }
        stats.best = Some(LaneCandidate {
            hops: chain.clone(),
            optimal_input: res.optimal_input.to::<u128>(),
            hop_outputs: res.hop_outputs.iter().map(|v| v.to::<u128>()).collect(),
            consumed_inputs: res.consumed_inputs.iter().map(|v| v.to::<u128>()).collect(),
            profit,
        });
    }
    stats
}

/// Admit one walker hop's pool at the frame's chain view (raw-RPC fallback)
/// and return its workspace id. A pool whose state reads as unusable
/// (zero reserves, incomplete slot0/liquidity) is skipped, never guessed.
async fn admit_hop_pool(
    rt: &MarketContext,
    solver: &mut BackrunSolver,
    scratch: &mut ScratchEvm<ScratchDb<'_>>,
    provider: &AlloyProvider,
    hop: &ResolvedHop,
    head: u64,
    trace_tx: &str,
) -> Option<u64> {
    let address = *hop.address();
    // Overlap reuse first: a pool this frame already admitted is usable NOW.
    // Re-admitting it refuses (`AlreadyRegistered`) and dropping the cycle
    // was the funnel's silent killer — every overlapping cycle died.
    if let Some(id) = solver.registered_pool_id(&address) {
        trace_jsonl(
            "hop_reuse",
            serde_json::json!({
                "tx": trace_tx,
                "pool": format!("0x{}", alloy::hex::encode(address)),
                "workspace_pool_id": id,
            }),
        );
        return Some(id);
    }
    match hop {
        ResolvedHop::V2(e) => {
            // The frame-replay scratch serves a connector the replay already
            // touched; a cold connector misses to the raw RPC. Both a failed
            // scratch read AND a failed RPC read are admission failures — the
            // sub-step that refused is traced so a declined hop is legible.
            let reserves = match view_v2_reserves(scratch, e.address) {
                Some(r) => r,
                None => {
                    match degenbot_rpc::abi::fetch_v2_reserves(provider, &e.address, None).await {
                        Ok((r0, r1)) => {
                            let (Ok(r0), Ok(r1)) = (u128::try_from(r0), u128::try_from(r1)) else {
                                trace_admit_fail(
                                    trace_tx,
                                    address,
                                    "reserves-rpc-width",
                                    "reserves exceed u128",
                                );
                                return None;
                            };
                            (r0, r1)
                        }
                        Err(err) => {
                            trace_admit_fail(trace_tx, address, "reserves-rpc", &err.to_string());
                            return None;
                        }
                    }
                }
            };
            let (Some(token0), Some(token1)) =
                (rt.token_addr(e.token0_id), rt.token_addr(e.token1_id))
            else {
                trace_admit_fail(trace_tx, address, "token-join", "V2 token id unresolved");
                return None;
            };
            match solver.admit_v2(&BackrunV2Pool {
                address: e.address,
                token0,
                token1,
                reserve0: reserves.0,
                reserve1: reserves.1,
                fees: e.fees,
            }) {
                Ok(id) => Some(id),
                Err(reason) => {
                    trace_admit_fail(trace_tx, address, "admit-v2", &reason.to_string());
                    None
                }
            }
        }
        ResolvedHop::V3(e) => {
            let (Some(token0), Some(token1)) =
                (rt.token_addr(e.token0_id), rt.token_addr(e.token1_id))
            else {
                trace_admit_fail(trace_tx, address, "token-join", "V3 token id unresolved");
                return None;
            };
            match solver
                .admit_v3_full(
                    provider,
                    e.address,
                    token0,
                    token1,
                    e.fee,
                    e.tick_spacing,
                    None,
                    head,
                    e.layout,
                    rt.ingress(),
                )
                .await
            {
                Ok(id) => Some(id),
                Err(reject) => {
                    // The refused step, not one lumped label: a Db read failure,
                    // an archive RPC failure, a width refusal, and an
                    // AlreadyRegistered duplicate will not share a line.
                    trace_admit_fail(trace_tx, address, reject.stage(), &reject.detail());
                    None
                }
            }
        }
    }
}

/// One line per failed hop admission: the exact hop pool + the sub-step that
/// refused it. A declined admission otherwise leaves the chain visibly
/// undeclared (and the solve 100% `unusable_pool_state`) with no cause.
fn trace_admit_fail(trace_tx: &str, pool: Address, stage: &str, detail: &str) {
    trace_jsonl(
        "admit_hop_fail",
        serde_json::json!({
            "tx": trace_tx,
            "pool": format!("0x{}", alloy::hex::encode(pool)),
            "stage": stage,
            "detail": detail,
        }),
    );
}

/// One WETH-entry cycle hop resolved against the frame: the workspace pool id
/// already admitted (or freshly admitted) for this cycle, its canonical token
/// order, and its lane family. [`cycle_refs`] orients each hop to the
/// traversal from these alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleHop {
    pub workspace_pool_id: u64,
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub family: LaneFamily,
}

/// The executable hop refs of one WETH-entry cycle from its resolved pools in
/// traversal order (`hops[0]` consumes WETH). Each ref's `zfo` orients its
/// canonical token order to the traversal; `None` unless every hop straddles
/// its neighbour and the traversal closes back on WETH.
#[must_use]
pub fn cycle_refs(hops: &[CycleHop], weth: Address) -> Option<Vec<BackrunHopRef>> {
    if hops.is_empty() {
        return None;
    }
    let last = hops.len() - 1;
    let mut in_addr = weth;
    let mut out = Vec::with_capacity(hops.len());
    for (i, h) in hops.iter().enumerate() {
        let zfo = h.token0 == in_addr;
        if !zfo && h.token1 != in_addr {
            return None;
        }
        let out_addr = if zfo { h.token1 } else { h.token0 };
        out.push(BackrunHopRef {
            pool_id: h.workspace_pool_id,
            pool: h.pool,
            token0: h.token0,
            token1: h.token1,
            zfo,
            family: h.family,
        });
        if i == last {
            if out_addr != weth {
                return None;
            }
        } else {
            in_addr = out_addr;
        }
    }
    Some(out)
}

/// The touched pins a cycle carries: how many of its hop pools anchor a
/// touched pool this frame. The per-cycle companion to the frame's pin
/// count, so a cycle riding several touched pools is legible in the trace.
#[must_use]
pub fn cycle_touched_legs(cycle: &DfsCycle, touched: &[AnchorPool]) -> usize {
    cycle
        .pools
        .iter()
        .filter(|key| {
            touched
                .iter()
                .any(|a| a.pool_id == key.0 && a.pool_kind == key.1)
        })
        .count()
}

/// One admissible WETH-entry cycle's walker outcome: the executable chain
/// (when the pool resolution closed) and the admission counters.
#[derive(Debug, Default)]
struct CycleWalk {
    chain: Option<Vec<BackrunHopRef>>,
    admitted: usize,
    unsupported_hop: usize,
}

/// Resolve every hop of one WETH-entry cycle against the frame's workspace. A
/// hop whose pool the frame already admitted (a touched pool) reuses its
/// workspace id; every other hop is admitted from its index edge through the
/// same [`admit_hop_pool`] ingress admission the mids use. A cycle whose resolution fails
/// (unresolvable hop, unadmittable pool, non-closing traversal) yields no
/// chain, never a guess.
#[expect(
    clippy::too_many_arguments,
    reason = "the workspace, index, solver, and provider are distinct seams the pipeline threads"
)]
async fn cycle_chain(
    rt: &MarketContext,
    idx: &V2ConnectorIndex,
    affected: &HbMap<u64, &AffectedPool>,
    cycle: &DfsCycle,
    weth_id: u64,
    scratch: &mut ScratchEvm<ScratchDb<'_>>,
    solver: &mut BackrunSolver,
    provider: &AlloyProvider,
    head: u64,
    trace_tx: &str,
) -> CycleWalk {
    let mut walk = CycleWalk::default();
    let Some(weth_addr) = rt.token_addr(weth_id) else {
        return walk;
    };
    let mut hops = Vec::with_capacity(cycle.pools.len());
    for key in &cycle.pools {
        if let Some(a) = affected.get(&key.0) {
            hops.push(CycleHop {
                workspace_pool_id: a.workspace_pool_id,
                pool: a.address,
                token0: a.token0,
                token1: a.token1,
                family: a.family,
            });
            continue;
        }
        let h = match resolve_hop(idx, *key) {
            Ok(Some(h)) => h,
            Ok(None) => return walk,
            Err(hop) => {
                walk.unsupported_hop += 1;
                trace_jsonl(
                    "unsupported_hop",
                    serde_json::json!({
                        "tx": trace_tx,
                        "pool_id": hop.pool_id,
                        "kind": format!("{:?}", hop.kind),
                    }),
                );
                return walk;
            }
        };
        let (Some(token0), Some(token1)) =
            (rt.token_addr(h.token0_id()), rt.token_addr(h.token1_id()))
        else {
            return walk;
        };
        let Some(ws) = admit_hop_pool(rt, solver, scratch, provider, &h, head, trace_tx).await
        else {
            return walk;
        };
        walk.admitted += 1;
        hops.push(CycleHop {
            workspace_pool_id: ws,
            pool: *h.address(),
            token0,
            token1,
            family: match h {
                ResolvedHop::V2(e) => {
                    let Ok(fees) = e.fees.resolve() else {
                        return walk;
                    };
                    LaneFamily::V2 { fees }
                }
                ResolvedHop::V3(e) => LaneFamily::V3 { fee: e.fee },
            },
        });
    }
    walk.chain = cycle_refs(&hops, weth_addr);
    walk
}

// ─────────────────────────────────────────────────────────────────────────
// The backrun strategy (the pending-tx seam's first implementation)
// ─────────────────────────────────────────────────────────────────────────

/// One strategy that reacts to observed pending transactions by backrunning
/// them: it settles a target's pool-state dislocation through a supported
/// quote and bids the surplus.
#[derive(Debug)]
pub struct BackrunStrategy {
    cmd_executor: CmdExecutorAdapter,
}

impl BackrunStrategy {
    #[must_use]
    pub fn new(context: EncodeContext) -> Self {
        Self {
            cmd_executor: CmdExecutorAdapter::new(context),
        }
    }
}

/// The pools one frame admitted into its fresh workspace.
pub type BackrunAffected = Vec<AffectedPool>;

/// The discovery output for one frame: the WETH-closing chains to solve, plus
/// whether a supported-quote fan was dropped for lack of a WETH lane.
pub struct BackrunIntents {
    pub chains: Vec<Vec<BackrunHopRef>>,
    /// Per-chain touched-pool count, aligned with `chains`, so the solve
    /// trace reports how many of a chain's hops pinned a touched pool.
    pub touched_legs: Vec<usize>,
    pub non_base_quote_dropped: bool,
    /// `true` when the context carries no discovery graph or WETH join: the
    /// frame ran no discovery at all and is observed with `no_candidate`.
    pub bailed: bool,
}

impl BackrunIntents {
    fn bailed() -> Self {
        Self {
            chains: Vec::new(),
            touched_legs: Vec::new(),
            non_base_quote_dropped: false,
            bailed: true,
        }
    }
}

/// The `discover` JSONL payload built from the frame's counters. `touched`
/// is the frame's pin set and `admitted_cycles` the WETH-entry cycles that
/// cleared admission, so the pinned-pool and multi-pinned fields are derived
/// here rather than at each call site; the offline harness asserts this
/// boundary because the stage itself needs the production scratch stack.
#[expect(
    clippy::too_many_arguments,
    reason = "one argument per discovery counter the trace publishes"
)]
#[must_use]
pub fn discover_trace_payload(
    trace_tx: &str,
    connectors: usize,
    dfs_cycles: usize,
    dfs_chains: usize,
    unsupported_hop: usize,
    affected: usize,
    non_weth_cycles: usize,
    non_base_quote_dropped: bool,
    cycle_max_hops: usize,
    touched: &[AnchorPool],
    admitted_cycles: &[DfsCycle],
) -> serde_json::Value {
    let cycles_with_multi_touched = admitted_cycles
        .iter()
        .filter(|c| cycle_touched_legs(c, touched) > 1)
        .count();
    serde_json::json!({
        "tx": trace_tx,
        "connectors": connectors,
        "cycles_proposed": dfs_cycles,
        "dfs_cycles": dfs_cycles,
        "dfs_chains": dfs_chains,
        "unsupported_hop": unsupported_hop,
        "affected": affected,
        "non_weth_cycles": non_weth_cycles,
        "cycle_reject": NON_WETH_CYCLE,
        "non_base_quote_dropped": non_base_quote_dropped,
        "cycle_max_hops": cycle_max_hops,
        "touched_pools": touched.len(),
        "cycles_with_multi_touched": cycles_with_multi_touched,
    })
}

/// The solved frame: the aggregate solve stats behind the best candidate.
pub struct BackrunEvaluated {
    pub stats: SolveStats,
}

impl PendingTxReaction for BackrunStrategy {
    type Affected = BackrunAffected;
    type Intents = BackrunIntents;
    type Evaluated = BackrunEvaluated;

    fn name(&self) -> &'static str {
        "backrun"
    }

    fn affected_is_empty(affected: &Self::Affected) -> bool {
        affected.is_empty()
    }

    async fn admit(
        &mut self,
        ctx: &MarketContext,
        workspace: &mut BackrunSolver,
        states: &[PoolPostState],
        seed_block: u64,
        trace_tx: &str,
        tick_window: Option<&dyn V3TickWindow>,
    ) -> Self::Affected {
        admit_extracted_verified(ctx, workspace, states, seed_block, trace_tx, tick_window).await
    }

    async fn discover(
        &mut self,
        ctx: &MarketContext,
        workspace: &mut BackrunSolver,
        scratch: &mut ScratchEvm<ScratchDb<'_>>,
        provider: &AlloyProvider,
        affected: &Self::Affected,
        head: u64,
        trace_tx: &str,
    ) -> Self::Intents {
        let Some(idx) = ctx.index() else {
            return BackrunIntents::bailed();
        };
        let Some(weth_id) = ctx.token_id(WETH) else {
            return BackrunIntents::bailed();
        };
        // Every cycle this instance admits is staked in WETH, so an unknown
        // settlement token leaves nothing admissible. Discovery is per-frame
        // over the touched set; WETH gates cycle admission, not pool
        // membership, since a touched pool with no WETH quote is still a
        // legal mid-cycle hop.
        let mut non_base_quote_dropped = false;
        let mut dfs_chains: Vec<Vec<BackrunHopRef>> = Vec::new();
        let mut chain_touched_legs: Vec<usize> = Vec::new();
        let mut dfs_cycles = 0usize;
        let mut non_weth_cycles = 0usize;
        let mut connectors_seen = 0usize;
        let mut unsupported_hop = 0usize;
        let mut touched: Vec<AnchorPool> = Vec::with_capacity(affected.len());
        let mut admitted_cycles: Vec<DfsCycle> = Vec::new();
        if let Some(graph) = ctx.dfs() {
            // Every touched pool pins on BOTH of its token pairs: a pool with
            // no WETH quote is still a legal mid-cycle hop.
            for a in affected {
                let (Some(token0_id), Some(token1_id)) =
                    (ctx.token_id(a.token0), ctx.token_id(a.token1))
                else {
                    continue;
                };
                touched.push(AnchorPool {
                    pool_id: a.index_pool_id,
                    pool_kind: PoolKind::from(a.family.tag()),
                    token_a_id: token0_id,
                    token_b_id: token1_id,
                });
            }

            // Cycle hop cap: the pin plus up to `cycle_max_hops - 1`
            // connectors (operator-set, minimum 2). Admission rotates each
            // cycle to its WETH stake entry; a no-WETH cycle is refused before
            // it can consume a cap slot.
            let (cycles, non_weth) = graph.weth_entry_cycles(
                &touched,
                weth_id,
                ctx.connector_cap.max(1),
                ctx.cycle_max_hops,
            );
            non_weth_cycles = non_weth;
            dfs_cycles = cycles.len() + non_weth;
            admitted_cycles = cycles;
            let mut affected_by_index: HbMap<u64, &AffectedPool> =
                HbMap::with_capacity(affected.len());
            for a in affected {
                affected_by_index.insert(a.index_pool_id, a);
            }

            // Each WETH-entry directed cycle solves once, whichever touched
            // pool anchored its discovery.
            let mut solved: HashSet<Vec<(u64, bool)>> = HashSet::new();
            for cycle in &admitted_cycles {
                let walk = cycle_chain(
                    ctx,
                    idx,
                    &affected_by_index,
                    cycle,
                    weth_id,
                    scratch,
                    workspace,
                    provider,
                    head,
                    trace_tx,
                )
                .await;
                connectors_seen += walk.admitted;
                unsupported_hop += walk.unsupported_hop;
                let Some(chain) = walk.chain else {
                    continue;
                };
                if !solved.insert(chain.iter().map(|h| (h.pool_id, h.zfo)).collect()) {
                    continue;
                }
                chain_touched_legs.push(cycle_touched_legs(cycle, &touched));
                dfs_chains.push(chain);
            }

            // The non-base-quote label is now a cycle-level verdict: it may
            // fire only when no WETH-entered cycle was admitted at all.
            let has_non_weth_quote_pool = affected
                .iter()
                .any(|a| a.quotes.iter().all(|q| q.quote != WETH));
            non_base_quote_dropped = has_non_weth_quote_pool && admitted_cycles.is_empty();
        }
        trace_jsonl(
            "discover",
            discover_trace_payload(
                trace_tx,
                connectors_seen,
                dfs_cycles,
                dfs_chains.len(),
                unsupported_hop,
                affected.len(),
                non_weth_cycles,
                non_base_quote_dropped,
                ctx.cycle_max_hops,
                &touched,
                &admitted_cycles,
            ),
        );
        BackrunIntents {
            chains: dfs_chains,
            touched_legs: chain_touched_legs,
            non_base_quote_dropped,
            bailed: false,
        }
    }

    fn evaluate(
        &mut self,
        workspace: &mut BackrunSolver,
        intents: Self::Intents,
        pl: &PipelineConfig,
        trace_tx: &str,
    ) -> Self::Evaluated {
        if intents.bailed {
            return BackrunEvaluated {
                stats: SolveStats::default(),
            };
        }
        let mut aggregate = SolveStats::default();
        if !intents.chains.is_empty() {
            let mut dfs_stats = solve_dfs_chains(workspace, &intents.chains, pl.gas_floor_wei);
            aggregate.dfs_declared += dfs_stats.dfs_declared;
            aggregate.dfs_evaluated += dfs_stats.dfs_evaluated;
            if dfs_stats.best.as_ref().is_some_and(|b| {
                aggregate
                    .best
                    .as_ref()
                    .is_none_or(|best| b.profit > best.profit)
            }) {
                aggregate.best = dfs_stats.best.take();
            }
            aggregate.chains.append(&mut dfs_stats.chains);
        }
        aggregate.non_base_quote_dropped |= intents.non_base_quote_dropped;
        trace_jsonl(
            "solve",
            serde_json::json!({
                "tx": trace_tx,
                "chains": aggregate
                    .chains
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let deficit_reasons = match &c.reject {
                            Some(PathReject::UnusablePoolState { reasons, .. }) => {
                                (!reasons.is_empty()).then(|| reasons.clone())
                            }
                            _ => None,
                        };
                        serde_json::json!({
                        "pools": c
                            .pools
                            .iter()
                            .map(|a| format!("0x{}", alloy::hex::encode(a)))
                            .collect::<Vec<_>>(),
                        "touched_legs": intents.touched_legs.get(i).copied().unwrap_or(0),
                        "evaluated": c.evaluated,
                        "profit_wei": c.profit_wei.map(|p| p.to_string()),
                        "reject": c.reject.as_ref().map(PathReject::label),
                        "reject_deficits": match c.reject {
                            Some(PathReject::UnusablePoolState { deficits, .. }) => Some(deficits),
                            _ => None,
                        },
                        "reject_deficit_reasons": deficit_reasons,
                        "reject_deficit_pools": match &c.reject {
                            Some(PathReject::UnusablePoolState { pools, .. }) => {
                                (!pools.is_empty()).then(|| pools.clone())
                            }
                            _ => None,
                        },
                        })
                    })
                    .collect::<Vec<_>>(),
                "best": aggregate.best.is_some(),
                "best_profit_wei": aggregate.best.as_ref().map(|b| b.profit.to_string()),
                "dfs_declared": aggregate.dfs_declared,
                "dfs_evaluated": aggregate.dfs_evaluated,
                "non_base_quote_dropped": aggregate.non_base_quote_dropped,
            }),
        );
        BackrunEvaluated { stats: aggregate }
    }

    fn compose(
        &mut self,
        evaluated: &Self::Evaluated,
        pl: &PipelineConfig,
        trace_tx: &str,
    ) -> Option<ComposedIntent> {
        let best = evaluated.stats.best.clone()?;
        let (path, result) = project_candidate_for_cmd_executor(&best);
        let outcome =
            self.cmd_executor
                .compose(&path, &result, backrun_encode_options(pl.bribe_bips));
        cmd_executor_bytes(outcome, trace_tx).map(|sim_calldata| ComposedIntent {
            sim_calldata,
            profit_wei: best.profit,
            optimal_input_wei: best.optimal_input,
        })
    }

    fn decide(
        &self,
        knobs: &BackrunConfig,
        pl: &PipelineConfig,
        evaluated: &Self::Evaluated,
        composed: Option<&ComposedIntent>,
        sim_ok: bool,
        spent: U256,
        trace_tx: &str,
    ) -> Decided {
        let best = evaluated.stats.best.as_ref();
        let mut requested_bid = U256::ZERO;
        let mut submit_calldata = None;
        let mut economics = None;
        let mut net_gated = false;
        let mut fixture_composed = false;
        if let Some(best) = best {
            if composed.is_some() {
                if pl.fixture_mode {
                    // Historical mode: the live bundle sim is skipped. The
                    // wallet gate still runs: whether the frame WOULD have
                    // bid is part of the historical answer.
                    let cap = u128::try_from(knobs.max_bundle_wei).unwrap_or(u128::MAX);
                    if net_bid(best.profit, pl.wallet_gas_cost(), pl.bribe_bips, cap).is_none() {
                        net_gated = true;
                    }
                    fixture_composed = true;
                } else if sim_ok {
                    // The wallet economics gate: the wallet funds only the
                    // gas (the bribe is drawn from flash proceeds on-chain).
                    let wallet_gas_cost = pl.wallet_gas_cost();
                    let bid_cap_wei = u128::try_from(knobs.max_bundle_wei).unwrap_or(u128::MAX);
                    if let Some(nb) =
                        net_bid(best.profit, wallet_gas_cost, pl.bribe_bips, bid_cap_wei)
                    {
                        // Re-compose the config word with the wallet-true
                        // bips. The recomposed bips are only LOWER than the
                        // ceiling the sim passed: a smaller bribe strictly
                        // eases the executor on-chain profit check, so the
                        // passed sim stays valid.
                        let (path, result) = project_candidate_for_cmd_executor(best);
                        let outcome = self.cmd_executor.compose(
                            &path,
                            &result,
                            backrun_encode_options(nb.bribe_bips),
                        );
                        if let Some(cd_net) = cmd_executor_bytes(outcome, trace_tx) {
                            requested_bid = nb.bid_wei.max(U256::from(1));
                            submit_calldata = Some(cd_net);
                            economics = Some(BidEconomics {
                                gross_profit_wei: best.profit,
                                wallet_gas_cost_wei: wallet_gas_cost,
                                bribe_bips: nb.bribe_bips,
                                bid_wei: nb.bid_wei,
                            });
                        }
                    } else {
                        net_gated = true;
                        trace_jsonl(
                            "composed",
                            serde_json::json!({
                                "tx": trace_tx,
                                "composed": false,
                                "reason": "net_after_gas_unprofitable",
                                "gross_profit_wei": best.profit,
                                "wallet_gas_cost_wei": wallet_gas_cost,
                            }),
                        );
                    }
                }
            }
        }
        let composed_any = submit_calldata.is_some();
        // The classifier is off the hot path: a frame that reached the
        // decision stage IS actionable — the sentinel routes decide()
        // through its actionable arm without consulting degenbot_decoders.
        let class = TargetClass::Swap(Vec::new());
        let decision = decide(
            knobs,
            knobs.stop_file.exists(),
            &class,
            composed_any,
            requested_bid,
            spent,
        );
        // The observe label tells the truth: a frame that never composed a
        // candidate must not read as "the sim rejected our work".
        let non_base_quote_dropped = evaluated.stats.non_base_quote_dropped;
        let solved_any = best.is_some();
        let decision = if composed_any {
            decision
        } else if net_gated {
            Decision::Observe {
                reason: "net_after_gas_unprofitable",
            }
        } else if fixture_composed {
            Decision::Observe {
                reason: "sim_skipped_fixture_mode",
            }
        } else {
            match decision {
                Decision::Observe {
                    reason: "sim_gate_failed",
                } => Decision::Observe {
                    reason: honest_observe("no_candidate", non_base_quote_dropped, solved_any),
                },
                Decision::Observe { reason } => Decision::Observe {
                    reason: honest_observe(reason, non_base_quote_dropped, solved_any),
                },
                d => d,
            }
        };
        Decided {
            decision,
            requested_bid,
            submit_calldata,
            economics,
        }
    }
}
