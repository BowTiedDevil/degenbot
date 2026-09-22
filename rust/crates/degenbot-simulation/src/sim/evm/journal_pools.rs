//! Journal → typed pool post-state extractors — reverse decoders over the
//! frame-replay seam.
//!
//! Given a [`ReplayOutcome`]'s touched set + journalled words, recover the
//! typed post-state of every tracked pool the frame touched — V2 pair, V3
//! pool, or a V4 pool inside the `PoolManager` singleton. The V4 layout is
//! pinned in `docs/architecture/v4_poolmanager_storage_layout.md`
//! (`S_state = keccak256(abi.encode(poolId, 6))`, slot0 at `S_state`,
//! liquidity at `S_state+3`, ticks base `S_state+4`), so each KNOWN poolId
//! decodes through the shared `degenbot_pools` slot math. An empty known set
//! or slots matching no known poolId stay [`PoolPostKind::Unsupported`] —
//! never a guessed decode. This replaces the unbounded router-calldata
//! decode problem with a bounded per-pool-family one: one row table per
//! family, invariant across routers/aggregators, shared with the PACK
//! direction through `degenbot_pools::slot_layout`.
//!
//! # Output contract
//!
//! [`PoolPostState`] is consumed by both the admission and the validation
//! harness: pool address + family + typed fields + the touched per-tick
//! words. V3 tick indices are recovered from the journalled keccak
//! preimages by hashing `tick_spacing`-aligned candidates around the pre-tx
//! current tick hint and the post-tx current tick (decoded from the
//! journal's own slot0) — bounded, and exact because initialized ticks are
//! discrete. An unmatched slot fabricates NOTHING.
//!
//! Pool identity comes from the caller's descriptor map (the engine's
//! tracked-pool registry projects it); a touched address without a
//! descriptor contributes no state.
// Solidity/EVM identifiers (slot0, uint128, keccak, SSTORE, CacheDB) are
// ubiquitous here — match the frame_replay convention.
#![expect(clippy::doc_markdown)]

use std::sync::Arc;

use alloy::primitives::{Address, B256, U256};
use degenbot_pools::slot_layout;
use degenbot_pools::{v3_storage_slots, v4_storage_slots, ClSlotLayout};
use hashbrown::HashMap;

use super::frame_replay::ReplayOutcome;

/// The per-pool layout knowledge the extractor needs, projected from the
/// engine's tracked-pool registry by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolFamily {
    /// A V2 pair: reserves are packed at the layout table's slot 8.
    V2Pair,
    /// A concentrated-liquidity pool (V3-shaped): slot0/liquidity/per-tick
    /// words under the pool's fork layout, with the engine's pre-tx current
    /// tick as the preimage anchor (`None` = unknown, the journal's own
    /// slot0 then carries the only anchor).
    V3 {
        layout: ClSlotLayout,
        tick_spacing: i32,
        current_tick_hint: Option<i32>,
    },
    /// A V4 PoolManager singleton: pool state lives at keccak-derived
    /// per-poolId bases (`S_state = keccak256(abi.encode(poolId, 6))`). The
    /// KNOWN tracked pool set decodes per poolId; an empty set (the
    /// descriptor default until a caller can supply the index's known
    /// poolIds) or slots matching no known poolId returns
    /// [`PoolPostKind::Unsupported`] (skip it with
    /// `observe reason="v4_unsupported"`), never a guessed decode.
    V4PoolManager { pools: V4PoolSet },
}

/// One tracked V4 pool the singleton manages: the `poolId` the `_pools`
/// mapping is keyed by, plus the `tick_spacing` the tick-preimage hunt
/// aligns candidates to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V4PoolDescriptor {
    pub pool_id: B256,
    pub tick_spacing: i32,
}

/// The tracked V4 pool set a [`PoolFamily::V4PoolManager`] descriptor
/// carries. [`Default`] is the EMPTY set: the extractor then reports the
/// family [`PoolPostKind::Unsupported`], preserving the explicit-unsupported
/// behavior until a caller supplies the index's known poolIds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct V4PoolSet {
    pools: Arc<[V4PoolDescriptor]>,
}

