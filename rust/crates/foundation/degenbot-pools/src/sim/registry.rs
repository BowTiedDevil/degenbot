//! Pool-registry sum type (`PoolEntry`), the V3/V4 concentrated-liquidity
//! family reader trait, the family's write door (re-exported from its tree),
//! and token-entry metadata. **Relocated** from
//! `degenbot-bot/src/bot_core/mod.rs`.

use crate::families::concentrated_liquidity::state_mutation::ConcentratedLiquidityPoolMut;
use crate::sim::tick_fetch::{
    FetchedTickWord, TickWordFingerprint, TickWordKey, TickWordMergeTarget, TickWordReader,
};
use crate::state::aerodrome_v2_state::{AerodromeV2PoolIdentity, AerodromeV2PoolState};
use crate::state::balancer_stable_state::{BalancerStablePoolIdentity, BalancerStablePoolState};
use crate::state::balancer_weighted_state::{
    BalancerWeightedPoolIdentity, BalancerWeightedPoolState,
};
use crate::state::curve_state::{CurvePoolIdentity, CurvePoolState};
use crate::state::state_history::{BalanceVectorPoolState, ReservePairPoolState};
use crate::state::v2_state::{V2PoolIdentity, V2PoolState};
use crate::state::v3_state::{PoolTickCoverage, V3PoolIdentity, V3PoolState};
use crate::state::v4_state::{V4PoolIdentity, V4PoolState};
use crate::TickInfo;
use alloy::primitives::{Address, U256};
use hashbrown::{HashMap, HashSet};

/// A single pool's state. Pool-type-specific fields are in the enum variants.
#[derive(Clone, Debug)]
pub enum PoolEntry {
    V2(Box<(V2PoolIdentity, V2PoolState)>),
    V3(Box<(V3PoolIdentity, V3PoolState)>),
    V4(Box<(V4PoolIdentity, V4PoolState)>),
    Curve(Box<(CurvePoolIdentity, CurvePoolState)>),
    BalancerWeighted(Box<(BalancerWeightedPoolIdentity, BalancerWeightedPoolState)>),
    BalancerStable(Box<(BalancerStablePoolIdentity, BalancerStablePoolState)>),
    AerodromeV2(Box<(AerodromeV2PoolIdentity, AerodromeV2PoolState)>),
}

/// The ONE family tag: which structural family a registration concerns.
///
/// It serves two readers, and there is deliberately only one type for both —
/// the address-keyed registration readers (PRG-1 registry unification) return
/// it so a `PyO3` build adapter can fast-path onto the registered entry of
/// record, and the registration-refusal vocabulary
/// ([`RegisterRefusal`](crate::sim::registration_refusal::RegisterRefusal)) rides
/// it beside a refusal so a consumer never re-derives the family from the
/// request. A second tag type would be a mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegisteredPoolFamily {
    V2,
    V3,
    /// V4 registers under `(PoolManager, pool_id)`, not an address — one
    /// manager hosts many pool ids — so this arm is resolved only by the
    /// `(pool_manager, pool_id)`-keyed reader; an address query cannot name a
    /// V4 pool and stays `None` there.
    V4,
    AerodromeV2,
    BalancerWeighted,
    BalancerStable,
    Curve,
}

impl RegisteredPoolFamily {
    /// Every variant, in declaration order — the closed set
    /// [`Self::from_tag`] admits a tag against, and the census [`Self::tag`]
    /// is pinned over.
    pub const ALL: [Self; 7] = [
        Self::V2,
        Self::V3,
        Self::V4,
        Self::AerodromeV2,
        Self::BalancerWeighted,
        Self::BalancerStable,
        Self::Curve,
    ];

