//! Rust-owned `composers::PathInfo` projection for an engine-registered path
//! (NXM2BF — the encode-relay flatten).
//!
//! [`ArbitrageEngine::path_info_for`] builds the engine-facing
//! [`degenbot_executor::composers::PathInfo`] directly from a registered
//! `path_id`'s stored [`MixedPath`] + the shared [`BotState`] pool identities
//! — eliminating the Python `build_hops_from_pools` → Python `PathInfo`
//! dataclass → `extract_path_info` round-trip.
//!
//! This is the Rust-side mirror of the exact identity fields the (deleted)
//! Python `build_hops_from_pools` read off the `PyPool` handles. Because the
//! projection feeds the SAME `composers::PathInfo` to the SAME
//! [`degenbot_executor::composers::encode_cmd_stream`], byte-for-byte
//! encoder parity reduces to "does the projection build identical `HopInfo`
//! field values?" — pinned by the `degenbot-executor` 59-fn golden suite +
//! the `tests/rust-seam/` dispatch spy tests.
//!
//! # Unsupported hop families
//!
//! `composers::HopInfo` has only `V2`/`V3`/`V4` variants. The engine's
//! `HopType` enum additionally covers `SolidlyStable`, `BalancerWeighted`,
//! `BalancerStable`, `CurveStableswap` — for which the command-stream encoder
//! has no arm (`composers.rs` "2-hop Solidly / mixed-V2-V3-with-Solidly: not
//! supported"). The projection returns
//! [`PathInfoBuildError::UnsupportedHopType`] for these — matching the
//! pre-flatten `extract_hop` behavior, which rejected `SolidlyHopInfo` with
//! "hop must be V2HopInfo/V3HopInfo/V4HopInfo". No new encoding capability is
//! added; the pre-existing Solidly/Balancer/Curve encoding gap is preserved
//! (out of scope for the flatten — a future `HopInfo::Solidly` /
//! `HopInfo::Balancer` task).

use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};
use thiserror::Error;

use crate::bot_core::BotState;
use ::degenbot_solvers::mixed::{HopType, MixedPoolRef};

use super::ArbitrageEngine;

/// Why [`ArbitrageEngine::path_info_for`] could not build a `PathInfo`.
#[derive(Debug, Error)]
pub enum PathInfoBuildError {
    /// A hop's `pool_key` is not registered in the associated `BotState`.
    #[error("pool_id {pool_id} is not registered in the associated BotState")]
    PoolNotRegistered { pool_id: u64 },
    /// The hop's family has no command-stream encoder arm (Solidly /
    /// Balancer / Curve). Matches the pre-flatten `extract_hop` rejection.
    #[error(
        "hop_type {hop_type:?} (pool_id {pool_id}) is not supported by the command-stream encoder"
    )]
    UnsupportedHopType { hop_type: HopType, pool_id: u64 },
}

impl ArbitrageEngine {
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
    /// engine under the core lock).
    #[must_use]
    pub fn path_info_for(&self, path_id: u64) -> Option<Result<PathInfo, PathInfoBuildError>> {
        let path = self.path_pools.get(&path_id)?;
        let core = self.core.read();
        let mut hops = Vec::with_capacity(path.pools.len());
        for pool_ref in &path.pools {
            match build_hop_info(&core, pool_ref) {
                Ok(hop) => hops.push(hop),
                Err(e) => return Some(Err(e)),
            }
        }
        Some(Ok(PathInfo::new(hops)))
    }
}

/// Resolve one registered hop to its encoder descriptor.
///
/// `pool_ref.pool_key` is the `BotState` `pool_id` (set at `register_path`
/// time — see `lifecycle::register_path`: `pool_key: hop.pool_id`).
fn build_hop_info(core: &BotState, pool_ref: &MixedPoolRef) -> Result<HopInfo, PathInfoBuildError> {
    match pool_ref.hop_type {
        HopType::V2 => {
            let id = core.get_v2_identity(pool_ref.pool_key).ok_or(
                PathInfoBuildError::PoolNotRegistered {
                    pool_id: pool_ref.pool_key,
                },
            )?;
            // The Python `build_hops_from_pools` computes
            // `fee = int(pool.fee_token0 * 10000)` where the Python
            // `fee_token0` is the FEE fraction `Fraction(denom - gamma, denom)`
            // derived from the Rust `(gamma_numer, fee_denom)` pair (see
            // `v2_liquidity_pool.py` — `self._fee_token0 = Fraction(denom - gamma, denom)`).
            // `int()` truncates toward zero; both operands are non-negative so
            // this is floor division. The projection matches by construction.
            let (gamma, denom) = if pool_ref.zero_for_one {
                id.fee_token0
            } else {
                id.fee_token1
            };
            let fee = v2_fee_bips(gamma, denom);
            Ok(HopInfo::V2(V2HopInfo {
                pool_address: id.address,
                token0_address: id.token0,
                token1_address: id.token1,
                fee,
                zfo: pool_ref.zero_for_one,
            }))
        }
        HopType::V3 => {
            let id = core.get_v3_identity(pool_ref.pool_key).ok_or(
                PathInfoBuildError::PoolNotRegistered {
                    pool_id: pool_ref.pool_key,
                },
            )?;
            Ok(HopInfo::V3(V3HopInfo {
                pool_address: id.address,
                token0_address: id.token0,
                token1_address: id.token1,
                fee: id.fee,
                zfo: pool_ref.zero_for_one,
            }))
        }
        HopType::V4 => {
            let id = core.get_v4_identity(pool_ref.pool_key).ok_or(
                PathInfoBuildError::PoolNotRegistered {
                    pool_id: pool_ref.pool_key,
                },
            )?;
            // `0x` + 64 lowercase hex chars — the canonical `V4PoolId` form,
            // matching `diagnostic::format_v4_pool_id` + the Python
            // `pool.pool_id.to_0x_hex()`. The encoder parses this back to the
            // 32-byte salted pool id.
            let pool_id_hex = format!("0x{}", alloy::hex::encode(id.pool_id));
            Ok(HopInfo::V4(V4HopInfo {
                pool_manager_address: id.pool_manager,
                pool_id_hex,
                currency0_address: id.pool_key.currency0,
                currency1_address: id.pool_key.currency1,
                fee: id.pool_key.fee,
                tick_spacing: id.pool_key.tick_spacing,
                hook_address: id.pool_key.hooks,
                zfo: pool_ref.zero_for_one,
            }))
        }
        HopType::SolidlyStable
        | HopType::BalancerWeighted
        | HopType::BalancerStable
        | HopType::CurveStableswap => Err(PathInfoBuildError::UnsupportedHopType {
            hop_type: pool_ref.hop_type,
            pool_id: pool_ref.pool_key,
        }),
    }
}

