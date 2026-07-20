//! Prototype `Pool` handle — structural primary seam (V2 only).

use crate::aerodrome_v2_state::{AerodromeV2PoolIdentity, AerodromeV2PoolState};
use crate::registry::PoolEntry;
use crate::simulate_swap::simulate_swap;
use crate::v2_state::{V2PoolIdentity, V2PoolState};
use alloy::primitives::{Address, U256};

/// Structural family of a pool. Mirrors Python `Structure` enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Structure {
    ReservePair,
    ConcentratedLiquidity,
    BalanceVector,
}

/// Identity value object — structural prototype only, V2/Aerodrome for now.
/// Long-term this resolves exchange+variant via deployments lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    ReservePair { variant: ReservePairVariant },
    ConcentratedLiquidity,
    BalanceVector,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReservePairVariant {
    UniswapV2,
    AerodromeV2 { stable: bool },
}

/// Shared pool handle presenting a structural interface over `BotState`.
pub struct Pool<'a> {
    entry: &'a PoolEntry,
}

impl<'a> Pool<'a> {
    #[must_use]
    pub fn new(entry: &'a PoolEntry) -> Self {
        Self { entry }
    }

    #[must_use]
    pub fn structure(&self) -> Structure {
        match self.entry {
            PoolEntry::V2(..) | PoolEntry::AerodromeV2(..) => Structure::ReservePair,
            PoolEntry::V3(..) | PoolEntry::V4(..) => Structure::ConcentratedLiquidity,
            PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..) => Structure::BalanceVector,
        }
    }

    #[must_use]
    pub fn identity(&self) -> Identity {
        match self.entry {
            PoolEntry::V2(_id, _) => Identity::ReservePair {
                variant: ReservePairVariant::UniswapV2,
            },
            PoolEntry::AerodromeV2(id, _) => Identity::ReservePair {
                variant: ReservePairVariant::AerodromeV2 { stable: id.stable },
            },
            PoolEntry::V3(..) | PoolEntry::V4(..) => Identity::ConcentratedLiquidity,
            PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..) => Identity::BalanceVector,
        }
    }

    #[must_use]
    pub fn reserve_pair(&self) -> Option<ReservePairView<'a>> {
        match self.entry {
            PoolEntry::V2(id, state) => Some(ReservePairView::V2(id, state)),
            PoolEntry::AerodromeV2(id, state) => Some(ReservePairView::Aerodrome(id, state)),
            _ => None,
        }
    }

    #[must_use]
    pub fn calculate_tokens_out(&self, zero_for_one: bool, amount_in: U256) -> Option<U256> {
        simulate_swap(self.entry, zero_for_one, amount_in).ok()
    }
}

#[allow(clippy::doc_markdown)]
/// Read-only reserve-pair view over V2 or AerodromeV2 state.
pub enum ReservePairView<'a> {
    V2(&'a V2PoolIdentity, &'a V2PoolState),
    Aerodrome(&'a AerodromeV2PoolIdentity, &'a AerodromeV2PoolState),
}

impl ReservePairView<'_> {
    #[must_use]
    pub fn reserve0(&self) -> U256 {
        match self {
            Self::V2(_, s) => s.reserve0.to::<U256>(),
            Self::Aerodrome(_, s) => s.reserve0.to::<U256>(),
        }
    }

    #[must_use]
    pub fn reserve1(&self) -> U256 {
        match self {
            Self::V2(_, s) => s.reserve1.to::<U256>(),
            Self::Aerodrome(_, s) => s.reserve1.to::<U256>(),
        }
    }

    #[must_use]
    pub fn token0(&self) -> Address {
        match self {
            Self::V2(id, _) => id.token0,
            Self::Aerodrome(id, _) => id.token0,
        }
    }

    #[must_use]
    pub fn token1(&self) -> Address {
        match self {
            Self::V2(id, _) => id.token1,
            Self::Aerodrome(id, _) => id.token1,
        }
    }
}
