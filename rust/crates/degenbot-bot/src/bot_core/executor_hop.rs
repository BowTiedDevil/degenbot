//! Shared construction of executor hop descriptors.
//!
//! Strategy-local pool identities differ, but the fields consumed by the
//! command encoder are the same. Keeping the conversion here prevents the
//! settlement and backrun projections from drifting on fees, directions, or
//! V4 identity formatting.

use alloy::primitives::{Address, B256};
use degenbot_executor::composers::{HopInfo, V2HopInfo, V3HopInfo, V4HopInfo};

/// Why a discovered V2 fee cannot enter the solver or executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum V2FeeRefusal {
    #[error("V2 fee is missing")]
    Missing,
    #[error("V2 fee is negative")]
    Negative,
    #[error("V2 fee denominator is zero")]
    ZeroDenominator,
    #[error("V2 fee exceeds its denominator")]
    FeeExceedsDenominator,
    #[error("V2 fee is not representable by the executor")]
    ExecutorOverflow,
}

/// A validated V2 fee charge. Construction is the only boundary that admits a
/// discovered fee; downstream values cannot invent or default one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Fee {
    fee_numer: u64,
    denominator: u64,
    executor_bips: u16,
}

impl V2Fee {
    /// Validate one executor-facing V2 fee charge.
    ///
    /// # Errors
    ///
    /// Refuses a zero denominator or a charge above one. A valid charge
    /// converts to at most 10,000 bips, within the executor's `u16`.
    pub fn new(fee_numer: u64, denominator: u64) -> Result<Self, V2FeeRefusal> {
        if denominator == 0 {
            return Err(V2FeeRefusal::ZeroDenominator);
        }
        if fee_numer > denominator {
            return Err(V2FeeRefusal::FeeExceedsDenominator);
        }
        let fee_bips = (u128::from(fee_numer) * 10_000) / u128::from(denominator);
        if fee_bips > u128::from(u16::MAX) {
            return Err(V2FeeRefusal::ExecutorOverflow);
        }
        Ok(Self {
            fee_numer,
            denominator,
            executor_bips: u16::try_from(fee_bips).unwrap_or(u16::MAX),
        })
    }

    /// Convert a solver identity's retained fraction into an executor fee.
    ///
    /// # Errors
    ///
    /// Refuses the same invalid fractions as [`Self::new`].
    pub fn from_retained(gamma_numer: u64, denominator: u64) -> Result<Self, V2FeeRefusal> {
        let Some(fee_numer) = denominator.checked_sub(gamma_numer) else {
            return Err(V2FeeRefusal::FeeExceedsDenominator);
        };
        Self::new(fee_numer, denominator)
    }

    /// The retained-fee fraction consumed by V2 pool-state registration.
    #[must_use]
    pub const fn retained_fraction(self) -> (u64, u64) {
        (self.denominator - self.fee_numer, self.denominator)
    }

    /// This direction's fee in the executor's bips-of-10,000 convention.
    #[must_use]
    pub const fn executor_bips(self) -> u16 {
        self.executor_bips
    }
}

/// A discovered V2 fee, or the typed reason it cannot be used.
pub type V2FeeProjection = Result<V2Fee, V2FeeRefusal>;

/// The two directional solver fees carried by a V2 pool identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Fees {
    pub token0: V2Fee,
    pub token1: V2Fee,
}

impl V2Fees {
    /// The executor fee for one canonical token direction.
    #[must_use]
    pub const fn direction(self, zfo: bool) -> V2Fee {
        if zfo {
            self.token0
        } else {
            self.token1
        }
    }
}

/// Direction-specific fee projections loaded with a V2 connector edge.
/// Refusals remain typed on the edge so admission can reject the pool before
/// workspace registration instead of discovering bad fee data during solve or
/// composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2FeePair {
    pub token0: V2FeeProjection,
    pub token1: V2FeeProjection,
}

impl V2FeePair {
    /// Construct a pair from already-validated directional fees.
    #[must_use]
    pub const fn new(token0: V2Fee, token1: V2Fee) -> Self {
        Self {
            token0: Ok(token0),
            token1: Ok(token1),
        }
    }

    /// Project the signed DB columns from one discovered V2 pool row.
    #[must_use]
    pub fn from_discovered(
        fee_token0: Option<i64>,
        fee_token1: Option<i64>,
        denominator: Option<i64>,
    ) -> Self {
        Self {
            token0: Self::project(fee_token0, denominator),
            token1: Self::project(fee_token1, denominator),
        }
    }

    /// The explicit refusal used when a base row has no discovered subclass
    /// identity. It carries no substitute fee.
    #[must_use]
    pub const fn missing() -> Self {
        Self {
            token0: Err(V2FeeRefusal::Missing),
            token1: Err(V2FeeRefusal::Missing),
        }
    }

    fn project(fee: Option<i64>, denominator: Option<i64>) -> V2FeeProjection {
        let (Some(fee), Some(denominator)) = (fee, denominator) else {
            return Err(V2FeeRefusal::Missing);
        };
        if fee < 0 || denominator < 0 {
            return Err(V2FeeRefusal::Negative);
        }
        let (Ok(fee), Ok(denominator)) = (u64::try_from(fee), u64::try_from(denominator)) else {
            return Err(V2FeeRefusal::Negative);
        };
        V2Fee::new(fee, denominator)
    }

