//! Choreography decode unit tests (task `F2R2OC`, epic `Z5CNPB`).
//!
//! The V2/V3/V4 + ERC-20 + tick encode→call→decode choreography moved core-side
//! into this module. These tests drive each `choreography` fn through a
//! `FakeRpc` (a [`crate::bot_core::construction_io::RpcConstruction`] double)
//! that returns pre-encoded response bytes per calldata selector, asserting the
//! decode — the coverage that previously lived in
//! `tests/builders/test_pybot_io.py` as Python fake-provider tests (removed
//! together with the Python-provider seam).

use std::collections::HashMap;

use alloy::dyn_abi::DynSolValue;
use alloy::primitives::{aliases::U112, aliases::U128, Address, Bytes, FixedBytes, I256, U256};
use async_trait::async_trait;
use degenbot_core::errors::ProviderError;
use degenbot_rpc::abi;

use super::{builder, choreography, curve_choreography};
use crate::bot_core::construction_io::{ConstructionIo, NoDb, RpcConstruction};

const TO: Address = alloy::primitives::address!("0x1111111111111111111111111111111111111111");
const SV: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");
const POOL_ID: [u8; 32] = [0xab; 32];

/// ABI-encode a single return value → return bytes (inherent
/// `DynSolValue::abi_encode`, the same independent encoder path the crate's
/// `degenbot-rpc::abi` decoder-oracle tests use).
#[allow(clippy::needless_pass_by_value)] // test helper takes owned DynSolValue for inline call sites
fn enc(v: DynSolValue) -> Vec<u8> {
    v.abi_encode()
}

/// ABI word with an address right-aligned in its low 20 bytes.
fn addr_word(a: Address) -> Vec<u8> {
    let mut v = vec![0u8; 12];
    v.extend_from_slice(a.as_slice());
    v
}

/// A top-level `string` return encodes as `[offset=0x20][len][data(32-padded)]`
/// (the canonical single-value ABI return, matching `DynSolType::String
/// .abi_decode`).
fn str_ret(s: &str) -> Vec<u8> {
    let n = s.len();
    let mut v = U256::from(32u64).to_be_bytes::<32>().to_vec(); // offset -> data
    v.extend_from_slice(&U256::from(n).to_be_bytes::<32>()); // length
    v.extend_from_slice(s.as_bytes());
    v.resize(v.len() + ((32 - n % 32) % 32), 0u8);
    v
}

struct FakeRpc {
    responses: HashMap<[u8; 4], Vec<u8>>,
    // Parameterized calls (e.g. `coins(uint256)` keyed by index) need the full
    // calldata, not just the selector; checked first, falls back to the
    // selector-keyed map.
    responses_full: HashMap<Vec<u8>, Vec<u8>>,
}

impl FakeRpc {
    fn new() -> Self {
        Self {
            responses: HashMap::new(),
            responses_full: HashMap::new(),
        }
    }
    fn set(&mut self, sel: [u8; 4], bytes: Vec<u8>) {
        self.responses.insert(sel, bytes);
    }
    fn set_full(&mut self, data: Vec<u8>, bytes: Vec<u8>) {
        self.responses_full.insert(data, bytes);
    }
}

#[async_trait]
impl RpcConstruction for FakeRpc {
    async fn get_block_number(&self) -> Result<u64, ProviderError> {
        Ok(0)
    }
    async fn get_block(
        &self,
        _b: u64,
    ) -> Result<Option<degenbot_rpc::provider::EthBlock>, ProviderError> {
        Ok(None)
    }
    async fn get_block_timestamp(&self, _b: u64) -> Result<Option<u64>, ProviderError> {
        Ok(None)
    }
    async fn get_code(&self, _a: Address, _b: Option<u64>) -> Result<Bytes, ProviderError> {
        Ok(Bytes::new())
    }
    async fn get_balance(&self, _a: Address, _b: Option<u64>) -> Result<U256, ProviderError> {
        Ok(U256::ZERO)
    }
    async fn call(
        &self,
        _to: Address,
        data: Bytes,
        _block: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        // Full-calldata match wins (parameterized calls); otherwise fall back
        // to the 4-byte selector.
        if let Some(b) = self.responses_full.get(data.as_ref()) {
            return Ok(b.clone().into());
        }
        match self.responses.get(&data[..4]) {
            Some(b) => Ok(b.clone().into()),
            None => Err(ProviderError::ExecutionReverted {
                code: -32000,
                message: "no fake response".into(),
            }),
        }
    }
}

fn io_with(fake: FakeRpc) -> ConstructionIo {
    ConstructionIo::new(std::sync::Arc::new(NoDb), std::sync::Arc::new(fake))
}

fn slot0_ret(sqrt: U256, tick: i32) -> Vec<u8> {
    enc(DynSolValue::Tuple(vec![
        DynSolValue::Uint(sqrt, 160),
        DynSolValue::Int(I256::try_from(i128::from(tick)).unwrap(), 24),
        DynSolValue::Uint(U256::ZERO, 16),
        DynSolValue::Uint(U256::ZERO, 16),
        DynSolValue::Uint(U256::ZERO, 16),
        DynSolValue::Uint(U256::ZERO, 8),
        DynSolValue::Bool(false),
    ]))
}

#[tokio::test]
async fn fetch_factory_address_decodes() {
    let mut f = FakeRpc::new();
    let factory: Address =
        alloy::primitives::address!("0x66f9664f97f2b50f62d13ea064982f936de76657");
    f.set(choreography::selector(b"factory()"), addr_word(factory));
    let io = io_with(f);
    let got = choreography::fetch_factory_address(&io, TO, None)
        .await
        .unwrap();
    assert_eq!(got, factory);
}

#[tokio::test]
async fn fetch_erc20_metadata_decodes_string_string_uint() {
    let mut f = FakeRpc::new();
    f.set(choreography::selector(b"name()"), str_ret("Dai Stablecoin"));
    f.set(choreography::selector(b"symbol()"), str_ret("DAI"));
    f.set(
        choreography::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(18), 256)),
    );
    let io = io_with(f);
    let got = choreography::fetch_erc20_metadata(&io, TO).await.unwrap();
    assert_eq!(
        got,
        Some(("Dai Stablecoin".to_string(), "DAI".to_string(), 18))
    );
}