    /// The kebab-case tag naming this family — the ONE spelling of the tag
    /// vocabulary.
    ///
    /// The registered-entry projection (`PoolEntry::registered_family`), the
    /// session identity (`PoolIdentity::family_tag`), and `BotState`'s
    /// `pool_family` string reader all render through here, so a family tag
    /// cannot be spelled two ways.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::V2 => "v2",
            Self::V3 => "v3",
            Self::V4 => "v4",
            Self::AerodromeV2 => "aerodrome-v2",
            Self::BalancerWeighted => "balancer-weighted",
            Self::BalancerStable => "balancer-stable",
            Self::Curve => "curve",
        }
    }

    /// Parse a family tag back into the enum — the inverse of [`Self::tag`],
    /// and the closed set a tag received as a string is admitted against.
    ///
    /// `None` for anything outside the vocabulary. The vocabulary is
    /// deliberately exact (kebab-case, no aliases, case-sensitive): the tag
    /// is a wire value the core itself emits, so a near-miss is a caller bug,
    /// never a spelling to tolerate.
    #[must_use]
    pub const fn from_tag(tag: &str) -> Option<Self> {
        match tag.as_bytes() {
            b"v2" => Some(Self::V2),
            b"v3" => Some(Self::V3),
            b"v4" => Some(Self::V4),
            b"aerodrome-v2" => Some(Self::AerodromeV2),
            b"balancer-weighted" => Some(Self::BalancerWeighted),
            b"balancer-stable" => Some(Self::BalancerStable),
            b"curve" => Some(Self::Curve),
            _ => None,
        }
    }
}

impl PoolEntry {
    /// Project the entry onto its registration family tag (ADR-059 D1). Total
    /// over the variants; the address-keyed reader consults it and rejects
    /// [`RegisteredPoolFamily::V4`] because an address query cannot name a
    /// V4 pool.
    #[must_use]
    pub const fn registered_family(&self) -> RegisteredPoolFamily {
        match self {
            Self::V2(..) => RegisteredPoolFamily::V2,
            Self::V3(..) => RegisteredPoolFamily::V3,
            Self::V4(..) => RegisteredPoolFamily::V4,
            Self::Curve(..) => RegisteredPoolFamily::Curve,
            Self::BalancerWeighted(..) => RegisteredPoolFamily::BalancerWeighted,
            Self::BalancerStable(..) => RegisteredPoolFamily::BalancerStable,
            Self::AerodromeV2(..) => RegisteredPoolFamily::AerodromeV2,
        }
    }
}

#[cfg(test)]
mod family_tag_tests {
    use super::RegisteredPoolFamily;

    #[test]
    fn tags_are_the_kebab_vocabulary_in_declaration_order() {
        // What: the closed set and its one spelling. A variant added to `ALL`
        // without a `tag` arm (or the reverse) fails to compile; the array
        // length is the census.
        assert_eq!(
            RegisteredPoolFamily::ALL.map(RegisteredPoolFamily::tag),
            [
                "v2",
                "v3",
                "v4",
                "aerodrome-v2",
                "balancer-weighted",
                "balancer-stable",
                "curve",
            ],
        );
    }

    #[test]
    fn from_tag_inverts_tag_over_the_closed_set() {
        for family in RegisteredPoolFamily::ALL {
            assert_eq!(RegisteredPoolFamily::from_tag(family.tag()), Some(family));
        }
    }

    #[test]
    fn from_tag_refuses_anything_outside_the_closed_set() {
        // The parse is exact: no aliases, no case folding, no trimming. The tag
        // is a value the core itself emits, so a near miss is a caller bug.
        for raw in [
            "",
            "V2",
            "v2 ",
            " v2",
            "uniswap_v2",
            "aerodrome",
            "concentrated_liquidity",
            "constant_product",
            "weighted",
            "stableswap",
        ] {
            assert_eq!(RegisteredPoolFamily::from_tag(raw), None, "{raw:?}");
        }
    }
}

