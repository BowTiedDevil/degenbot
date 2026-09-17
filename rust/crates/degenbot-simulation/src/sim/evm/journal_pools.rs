//! Journal → typed pool post-state extractors — reverse decoders over the
//! frame-replay seam.
//!
//! Given a [`ReplayOutcome`]'s touched set + journalled words, recover the
//! typed post-state of every tracked pool the frame touched — V2 pair vs V3
//! pool (V4 PoolManager is *explicitly* `Unsupported`: no guessed decode on
//! a layout nobody pinned). This replaces the unbounded router-calldata
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

use alloy::primitives::{Address, U256};
use degenbot_pools::slot_layout;
use degenbot_pools::{v3_storage_slots, ClSlotLayout};
use hashbrown::HashMap;

use super::frame_replay::ReplayOutcome;

/// The per-pool layout knowledge the extractor needs, projected from the
/// engine's tracked-pool registry by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// per-poolId bases. Extraction is deliberately NOT supported — the
    /// extractor returns [`PoolPostKind::Unsupported`] for a V4-only frame
    /// (skip it with `observe reason="v4_unsupported"`) instead of guessing
    /// a decode against the packed Pool.State layout.
    V4PoolManager,
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
}

/// Typed vs explicitly-unsupported extraction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolPostKind {
    Typed(TypedPoolPost),
    /// The frame touched ONLY V4 PoolManager state — skipped upstream with
    /// `observe reason="v4_unsupported"`, never guessed.
    Unsupported,
}

/// The extracted post-state of ONE touched tracked pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolPostState {
    /// The pool contract (V2 pair / V3 pool; a V4 descriptor keys the
    /// PoolManager singleton).
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
        let Some(&family) = descriptors.get(address) else {
            continue;
        };
        let journal_word = |slot: U256| {
            outcome
                .state
                .get(address)
                .and_then(|account| account.storage.get(&slot))
                .map(|s| s.present_value)
        };
        let kind = match family {
            PoolFamily::V4PoolManager => PoolPostKind::Unsupported,
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
                touched_ticks: recover_touched_ticks(
                    address,
                    slots,
                    layout,
                    tick_spacing,
                    current_tick_hint,
                    &journal_word,
                ),
            }),
        };
        out.push(PoolPostState {
            address: *address,
            family,
            kind,
        });
    }
    out
}

/// Recover the per-tick words among a V3 pool's journalled slots: every
/// slot that preimages to a `tick_spacing`-aligned tick near the pre-tx
/// hint / post-tx current tick decodes to a [`TouchedTickWord`]; every
/// OTHER slot (tickBitmap words, fee growth, fee-growth-outside words) is
/// deliberately NOT decoded — nothing is fabricated.
#[must_use]
fn recover_touched_ticks(
    _address: &Address,
    slots: &[U256],
    layout: ClSlotLayout,
    tick_spacing: i32,
    current_tick_hint: Option<i32>,
    journal_word: &impl Fn(U256) -> Option<U256>,
) -> Vec<TouchedTickWord> {
    let spacing = tick_spacing.max(1);
    // Anchors: the pre-tx hint + the post-tx current tick (from the
    // journal's own slot0). Every touched tick word of a swap lies between
    // (plus one spacing past) the two, so the window covers |post - pre|
    // plus a floor for non-slot0 frames.
    let post_tick = journal_word(U256::ZERO).map(|w| v3_storage_slots::decode_v3_slot0(w).tick);
    let mut anchors: Vec<i32> = current_tick_hint.into_iter().chain(post_tick).collect();
    anchors.sort_unstable();
    anchors.dedup();
    let spread = match (anchors.first(), anchors.last()) {
        (Some(&low), Some(&high)) => i64::from(high - low),
        _ => 0,
    };
    let window: i64 = spread + 4 * i64::from(spacing) + 4_096;

    let mut ticks = Vec::new();
    for slot in slots {
        if *slot == U256::ZERO || *slot == U256::from(layout.liquidity_slot()) {
            continue;
        }
        let Some(tick) = slot_layout::recover_cl_tick_from_slot(
            *slot,
            layout,
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