#[tokio::test]
async fn fetch_erc20_metadata_returns_none_on_missing_selector() {
    // No responses configured → every call reverts → Ok(None).
    let io = io_with(FakeRpc::new());
    assert_eq!(
        choreography::fetch_erc20_metadata(&io, TO).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn fetch_v2_immutable_data_decodes() {
    let mut f = FakeRpc::new();
    let tok0: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000001");
    let tok1: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000002");
    f.set(choreography::selector(b"factory()"), addr_word(tok0));
    f.set(choreography::selector(b"token0()"), addr_word(tok0));
    f.set(choreography::selector(b"token1()"), addr_word(tok1));
    let io = io_with(f);
    let d = choreography::fetch_v2_immutable_data(&io, TO, None)
        .await
        .unwrap();
    assert_eq!(d.token0, tok0);
    assert_eq!(d.token1, tok1);
}

#[tokio::test]
async fn fetch_v2_reserves_decodes_two_uint256() {
    let mut f = FakeRpc::new();
    f.set(
        abi::encode_get_reserves()[..4].try_into().unwrap(),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(1_000_000u64), 256),
            DynSolValue::Uint(U256::from(2_000_000u64), 256),
            // `getReserves()` returns a 3-tuple (reserve0, reserve1,
            // blockTimestampLast); the decoder expects all three fields.
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    let io = io_with(f);
    let (r0, r1) = choreography::fetch_v2_reserves(&io, TO, None)
        .await
        .unwrap();
    assert_eq!(r0, U256::from(1_000_000u64));
    assert_eq!(r1, U256::from(2_000_000u64));
}

#[tokio::test]
async fn fetch_v3_immutable_data_decodes_incl_negative_tick_spacing() {
    let mut f = FakeRpc::new();
    let tok0: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000001");
    let tok1: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000002");
    f.set(choreography::selector(b"factory()"), addr_word(tok0));
    f.set(choreography::selector(b"token0()"), addr_word(tok0));
    f.set(choreography::selector(b"token1()"), addr_word(tok1));
    f.set(
        choreography::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(30), 24)),
    );
    f.set(
        choreography::selector(b"tickSpacing()"),
        enc(DynSolValue::Int(I256::try_from(-10i128).unwrap(), 24)),
    );
    let io = io_with(f);
    let d = choreography::fetch_v3_immutable_data(&io, TO, None)
        .await
        .unwrap();
    assert_eq!(d.token0, tok0);
    assert_eq!(d.token1, tok1);
    assert_eq!(d.fee, 30);
    assert_eq!(d.tick_spacing, -10);
}

#[tokio::test]
async fn fetch_v3_slot0_liquidity_decodes() {
    let mut f = FakeRpc::new();
    let sqrt = U256::from(1u128 << 96);
    f.set(
        abi::encode_slot0()[..4].try_into().unwrap(),
        slot0_ret(sqrt, -123),
    );
    f.set(
        abi::encode_liquidity()[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(U256::from(42), 128)),
    );
    let io = io_with(f);
    let (s, t, liq) = choreography::fetch_v3_slot0_liquidity(&io, TO, None)
        .await
        .unwrap();
    assert_eq!(s, sqrt);
    assert_eq!(t, I256::try_from(-123i128).unwrap());
    assert_eq!(liq, U256::from(42));
}

#[tokio::test]
async fn fetch_v4_slot0_liquidity_decodes() {
    let mut f = FakeRpc::new();
    let sqrt = U256::from(1u128 << 95);
    f.set(
        abi::encode_get_slot0(&POOL_ID)[..4].try_into().unwrap(),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(sqrt, 160),
            DynSolValue::Int(I256::try_from(7i128).unwrap(), 24),
            DynSolValue::Uint(U256::from(1), 24),
            DynSolValue::Uint(U256::from(2), 24),
        ])),
    );
    f.set(
        abi::encode_get_liquidity(&POOL_ID)[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(U256::from(99), 128)),
    );
    let io = io_with(f);
    let (s, t, pf, lf, liq) = choreography::fetch_v4_slot0_liquidity(&io, SV, POOL_ID, None)
        .await
        .unwrap();
    assert_eq!(s, sqrt);
    assert_eq!(t, I256::try_from(7i128).unwrap());
    assert_eq!(pf, U256::from(1));
    assert_eq!(lf, U256::from(2));
    assert_eq!(liq, U256::from(99));
}

#[tokio::test]
async fn fetch_tick_bitmap_decodes() {
    let mut f = FakeRpc::new();
    let bitmap = U256::from(0xFFu64) << 70;
    f.set(
        abi::encode_tick_bitmap(3)[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(bitmap, 256)),
    );
    let io = io_with(f);
    assert_eq!(
        choreography::fetch_tick_bitmap(&io, TO, 3, None)
            .await
            .unwrap(),
        bitmap
    );
}

#[tokio::test]
async fn fetch_tick_data_decodes() {
    let mut f = FakeRpc::new();
    f.set(
        abi::encode_tick_data(120)[..4].try_into().unwrap(),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(111), 128),
            DynSolValue::Int(I256::try_from(-25i128).unwrap(), 128),
        ])),
    );
    let io = io_with(f);
    let (gross, net) = choreography::fetch_tick_data(&io, TO, 120, None)
        .await
        .unwrap();
    assert_eq!(gross.to_string(), "111");
    assert_eq!(net, I256::try_from(-25i128).unwrap());
}

#[tokio::test]
async fn fetch_v4_tick_bitmap_decodes() {
    let mut f = FakeRpc::new();
    let bitmap = U256::from(0x7u64) << 3;
    f.set(
        abi::encode_v4_tick_bitmap(&POOL_ID, -4)[..4]
            .try_into()
            .unwrap(),
        enc(DynSolValue::Uint(bitmap, 256)),
    );
    let io = io_with(f);
    assert_eq!(
        choreography::fetch_v4_tick_bitmap(&io, SV, POOL_ID, -4, None)
            .await
            .unwrap(),
        bitmap
    );
}

#[tokio::test]
async fn fetch_v4_tick_data_decodes() {
    let mut f = FakeRpc::new();
    f.set(
        abi::encode_v4_tick_data(&POOL_ID, 5)[..4]
            .try_into()
            .unwrap(),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(7), 128),
            DynSolValue::Int(I256::try_from(3i128).unwrap(), 128),
        ])),
    );
    let io = io_with(f);
    let (gross, net) = choreography::fetch_v4_tick_data(&io, SV, POOL_ID, 5, None)
        .await
        .unwrap();
    assert_eq!(gross.to_string(), "7");
    assert_eq!(net, I256::try_from(3i128).unwrap());
}

// ── PoolBuilder: probe → variant-resolution → build_v2 (task 3FVZF4) ──