/// Per-variant projection methods for [`PoolEntry`] (ADR-014 D5).
///
/// Each `vN()` / `vN_mut()` returns the `(identity, state)` pair when the
/// entry matches the family, `None` otherwise — replacing the per-reader
/// `match` arms that listed all sibling variants only to return `None`
/// (`V3 | V4 | Curve | … => None`). Readers on `BotState` now collapse to
/// `self.pools.get(&pool_id).and_then(PoolEntry::vN).map(|(i,_)| i)`.
///
/// The identity/state sibling split in each variant is preserved — callers
/// asking for state get `&VxPoolState`, callers asking for identity get
/// `&VxPoolIdentity`, both via the same projection destructured differently.
impl PoolEntry {
    /// V2 `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn v2(&self) -> Option<(&V2PoolIdentity, &V2PoolState)> {
        if let Self::V2(p) = self {
            Some((&p.0, &p.1))
        } else {
            None
        }
    }
    /// V2 `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn v2_mut(&mut self) -> Option<(&mut V2PoolIdentity, &mut V2PoolState)> {
        if let Self::V2(p) = self {
            Some((&mut p.0, &mut p.1))
        } else {
            None
        }
    }

    /// V3 `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn v3(&self) -> Option<(&V3PoolIdentity, &V3PoolState)> {
        if let Self::V3(p) = self {
            Some((&p.0, &p.1))
        } else {
            None
        }
    }
    /// V3 `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn v3_mut(&mut self) -> Option<(&mut V3PoolIdentity, &mut V3PoolState)> {
        if let Self::V3(p) = self {
            Some((&mut p.0, &mut p.1))
        } else {
            None
        }
    }

    /// V4 `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn v4(&self) -> Option<(&V4PoolIdentity, &V4PoolState)> {
        if let Self::V4(p) = self {
            Some((&p.0, &p.1))
        } else {
            None
        }
    }
    /// V4 `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn v4_mut(&mut self) -> Option<(&mut V4PoolIdentity, &mut V4PoolState)> {
        if let Self::V4(p) = self {
            Some((&mut p.0, &mut p.1))
        } else {
            None
        }
    }

    /// Curve `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn curve(&self) -> Option<(&CurvePoolIdentity, &CurvePoolState)> {
        if let Self::Curve(p) = self {
            Some((&p.0, &p.1))
        } else {
            None
        }
    }
    /// Curve `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn curve_mut(&mut self) -> Option<(&mut CurvePoolIdentity, &mut CurvePoolState)> {
        if let Self::Curve(p) = self {
            Some((&mut p.0, &mut p.1))
        } else {
            None
        }
    }

    /// Balancer weighted `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn balancer_weighted(
        &self,
    ) -> Option<(&BalancerWeightedPoolIdentity, &BalancerWeightedPoolState)> {
        if let Self::BalancerWeighted(p) = self {
            Some((&p.0, &p.1))
        } else {
            None
        }
    }
    /// Balancer weighted `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn balancer_weighted_mut(
        &mut self,
    ) -> Option<(
        &mut BalancerWeightedPoolIdentity,
        &mut BalancerWeightedPoolState,
    )> {
        if let Self::BalancerWeighted(p) = self {
            Some((&mut p.0, &mut p.1))
        } else {
            None
        }
    }

    /// Balancer stable `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn balancer_stable(
        &self,
    ) -> Option<(&BalancerStablePoolIdentity, &BalancerStablePoolState)> {
        if let Self::BalancerStable(p) = self {
            Some((&p.0, &p.1))
        } else {
            None
        }
    }
    /// Balancer stable `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn balancer_stable_mut(
        &mut self,
    ) -> Option<(
        &mut BalancerStablePoolIdentity,
        &mut BalancerStablePoolState,
    )> {
        if let Self::BalancerStable(p) = self {
            Some((&mut p.0, &mut p.1))
        } else {
            None
        }
    }

    /// Aerodrome V2 `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn aerodrome_v2(&self) -> Option<(&AerodromeV2PoolIdentity, &AerodromeV2PoolState)> {
        if let Self::AerodromeV2(p) = self {
            Some((&p.0, &p.1))
        } else {
            None
        }
    }
    /// Aerodrome V2 `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn aerodrome_v2_mut(
        &mut self,
    ) -> Option<(&mut AerodromeV2PoolIdentity, &mut AerodromeV2PoolState)> {
        if let Self::AerodromeV2(p) = self {
            Some((&mut p.0, &mut p.1))
        } else {
            None
        }
    }

    /// Project to `&mut dyn ConcentratedLiquidityPoolMut` (ADR-017) — the
    /// mutable concentrated-liquidity-family apply-twin of [`Self::as_reorg_state_mut`] (the
    /// reorg projection from ADR-016). Used by `BotState`'s concentrated liquidity apply
    /// dispatchers (`merge_tick_word`, and the future `apply_swap` /
    /// `apply_liquidity_update` slices) to reach the trait method without a
    /// per-family variant match at each call site.
    #[must_use]
    pub fn as_concentrated_liquidity_mut(
        &mut self,
    ) -> Option<&mut dyn ConcentratedLiquidityPoolMut> {
        some_concentrated_liquidity_mut(self)
    }

    /// The pool's tick-word read handle — its read descriptor (minted from its
    /// own identity) paired with the stored source — or `None` when the entry
    /// is not a concentrated-liquidity pool, or stores no source.
    ///
    /// The ONE key minting for a miss path: a caller holding an entry never
    /// hand-builds a [`TickWordKey`] beside the source it pairs it with.
    #[must_use]
    pub fn tick_word_reader(&self) -> Option<TickWordReader> {
        match self {
            Self::V3(p) => Some(TickWordReader::new(
                TickWordKey::from(&p.0),
                p.1.fetcher.clone()?,
            )),
            Self::V4(p) => Some(TickWordReader::new(
                TickWordKey::from(&p.0),
                p.1.fetcher.clone()?,
            )),
            Self::V2(..)
            | Self::Curve(..)
            | Self::BalancerWeighted(..)
            | Self::BalancerStable(..)
            | Self::AerodromeV2(..) => None,
        }
    }

    /// Bump the state nonce WITHOUT a semantic state change — the external
    /// invalidation seam (ADR-040): quarantine/release transitions bump the
    /// nonce so cached hop projections invalidate and in-flight solver
    /// snapshots (solve-time nonce vs sim-time re-read) drop their stale
    /// candidates. Deliberately does NOT touch `update_block`: a quarantine
    /// is not a chain event, and fishing `update_block` would corrupt the
    /// tripwire's anchor model.
    pub fn bump_state_nonce(&mut self) {
        match self {
            PoolEntry::V2(p) => p.1.state_nonce = p.1.state_nonce.wrapping_add(1),
            PoolEntry::V3(p) => p.1.state_nonce = p.1.state_nonce.wrapping_add(1),
            PoolEntry::V4(p) => p.1.state_nonce = p.1.state_nonce.wrapping_add(1),
            PoolEntry::Curve(p) => p.1.state_nonce = p.1.state_nonce.wrapping_add(1),
            PoolEntry::BalancerWeighted(p) => p.1.state_nonce = p.1.state_nonce.wrapping_add(1),
            PoolEntry::BalancerStable(p) => p.1.state_nonce = p.1.state_nonce.wrapping_add(1),
            PoolEntry::AerodromeV2(p) => p.1.state_nonce = p.1.state_nonce.wrapping_add(1),
        }
    }

    /// The pool's per-mutation state nonce. Bumped on every state
    /// change (`apply_swap`, `apply_liquidity_update`, `replace_tick_data`,
    /// `merge_tick_word`, `apply_sync`, `apply_balance_update`,
    /// `restore_before_block`). The solver snapshots this per-hop at resolve
    /// time so the dispatch seam can detect staleness: if a pool's current
    /// nonce has advanced past the snapshot, the solver computed its result
    /// against state that has since been superseded → skip the stale
    /// candidate (the block-N solve used pool@N-1 while on-chain@N has pool@N
    /// after a user swap landed between the solve and the sim).
    #[must_use]
    pub fn state_nonce(&self) -> u64 {
        match self {
            PoolEntry::V2(p) => p.1.state_nonce,
            PoolEntry::V3(p) => p.1.state_nonce,
            PoolEntry::V4(p) => p.1.state_nonce,
            PoolEntry::Curve(p) => p.1.state_nonce,
            PoolEntry::BalancerWeighted(p) => p.1.state_nonce,
            PoolEntry::BalancerStable(p) => p.1.state_nonce,
            PoolEntry::AerodromeV2(p) => p.1.state_nonce,
        }
    }

    /// The block the pool's reserves / `sqrt_price` / `tick` / liquidity were
    /// last mutated by a forward event — the freshness signal for the per-path
    /// mid-block solve gate . A path is solved at `solve_block`
    /// only when EVERY hop's `update_block >= solve_block`; a stale pool defers
    /// its path until its block-N log lands and re-dirties it (the settlement-arbitrage
    /// otherwise lands at a block where one pool still holds N-1 state).
    #[must_use]
    pub fn update_block(&self) -> u64 {
        match self {
            PoolEntry::V2(p) => p.1.update_block,
            PoolEntry::V3(p) => p.1.update_block,
            PoolEntry::V4(p) => p.1.update_block,
            PoolEntry::Curve(p) => p.1.update_block,
            PoolEntry::BalancerWeighted(p) => p.1.update_block,
            PoolEntry::BalancerStable(p) => p.1.update_block,
            PoolEntry::AerodromeV2(p) => p.1.update_block,
        }
    }

    /// The **liquidity** clock — the block the tick map reflects (two-stamp
    /// the `tick_data_block` twin of [`Self::update_block`]). Only
    /// the concentrated-liquidity families carry a tick map; the AMM/Curve/
    /// Balancer variants have no tick-data clock and fall back to their
    /// `update_block`. A concentrated liquidity pool can have a FRESH price clock but a liquidity
    /// map that lags (the staged-clock `0x5653` class) — consumers that read
    /// the tick map MUST key on this clock, not `update_block`.
    #[must_use]
    pub fn tick_data_block(&self) -> u64 {
        match self {
            PoolEntry::V3(p) => p.1.tick_data_block,
            PoolEntry::V4(p) => p.1.tick_data_block,
            // No tick-data clock on these families — fall back to the price
            // clock so the accessor is total.
            PoolEntry::V2(p) => p.1.update_block,
            PoolEntry::Curve(p) => p.1.update_block,
            PoolEntry::BalancerWeighted(p) => p.1.update_block,
            PoolEntry::BalancerStable(p) => p.1.update_block,
            PoolEntry::AerodromeV2(p) => p.1.update_block,
        }
    }

    /// Project to `&dyn ReorgPoolState` (ADR-016). One match over all 7
    /// variants — used by `BotState`'s unified reorg dispatchers
    /// (`restore_pool_before_block` / `discard_pool_before_block` /
    /// `pool_journal_len`) to dispatch through the trait without re-matching
    /// family inline.
    #[must_use]
    pub fn as_reorg_state(&self) -> Option<&dyn crate::state::state_history::ReorgPoolState> {
        Some(match self {
            PoolEntry::V2(p) => &p.1,
            PoolEntry::V3(p) => &p.1,
            PoolEntry::V4(p) => &p.1,
            PoolEntry::Curve(p) => &p.1,
            PoolEntry::BalancerWeighted(p) => &p.1,
            PoolEntry::BalancerStable(p) => &p.1,
            PoolEntry::AerodromeV2(p) => &p.1,
        })
    }

    /// Mutable projection to `&mut dyn ReorgPoolState` (ADR-016) — the write
    /// twin of [`as_reorg_state`](Self::as_reorg_state).
    #[must_use]
    pub fn as_reorg_state_mut(
        &mut self,
    ) -> Option<&mut dyn crate::state::state_history::ReorgPoolState> {
        Some(match self {
            PoolEntry::V2(p) => &mut p.1,
            PoolEntry::V3(p) => &mut p.1,
            PoolEntry::V4(p) => &mut p.1,
            PoolEntry::Curve(p) => &mut p.1,
            PoolEntry::BalancerWeighted(p) => &mut p.1,
            PoolEntry::BalancerStable(p) => &mut p.1,
            PoolEntry::AerodromeV2(p) => &mut p.1,
        })
    }

    /// Project to `&mut dyn BalanceVectorPoolState` (ADR-017 D1) — the
    /// forward-apply view over the three balance-vector families
    /// (Curve / `BalancerWeighted` / `BalancerStable`). Used by `BotState`'s
    /// unified `apply_balance_update_by_pool_id` dispatcher to reach the
    /// trait method without a per-family variant match. Returns `None` for
    /// the four non-balance-vector families.
    #[must_use]
    pub fn as_balance_vector_mut(&mut self) -> Option<&mut dyn BalanceVectorPoolState> {
        match self {
            PoolEntry::Curve(p) => Some(&mut p.1),
            PoolEntry::BalancerWeighted(p) => Some(&mut p.1),
            PoolEntry::BalancerStable(p) => Some(&mut p.1),
            PoolEntry::V2(..)
            | PoolEntry::V3(..)
            | PoolEntry::V4(..)
            | PoolEntry::AerodromeV2(..) => None,
        }
    }

    /// Project to `&mut dyn ReservePairPoolState` (ADR-017 D3) — the
    /// forward-apply view over the two reserve-pair families (V2 /
    /// `AerodromeV2`). Used by `BotState`'s unified `apply_sync_by_pool_id`
    /// dispatcher to reach the trait method without a per-family variant
    /// match. Returns `None` for the five non-reserve-pair families.
    #[must_use]
    pub fn as_reserve_pair_mut(&mut self) -> Option<&mut dyn ReservePairPoolState> {
        match self {
            PoolEntry::V2(p) => Some(&mut p.1),
            PoolEntry::AerodromeV2(p) => Some(&mut p.1),
            PoolEntry::V3(..)
            | PoolEntry::V4(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..) => None,
        }
    }
}