impl V4PoolSet {
    #[must_use]
    pub fn new(pools: Vec<V4PoolDescriptor>) -> Self {
        Self {
            pools: pools.into(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pools.is_empty()
    }

    #[must_use]
    pub fn as_slice(&self) -> &[V4PoolDescriptor] {
        &self.pools
    }
}

/// One recovered per-tick word: the tick index (recovered from the keccak
/// preimage) + the decoded `liquidityGross`/`liquidityNet` fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchedTickWord {
    pub tick: i32,
    pub liquidity_gross: u128,
    pub liquidity_net: i128,
}

/// The typed post-state of one family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypedPoolPost {
    /// `reserves` decoded from the journalled slot-8 word (0-wei exact; the
    /// untracked `blockTimestampLast` reads through as 0 — the engine packs
    /// none).
    V2 {
        reserves: slot_layout::V2ReservesParts,
    },
    /// slot0 (`sqrtPriceX96` + `tick`), `liquidity`, and the touched per-tick
    /// words. `None` = that word was not touched by the frame.
    V3 {
        sqrt_price_x96: Option<U256>,
        tick: Option<i32>,
        liquidity: Option<u128>,
        touched_ticks: Vec<TouchedTickWord>,
    },
    /// A KNOWN V4 pool's `Pool.State`, decoded from the singleton's
    /// journalled slots at the poolId-derived bases. `None` = that scalar
    /// word was not touched; an empty `touched_ticks` = no recoverable tick
    /// word.
    V4 {
        pool_id: B256,
        sqrt_price_x96: Option<U256>,
        tick: Option<i32>,
        liquidity: Option<u128>,
        touched_ticks: Vec<TouchedTickWord>,
    },
}

/// Typed vs explicitly-unsupported extraction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolPostKind {
    Typed(TypedPoolPost),
    /// The frame touched ONLY state the extractor cannot decode — a V4
    /// `PoolManager` whose slots match no known poolId, or an empty known
    /// set. Skipped upstream with `observe reason="v4_unsupported"`, never
    /// guessed.
    Unsupported,
}

/// The extracted post-state of ONE touched tracked pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolPostState {
    /// The pool contract (V2 pair / V3 pool; a V4 descriptor keys the
    /// singleton `PoolManager`).
    pub address: Address,
    pub family: PoolFamily,
    pub kind: PoolPostKind,
}

/// Extract the typed post-state of every touched pool that appears in
/// `descriptors`, in `ReplayOutcome::touched` order (address-sorted).
///
/// Reads journalled words ONLY from `outcome.state` (the settled, revert-
/// filtered journal) — a reverted frame surfaces no pool state at all.
#[must_use]
pub fn extract_pool_post_states(
    outcome: &ReplayOutcome,
    descriptors: &HashMap<Address, PoolFamily>,
) -> Vec<PoolPostState> {
    let mut out = Vec::new();
    for (address, slots) in &outcome.touched {
        let Some(family) = descriptors.get(address) else {
            continue;
        };
        let journal_word = |slot: U256| {
            outcome
                .state
                .get(address)
                .and_then(|account| account.storage.get(&slot))
                .map(|s| s.present_value)
        };
        if let PoolFamily::V4PoolManager { pools } = family {
            let typed = extract_v4_pool_states(pools, slots, &journal_word);
            if typed.is_empty() {
                out.push(PoolPostState {
                    address: *address,
                    family: family.clone(),
                    kind: PoolPostKind::Unsupported,
                });
            } else {
                out.extend(typed.into_iter().map(|post| PoolPostState {
                    address: *address,
                    family: family.clone(),
                    kind: PoolPostKind::Typed(post),
                }));
            }
            continue;
        }
        let kind = match family {
            PoolFamily::V4PoolManager { .. } => unreachable!("V4 handled above"),
            PoolFamily::V2Pair => PoolPostKind::Typed(TypedPoolPost::V2 {
                reserves: journal_word(U256::from(slot_layout::V2_RESERVES_SLOT)).map_or_else(
                    || slot_layout::decode_v2_reserves_word(U256::ZERO),
                    slot_layout::decode_v2_reserves_word,
                ),
            }),
            PoolFamily::V3 {
                layout,
                tick_spacing,
                current_tick_hint,
            } => PoolPostKind::Typed(TypedPoolPost::V3 {
                sqrt_price_x96: journal_word(U256::ZERO)
                    .map(v3_storage_slots::decode_v3_slot0)
                    .map(|parts| parts.sqrt_price_x96),
                tick: journal_word(U256::ZERO)
                    .map(v3_storage_slots::decode_v3_slot0)
                    .map(|parts| parts.tick),
                liquidity: journal_word(U256::from(layout.liquidity_slot()))
                    .map(|w| (w & U256::from(u128::MAX)).to::<u128>()),
                touched_ticks: {
                    let post_tick =
                        journal_word(U256::ZERO).map(|w| v3_storage_slots::decode_v3_slot0(w).tick);
                    let mut anchors: Vec<i32> =
                        (*current_tick_hint).into_iter().chain(post_tick).collect();
                    anchors.sort_unstable();
                    anchors.dedup();
                    recover_touched_ticks(
                        slots,
                        U256::from(layout.ticks_mapping_slot()),
                        &[U256::ZERO, U256::from(layout.liquidity_slot())],
                        *tick_spacing,
                        &anchors,
                        &journal_word,
                    )
                },
            }),
        };
        out.push(PoolPostState {
            address: *address,
            family: family.clone(),
            kind,
        });
    }
    out
}