#[tokio::test]
async fn probe_pool_type_dispatches() {
    // No responses → every probe reverts → Curve.
    assert_eq!(
        builder::probe_pool_type(&io_with(FakeRpc::new()), TO, None).await,
        builder::PoolFamily::Curve
    );

    // slot0() present → V3.
    let mut f = FakeRpc::new();
    f.set(
        abi::encode_slot0()[..4].try_into().unwrap(),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None).await,
        builder::PoolFamily::V3
    );

    // getReserves() present, no slot0 → V2.
    let mut f = FakeRpc::new();
    f.set(
        abi::encode_get_reserves()[..4].try_into().unwrap(),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::ZERO, 112),
            DynSolValue::Uint(U256::ZERO, 112),
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None).await,
        builder::PoolFamily::V2
    );

    // getPoolId() present, no getNormalizedWeights → BalancerStable.
    let mut f = FakeRpc::new();
    f.set(choreography::selector(b"getPoolId()"), addr_word(TO));
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None).await,
        builder::PoolFamily::BalancerStable
    );

    // getPoolId() + getNormalizedWeights() → BalancerWeighted.
    let mut f = FakeRpc::new();
    f.set(choreography::selector(b"getPoolId()"), addr_word(TO));
    f.set(
        choreography::selector(b"getNormalizedWeights()"),
        enc(DynSolValue::FixedArray(vec![DynSolValue::Uint(
            U256::from(50),
            18,
        )])),
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None).await,
        builder::PoolFamily::BalancerWeighted
    );
}

#[tokio::test]
async fn resolve_v2_dex_matches_uniswap_factory_without_read() {
    use degenbot_uniswap::dex_identity::{self, DexVariant};
    let uniswap = dex_identity::UNISWAP_V2;
    let io = io_with(FakeRpc::new()); // empty → any read would revert
    let id = builder::resolve_v2_dex(&io, TO, uniswap.factory, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id.variant, DexVariant::UniswapV2);
    assert_eq!(id.fee_token0, (997, 1000));
}

#[tokio::test]
async fn resolve_v2_dex_reads_stable_flag_for_camelot_factory() {
    use degenbot_uniswap::dex_identity::{self, DexVariant};
    // Camelot volatile+stable share a factory; the pool's `stableSwap()` read
    // disambiguates. Give the read `true` → CamelotV2Stable.
    let factory = dex_identity::CAMELOT_V2_STABLE.factory;
    let mut f = FakeRpc::new();
    f.set(
        choreography::selector(b"stableSwap()"),
        enc(DynSolValue::Bool(true)),
    );
    let io = io_with(f);
    let id = builder::resolve_v2_dex(&io, TO, factory, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id.variant, DexVariant::CamelotV2Stable);
}

#[tokio::test]
async fn fetch_camelot_state_decodes_all_four_probes() {
    let mut f = FakeRpc::new();
    f.set(
        choreography::selector(b"stableSwap()"),
        enc(DynSolValue::Bool(true)),
    );
    f.set(
        choreography::selector(b"FEE_DENOMINATOR()"),
        enc(DynSolValue::Uint(U256::from(10_000u64), 256)),
    );
    f.set(
        choreography::selector(b"token0FeePercent()"),
        enc(DynSolValue::Uint(U256::from(20u64), 16)),
    );
    f.set(
        choreography::selector(b"token1FeePercent()"),
        enc(DynSolValue::Uint(U256::from(200u64), 16)),
    );
    let io = io_with(f);
    let s = choreography::fetch_camelot_state(&io, TO, None)
        .await
        .unwrap();
    assert!(s.stable);
    assert_eq!(s.fee_denominator, U256::from(10_000u64));
    assert_eq!(s.token0_fee_percent, 20);
    assert_eq!(s.token1_fee_percent, 200);
}

#[tokio::test]
async fn build_v2_assembles_register_params_from_onchain() {
    use degenbot_uniswap::create2::compute_v2_address;
    use degenbot_uniswap::dex_identity::{DexVariant, UNISWAP_V2};

    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool = compute_v2_address(UNISWAP_V2.factory, tok0, tok1, UNISWAP_V2.init_hash);

    let mut f = FakeRpc::new();
    f.set(
        choreography::selector(b"factory()"),
        addr_word(UNISWAP_V2.factory),
    );
    f.set(choreography::selector(b"token0()"), addr_word(tok0));
    f.set(choreography::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::encode_get_reserves()[..4].try_into().unwrap(),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(123_456_789u64), 112),
            DynSolValue::Uint(U256::from(987_654_321u64), 112),
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    let io = io_with(f);

    let params = builder::build_v2(1, pool, &io, Some(18_000_000))
        .await
        .unwrap();
    assert_eq!(params.address, pool);
    assert_eq!(params.factory, UNISWAP_V2.factory);
    assert_eq!(params.token0, tok0);
    assert_eq!(params.token1, tok1);
    assert_eq!(params.variant, DexVariant::UniswapV2);
    assert_eq!(params.fee_token0, (997, 1000));
    assert_eq!(params.update_block, 18_000_000);
    assert!(!params.stable_swap);
    // Proxy preserves the resulting identity/state build.
    let (identity, state) = degenbot_pools::v2_state::V2PoolState::from_params(&params, 8);
    assert_eq!(identity.address, pool);
    assert_eq!(state.reserve0, U112::from(123_456_789u64));
}

#[tokio::test]
async fn build_v3_assembles_sparse_register_params_from_onchain() {
    use crate::bot_core::PoolTickCoverage;
    // Fake factory NOT in the JSON → CREATE2 verify is skipped (ad-hoc path).
    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");

    let mut f = FakeRpc::new();
    f.set(choreography::selector(b"factory()"), addr_word(factory));
    f.set(choreography::selector(b"token0()"), addr_word(tok0));
    f.set(choreography::selector(b"token1()"), addr_word(tok1));
    f.set(
        choreography::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(3000u32), 24)),
    );
    f.set(
        choreography::selector(b"tickSpacing()"),
        enc(DynSolValue::Int(I256::try_from(60i32).unwrap(), 24)),
    );
    f.set(
        abi::encode_slot0()[..4].try_into().unwrap(),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    f.set(
        abi::encode_liquidity()[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(U256::from(1_000_000_000u64), 128)),
    );
    // tick=0, spacing=60 → word 0; zero bitmap → sparse, no per-tick reads.
    f.set(
        abi::encode_tick_bitmap(0)[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(U256::ZERO, 256)),
    );
    let io = io_with(f);

    let params = builder::build_v3(1, pool, None, &io, Some(9_000_000))
        .await
        .unwrap();
    assert_eq!(params.address, pool);
    assert_eq!(params.token0, tok0);
    assert_eq!(params.token1, tok1);
    assert_eq!(params.fee, 3000);
    assert_eq!(params.tick_spacing, 60);
    assert_eq!(params.factory, factory);
    assert_eq!(params.tick, 0);
    assert_eq!(params.update_block, 9_000_000);
    assert_eq!(params.tick_data_block, Some(9_000_000));
    assert_eq!(params.coverage, PoolTickCoverage::Sparse);
    assert!(params.tick_data.is_empty());
}

