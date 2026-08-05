//! Structural `Pool` handle — the primary shared seam across all pool families.
//!
//! This module exposes a single [`Pool`] handle that projects a registered
//! [`PoolEntry`] into one of three structural shapes:
//!
//! - [`Structure::ReservePair`] — two-token reserve pools (Uniswap V2, Aerodrome V2)
//! - [`Structure::ConcentratedLiquidity`] — tick-based CL pools (Uniswap V3, Uniswap V4)
//! - [`Structure::BalanceVector`] — N-token balance pools (Curve, Balancer weighted/stable)
//!
//! The handle is intentionally value- and protocol-agnostic: it does not know
//! about DEX names, chain IDs, or whether a V2 pool is Uniswap vs. Sushiswap
//! (long-term that resolution belongs in `degenbot-uniswap::deployments`).
//! It *does* distinguish the protocol family (V3 vs. V4, Curve vs. Balancer)
//! because those families have materially different state shapes.

use crate::aerodrome_v2_state::{AerodromeV2PoolIdentity, AerodromeV2PoolState};
use crate::balancer_stable_state::{BalancerStablePoolIdentity, BalancerStablePoolState};
use crate::balancer_weighted_state::{BalancerWeightedPoolIdentity, BalancerWeightedPoolState};
use crate::curve_state::{CurvePoolIdentity, CurvePoolState};
use crate::registry::PoolEntry;
use crate::simulate_swap::simulate_swap;
use crate::v2_state::{V2PoolIdentity, V2PoolState};
use crate::v3_state::{V3PoolIdentity, V3PoolState};
use crate::v4_state::{V4PoolIdentity, V4PoolState};
use alloy::primitives::{Address, U256};
use degenbot_uniswap::deployments::resolve_dex_name;
use degenbot_uniswap::dex_identity::DexName;

/// Structural family of a pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Structure {
    ReservePair,
    ConcentratedLiquidity,
    BalanceVector,
}

/// Identity value object — structural projection over a registered pool.
///
/// Surfaces the protocol family sub-variant AND the resolved DEX name
/// (`Uniswap` vs `SushiSwap` …). `dex: None` means the `(chain_id, factory)`
/// deployment is unknown — the caller degrades to the generic family variant,
/// never an error (QHGN2E).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    ReservePair {
        variant: ReservePairVariant,
        dex: Option<DexName>,
    },
    ConcentratedLiquidity {
        variant: ConcentratedLiquidityVariant,
        dex: Option<DexName>,
    },
    BalanceVector {
        variant: BalanceVectorVariant,
        dex: Option<DexName>,
    },
}

/// Concrete DEX variant behind a [`Structure::ReservePair`] pool.
///
/// Distinguishes the Uniswap-V2-style constant-product family from the
/// Aerodrome-V2 volatile/stable pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReservePairVariant {
    UniswapV2,
    AerodromeV2 { stable: bool },
}

/// Concrete DEX variant behind a [`Structure::ConcentratedLiquidity`] pool.
///
/// Distinguishes Uniswap V3 (factory-addressed pool) from Uniswap V4
/// (singleton + `PoolId`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConcentratedLiquidityVariant {
    UniswapV3,
    UniswapV4,
}

/// Concrete DEX variant behind a [`Structure::BalanceVector`] pool.
///
/// Distinguishes the N-token balance-pool families: Curve stableswap and
/// the Balancer weighted / stable subsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalanceVectorVariant {
    Curve,
    BalancerWeighted,
    BalancerStable,
}

/// Shared pool handle presenting a structural interface over `BotState`.
///
/// Carries the chain id so [`Pool::identity`] can resolve the DEX name for a
/// `(chain_id, factory)` deployment via the Rust deployments lookup.
pub struct Pool<'a> {
    entry: &'a PoolEntry,
    chain_id: u64,
}

impl<'a> Pool<'a> {
    #[must_use]
    pub fn new(entry: &'a PoolEntry, chain_id: u64) -> Self {
        Self { entry, chain_id }
    }