/// Decode every KNOWN V4 pool whose `Pool.State` slots appear in the frame's
/// journalled set. A pool matches only when one of its slot0/liquidity words
/// (or a recoverable tick preimage) is present — a frame that touched the
/// singleton for other reasons contributes nothing, leaving the caller to
/// report the family [`PoolPostKind::Unsupported`].
fn extract_v4_pool_states(
    pools: &V4PoolSet,
    slots: &[U256],
    journal_word: &impl Fn(U256) -> Option<U256>,
) -> Vec<TypedPoolPost> {
    let mut out = Vec::new();
    for pool in pools.pools.iter() {
        let base = v4_storage_slots::v4_pool_state_base_slot(pool.pool_id);
        let slot0_slot = v4_storage_slots::v4_slot0_slot(base);
        let liquidity_slot = v4_storage_slots::v4_liquidity_slot(base);
        let ticks_base = v4_storage_slots::v4_ticks_mapping_base_slot(base);

        let slot0_parts = journal_word(slot0_slot).map(v4_storage_slots::decode_v4_slot0);
        let liquidity =
            journal_word(liquidity_slot).map(|w| (w & U256::from(u128::MAX)).to::<u128>());
        let anchors: Vec<i32> = slot0_parts
            .as_ref()
            .map(|parts| parts.tick)
            .into_iter()
            .collect();
        let touched_ticks = recover_touched_ticks(
            slots,
            ticks_base,
            &[slot0_slot, liquidity_slot],
            pool.tick_spacing,
            &anchors,
            journal_word,
        );

        if slot0_parts.is_none() && liquidity.is_none() && touched_ticks.is_empty() {
            continue;
        }
        out.push(TypedPoolPost::V4 {
            pool_id: pool.pool_id,
            sqrt_price_x96: slot0_parts.as_ref().map(|parts| parts.sqrt_price_x96),
            tick: slot0_parts.as_ref().map(|parts| parts.tick),
            liquidity,
            touched_ticks,
        });
    }
    out
}

/// Recover the per-tick words among a pool's journalled slots: every slot
/// that preimages to a `tick_spacing`-aligned tick near one of `anchors`
/// (the pool's known current ticks) decodes to a [`TouchedTickWord`]; every
/// OTHER slot (tickBitmap words, fee growth, fee-growth-outside words) is
/// deliberately NOT decoded — nothing is fabricated.
///
/// `ticks_base` is the pool's `ticks(tick)` mapping base (a V3 layout base,
/// or a V4 pool's `S_state+4`); `skip_slots` are the pool's scalar slots
/// (slot0/liquidity) that must never be swept as tick preimages.
#[must_use]
fn recover_touched_ticks(
    slots: &[U256],
    ticks_base: U256,
    skip_slots: &[U256],
    tick_spacing: i32,
    anchors: &[i32],
    journal_word: &impl Fn(U256) -> Option<U256>,
) -> Vec<TouchedTickWord> {
    let spacing = tick_spacing.max(1);
    let mut anchors: Vec<i32> = anchors.to_vec();
    anchors.sort_unstable();
    anchors.dedup();
    // Every touched tick word of a swap lies between (plus one spacing past)
    // the anchors, so the window covers |post - pre| plus a floor for
    // frames that never moved slot0.
    let spread = match (anchors.first(), anchors.last()) {
        (Some(&low), Some(&high)) => i64::from(high - low),
        _ => 0,
    };
    let window: i64 = spread + 4 * i64::from(spacing) + 4_096;

    let mut ticks = Vec::new();
    for slot in slots {
        if *slot == U256::ZERO || skip_slots.contains(slot) {
            continue;
        }
        let Some(tick) = slot_layout::recover_tick_from_slot_at_base(
            *slot,
            ticks_base,
            spacing,
            &anchors,
            i32::try_from(window).unwrap_or(i32::MAX),
        ) else {
            continue;
        };
        let Some(word) = journal_word(*slot) else {
            continue;
        };
        let (liquidity_gross, liquidity_net) = slot_layout::decode_tick_word(word);
        ticks.push(TouchedTickWord {
            tick,
            liquidity_gross,
            liquidity_net,
        });
    }
    ticks.sort_unstable_by_key(|t| t.tick);
    ticks
}