#[tokio::test]
async fn build_v4_assembles_sparse_register_params_from_onchain() {
    use crate::bot_core::PoolTickCoverage;
    let pm: Address = alloy::primitives::address!("0x3333333333333333333333333333333333333333");
    let pid: [u8; 32] = [0xcd; 32];

    let mut f = FakeRpc::new();
    f.set(
        abi::encode_get_slot0(&pid)[..4].try_into().unwrap(),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(1u128 << 96), 160),
            DynSolValue::Int(I256::try_from(5i32).unwrap(), 24),
            DynSolValue::Uint(U256::from(300u32), 24), // protocol_fee
            DynSolValue::Uint(U256::from(50u32), 24),  // lp_fee
        ])),
    );
    f.set(
        abi::encode_get_liquidity(&pid)[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(U256::from(2_000_000_000u64), 128)),
    );
    // tick=5, spacing=1 -> word 0; zero bitmap -> sparse, no per-tick reads.
    f.set(
        abi::encode_v4_tick_bitmap(&pid, 0)[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(U256::ZERO, 256)),
    );
    let io = io_with(f);

    let params = builder::build_v4(
        builder::V4PoolBuildIdentity {
            pool_manager: pm,
            state_view: pm,
            pool_id: pid,
            currency0: TO,
            currency1: SV,
            fee: 500,
            tick_spacing: 1,
            hook_flags: 0,
        },
        None,
        &io,
        Some(11_000_000),
    )
    .await
    .unwrap();
    assert_eq!(params.pool_manager, pm);
    assert_eq!(params.pool_id, pid);
    assert_eq!(params.pool_key.currency0, TO);
    assert_eq!(params.pool_key.currency1, SV);
    assert_eq!(params.pool_key.fee, 500);
    assert_eq!(params.pool_key.tick_spacing, 1);
    assert_eq!(params.protocol_fee, 300);
    assert_eq!(params.hook_flags, 0);
    assert_eq!(params.tick, 5);
    assert_eq!(params.coverage, PoolTickCoverage::Sparse);
    assert!(params.tick_data.is_empty());
}

/// A `TickMapDb` fake returning a canned `LiquidityMap` (V3 + V4 both query it).
struct FakeTickMapDb(degenbot_db::snapshot::LiquidityMap);

impl degenbot_db::snapshot::TickMapDb for FakeTickMapDb {
    fn fetch_liquidity_map(
        &self,
        _address: Address,
    ) -> Result<Option<degenbot_db::snapshot::LiquidityMap>, degenbot_db::error::DbError> {
        Ok(Some(self.0.clone()))
    }
    fn fetch_liquidity_map_v4(
        &self,
        _pool_manager: Address,
        _pool_id_hash: alloy::primitives::B256,
    ) -> Result<Option<degenbot_db::snapshot::LiquidityMap>, degenbot_db::error::DbError> {
        Ok(Some(self.0.clone()))
    }
    fn fetch_newest_update_block(
        &self,
        _chain: i64,
        _family: degenbot_db::read::ExchangeFamily,
    ) -> Result<Option<i64>, degenbot_db::error::DbError> {
        Ok(None)
    }
}

/// A DB hit gives a TRACKED pool (the cross-task capture in 4GQWZ4): the builder
/// favors the `TickMapDb` full tick map (Tracked → feeds the verify lifecycle)
/// over the chain-arm single-word bootstrap, and must NOT consult the chain
/// (no `tick_bitmap` response is seeded, so a chain probe would error).
#[tokio::test]
async fn build_v3_db_hit_yields_tracked_without_chain() {
    use crate::bot_core::PoolTickCoverage;
    use degenbot_db::snapshot::{BitmapAtWord, LiquidityAtTick, LiquidityMap};
    use std::collections::HashMap;

    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");

    let mut f = FakeRpc::new();
    f.set(choreography::selector(b"factory()"), addr_word(factory));
    f.set(choreography::selector(b"token0()"), addr_word(tok0));
    f.set(choreography::selector(b"token1()"), addr_word(tok1));
    f.set(
        choreography::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(3000u32), 24)),
    );
    f.set(
        choreography::selector(b"tickSpacing()"),
        enc(DynSolValue::Int(I256::try_from(60i32).unwrap(), 24)),
    );
    f.set(
        abi::encode_slot0()[..4].try_into().unwrap(),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    f.set(
        abi::encode_liquidity()[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(U256::from(1_000_000_000u64), 128)),
    );
    // NOTE: no tick_bitmap response — if the chain arm were reached it would error.
    let io = io_with(f);

    let db = FakeTickMapDb(LiquidityMap {
        tick_bitmap: HashMap::from([(
            0i64,
            BitmapAtWord {
                bitmap: U256::from(1u128 << 60),
            },
        )]),
        tick_data: HashMap::from([(
            60,
            LiquidityAtTick {
                liquidity_gross: U256::from(100u64),
                liquidity_net: I256::try_from(100i128).unwrap(),
            },
        )]),
    });

    let params = builder::build_v3(1, pool, Some(&db), &io, Some(9_000_000))
        .await
        .unwrap();
    assert_eq!(params.coverage, PoolTickCoverage::Tracked);
    assert_eq!(params.tick_data.len(), 1);
    assert_eq!(params.tick_data[&60].liquidity_gross, U128::from(100u64));
    assert_eq!(
        params.tick_data[&60].liquidity_net,
        I256::try_from(100i128).unwrap()
    );
}

// --- SSSXG6: Balancer choreography + builder decode tests -------------------

fn pool_id_ret(pool_id: &[u8; 32]) -> Vec<u8> {
    enc(DynSolValue::FixedBytes(FixedBytes::from(*pool_id), 32))
}