/// The typed lens over a [`PoolEntry`] — one closed projection per registered
/// state or identity slice (the GLOSSARY "state slice" vocabulary). The
/// family-neutral registry rows read a slice through this trait, so the
/// family knowledge lives in the caller's type parameter, never in a method
/// name (ADR-073 D3: the surface names do not re-declare the family
/// taxonomy).
///
/// The projection set is closed in substance: every implementing type is a
/// slice this crate owns, and the orphan rule keeps foreign crates from
/// implementing this trait for them — a new family extends the impl block
/// below, not the call sites.
pub trait PoolEntrySlice: 'static {
    /// The slice of `entry`'s family this type projects, or `None` when the
    /// entry belongs to a different family.
    fn from_entry(entry: &PoolEntry) -> Option<&Self>;
}

macro_rules! entry_slice {
    ($($ty:ty => $variant:ident . $field:tt),* $(,)?) => {
        $(
            impl PoolEntrySlice for $ty {
                fn from_entry(pooled: &PoolEntry) -> Option<&Self> {
                    match pooled {
                        PoolEntry::$variant(p) => Some(&p.$field),
                        _ => None,
                    }
                }
            }
        )*
    };
}

entry_slice! {
    V2PoolState => V2 . 1,
    V2PoolIdentity => V2 . 0,
    V3PoolState => V3 . 1,
    V3PoolIdentity => V3 . 0,
    V4PoolState => V4 . 1,
    V4PoolIdentity => V4 . 0,
    CurvePoolState => Curve . 1,
    CurvePoolIdentity => Curve . 0,
    BalancerWeightedPoolState => BalancerWeighted . 1,
    BalancerWeightedPoolIdentity => BalancerWeighted . 0,
    BalancerStablePoolState => BalancerStable . 1,
    BalancerStablePoolIdentity => BalancerStable . 0,
    AerodromeV2PoolState => AerodromeV2 . 1,
    AerodromeV2PoolIdentity => AerodromeV2 . 0,
}

