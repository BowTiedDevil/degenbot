//! Shared construction of executor hop descriptors.
//!
//! Strategy-local pool identities differ, but the fields consumed by the
//! command encoder are the same. Keeping the conversion here prevents the
//! settlement and backrun projections from drifting on fees, directions, or
//! V4 identity formatting.

use alloy::primitives::{Address, B256};
use degenbot_core::fee::V2Fee;
use degenbot_execution::{HopInfo, V2HopInfo, V3HopInfo, V4HopInfo};

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
    use super::{v2_hop, v3_hop, v4_hop};
    use alloy::primitives::{address, B256};
    use degenbot_core::fee::{V2Fee, V2FeePair, V2FeeRefusal};
    use degenbot_execution::HopInfo;

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