fn pool_tokens_ret(tokens: &[Address], balances: &[U256]) -> Vec<u8> {
    // getPoolTokens returns (address[], uint256[], uint256). Manual ABI encode:
    // head = 3 words (offset to address[], offset to uint256[], lastChangeBlock),
    // tail = the two dynamic arrays. Deterministic — bypasses the DynSolValue
    // tuple inference so the decode is exercised against a fixed layout.
    let n = tokens.len() as u64;
    let addr_offset = 96u64; // 3-word head
    let uint_offset = 96 + 32 + n * 32; // after the address[] tail
    let mut out = Vec::new();
    out.extend_from_slice(&U256::from(addr_offset).to_be_bytes::<32>());
    out.extend_from_slice(&U256::from(uint_offset).to_be_bytes::<32>());
    out.extend_from_slice(&U256::ZERO.to_be_bytes::<32>()); // lastChangeBlock
    out.extend_from_slice(&U256::from(n).to_be_bytes::<32>());
    for a in tokens {
        let mut w = vec![0u8; 12];
        w.extend_from_slice(a.as_slice());
        out.extend_from_slice(&w);
    }
    out.extend_from_slice(&U256::from(n).to_be_bytes::<32>());
    for b in balances {
        out.extend_from_slice(&b.to_be_bytes::<32>());
    }
    out
}

fn u256_array_ret(vals: &[u64]) -> Vec<u8> {
    enc(DynSolValue::Array(
        vals.iter()
            .map(|v| DynSolValue::Uint(U256::from(*v), 256))
            .collect::<Vec<_>>(),
    ))
}

const VAULT: Address = alloy::primitives::address!("0xba12222222228d8ba445958a75a0704d566bf2c8");
const T0: Address = alloy::primitives::address!("0x6b175474e89094c44da98b954eedeac495271d0f"); // DAI
const T1: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"); // USDC

#[tokio::test]
async fn fetch_balancer_weights_decodes_uint256_array() {
    let mut f = FakeRpc::new();
    f.set(
        abi::encode_get_weights()[..4].try_into().unwrap(),
        u256_array_ret(&[500_000_000_000_000_000u64, 500_000_000_000_000_000u64]),
    );
    let io = io_with(f);
    let w = choreography::fetch_balancer_weights(&io, TO, None)
        .await
        .unwrap();
    assert_eq!(w.len(), 2);
    assert_eq!(w[0], U256::from(500_000_000_000_000_000u64));
    assert_eq!(w[1], U256::from(500_000_000_000_000_000u64));
}

#[tokio::test]
async fn probe_balancer_type_weights_first_then_stable() {
    // No responses → both probes revert → error.
    let io = io_with(FakeRpc::new());
    assert!(choreography::probe_balancer_type(&io, TO, None)
        .await
        .is_err());

    // getNormalizedWeights responds → Weighted.
    let mut f = FakeRpc::new();
    f.set(
        abi::encode_get_weights()[..4].try_into().unwrap(),
        u256_array_ret(&[500_000_000_000_000_000u64]),
    );
    let io = io_with(f);
    assert_eq!(
        choreography::probe_balancer_type(&io, TO, None)
            .await
            .unwrap(),
        choreography::BalancerFamily::Weighted
    );

    // Only getAmplificationParameter → Stable.
    let mut f = FakeRpc::new();
    f.set(
        abi::encode_get_amp()[..4].try_into().unwrap(),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(100u64), 256),
            DynSolValue::Bool(false),
            DynSolValue::Uint(U256::from(100u64), 256),
        ])),
    );
    let io = io_with(f);
    assert_eq!(
        choreography::probe_balancer_type(&io, TO, None)
            .await
            .unwrap(),
        choreography::BalancerFamily::Stable
    );
}

#[tokio::test]
async fn build_balancer_weighted_assembles_params() {
    let mut f = FakeRpc::new();
    let pool_id = [0xcd; 32];
    f.set(
        abi::encode_get_pool_id()[..4].try_into().unwrap(),
        pool_id_ret(&pool_id),
    );
    f.set(
        abi::encode_get_pool_tokens(&pool_id)[..4]
            .try_into()
            .unwrap(),
        pool_tokens_ret(&[T0, T1], &[U256::from(1_000u64), U256::from(2_000u64)]),
    );
    f.set(
        abi::encode_get_swap_fee()[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(U256::from(5_000_000_000_000_000u64), 256)),
    );
    f.set(
        abi::encode_get_weights()[..4].try_into().unwrap(),
        u256_array_ret(&[500_000_000_000_000_000u64, 500_000_000_000_000_000u64]),
    );
    // Token decimals: both 18 → scaling factor ONE (1e18) each. (The FakeRpc
    // keys responses by 4-byte selector only, so per-address decimals can't be
    // distinguished here — variance is covered by a dedicated scaling test.)
    f.set(
        choreography::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(18), 256)),
    );
    // get_code returns empty → PowVersion V1 (1).
    let io = io_with(f);
    let p = builder::build_balancer_weighted(VAULT, TO, &io, None)
        .await
        .unwrap();
    assert_eq!(p.pool_id, pool_id);
    assert_eq!(p.tokens, vec![T0, T1]);
    assert_eq!(p.vault, VAULT);
    assert_eq!(p.balances, vec![U256::from(1_000u64), U256::from(2_000u64)]);
    assert_eq!(p.pow_version, 1);
    assert_eq!(p.swap_fee, 5_000_000_000_000_000u128);
    assert_eq!(p.weights, vec![U256::from(500_000_000_000_000_000u64); 2]);
    let one = U256::from(1_000_000_000_000_000_000u128);
    assert_eq!(p.scaling_factors, vec![one, one]);
}

#[tokio::test]
async fn build_balancer_stable_assembles_params_with_rate_providers() {
    let mut f = FakeRpc::new();
    let pool_id = [0xef; 32];
    f.set(
        abi::encode_get_pool_id()[..4].try_into().unwrap(),
        pool_id_ret(&pool_id),
    );
    f.set(
        abi::encode_get_pool_tokens(&pool_id)[..4]
            .try_into()
            .unwrap(),
        pool_tokens_ret(&[T0, T1], &[U256::from(100u64), U256::from(200u64)]),
    );
    f.set(
        abi::encode_get_swap_fee()[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(U256::from(5_000_000_000_000_000u64), 256)),
    );
    f.set(
        abi::encode_get_amp()[..4].try_into().unwrap(),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(100u64), 256),
            DynSolValue::Bool(false),
            DynSolValue::Uint(U256::from(100u64), 256),
        ])),
    );
    // Both tokens 18 decimals; one rate provider returning 1.02e18.
    f.set(
        choreography::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(18), 256)),
    );
    f.set(
        choreography::selector(b"getRateProviders()"),
        enc(DynSolValue::Array(vec![
            DynSolValue::Address(alloy::primitives::address!(
                "0x1111111111111111111111111111111111111111"
            )),
            DynSolValue::Address(Address::ZERO),
        ])),
    );
    f.set(
        abi::encode_get_rate()[..4].try_into().unwrap(),
        enc(DynSolValue::Uint(
            U256::from(1_020_000_000_000_000_000u64),
            256,
        )),
    );
    let io = io_with(f);
    // specialization from pool_id[20..22] = 0xef,ef → not 1 → INVARIANT_V1 (1).
    let p = builder::build_balancer_stable(VAULT, TO, &io, None, None)
        .await
        .unwrap();
    assert_eq!(p.invariant_version, 1);
    assert_eq!(p.bpt_idx, None);
    let one = U256::from(1_000_000_000_000_000_000u128);
    // First token rate-multiplied: ONE*10^0 * 1.02e18 / 1e18 = 1.02e18.
    assert_eq!(
        p.scaling_factors[0],
        one * U256::from(102u128) / U256::from(100u128)
    );
    // Second token zero-sentinel provider → unscaled ONE.
    assert_eq!(p.scaling_factors[1], one);
}