impl PoolEntrySlice for dyn ConcentratedLiquidityPool {
    fn from_entry(pooled: &PoolEntry) -> Option<&Self> {
        match pooled {
            PoolEntry::V3(p) => Some(&p.1),
            PoolEntry::V4(p) => Some(&p.1),
            _ => None,
        }
    }
}

impl PoolEntry {
    /// Project this entry onto the slice type `T` (the family-neutral typed
    /// lens; see [`PoolEntrySlice`]). `None` when the entry's family does not
    /// carry a `T`.
    #[must_use]
    pub fn slice<T: PoolEntrySlice + ?Sized>(&self) -> Option<&T> {
        T::from_entry(self)
    }
}

/// `PoolEntry` as a tick-word merge target — the registered-pool arm of the
/// tick-word merge policy (GLOSSARY **Tick-word merge**): its fingerprint is the
/// concentrated-liquidity state's own, and a fetched word lands through the same
/// state verb a direct merge reaches. A non-concentrated-liquidity entry is not a
/// merge target at all, so it answers `None` and takes no word.
impl TickWordMergeTarget for PoolEntry {
    fn tick_word_fingerprint(&self) -> Option<TickWordFingerprint> {
        match self {
            Self::V3(p) => p.1.tick_word_fingerprint(),
            Self::V4(p) => p.1.tick_word_fingerprint(),
            _ => None,
        }
    }