    /// Resolve both directions for solver admission.
    ///
    /// # Errors
    ///
    /// Returns the first typed directional refusal.
    pub fn resolve(self) -> Result<V2Fees, V2FeeRefusal> {
        Ok(V2Fees {
            token0: self.token0?,
            token1: self.token1?,
        })
    }

    /// Resolve the fee used by one executable direction.
    ///
    /// # Errors
    ///
    /// Returns the typed directional refusal.
    pub fn resolve_direction(self, zfo: bool) -> Result<V2Fee, V2FeeRefusal> {
        if zfo {
            self.token0
        } else {
            self.token1
        }
    }
}

/// Build the executor descriptor for a V2 hop.
#[must_use]
pub fn v2_hop(
    pool_address: Address,
    token0_address: Address,
    token1_address: Address,
    fee: V2Fee,
    zfo: bool,
) -> HopInfo {
    HopInfo::V2(V2HopInfo {
        pool_address,
        token0_address,
        token1_address,
        fee: fee.executor_bips(),
        zfo,
    })
}

/// Build the executor descriptor for a V3 hop.
#[must_use]
pub fn v3_hop(
    pool_address: Address,
    token0_address: Address,
    token1_address: Address,
    fee: u32,
    zfo: bool,
) -> HopInfo {
    HopInfo::V3(V3HopInfo {
        pool_address,
        token0_address,
        token1_address,
        fee,
        zfo,
    })
}

/// Build the executor descriptor for a V4 hop, including canonical pool-id
/// formatting shared by every strategy.
#[expect(
    clippy::too_many_arguments,
    reason = "the V4 executor identity is one atomic hop descriptor"
)]
#[must_use]
pub fn v4_hop(
    pool_manager_address: Address,
    pool_id: B256,
    currency0_address: Address,
    currency1_address: Address,
    fee: u32,
    tick_spacing: i32,
    hook_address: Address,
    zfo: bool,
) -> HopInfo {
    HopInfo::V4(V4HopInfo {
        pool_manager_address,
        pool_id_hex: format!("0x{}", alloy::hex::encode(pool_id)),
        currency0_address,
        currency1_address,
        fee,
        tick_spacing,
        hook_address,
        zfo,
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{v2_hop, v3_hop, v4_hop, V2Fee, V2FeePair, V2FeeRefusal};
    use alloy::primitives::{address, B256};
    use degenbot_executor::composers::HopInfo;

    #[test]
    fn v2_fee_projection_uses_discovered_fractions_and_refuses_bad_values() {
        let fees = V2FeePair::from_discovered(Some(5), Some(30), Some(10_000));
        let resolved = fees.resolve().expect("valid discovered fees");
        assert_eq!(resolved.token0.executor_bips(), 5);
        assert_eq!(resolved.token1.executor_bips(), 30);
        assert_eq!(V2Fee::new(1, 0).err(), Some(V2FeeRefusal::ZeroDenominator));
        assert_eq!(
            V2FeePair::missing().resolve_direction(false).err(),
            Some(V2FeeRefusal::Missing)
        );
    }

    #[test]
    fn hop_builders_preserve_family_fields() {
        let pool = address!("0000000000000000000000000000000000000001");
        let token0 = address!("0000000000000000000000000000000000000002");
        let token1 = address!("0000000000000000000000000000000000000003");
        let manager = address!("0000000000000000000000000000000000000004");
        let pool_id = B256::new([0xabu8; 32]);

        let v2_hop = v2_hop(pool, token0, token1, V2Fee::new(3, 1_000).unwrap(), true);
        assert!(matches!(v2_hop, HopInfo::V2(_)));
        if let HopInfo::V2(v2) = v2_hop {
            assert_eq!(v2.pool_address, pool);
            assert_eq!(v2.fee, 30);
            assert!(v2.zfo);
        }

        let v3_hop = v3_hop(pool, token0, token1, 500, false);
        assert!(matches!(v3_hop, HopInfo::V3(_)));
        if let HopInfo::V3(v3) = v3_hop {
            assert_eq!(v3.pool_address, pool);
            assert_eq!(v3.fee, 500);
            assert!(!v3.zfo);
        }

        let v4_hop = v4_hop(manager, pool_id, token0, token1, 500, 10, pool, true);
        assert!(matches!(v4_hop, HopInfo::V4(_)));
        if let HopInfo::V4(v4) = v4_hop {
            assert_eq!(v4.pool_manager_address, manager);
            assert_eq!(v4.pool_id_hex, format!("0x{}", "ab".repeat(32)));
            assert_eq!(v4.currency0_address, token0);
            assert_eq!(v4.currency1_address, token1);
            assert_eq!(v4.fee, 500);
            assert_eq!(v4.tick_spacing, 10);
            assert_eq!(v4.hook_address, pool);
            assert!(v4.zfo);
        }
    }
}