// ---------------------------------------------------------------------------
// Curve detection choreography (task 4EBHRC / epic TV72EG)
// ---------------------------------------------------------------------------

/// ABI-encode a fixed `address[8]` return value.
fn addr8(v: [Address; 8]) -> Vec<u8> {
    enc(DynSolValue::FixedArray(
        v.iter().map(|a| DynSolValue::Address(*a)).collect(),
    ))
}

const REGISTRY: Address = alloy::primitives::address!("0x9999999999999999999999999999999999999999");
const REGISTRY2: Address =
    alloy::primitives::address!("0x8888888888888888888888888888888888888888");

#[tokio::test]
async fn discover_curve_coins_uint256_prototype_stops_at_zero() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000001");
    // Coin 0 populated; coin 1 is the zero address → discovery stops after 1.
    f.set_full(abi::encode_curve_coins_uint(0), addr_word(coin0));
    f.set_full(abi::encode_curve_coins_uint(1), addr_word(Address::ZERO));
    f.set_full(
        abi::encode_curve_balances_uint(0),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    let io = io_with(f);
    let r = curve_choreography::discover_curve_coins(&io, TO, None).await;
    assert_eq!(r.token_addresses, vec![coin0]);
    assert_eq!(r.balances, vec![U256::from(1_000_000u64)]);
    assert_eq!(
        r.coin_prototype,
        Some(curve_choreography::CurvePrototype::Uint256)
    );
    assert_eq!(
        r.balance_prototype,
        Some(curve_choreography::CurvePrototype::Uint256)
    );
}

#[tokio::test]
async fn discover_curve_coins_falls_back_to_int128_prototype() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xbbb0000000000000000000000000000000000001");
    // No uint256 responses → that path reverts; int128 prototype is used.
    f.set_full(abi::encode_curve_coins_int128(0), addr_word(coin0));
    f.set_full(
        abi::encode_curve_balances_int128(0),
        enc(DynSolValue::Uint(U256::from(42u64), 256)),
    );
    let io = io_with(f);
    let r = curve_choreography::discover_curve_coins(&io, TO, None).await;
    assert_eq!(r.token_addresses, vec![coin0]);
    assert_eq!(r.balances, vec![U256::from(42u64)]);
    assert_eq!(
        r.coin_prototype,
        Some(curve_choreography::CurvePrototype::Int128)
    );
}

#[tokio::test]
async fn discover_curve_coins_empty_when_no_prototype_works() {
    // No coin responses at all → both prototypes revert → empty result.
    let io = io_with(FakeRpc::new());
    let r = curve_choreography::discover_curve_coins(&io, TO, None).await;
    assert!(r.token_addresses.is_empty());
    assert!(r.balances.is_empty());
    assert_eq!(r.coin_prototype, None);
}

#[tokio::test]
async fn fetch_curve_pool_params_decodes_a_fee_admin_fee() {
    let mut f = FakeRpc::new();
    f.set(
        choreography::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(1000u64), 256)),
    );
    f.set(
        choreography::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set(
        choreography::selector(b"admin_fee()"),
        enc(DynSolValue::Uint(U256::from(500_000_000u64), 256)),
    );
    let io = io_with(f);
    let p = curve_choreography::fetch_curve_pool_params(&io, TO, None)
        .await
        .unwrap();
    assert_eq!(p.a_coefficient, 1000);
    assert_eq!(p.fee, 1_000_000);
    assert_eq!(p.admin_fee, 500_000_000);
}

#[tokio::test]
async fn fetch_curve_pool_params_propagates_missing_read() {
    // fee() not configured → whole fetch errors (required, not optional).
    let mut f = FakeRpc::new();
    f.set(
        choreography::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(1000u64), 256)),
    );
    let io = io_with(f);
    assert!(curve_choreography::fetch_curve_pool_params(&io, TO, None)
        .await
        .is_err());
}