/// V2 fee in bips-of-10000 from the `(gamma_numer, fee_denom)` retained
/// fraction. Mirrors `int(Fraction(denom - gamma, denom) * 10000)` (Python
/// `int()` truncates toward zero).
///
/// `fee_denom == 0` is guarded: a registered V2 pool always has a non-zero
/// denominator (validated at registration), so this branch is diagnostic
/// only — it yields `0` rather than panicking, preserving the "never crash
/// the pump on a malformed identity" contract.
fn v2_fee_bips(gamma: u64, denom: u64) -> u16 {
    if denom == 0 || gamma > denom {
        return 0;
    }
    let fee_numer = u128::from(denom - gamma);
    let fee_denom = u128::from(denom);
    // `fee_bips = (1 − gamma/denom) × 10_000`. With `gamma ≤ denom` (guarded
    // above), `fee_bips ≤ 10_000 ≤ u16::MAX`, so `try_into` cannot fail on a
    // valid path; `.expect` surfaces a real invariant break loudly rather than
    // masking to `u16::MAX` (the prior `.unwrap_or(u16::MAX)` would have
    // surfaced a bogus 65535 fee tier if `gamma > denom` ever slipped past the
    // guard, hiding the bug instead of failing).
    u16::try_from((fee_numer * 10_000) / fee_denom)
        .expect("fee_bips <= 10000 <= u16::MAX under the gamma <= denom guard")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use alloy::primitives::{aliases::U112, Address, U256};
    use degenbot_executor::composers::HopInfo;

    use crate::bot_core::{PoolTickCoverage, RegisterV3PoolParams, RegisterV4PoolParams};
    use crate::solvers::arb_engine::ArbitrageEngine;
    use ::degenbot_decoders::v4_swap_decoder::V4PoolId;
    use ::degenbot_pools::v4_state::V4PoolKey;
    use ::degenbot_solvers::mixed::PoolHop;

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
        let path_id = engine
            .register_path(vec![PoolHop {
                pool_id,
                zero_for_one: true,
            }])
            .expect("register_path");

        let path = engine
            .path_info_for(path_id)
            .expect("path exists")
            .expect("v2 supported");
        assert_eq!(path.hops.len(), 1);
        let HopInfo::V2(v2) = &path.hops[0] else {
            panic!("expected V2 hop, got {:?}", path.hops[0]);
        };
        assert_eq!(v2.pool_address, pool_addr);
        assert_eq!(v2.token0_address, Address::ZERO);
        assert_eq!(v2.token1_address, Address::ZERO);
        assert_eq!(v2.fee, 30);
        assert!(v2.zfo);
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
        let path_id = engine
            .register_path(vec![PoolHop {
                pool_id,
                zero_for_one: false,
            }])
            .expect("register_path");
        let path = engine
            .path_info_for(path_id)
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
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            deployer: Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            ..Default::default()
        });
        let path_id = engine
            .register_path(vec![PoolHop {
                pool_id,
                zero_for_one: true,
            }])
            .expect("register_path");
        let path = engine
            .path_info_for(path_id)
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
                sqrt_price_x96: U256::from(SQRT_PRICE_1_1),
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
            })
            .expect("register_v4_pool");
        let path_id = engine
            .register_path(vec![PoolHop {
                pool_id,
                zero_for_one: true,
            }])
            .expect("register_path");
        let path = engine
            .path_info_for(path_id)
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
        assert!(engine.path_info_for(999).is_none());
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
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            deployer: Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            ..Default::default()
        });
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3,
                    zero_for_one: false,
                },
            ])
            .expect("register_path");
        let path = engine
            .path_info_for(path_id)
            .expect("path exists")
            .expect("supported");
        assert_eq!(path.hops.len(), 2);
        assert!(matches!(path.hops[0], HopInfo::V2(_)));
        assert!(matches!(path.hops[1], HopInfo::V3(_)));
    }
}