    /// The chain id this pool's (`chain, factory`) deployment belongs to —
    /// required to resolve the DEX name via `deployments.json`.
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.chain_id
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
            PoolEntry::V2(id, _) => Identity::ReservePair {
                variant: ReservePairVariant::UniswapV2,
                dex: resolve_dex_name(self.chain_id, id.factory),
            },
            PoolEntry::AerodromeV2(id, _) => Identity::ReservePair {
                variant: ReservePairVariant::AerodromeV2 { stable: id.stable },
                dex: resolve_dex_name(self.chain_id, id.factory),
            },
            PoolEntry::V3(id, _) => Identity::ConcentratedLiquidity {
                variant: ConcentratedLiquidityVariant::UniswapV3,
                dex: resolve_dex_name(self.chain_id, id.factory),
            },
            PoolEntry::V4(..) => Identity::ConcentratedLiquidity {
                variant: ConcentratedLiquidityVariant::UniswapV4,
                // V4 is the Uniswap singleton; no `(chain, factory)` deployment
                // row exists, so it degrades to the generic variant for now.
                dex: None,
            },
            PoolEntry::Curve(..) => Identity::BalanceVector {
                variant: BalanceVectorVariant::Curve,
                dex: None,
            },
            PoolEntry::BalancerWeighted(..) => Identity::BalanceVector {
                variant: BalanceVectorVariant::BalancerWeighted,
                // Balancer/Curve balance-vector pools don't carry a `factory`
                // on the identity; DEX-name resolution is scoped to V2/V3/V4.
                dex: None,
            },
            PoolEntry::BalancerStable(..) => Identity::BalanceVector {
                variant: BalanceVectorVariant::BalancerStable,
                dex: None,
            },
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
    pub fn concentrated_liquidity(&self) -> Option<ConcentratedLiquidityView<'a>> {
        match self.entry {
            PoolEntry::V3(id, state) => Some(ConcentratedLiquidityView::V3(id, state)),
            PoolEntry::V4(id, state) => Some(ConcentratedLiquidityView::V4(id, state)),
            _ => None,
        }
    }

    #[must_use]
    pub fn balance_vector(&self) -> Option<BalanceVectorView<'a>> {
        match self.entry {
            PoolEntry::Curve(id, state) => Some(BalanceVectorView::Curve(id, state)),
            PoolEntry::BalancerWeighted(id, state) => {
                Some(BalanceVectorView::BalancerWeighted(id, state))
            }
            PoolEntry::BalancerStable(id, state) => {
                Some(BalanceVectorView::BalancerStable(id, state))
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn calculate_tokens_out(&self, zero_for_one: bool, amount_in: U256) -> Option<U256> {
        simulate_swap(self.entry, zero_for_one, amount_in).ok()
    }
}

/// Read-only reserve-pair view over V2 or AerodromeV2 state.
#[allow(clippy::doc_markdown)]
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

/// Read-only concentrated-liquidity view over V3 or V4 state.
#[allow(clippy::doc_markdown)]
pub enum ConcentratedLiquidityView<'a> {
    V3(&'a V3PoolIdentity, &'a V3PoolState),
    V4(&'a V4PoolIdentity, &'a V4PoolState),
}

impl ConcentratedLiquidityView<'_> {
    #[must_use]
    pub fn token0(&self) -> Address {
        match self {
            Self::V3(id, _) => id.token0,
            Self::V4(id, _) => id.pool_key.currency0,
        }
    }

    #[must_use]
    pub fn token1(&self) -> Address {
        match self {
            Self::V3(id, _) => id.token1,
            Self::V4(id, _) => id.pool_key.currency1,
        }
    }

    #[must_use]
    pub fn fee(&self) -> u32 {
        match self {
            Self::V3(id, _) => id.fee,
            Self::V4(id, _) => id.pool_key.fee,
        }
    }

    #[must_use]
    pub fn tick_spacing(&self) -> i32 {
        match self {
            Self::V3(id, _) => id.tick_spacing,
            Self::V4(id, _) => id.pool_key.tick_spacing,
        }
    }

    #[must_use]
    pub fn sqrt_price_x96(&self) -> U256 {
        match self {
            Self::V3(_, s) => s.sqrt_price_x96,
            Self::V4(_, s) => s.sqrt_price_x96,
        }
    }

    #[must_use]
    pub fn liquidity(&self) -> u128 {
        match self {
            Self::V3(_, s) => s.liquidity,
            Self::V4(_, s) => s.liquidity,
        }
    }

    #[must_use]
    pub fn tick(&self) -> i32 {
        match self {
            Self::V3(_, s) => s.tick,
            Self::V4(_, s) => s.tick,
        }
    }
}

/// Read-only N-token balance view over Curve or Balancer state.
#[allow(clippy::doc_markdown)]
pub enum BalanceVectorView<'a> {
    Curve(&'a CurvePoolIdentity, &'a CurvePoolState),
    BalancerWeighted(
        &'a BalancerWeightedPoolIdentity,
        &'a BalancerWeightedPoolState,
    ),
    BalancerStable(&'a BalancerStablePoolIdentity, &'a BalancerStablePoolState),
}

impl BalanceVectorView<'_> {
    #[must_use]
    pub fn tokens(&self) -> &[Address] {
        match self {
            Self::Curve(id, _) => &id.tokens,
            Self::BalancerWeighted(id, _) => &id.tokens,
            Self::BalancerStable(id, _) => &id.tokens,
        }
    }

    #[must_use]
    pub fn balances(&self) -> &[U256] {
        match self {
            Self::Curve(_, s) => &s.balances,
            Self::BalancerWeighted(_, s) => &s.balances,
            Self::BalancerStable(_, s) => &s.balances,
        }
    }

    #[must_use]
    pub fn n_tokens(&self) -> usize {
        self.tokens().len()
    }
}