    fn apply_fetched_word(&mut self, fetched: &FetchedTickWord) {
        match self {
            Self::V3(p) => p.1.apply_fetched_word(fetched),
            Self::V4(p) => p.1.apply_fetched_word(fetched),
            _ => {}
        }
    }
}

/// `PoolEntry::as_concentrated_liquidity_mut` helper — one match over V3 | V4. Free-standing so
/// `PoolEntry::as_concentrated_liquidity_mut` stays a one-liner (matches the `as_reorg_state`
/// shape). Returns `None` for the five non-concentrated-liquidity families.
fn some_concentrated_liquidity_mut(
    entry: &mut PoolEntry,
) -> Option<&mut dyn ConcentratedLiquidityPoolMut> {
    match entry {
        PoolEntry::V3(p) => Some(&mut p.1),
        PoolEntry::V4(p) => Some(&mut p.1),
        PoolEntry::V2(..)
        | PoolEntry::Curve(..)
        | PoolEntry::BalancerWeighted(..)
        | PoolEntry::BalancerStable(..)
        | PoolEntry::AerodromeV2(..) => None,
    }
}

/// Read-only surface shared by [`V3PoolState`] and [`V4PoolState`] — the
/// fields the per-handle `PyLiquidityPool` reader API presents uniformly
/// across the V3/V4 concentrated-liquidity families.
///
/// Both variants store the same mutable scalars (`sqrt_price_x96`/
/// `liquidity`/`tick`/`update_block`) and an identical `tick_data:
/// HashMap<i32, TickInfo>`; V4 additionally nests `fee`/`tick_spacing`
/// inside `pool_key`, which the impl projects out. The trait lets
/// [`BotState::get_v3_or_v4_pool`] return one borrowed view covering both
/// families without cloning — the reader twin of the apply dispatchers.
///
/// V2 is intentionally excluded (different state shape — reserves, not
/// scalars); a V2 `pool_id` yields `None` from the accessor, matching the
/// prior V3-only contract.
/// Mutable-reader trait for V3/V4 concentrated-liquidity pools.
///
/// Projects only mutable runtime scalars (`sqrt_price_x96`/`liquidity`/`tick`/
/// `update_block`/`tick_data`) — the values a swap calc consumes. Immutable
/// config (`fee`/`tick_spacing`) lives on `V3PoolIdentity`/`V4PoolIdentity`;
/// read it via `BotState::pool_slice`.
/// The dyn-dispatch surface is a `&VxPoolState` borrowed from the registry.
pub trait ConcentratedLiquidityPool {
    fn sqrt_price_x96(&self) -> U256;
    fn liquidity(&self) -> u128;
    fn tick(&self) -> i32;
    /// The **price** clock — block the slot0 scalars reflect (two-stamp rule).
    fn update_block(&self) -> u64;
    /// The **liquidity** clock — block the `tick_data` map reflects (the
    /// two-stamp rule). A pool can have a fresh price but a liquidity map
    /// that lags.
    fn tick_data_block(&self) -> u64;
    fn tick_data(&self) -> &HashMap<i32, TickInfo>;
    /// Registration coverage (Sparse/Tracked). Sparse pools consult
    /// `known_bitmap_words` for miss-detection; a Tracked pool's bitmap is
    /// derived from its complete tick rows (word absent = known-empty).
    fn coverage(&self) -> PoolTickCoverage;
    /// The checked tick-bitmap words (the sparse miss-detection set).
    /// Sparse-only state: seeded at registration from the initial tick
    /// keys, grown by fetch-merge + full-sync + the FFI checked-word seam;
    /// never written for Tracked pools.
    fn known_bitmap_words(&self) -> &HashSet<i32>;
}

