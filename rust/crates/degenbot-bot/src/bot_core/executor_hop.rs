//! Shared construction of executor hop descriptors.
//!
//! Strategy-local pool identities differ, but the fields consumed by the
//! command encoder are the same. Keeping the conversion here prevents the
//! settlement and backrun projections from drifting on fees, directions, or
//! V4 identity formatting.

use alloy::primitives::{Address, B256};
use degenbot_executor::composers::{HopInfo, V2HopInfo, V3HopInfo, V4HopInfo};

/// Convert a V2 retained-fee fraction to the executor's bips-of-10,000 fee.
#[must_use]
pub fn v2_fee_bips(gamma: u64, denom: u64) -> u16 {
    if denom == 0 || gamma > denom {
        return 0;
    }
    let fee_numer = u128::from(denom - gamma);
    let fee_denom = u128::from(denom);
    let fee_bips = (fee_numer * 10_000) / fee_denom;
    u16::try_from(fee_bips).expect("V2 fee bips are bounded by 10,000")
}

/// Build the executor descriptor for a V2 hop.
#[must_use]
pub fn v2_hop(
    pool_address: Address,
    token0_address: Address,
    token1_address: Address,
    fee: u16,
    zfo: bool,
) -> HopInfo {
    HopInfo::V2(V2HopInfo {
        pool_address,
        token0_address,
        token1_address,
        fee,
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
mod tests {
    use super::{v2_fee_bips, v2_hop, v3_hop, v4_hop};
    use alloy::primitives::{address, B256};
    use degenbot_executor::composers::HopInfo;

    #[test]
    fn v2_fee_conversion_uses_retained_fee_bips() {
        assert_eq!(v2_fee_bips(997, 1_000), 30);
        assert_eq!(v2_fee_bips(0, 0), 0);
    }

    #[test]
    fn hop_builders_preserve_family_fields() {
        let pool = address!("0000000000000000000000000000000000000001");
        let token0 = address!("0000000000000000000000000000000000000002");
        let token1 = address!("0000000000000000000000000000000000000003");
        let manager = address!("0000000000000000000000000000000000000004");
        let pool_id = B256::new([0xabu8; 32]);

        let HopInfo::V2(v2) = v2_hop(pool, token0, token1, 30, true) else {
            panic!("expected V2 descriptor");
        };
        assert_eq!(v2.pool_address, pool);
        assert_eq!(v2.fee, 30);
        assert!(v2.zfo);

        let HopInfo::V3(v3) = v3_hop(pool, token0, token1, 500, false) else {
            panic!("expected V3 descriptor");
        };
        assert_eq!(v3.pool_address, pool);
        assert_eq!(v3.fee, 500);
        assert!(!v3.zfo);

        let HopInfo::V4(v4) = v4_hop(manager, pool_id, token0, token1, 500, 10, pool, true) else {
            panic!("expected V4 descriptor");
        };
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
