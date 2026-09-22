//! The backrun strategy — the first [`PendingTxReaction`] implementation.
//!
//! Given a pending transaction's recovered pool post-states, [`BackrunStrategy`]
//! admits the pools that settle through a supported quote, walks the anchored
//! connector graph for WETH-closing cycles, solves them against the fresh
//! workspace, and prices the best candidate into the wallet-true bid the
//! driver simulates and submits.

use std::time::Duration;

use crate::backrun::{decide, BackrunConfig, Decision};
use crate::backrun_engine::{
    compose_candidate, BackrunHopRef, BackrunSolver, BackrunV2Pool, LaneCandidate, LaneFamily,
    PathReject,
};
use alloy::primitives::{address, Address, B256, U256};
use degenbot_bot::connector_index::V2ConnectorIndex;
use degenbot_decoders::target_class::TargetClass;
use degenbot_executor::encoders::V4_FEE_ENCODER_MAX;
use degenbot_pathfinding::PoolKind;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_pools::{slot_layout, v3_storage_slots, v4_storage_slots, TickInfo};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost,
};
use degenbot_simulation::sim::evm::{read_view_word, ScratchDb, ScratchEvm};
use hashbrown::HashMap as HbMap;

use crate::anchored_dfs::{resolve_hop, AnchorPool, DfsCycle, DiscoveryBudget, ResolvedHop};
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