impl ConcentratedLiquidityPool for V3PoolState {
    fn sqrt_price_x96(&self) -> U256 {
        self.sqrt_price_x96
    }
    fn liquidity(&self) -> u128 {
        self.liquidity
    }
    fn tick(&self) -> i32 {
        self.tick
    }
    fn update_block(&self) -> u64 {
        self.update_block
    }
    fn tick_data_block(&self) -> u64 {
        self.tick_data_block
    }
    fn tick_data(&self) -> &HashMap<i32, TickInfo> {
        &self.tick_data
    }
    fn coverage(&self) -> PoolTickCoverage {
        self.coverage
    }
    fn known_bitmap_words(&self) -> &HashSet<i32> {
        &self.known_bitmap_words
    }
}

impl ConcentratedLiquidityPool for V4PoolState {
    fn sqrt_price_x96(&self) -> U256 {
        self.sqrt_price_x96
    }
    fn liquidity(&self) -> u128 {
        self.liquidity
    }
    fn tick(&self) -> i32 {
        self.tick
    }
    fn update_block(&self) -> u64 {
        self.update_block
    }
    fn tick_data_block(&self) -> u64 {
        self.tick_data_block
    }
    fn tick_data(&self) -> &HashMap<i32, TickInfo> {
        &self.tick_data
    }
    fn coverage(&self) -> PoolTickCoverage {
        self.coverage
    }
    fn known_bitmap_words(&self) -> &HashSet<i32> {
        &self.known_bitmap_words
    }
}