#[tokio::test]
async fn detect_curve_a_ramping_all_four_values() {
    let mut f = FakeRpc::new();
    f.set(
        choreography::selector(b"initial_A()"),
        enc(DynSolValue::Uint(U256::from(1_000u64), 256)),
    );
    f.set(
        choreography::selector(b"initial_A_time()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        choreography::selector(b"future_A()"),
        enc(DynSolValue::Uint(U256::from(5_000u64), 256)),
    );
    f.set(
        choreography::selector(b"future_A_time()"),
        enc(DynSolValue::Uint(U256::from(200u64), 256)),
    );
    let io = io_with(f);
    let r = curve_choreography::detect_curve_a_ramping(&io, TO, None).await;
    assert!(r.has_ramping);
    assert_eq!(r.initial_a, Some(1_000));
    assert_eq!(r.initial_a_time, Some(100));
    assert_eq!(r.future_a, Some(5_000));
    assert_eq!(r.future_a_time, Some(200));
}

#[tokio::test]
async fn detect_curve_a_ramping_none_when_any_reverts() {
    // initial_A missing → whole detection treated as non-ramping.
    let mut f = FakeRpc::new();
    f.set(
        choreography::selector(b"future_A()"),
        enc(DynSolValue::Uint(U256::from(5_000u64), 256)),
    );
    let io = io_with(f);
    let r = curve_choreography::detect_curve_a_ramping(&io, TO, None).await;
    assert!(!r.has_ramping);
    assert_eq!(r.initial_a, None);
}

#[tokio::test]
async fn detect_curve_crypto_params_populated_when_fee_gamma_positive() {
    let mut f = FakeRpc::new();
    f.set(
        choreography::selector(b"fee_gamma()"),
        enc(DynSolValue::Uint(
            U256::from(500_000_000_000_000_000u64),
            256,
        )),
    );
    f.set(
        choreography::selector(b"mid_fee()"),
        enc(DynSolValue::Uint(U256::from(3_000_000u64), 256)),
    );
    f.set(
        choreography::selector(b"out_fee()"),
        enc(DynSolValue::Uint(U256::from(30_000_000u64), 256)),
    );
    f.set(
        choreography::selector(b"gamma()"),
        enc(DynSolValue::Uint(U256::from(145_000_000_000_000u64), 256)),
    );
    f.set(
        choreography::selector(b"offpeg_fee_multiplier()"),
        enc(DynSolValue::Uint(U256::from(200_000_000u64), 256)),
    );
    let io = io_with(f);
    let r = curve_choreography::detect_curve_crypto_params(&io, TO, None).await;
    assert!(r.is_crypto);
    assert_eq!(r.fee_gamma, Some(500_000_000_000_000_000));
    assert_eq!(r.mid_fee, Some(3_000_000));
    assert_eq!(r.out_fee, Some(30_000_000));
    assert_eq!(r.gamma, Some(145_000_000_000_000));
    assert_eq!(r.offpeg_fee_multiplier, Some(200_000_000));
}

#[tokio::test]
async fn detect_curve_crypto_params_not_crypto_when_fee_gamma_reverts() {
    // fee_gamma reverts → not crypto; offpeg still fetched.
    let mut f = FakeRpc::new();
    f.set(
        choreography::selector(b"offpeg_fee_multiplier()"),
        enc(DynSolValue::Uint(U256::from(500_000_000u64), 256)),
    );
    let io = io_with(f);
    let r = curve_choreography::detect_curve_crypto_params(&io, TO, None).await;
    assert!(!r.is_crypto);
    assert_eq!(r.fee_gamma, None);
    assert_eq!(r.offpeg_fee_multiplier, Some(500_000_000));
}

#[tokio::test]
async fn detect_curve_lending_ctoken_override_precision() {
    let mut f = FakeRpc::new();
    let ctoken: Address = alloy::primitives::address!("0xccc0000000000000000000000000000000000001");
    let underlying: Address =
        alloy::primitives::address!("0xddd0000000000000000000000000000000000001");
    // cToken probe on the token address.
    f.set(
        abi::encode_lending_is_ctoken()[..4].try_into().unwrap(),
        enc(DynSolValue::Bool(true)),
    );
    f.set(
        abi::encode_lending_underlying()[..4].try_into().unwrap(),
        addr_word(underlying),
    );
    f.set(
        choreography::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(8u64), 256)),
    );
    let io = io_with(f);
    // token_decimals = [18] (passed by driver for the wrapped token).
    let r = curve_choreography::detect_curve_lending_tokens(&io, &[ctoken], &[18], None).await;
    assert_eq!(r.use_lending, vec![true]);
    // Underlying decimals = 8 → precision multiplier 10^(18-8) = 10^10.
    let pm = r.precision_multipliers.unwrap();
    assert_eq!(pm, vec![U256::from(10u64).pow(U256::from(10u64))]);
}

#[tokio::test]
async fn detect_curve_lending_ytoken_no_override() {
    let mut f = FakeRpc::new();
    let ytoken: Address = alloy::primitives::address!("0xeee0000000000000000000000000000000000001");
    let underlying: Address =
        alloy::primitives::address!("0xddd0000000000000000000000000000000000002");
    // isCToken reverts (no response) → falls through to token() probe.
    f.set(
        abi::encode_lending_token()[..4].try_into().unwrap(),
        addr_word(underlying),
    );
    let io = io_with(f);
    let r = curve_choreography::detect_curve_lending_tokens(&io, &[ytoken], &[18], None).await;
    assert_eq!(r.use_lending, vec![true]);
    // yToken: no override → default precision from token decimals (18) = 10^0.
    let pm = r.precision_multipliers.unwrap();
    assert_eq!(pm, vec![U256::from(1u64)]);
}

#[tokio::test]
async fn detect_curve_lending_no_lending_tokens() {
    // isCToken reverts and token() reverts → not lending, no multipliers.
    let coin: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000002");
    let io = io_with(FakeRpc::new());
    let r = curve_choreography::detect_curve_lending_tokens(&io, &[coin], &[18], None).await;
    assert_eq!(r.use_lending, vec![false]);
    assert_eq!(r.precision_multipliers, None);
}

#[tokio::test]
async fn find_curve_lp_token_returns_none_when_all_zero_or_revert() {
    let mut f = FakeRpc::new();
    // The fake resolves by full calldata (`get_lp_token(pool)` — identical for
    // every registry target) → zero on every registry → None.
    f.set_full(
        abi::encode_curve_get_lp_token(&TO),
        addr_word(Address::ZERO),
    );
    let io = io_with(f);
    assert_eq!(
        curve_choreography::find_curve_lp_token(&io, TO, &[REGISTRY, REGISTRY2], None).await,
        None
    );
}

#[tokio::test]
async fn find_curve_lp_token_returns_first_nonzero() {
    let mut f = FakeRpc::new();
    let lp: Address = alloy::primitives::address!("0xfff0000000000000000000000000000000000002");
    // First registry returns the LP (non-zero) → Some, no second probe needed.
    f.set_full(abi::encode_curve_get_lp_token(&TO), addr_word(lp));
    let io = io_with(f);
    assert_eq!(
        curve_choreography::find_curve_lp_token(&io, TO, &[REGISTRY], None).await,
        Some(lp)
    );
}

#[tokio::test]
async fn detect_curve_metapool_resolves_base_and_underlying() {
    let mut f = FakeRpc::new();
    let base: Address = alloy::primitives::address!("0x0bb0000000000000000000000000000000000001");
    let u1: Address = alloy::primitives::address!("0x0111000000000000000000000000000000000001");
    let u2: Address = alloy::primitives::address!("0x0111000000000000000000000000000000000002");
    f.set_full(abi::encode_curve_is_meta(&TO), enc(DynSolValue::Bool(true)));
    f.set_full(
        abi::encode_curve_get_underlying_coins(&TO),
        addr8([
            u1,
            u2,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
        ]),
    );
    // base_pool() on the pool contract.
    f.set(choreography::selector(b"base_pool()"), addr_word(base));
    let io = io_with(f);
    let r = curve_choreography::detect_curve_metapool(&io, TO, &[u1, u2], &[REGISTRY], None).await;
    assert!(r.is_meta);
    assert_eq!(r.base_pool_address, Some(base));
    assert_eq!(r.tokens_underlying, Some(vec![u1, u2]));
}

#[tokio::test]
async fn detect_curve_metapool_not_meta() {
    let mut f = FakeRpc::new();
    f.set_full(
        abi::encode_curve_is_meta(&TO),
        enc(DynSolValue::Bool(false)),
    );
    let io = io_with(f);
    let r = curve_choreography::detect_curve_metapool(&io, TO, &[], &[REGISTRY], None).await;
    assert!(!r.is_meta);
    assert_eq!(r.base_pool_address, None);
}