/// The per-frame discovery slice: how long the anchored walker may grind on
/// one frame before its cancel flag fires. The slice bounds only the walker
/// (its churn is checked between yields) — never the solve/sim stages.
const FRAME_DISCOVERY_SLICE: Duration = Duration::from_millis(2);
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
#[expect(
    clippy::too_many_lines,
    reason = "the V2/V3/V4 admission arms read top-to-bottom per family"
)]
#[must_use]
pub fn admit_extracted(
    rt: &MarketContext,
    solver: &mut BackrunSolver,
    states: &[PoolPostState],
    seed_block: u64,
    trace_tx: &str,
    tick_window: Option<&dyn V3TickWindow>,
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
                let Ok(p_id) = solver.admit_v2(&BackrunV2Pool {
                    address: st.address,
                    token0,
                    token1,
                    reserve0: r0,
                    reserve1: r1,
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
                        family: LaneFamily::V2,
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
                // then rejects `unusable_pool_state`, deficits=1). Merge the
                // in-range initialized-tick window from the same chain view
                // the frames replay over — swaps never change a tick's stored
                // net/gross, so the window is valid post-frame. Replayed
                // touched ticks win (they are the post-frame facts).
                if let Some(window) = tick_window {
                    for (tick, info) in
                        window.tick_window(st.address, *layout, spacing, tk, seed_block)
                    {
                        tick_data.entry(tick).or_insert(info);
                    }
                }
                let Some(p_id) = solver.admit_v3_explicit(
                    st.address, token0, token1, edge.fee, spacing, sqrt, liq, tk, tick_data,
                    seed_block,
                ) else {
                    skip(st.address, "v3-admit");
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
                // The V4 twin of the V3 anchor merge: the replayed journal
                // carries only the ticks the target CROSSED, so seed the
                // in-range window from the same chain view the frames replay
                // over (manager-addressed `poolId`-derived bases).
                if let Some(window) = tick_window {
                    for (tick, info) in window.v4_tick_window(
                        st.address,
                        *pool_id,
                        edge.tick_spacing,
                        tk,
                        seed_block,
                    ) {
                        tick_data.entry(tick).or_insert(info);
                    }
                }
                let Some(p_id) = solver.admit_v4_explicit(
                    st.address,
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
                ) else {
                    skip(st.address, "v4-admit");
                    continue;
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

/// Read a V3 pool's in-range tick window through a layered [`DatabaseRef`]
/// view: the bitmap words at `current_tick` ± 1, then the initialized tick
/// words those bitmaps select (the same window as the raw-RPC bootstrap
/// ladder). Swaps never change a tick's stored net/gross, so this window is
/// valid for both the pre-frame chain view and a replayed post-target tick.
///
/// The replayed journal only carries ticks the target CROSSED. A shallow
/// target swap touches none, leaving a map too sparse to build a solver range
/// sequence — anchor admission sources the window here instead.
fn read_v3_tick_window(
    ext: &ScratchDb<'_>,
    pool: Address,
    layout: ClSlotLayout,
    tick_spacing: i32,
    current_tick: i32,
    head: u64,
) -> HbMap<i32, TickInfo> {
    let spacing = i64::from(tick_spacing.max(1));
    let (word_pos, _) = floor_word_pos(current_tick, spacing);
    let w0 = i64::from(word_pos).saturating_sub(1);
    let w1 = i64::from(word_pos).saturating_add(1);
    let mut tick_data = HbMap::default();
    for w in w0..=w1 {
        let Ok(word_pos_i16) = i16::try_from(w) else {
            continue;
        };
        let slot = slot_layout::cl_tick_bitmap_word_slot(layout, word_pos_i16);
        let Some(bitmap) = read_view_word(ext, pool, slot) else {
            continue;
        };
        for bit in 0..256i64 {
            let Ok(bit_u256) = U256::try_from(bit) else {
                continue;
            };
            if (bitmap >> bit_u256) & U256::ONE != U256::ONE {
                continue;
            }
            let tick = i64::from(word_pos_i16) * 256 + bit;
            let Some(tick_scaled) = tick.checked_mul(spacing) else {
                continue;
            };
            let Ok(tick_i32) = i32::try_from(tick_scaled) else {
                continue;
            };
            let Some(word) = read_view_word(
                ext,
                pool,
                slot_layout::cl_tick_mapping_slot(layout, tick_i32),
            ) else {
                continue;
            };
            let (gross, net) = slot_layout::decode_tick_word(word);
            tick_data.insert(
                tick_i32,
                TickInfo {
                    liquidity_gross: alloy::primitives::aliases::U128::from(gross),
                    liquidity_net: net,
                    block: head,
                },
            );
        }
    }
    tick_data
}

/// Read a V4 pool's in-range tick window through a layered [`DatabaseRef`]
/// view: the bitmap words at `current_tick` ± 1 at the `poolId`-derived
/// `tickBitmap` base under the `PoolManager` singleton, then the initialized
/// tick words those bitmaps select. The V4 twin of [`read_v3_tick_window`]:
/// same window shape, manager-addressed `keccak` bases.
fn read_v4_tick_window(
    ext: &ScratchDb<'_>,
    manager: Address,
    pool_id: B256,
    tick_spacing: i32,
    current_tick: i32,
    head: u64,
) -> HbMap<i32, TickInfo> {
    let base = v4_storage_slots::v4_pool_state_base_slot(pool_id);
    let spacing = i64::from(tick_spacing.max(1));
    let (word_pos, _) = floor_word_pos(current_tick, spacing);
    let w0 = i64::from(word_pos).saturating_sub(1);
    let w1 = i64::from(word_pos).saturating_add(1);
    let mut tick_data = HbMap::default();
    for w in w0..=w1 {
        let Ok(word_pos_i16) = i16::try_from(w) else {
            continue;
        };
        let slot = v4_storage_slots::v4_tick_bitmap_word_slot(word_pos_i16, base);
        let Some(bitmap) = read_view_word(ext, manager, slot) else {
            continue;
        };
        for bit in 0..256i64 {
            let Ok(bit_u256) = U256::try_from(bit) else {
                continue;
            };
            if (bitmap >> bit_u256) & U256::ONE != U256::ONE {
                continue;
            }
            let tick = i64::from(word_pos_i16) * 256 + bit;
            let Some(tick_scaled) = tick.checked_mul(spacing) else {
                continue;
            };
            let Ok(tick_i32) = i32::try_from(tick_scaled) else {
                continue;
            };
            let Some(word) = read_view_word(
                ext,
                manager,
                v4_storage_slots::v4_tick_mapping_slot(tick_i32, base),
            ) else {
                continue;
            };
            let (gross, net) = slot_layout::decode_tick_word(word);
            tick_data.insert(
                tick_i32,
                TickInfo {
                    liquidity_gross: alloy::primitives::aliases::U128::from(gross),
                    liquidity_net: net,
                    block: head,
                },
            );
        }
    }
    tick_data
}

impl V3TickWindow for ScratchDb<'_> {
    fn tick_window(
        &self,
        pool: Address,
        layout: ClSlotLayout,
        tick_spacing: i32,
        current_tick: i32,
        head: u64,
    ) -> HbMap<i32, TickInfo> {
        read_v3_tick_window(self, pool, layout, tick_spacing, current_tick, head)
    }

    fn v4_tick_window(
        &self,
        manager: Address,
        pool_id: B256,
        tick_spacing: i32,
        current_tick: i32,
        head: u64,
    ) -> HbMap<i32, TickInfo> {
        read_v4_tick_window(self, manager, pool_id, tick_spacing, current_tick, head)
    }
}

/// V3 CL state read through the frame-replay chain view: slot0, liquidity,
/// and the in-range tick window (the same window as the RPC bootstrap
/// ladder).
///
/// `None` on a failed read or a zero in-range liquidity (unsolvable CL state
/// — the raw-RPC ladder decides).
fn read_v3_view(
    scratch: &mut ScratchEvm<ScratchDb<'_>>,
    pool: Address,
    layout: ClSlotLayout,
    tick_spacing: i32,
    head: u64,
) -> Option<(U256, i32, u128, HbMap<i32, TickInfo>)> {
    let slot0 = read_view_word(scratch.ext(), pool, U256::ZERO)?;
    let parts = v3_storage_slots::decode_v3_slot0(slot0);
    let liq_word = read_view_word(scratch.ext(), pool, U256::from(layout.liquidity_slot()))?;
    let liquidity = (liq_word & U256::from(u128::MAX)).to::<u128>();
    if liquidity == 0 {
        return None;
    }
    let tick_data =
        read_v3_tick_window(scratch.ext(), pool, layout, tick_spacing, parts.tick, head);
    Some((parts.sqrt_price_x96, parts.tick, liquidity, tick_data))
}

/// `(word position, bit position)` per the CL bitmap layout —
/// `compressed = floor_div(tick / spacing)`; `word = compressed >> 8`,
/// `bit = compressed & 0xFF`. Local floor-division twin of
/// `degenbot_math::cl::liquidity_mapping` (kept inline so this crate needs
/// no extra edge).
fn floor_word_pos(tick: i32, spacing: i64) -> (i16, u16) {
    let compressed = floor_div_i64(i64::from(tick), spacing);
    (
        i16::try_from(compressed >> 8).unwrap_or(0),
        u16::try_from(compressed & 0xFF).unwrap_or(0),
    )
}

fn floor_div_i64(a: i64, b: i64) -> i64 {
    let q = a / b;
    if a % b != 0 && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
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

/// The hop refs of one WETH-entry 3-hop walker cycle:
/// `WETH →(anchor) tok →(h1) mid →(h2) WETH`. `None` unless the resolved
/// bridges straddle exactly that token path — the cycle came from the
/// walker, and this re-derives its traversal from index identity instead
/// of trusting orientation.
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
            }) {
                Ok(id) => Some(id),
                Err(reason) => {
                    trace_admit_fail(trace_tx, address, "admit-v2", &reason);
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
            if let Some((sqrt, tk, liq, tick_data)) = read_v3_view(
                scratch,
                e.address,
                ClSlotLayout::UniswapV3,
                e.tick_spacing,
                head,
            ) {
                solver.admit_v3_explicit(
                    e.address,
                    token0,
                    token1,
                    e.fee,
                    e.tick_spacing,
                    sqrt,
                    liq,
                    tk,
                    tick_data,
                    head,
                )
            } else {
                let admitted = solver
                    .admit_v3_full(
                        provider,
                        e.address,
                        token0,
                        token1,
                        e.fee,
                        e.tick_spacing,
                        None,
                        head,
                    )
                    .await;
                if admitted.is_none() {
                    trace_admit_fail(trace_tx, address, "admit-v3", "view + full ladder failed");
                }
                admitted
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

/// The hop refs of one WETH-entry walker cycle of arbitrary depth >= 2,
/// anchored at `a`: quote →(anchor) `first_out` →(mids...) quote. Each mid hop
/// is re-oriented from index identity (the walker's traversal is re-derived,
/// never trusted). `None` unless the mid chain straddles a contiguous token
/// path back to the quote token.
///
/// `mids` carries one entry per non-anchor pool in traversal order:
/// (`out_token_id`, `out_token_address`, resolved pool, admitted workspace id),
/// where "out" is the token the frame holds AFTER that hop.
#[must_use]
pub fn n_hop_refs(
    a: &AffectedPool,
    quote_id: u64,
    quote_addr: Address,
    first_out_id: u64,
    first_out_addr: Address,
    mids: &[(u64, Address, ResolvedHop, u64)],
) -> Option<Vec<BackrunHopRef>> {
    if mids.is_empty() {
        return None;
    }
    let anchor = BackrunHopRef {
        pool_id: a.workspace_pool_id,
        pool: a.address,
        token0: a.token0,
        token1: a.token1,
        // The anchor hop consumes the quote: `zfo` ⇔ the input is the hop's token0.
        zfo: a.token0 == quote_addr,
        family: a.family,
    };
    let mut out = Vec::with_capacity(mids.len() + 1);
    out.push(anchor);
    let mut in_id = first_out_id;
    let mut in_addr = first_out_addr;
    let last_idx = mids.len() - 1;
    for (i, (out_id, out_addr, h, ws_id)) in mids.iter().enumerate() {
        let straddles = (h.token0_id() == in_id && h.token1_id() == *out_id)
            || (h.token1_id() == in_id && h.token0_id() == *out_id);
        if !straddles {
            return None;
        }
        let zfo = h.token0_id() == in_id;
        let (t0, t1) = if zfo {
            (in_addr, *out_addr)
        } else {
            (*out_addr, in_addr)
        };
        out.push(BackrunHopRef {
            pool_id: *ws_id,
            pool: *h.address(),
            token0: t0,
            token1: t1,
            zfo,
            family: match h {
                ResolvedHop::V2(_) => LaneFamily::V2,
                ResolvedHop::V3(e) => LaneFamily::V3 { fee: e.fee },
            },
        });
        if i == last_idx {
            // The final mid must close on the quote token.
            if *out_id != quote_id {
                return None;
            }
        } else {
            in_id = *out_id;
            in_addr = *out_addr;
        }
    }
    Some(out)
}

/// One affected pool's walker outcome: the connectors admitted, the solved
/// chains, and the cycles dropped because a hop's family has no index lane.
#[derive(Debug, Default)]
struct DfsWalk {
    admitted: usize,
    chains: Vec<Vec<BackrunHopRef>>,
    unsupported_hop: usize,
}

/// Resolve and admit every mid hop of each WETH-entry cycle of ANY depth the
/// anchored walker produced for this affected pool, returning the solved
/// chain refs. Cycles whose mid chain re-derivation fails (unresolvable hop,
/// unadmittable pool, straddle mismatch) are skipped, never guessed.
#[expect(
    clippy::too_many_arguments,
    reason = "the workspace, index, solver, and provider are distinct seams the pipeline threads"
)]
async fn dfs_cycle_chains(
    rt: &MarketContext,
    idx: &V2ConnectorIndex,
    a: &AffectedPool,
    wq: &AffectedQuote,
    cycles: &[DfsCycle],
    scratch: &mut ScratchEvm<ScratchDb<'_>>,
    solver: &mut BackrunSolver,
    provider: &AlloyProvider,
    head: u64,
    trace_tx: &str,
) -> DfsWalk {
    let mut chains = Vec::new();
    let mut admitted = 0usize;
    let mut unsupported_hop = 0usize;
    let Some(tok_addr) = rt.token_addr(wq.tok_id) else {
        return DfsWalk::default();
    };
    let Some(quote_addr) = rt.token_addr(wq.quote_id) else {
        return DfsWalk::default();
    };
    for cycle in cycles {
        if cycle.entry_token_id != wq.quote_id || cycle.pools.len() < 2 {
            continue;
        }
        let mut mids: Vec<(u64, Address, ResolvedHop, u64)> =
            Vec::with_capacity(cycle.pools.len() - 1);
        let mut in_id = wq.tok_id;
        let mut rejected = false;
        for key in &cycle.pools[1..] {
            let h = match resolve_hop(idx, *key) {
                Ok(Some(h)) => h,
                Ok(None) => {
                    rejected = true;
                    break;
                }
                Err(hop) => {
                    unsupported_hop += 1;
                    trace_jsonl(
                        "unsupported_hop",
                        serde_json::json!({
                            "tx": trace_tx,
                            "pool_id": hop.pool_id,
                            "kind": format!("{:?}", hop.kind),
                        }),
                    );
                    rejected = true;
                    break;
                }
            };
            let Some(out_id) = (if h.token0_id() == in_id {
                h.token1_id().into()
            } else if h.token1_id() == in_id {
                h.token0_id().into()
            } else {
                rejected = true;
                None
            }) else {
                break;
            };
            let Some(out_addr) = rt.token_addr(out_id) else {
                rejected = true;
                break;
            };
            let Some(ws) = admit_hop_pool(rt, solver, scratch, provider, &h, head, trace_tx).await
            else {
                rejected = true;
                break;
            };
            admitted += 1;
            mids.push((out_id, out_addr, h, ws));
            in_id = out_id;
        }
        if rejected {
            continue;
        }
        if let Some(chain) = n_hop_refs(a, wq.quote_id, quote_addr, wq.tok_id, tok_addr, &mids) {
            // Parity check: the re-derived traversal must close on the quote.
            if mids.last().is_some_and(|(id, _, _, _)| *id == wq.quote_id) {
                chains.push(chain);
            }
        }
    }
    DfsWalk {
        admitted,
        chains,
        unsupported_hop,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The backrun strategy (the pending-tx seam's first implementation)
// ─────────────────────────────────────────────────────────────────────────

/// One strategy that reacts to observed pending transactions by backrunning
/// them: it settles a target's pool-state dislocation through a supported
/// quote and bids the surplus.
#[derive(Debug, Default)]
pub struct BackrunStrategy;

impl BackrunStrategy {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// The pools one frame admitted into its fresh workspace.
pub type BackrunAffected = Vec<AffectedPool>;

/// The discovery output for one frame: the WETH-closing chains to solve, plus
/// whether a supported-quote fan was dropped for lack of a WETH lane.
pub struct BackrunIntents {
    pub chains: Vec<Vec<BackrunHopRef>>,
    pub non_base_quote_dropped: bool,
    /// `true` when the context carries no discovery graph or WETH join: the
    /// frame ran no discovery at all and is observed with `no_candidate`.
    pub bailed: bool,
}

impl BackrunIntents {
    fn bailed() -> Self {
        Self {
            chains: Vec::new(),
            non_base_quote_dropped: false,
            bailed: true,
        }
    }
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

    fn admit(
        &mut self,
        ctx: &MarketContext,
        workspace: &mut BackrunSolver,
        states: &[PoolPostState],
        seed_block: u64,
        trace_tx: &str,
        tick_window: Option<&dyn V3TickWindow>,
    ) -> Self::Affected {
        admit_extracted(ctx, workspace, states, seed_block, trace_tx, tick_window)
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
        if ctx.token_id(WETH).is_none() {
            return BackrunIntents::bailed();
        }
        // WETH-only instance: a frame whose affected pool trades no WETH
        // quote composes no candidate. The anchored walker owns discovery
        // entirely (2-hop parity and deep cycles both land in the DFS lane).
        let mut non_base_quote_dropped = false;
        let mut dfs_chains: Vec<Vec<BackrunHopRef>> = Vec::new();
        let mut dfs_cycles = 0usize;
        let mut connectors_seen = 0usize;
        let mut unsupported_hop = 0usize;
        if let Some(graph) = ctx.dfs.as_ref() {
            let budget = DiscoveryBudget::after(FRAME_DISCOVERY_SLICE);
            for a in affected {
                let Some(wq) = a.quotes.iter().find(|q| q.quote == WETH) else {
                    non_base_quote_dropped |= !a.quotes.is_empty();
                    continue;
                };
                let anchor = AnchorPool {
                    pool_id: a.index_pool_id,
                    pool_kind: match a.family {
                        LaneFamily::V2 => PoolKind::V2,
                        LaneFamily::V3 { .. } => PoolKind::V3,
                        LaneFamily::V4 { .. } => PoolKind::V4,
                    },
                    token_a_id: wq.quote_id,
                    token_b_id: wq.tok_id,
                };
                let cycles = graph.cycles_through_pool(anchor, &budget, ctx.connector_cap.max(1));
                dfs_cycles += cycles.len();
                let walk = dfs_cycle_chains(
                    ctx, idx, a, wq, &cycles, scratch, workspace, provider, head, trace_tx,
                )
                .await;
                connectors_seen += walk.admitted;
                dfs_chains.extend(walk.chains);
                unsupported_hop += walk.unsupported_hop;
                if budget.expired() {
                    break;
                }
            }
        }
        trace_jsonl(
            "discover",
            serde_json::json!({
                "tx": trace_tx,
                "connectors": connectors_seen,
                "cycles_proposed": dfs_cycles,
                "dfs_cycles": dfs_cycles,
                "dfs_chains": dfs_chains.len(),
                "unsupported_hop": unsupported_hop,
                "affected": affected.len(),
                "non_base_quote_dropped": non_base_quote_dropped,
            }),
        );
        BackrunIntents {
            chains: dfs_chains,
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
                    .map(|c| serde_json::json!({
                        "pools": c
                            .pools
                            .iter()
                            .map(|a| format!("0x{}", alloy::hex::encode(a)))
                            .collect::<Vec<_>>(),
                        "evaluated": c.evaluated,
                        "profit_wei": c.profit_wei.map(|p| p.to_string()),
                        "reject": c.reject.map(PathReject::label),
                        "reject_deficits": match c.reject {
                            Some(PathReject::UnusablePoolState { deficits }) => Some(deficits),
                            _ => None,
                        },
                    }))
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
        match compose_candidate(&best, pl.exec, WETH, pl.bribe_bips) {
            Ok(cd) => Some(ComposedIntent {
                sim_calldata: cd,
                profit_wei: best.profit,
                optimal_input_wei: best.optimal_input,
            }),
            Err(reject) => {
                trace_jsonl(
                    "composed",
                    serde_json::json!({
                        "tx": trace_tx,
                        "composed": false,
                        "reason": reject.label(),
                    }),
                );
                None
            }
        }
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
                        match compose_candidate(best, pl.exec, WETH, nb.bribe_bips) {
                            Ok(cd_net) => {
                                requested_bid = nb.bid_wei.max(U256::from(1));
                                submit_calldata = Some(cd_net);
                                economics = Some(BidEconomics {
                                    gross_profit_wei: best.profit,
                                    wallet_gas_cost_wei: wallet_gas_cost,
                                    bribe_bips: nb.bribe_bips,
                                    bid_wei: nb.bid_wei,
                                });
                            }
                            Err(reject) => {
                                trace_jsonl(
                                    "composed",
                                    serde_json::json!({
                                        "tx": trace_tx,
                                        "composed": false,
                                        "reason": reject.label(),
                                    }),
                                );
                            }
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
