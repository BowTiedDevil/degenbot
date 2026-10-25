//! The family-tagged buffered-event payload.
//!
//! The two concentrated-liquidity designs decode liquidity events into
//! different shapes, so an event carries its family with it: one door takes the
//! payload, and the key says which pool. A key whose family disagrees with the
//! payload is a caller bug, refused rather than silently dropped.

use crate::sim::registry::RegisteredPoolFamily;
use crate::state::v3_state::BufferedV3PoolEvent;
use crate::state::v4_state::BufferedV4PoolEvent;

/// A buffered concentrated-liquidity event, tagged with its family.
#[derive(Clone, Debug)]
pub enum BufferedPoolEvent {
    /// An address-keyed (V3) event.
    V3(BufferedV3PoolEvent),
    /// A V4 (`PoolManager` + pool id) event.
    V4(BufferedV4PoolEvent),
}

impl BufferedPoolEvent {
    /// The family this event belongs to.
    #[must_use]
    pub const fn family(&self) -> RegisteredPoolFamily {
        match self {
            Self::V3(_) => RegisteredPoolFamily::V3,
            Self::V4(_) => RegisteredPoolFamily::V4,
        }
    }
}
/// A liquidity-position change, canonical across the concentrated-liquidity
/// families.
///
/// The designs spell liquidity differently on the wire: v3-core takes an
/// unsigned `uint128 amount` on `mint`/`burn` (direction is which event fired),
/// v4-core a signed `int256 liquidityDelta` on `modifyLiquidity`. Both narrow to
/// `int128` before acting and revert outside it (`toInt128()`), so no event from
/// a canonical pool or manager can carry a larger delta and `i128` is the
/// honest shared payload. A forged log or a patched emitter can still present
/// one, so [`PoolLiquidityUpdate::from_v4_wire`] is fallible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolLiquidityUpdate {
    /// The position's lower tick.
    pub tick_lower: i32,
    /// The position's upper tick.
    pub tick_upper: i32,
    /// The signed liquidity change (`> 0` add, `< 0` remove).
    pub liquidity_delta: i128,
    /// The block the change belongs to.
    pub block_number: u64,
}

impl PoolLiquidityUpdate {
    /// A liquidity change already within the contracts' `int128` range.
    #[must_use]
    pub const fn new(
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Self {
        Self {
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        }
    }

    /// A liquidity change decoded from the 256-bit V4 wire value.
    ///
    /// `None` when the delta is outside `int128`: `modifyLiquidity` would have
    /// reverted on that value, so the log is not from a canonical pool.
    #[must_use]
    pub fn from_v4_wire(
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    ) -> Option<Self> {
        Some(Self::new(
            tick_lower,
            tick_upper,
            i128::try_from(liquidity_delta).ok()?,
            block_number,
        ))
    }
}
