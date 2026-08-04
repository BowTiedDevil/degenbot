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
use alloy::primitives::{aliases::U112, Address, Bytes, I256, U256};
use async_trait::async_trait;
use degenbot_core::errors::ProviderError;
use degenbot_rpc::abi;

use super::{builder, choreography};
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
}

impl FakeRpc {
    fn new() -> Self {
        Self {
            responses: HashMap::new(),
        }
    }
    fn set(&mut self, sel: [u8; 4], bytes: Vec<u8>) {
        self.responses.insert(sel, bytes);
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

    let params = builder::build_v3(1, pool, &io, Some(9_000_000))
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
            pool_id: pid,
            currency0: TO,
            currency1: SV,
            fee: 500,
            tick_spacing: 1,
            hook_flags: 0,
        },
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