#[tokio::test]
async fn detect_curve_metapool_3crv_base_fallback() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000001");
    // base_pool() reverts (no response), get_base_pool reverts (no response),
    // second coin == 3Crv LP → tripool fallback.
    f.set_full(abi::encode_curve_is_meta(&TO), enc(DynSolValue::Bool(true)));
    f.set_full(
        abi::encode_curve_get_underlying_coins(&TO),
        addr8([
            coin0,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
        ]),
    );
    let three_crv_lp: Address =
        alloy::primitives::address!("0x6c3F90f043a72FA612Cbac8115ee7e52bDE6E490");
    let tripool: Address =
        alloy::primitives::address!("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7");
    let io = io_with(f);
    let r = curve_choreography::detect_curve_metapool(
        &io,
        TO,
        &[coin0, three_crv_lp],
        &[REGISTRY],
        None,
    )
    .await;
    assert!(r.is_meta);
    assert_eq!(r.base_pool_address, Some(tripool));
}

// ---------------------------------------------------------------------------
// Curve build assembly (task 4TPB35 / epic TV72EG)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn build_curve_pool_assembles_plain_pool_params() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000001");
    let coin1: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000002");

    // Coins (uint256 prototype) + balances.
    f.set_full(abi::encode_curve_coins_uint(0), addr_word(coin0));
    f.set_full(abi::encode_curve_coins_uint(1), addr_word(coin1));
    // coin index 2 reverts (no response) → discovery stops at 2 coins.
    f.set_full(
        abi::encode_curve_balances_uint(0),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set_full(
        abi::encode_curve_balances_uint(1),
        enc(DynSolValue::Uint(U256::from(2_000_000u64), 256)),
    );

    // Pool params.
    f.set(
        choreography::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(2000u64), 256)),
    );
    f.set(
        choreography::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set(
        choreography::selector(b"admin_fee()"),
        enc(DynSolValue::Uint(U256::from(500_000_000u64), 256)),
    );

    // Both coins have 6 decimals → rate_multiplier 10^(36-6)=10^30.
    f.set(
        choreography::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(6u64), 256)),
    );

    let io = io_with(f);
    let p = builder::build_curve_pool(TO, &[REGISTRY], &io, Some(123))
        .await
        .unwrap();

    assert_eq!(p.tokens, vec![coin0, coin1]);
    assert_eq!(p.a_coefficient, 2000);
    assert_eq!(p.a_precision, 100);
    assert_eq!(p.fee, 1_000_000);
    assert_eq!(p.admin_fee, 500_000_000);
    assert_eq!(
        p.balances,
        vec![U256::from(1_000_000u64), U256::from(2_000_000u64)]
    );
    assert_eq!(p.update_block, 123);

    // Rate/precision multipliers from 6-decimal coins (no lending overrides).
    let r30 = U256::from(10u64).pow(U256::from(30u64));
    let r12 = U256::from(10u64).pow(U256::from(12u64));
    assert_eq!(p.rate_multipliers, vec![r30, r30]);
    assert_eq!(p.precision_multipliers, vec![r12, r12]);

    // Strategy defaults (plain, unmapped address).
    assert_eq!(p.swap_style, 1); // STANDARD
    assert_eq!(p.lending_rate_style, 1); // NONE
    assert_eq!(p.d_variant, 1);

    // Plain pool: no base/underlying, no lp, no ramping, no crypto.
    assert_eq!(p.base_pool, None);
    assert_eq!(p.tokens_underlying, None);
    assert_eq!(p.lp_token, None);
    assert_eq!(p.fee_gamma, None);
    assert!(p.create_timestamp.is_some());

    // The Rust data provider is attached.
    assert!(p.data_provider.is_some());

    // use_lending all false for a plain pool.
    assert_eq!(p.use_lending, vec![false, false]);
}

#[tokio::test]
async fn build_curve_pool_rejects_fewer_than_two_coins() {
    // Only one coin discovered → Spec error (BrokenPool guard).
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000003");
    f.set_full(abi::encode_curve_coins_uint(0), addr_word(coin0));
    f.set_full(abi::encode_curve_coins_uint(1), addr_word(Address::ZERO));
    // Params must succeed (they're fetched before the min-tokens guard).
    f.set(
        choreography::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        choreography::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        choreography::selector(b"admin_fee()"),
        enc(DynSolValue::Uint(U256::from(0u64), 256)),
    );
    let io = io_with(f);
    assert!(matches!(
        builder::build_curve_pool(TO, &[REGISTRY], &io, Some(1)).await,
        Err(builder::PoolBuilderError::Spec)
    ));
}

/// The three ERC-20 token reads (`balanceOf`/`allowance`/`totalSupply`) decode
/// a `uint256` return losslessly through the core choreography (SUB-TASK: ERC-20
/// token-balance family, LWKLMP). Selector-keyed [`FakeRpc`] responses.
#[tokio::test]
async fn fetch_token_balance_supply_allowance_decode_uint256() {
    // balanceOf(address) returns 1234... (a 64-bit value).
    let mut f = FakeRpc::new();
    f.set(
        choreography::selector(b"balanceOf(address)"),
        U256::from(1_000_000_000u64).to_be_bytes::<32>().to_vec(),
    );
    let io = io_with(f);
    let bal = choreography::fetch_token_balance(&io, TO, SV, None)
        .await
        .expect("balanceOf decodes");
    assert_eq!(bal, U256::from(1_000_000_000u64));

    // totalSupply() returns a large value (must survive lossless: > u64 to
    // prove the u256 path).
    let total: U256 = (U256::from(1u64) << 100) | U256::from(0xDEAD_BEEFu64);
    let mut f2 = FakeRpc::new();
    f2.set(
        choreography::selector(b"totalSupply()"),
        total.to_be_bytes::<32>().to_vec(),
    );
    let io2 = io_with(f2);
    let sup = choreography::fetch_token_total_supply(&io2, TO, None)
        .await
        .expect("totalSupply decodes");
    assert_eq!(sup, total);

    // allowance(owner,spender) returns 4321.
    let mut f3 = FakeRpc::new();
    f3.set(
        choreography::selector(b"allowance(address,address)"),
        U256::from(4_321u64).to_be_bytes::<32>().to_vec(),
    );
    let io3 = io_with(f3);
    let allowance = choreography::fetch_token_allowance(&io3, TO, SV, TO, None)
        .await
        .expect("allowance decodes");
    assert_eq!(allowance, U256::from(4_321u64));
}