/// ERC20 token metadata.
#[derive(Clone, Debug)]
pub struct TokenEntry {
    pub address: Address,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub chain_id: u64,
}

#[expect(clippy::unwrap_used)]
#[cfg(test)]
mod projection_tests {
    use super::*;
    use crate::state::v3_state::{
        ConcentratedLiquiditySlotLayout, RegisterV3PoolParams, V3PoolState,
    };

    fn v3_entry() -> PoolEntry {
        let (identity, state) = V3PoolState::from_params(
            RegisterV3PoolParams {
                address: Address::ZERO,
                token0: Address::ZERO,
                token1: Address::ZERO,
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 10_000_000_000_000u128,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
                deployer: Address::ZERO,
                init_hash: alloy::primitives::B256::ZERO,
                slot_layout: ConcentratedLiquiditySlotLayout::UniswapV3,
                known_bitmap_words: Vec::new(),
            },
            8,
        );
        PoolEntry::V3(Box::new((identity, state)))
    }

    #[test]
    fn v3_projection_returns_some_for_v3_and_none_for_sibling_families() {
        // What: v3() returns Some when the entry is V3, None for every other
        // family. The sibling-family None is the collapse that removes the
        // variant-exhaustion arms in the get_* readers (D5).
        let entry = v3_entry();
        assert!(entry.v3().is_some());
        assert!(entry.v2().is_none());
        assert!(entry.v4().is_none());
        assert!(entry.curve().is_none());
        assert!(entry.balancer_weighted().is_none());
        assert!(entry.balancer_stable().is_none());
        assert!(entry.aerodrome_v2().is_none());
    }

    #[test]
    fn v3_mut_projection_returns_mutable_borrow() {
        // What: v3_mut() returns a mutable (identity, state) borrow.
        let mut entry = v3_entry();
        if let Some((_, state)) = entry.v3_mut() {
            state.update_block = 7;
        }
        assert_eq!(entry.v3().unwrap().1.update_block, 7);
    }
}

// --- the concentrated-liquidity coverage reader (the read twin's two-adapter
// contract) ---

#[cfg(test)]
mod known_word_tests {
    use hashbrown::HashMap;

    use alloy::primitives::{Address, U256};

    use super::ConcentratedLiquidityPool;
    use crate::state::v3_state::{PoolTickCoverage, RegisterV3PoolParams, V3PoolState};
    use crate::state::v4_state::{RegisterV4PoolParams, V4PoolKey, V4PoolState};

    fn v3_state(coverage: PoolTickCoverage) -> V3PoolState {
        let (_, state) = V3PoolState::from_params(
            RegisterV3PoolParams {
                address: Address::ZERO,
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 10_000_000_000_000u128,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage,
                fetcher: None,
                deployer: Address::ZERO,
                init_hash: alloy::primitives::B256::ZERO,
                slot_layout: crate::state::v3_state::ConcentratedLiquiditySlotLayout::UniswapV3,
                known_bitmap_words: Vec::new(),
            },
            8,
        );
        state
    }

    fn v4_state(coverage: PoolTickCoverage) -> V4PoolState {
        let pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId = [0x99u8; 32];
        let (_, state) = V4PoolState::from_params(
            RegisterV4PoolParams {
                pool_manager: Address::from([0x88u8; 20]),
                pool_id,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                state_view: Address::ZERO,
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage,
                fetcher: None,
                known_bitmap_words: Vec::new(),
            },
            8,
        );
        state
    }

    #[test]
    fn coverage_reader_reports_registration_coverage() {
        let sparse = v3_state(PoolTickCoverage::Sparse);
        let tracked = v3_state(PoolTickCoverage::Tracked);
        assert_eq!(sparse.coverage(), PoolTickCoverage::Sparse);
        assert_eq!(tracked.coverage(), PoolTickCoverage::Tracked);
    }

    #[test]
    fn v4_coverage_reader_reports_registration_coverage() {
        let sparse = v4_state(PoolTickCoverage::Sparse);
        let tracked = v4_state(PoolTickCoverage::Tracked);
        assert_eq!(sparse.coverage(), PoolTickCoverage::Sparse);
        assert_eq!(tracked.coverage(), PoolTickCoverage::Tracked);
    }
}
