//! The engine-side path-info seam: `ArbitrageEngine`-registered paths resolve
//! to the executor's `composers::PathInfo` (NXM2BF — the encode-relay
//! flatten).
//!
//! The pure projection (a hop list + `BotState` pool identities →
//! `degenbot_executor::composers::PathInfo`) lives in
//! `degenbot_substrate::path_info`, where the strategy plane can reach it
//! without the engine. What stays here is the engine-registry lookup
//! ([`path_info_for`]) and the telemetry hop description — both ride the
//! engine's path registry and core read lock.

use ::degenbot_solvers::mixed::HopType;
use degenbot_executor::composers::PathInfo;

use super::ArbitrageEngine;
use degenbot_substrate::path_info::PathInfoBuildError;
use degenbot_substrate::BotState;

/// Build the engine-facing `composers::PathInfo` for `path_id` by
/// resolving each registered hop's identity from the shared `BotState`.
///
/// Returns:
/// - `None` if `path_id` was never registered.
/// - `Some(Err(..))` if a hop's identity is missing or its family has no
///   command-stream encoder arm.
///
/// # Lock discipline
///
/// Acquires the `BotState` read lock once for the whole projection
/// (engine-then-core is the only nested order — no re-entry into the
/// engine under the core lock). A crate-internal free function (T5): the
/// stage surface owns the public seam.
#[must_use]
pub(crate) fn path_info_for(
    engine: &ArbitrageEngine,
    path_id: u64,
) -> Option<Result<PathInfo, PathInfoBuildError>> {
    let path = engine.registry.get(path_id)?;
    let core = engine
        .core
        .read_at(degenbot_substrate::state_lock::LockSite::Solver);
    Some(degenbot_substrate::path_info::build_path_info(
        &core,
        &path.pools,
    ))
}

/// Telemetry helper: render one hop as `FAMILY:pool(zfo=N)`. Unresolvable
/// identities degrade to the raw `pool_id` rather than failing — this only
/// ever feeds trace fields, never solver or encoder logic.
#[must_use]
pub(crate) fn describe_hop(
    core: &BotState,
    hop_type: HopType,
    pool_id: u64,
    zero_for_one: bool,
) -> String {
    let zfo = u8::from(zero_for_one);
    match hop_type {
        HopType::V2 => core.get_v2_identity(pool_id).map_or_else(
            || format!("V2:pool_id={pool_id}(zfo={zfo})"),
            |id| format!("V2:{:#x}(zfo={zfo})", id.address),
        ),
        HopType::V3 => core.get_v3_identity(pool_id).map_or_else(
            || format!("V3:pool_id={pool_id}(zfo={zfo})"),
            |id| format!("V3:{:#x}(zfo={zfo})", id.address),
        ),
        HopType::V4 => core.get_v4_identity(pool_id).map_or_else(
            || format!("V4:pool_id={pool_id}(zfo={zfo})"),
            |id| {
                format!(
                    "V4:{:#x}:0x{}(zfo={zfo})",
                    id.pool_manager,
                    alloy::hex::encode(id.pool_id)
                )
            },
        ),
        other => format!("{other:?}:pool_id={pool_id}"),
    }
}

#[expect(clippy::expect_used, clippy::panic, clippy::similar_names)]
#[cfg(test)]
mod tests {
    use crate::arb_engine::lifecycle::register_path;
    use crate::arb_engine::ArbitrageEngine;
    use crate::bot_core::{PoolTickCoverage, RegisterV3PoolParams, RegisterV4PoolParams};
    use ::degenbot_decoders::v4_swap_decoder::V4PoolId;
    use ::degenbot_pools::v4_state::V4PoolKey;
    use ::degenbot_solvers::mixed::PoolHop;
    use alloy::primitives::{aliases::U112, Address, U256};
    use degenbot_executor::composers::HopInfo;
    use hashbrown::HashMap;

    fn usdc(amount: u64) -> U112 {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(6))).to::<U112>()
    }

    fn weth(amount: u64) -> U112 {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(18))).to::<U112>()
    }

    const GAMMA_03: u64 = 997;
    const FEE_DENOM_03: u64 = 1000;
    const SQRT_PRICE_1_1: u128 = 79_228_162_514_264_337_593_543_950_336;

    /// A V2 0.3% hop resolves to `V2HopInfo { fee: 30, zfo: true }` — the
    /// exact `int(Fraction(1000-997, 1000) * 10000)` value the Python
    /// `build_hops_from_pools` produced.
    #[test]
    fn v2_path_projects_to_hop_info_with_fee_bips() {
        let mut engine = ArbitrageEngine::new();
        let pool_addr = Address::from([0x11u8; 20]);
        let pool_id = engine.register_v2_pool(
            pool_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let pool2_id = engine.register_v2_pool(
            Address::from([0x12u8; 20]),
            usdc(1_600_000),
            weth(820),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let path_id = register_path(
            &mut engine,
            vec![
                PoolHop {
                    pool_id,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: pool2_id,
                    zero_for_one: true,
                },
            ],
        )
        .expect("register_path");
        let path = super::path_info_for(&engine, path_id)
            .expect("path exists")
            .expect("v2 supported");
        assert_eq!(path.hops.len(), 2);
        let HopInfo::V2(v2) = &path.hops[0] else {
            panic!("expected V2 hop, got {:?}", path.hops[0]);
        };
        assert_eq!(v2.pool_address, pool_addr);
        assert_eq!(v2.token0_address, Address::ZERO);
        assert_eq!(v2.token1_address, Address::ZERO);
        assert_eq!(v2.fee, 30);
        assert!(v2.zfo);
    }

    /// Telemetry: `describe_path` names the CONCRETE pool addresses (the
    /// operator ask — "which pools are in the path"), degrading to raw
    /// `pool_id` for unregistered ids.
    #[test]
    fn describe_path_names_concrete_pools() {
        let mut engine = ArbitrageEngine::new();
        let pool_addr = Address::from([0x44u8; 20]);
        let pid = engine.register_v2_pool(
            pool_addr,
            usdc(1_000_000),
            weth(500),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let pid2 = engine.register_v2_pool(
            Address::from([0x45u8; 20]),
            usdc(1_100_000),
            weth(510),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let path_id = register_path(
            &mut engine,
            vec![
                PoolHop {
                    pool_id: pid,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: pid2,
                    zero_for_one: true,
                },
            ],
        )
        .expect("register_path");
        let desc = engine.cycle.describe_path(path_id, &engine.registry);
        assert!(
            desc.contains("V2:0x4444"),
            "describe_path must carry the concrete pool address: {desc}"
        );
        assert!(desc.contains("zfo=1"), "zfo flag missing: {desc}");
        assert_eq!(
            engine.cycle.describe_path(99_999, &engine.registry),
            "path_id=99999 (unregistered)"
        );
    }

    /// Reverse direction selects `fee_token1` (identical fee here) + `zfo: false`.
    #[test]
    fn v2_path_reverse_direction_sets_zfo_false() {
        let mut engine = ArbitrageEngine::new();
        let pool_addr = Address::from([0x22u8; 20]);
        let pool_id = engine.register_v2_pool(
            pool_addr,
            usdc(1_000_000),
            weth(500),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let pool2_id = engine.register_v2_pool(
            Address::from([0x23u8; 20]),
            usdc(1_100_000),
            weth(510),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let path_id = register_path(
            &mut engine,
            vec![
                PoolHop {
                    pool_id,
                    zero_for_one: false,
                },
                PoolHop {
                    pool_id: pool2_id,
                    zero_for_one: true,
                },
            ],
        )
        .expect("register_path");
        let path = super::path_info_for(&engine, path_id)
            .expect("path exists")
            .expect("v2 supported");
        let HopInfo::V2(v2) = &path.hops[0] else {
            panic!("expected V2 hop");
        };
        assert!(!v2.zfo);
        assert_eq!(v2.fee, 30);
    }

    #[test]
    fn v3_path_projects_to_hop_info() {
        let mut engine = ArbitrageEngine::new();
        let pool_id = engine.register_v3_pool(&RegisterV3PoolParams {
            address: Address::from([0x33u8; 20]),
            token0: Address::from([0u8; 20]),
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            sqrt_price_x96: U256::from(SQRT_PRICE_1_1),
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            deployer: Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            ..Default::default()
        });
        let pool2_id = engine.register_v2_pool(
            Address::from([0x34u8; 20]),
            usdc(1_200_000),
            weth(520),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let path_id = register_path(
            &mut engine,
            vec![
                PoolHop {
                    pool_id,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: pool2_id,
                    zero_for_one: true,
                },
            ],
        )
        .expect("register_path");
        let path = super::path_info_for(&engine, path_id)
            .expect("path exists")
            .expect("v3 supported");
        let HopInfo::V3(v3) = &path.hops[0] else {
            panic!("expected V3 hop");
        };
        assert_eq!(v3.pool_address, Address::from([0x33u8; 20]));
        assert_eq!(v3.token0_address, Address::from([0u8; 20]));
        assert_eq!(v3.token1_address, Address::from([1u8; 20]));
        assert_eq!(v3.fee, 3000);
        assert!(v3.zfo);
    }

    #[test]
    fn v4_path_projects_to_hop_info_with_pool_id_hex() {
        let mut engine = ArbitrageEngine::new();
        let pool_id_bytes: V4PoolId = [0xabu8; 32];
        let pool_id = engine
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: Address::from([0x44u8; 20]),
                pool_id: pool_id_bytes,
                pool_key: V4PoolKey {
                    currency0: Address::from([0u8; 20]),
                    currency1: Address::from([2u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(SQRT_PRICE_1_1),
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
            })
            .expect("register_v4_pool");
        let pool2_id = engine.register_v2_pool(
            Address::from([0x46u8; 20]),
            usdc(1_300_000),
            weth(530),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let path_id = register_path(
            &mut engine,
            vec![
                PoolHop {
                    pool_id,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: pool2_id,
                    zero_for_one: true,
                },
            ],
        )
        .expect("register_path");
        let path = super::path_info_for(&engine, path_id)
            .expect("path exists")
            .expect("v4 supported");
        let HopInfo::V4(v4) = &path.hops[0] else {
            panic!("expected V4 hop");
        };
        assert_eq!(v4.pool_manager_address, Address::from([0x44u8; 20]));
        assert_eq!(v4.pool_id_hex, "0x".to_string() + &"ab".repeat(32));
        assert_eq!(v4.currency0_address, Address::from([0u8; 20]));
        assert_eq!(v4.currency1_address, Address::from([2u8; 20]));
        assert_eq!(v4.fee, 500);
        assert_eq!(v4.tick_spacing, 10);
        assert_eq!(v4.hook_address, Address::ZERO);
        assert!(v4.zfo);
    }

    /// Unknown `path_id` → `None` (matches "no Python `PathInfo` in the registry").
    #[test]
    fn unknown_path_id_returns_none() {
        let engine = ArbitrageEngine::new();
        assert!(super::path_info_for(&engine, 999).is_none());
    }

    /// A multi-hop V2→V3 path projects to a two-hop `PathInfo` in order.
    #[test]
    fn multi_hop_path_projects_in_order() {
        let mut engine = ArbitrageEngine::new();
        let v2 = engine.register_v2_pool(
            Address::from([0xaau8; 20]),
            usdc(1_000_000),
            weth(500),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let v3 = engine.register_v3_pool(&RegisterV3PoolParams {
            address: Address::from([0xbbu8; 20]),
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 500,
            tick_spacing: 10,
            sqrt_price_x96: U256::from(SQRT_PRICE_1_1),
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            deployer: Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            ..Default::default()
        });
        let path_id = register_path(
            &mut engine,
            vec![
                PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3,
                    zero_for_one: false,
                },
            ],
        )
        .expect("register_path");
        let path = super::path_info_for(&engine, path_id)
            .expect("path exists")
            .expect("supported");
        assert_eq!(path.hops.len(), 2);
        assert!(matches!(path.hops[0], HopInfo::V2(_)));
        assert!(matches!(path.hops[1], HopInfo::V3(_)));
    }
}
