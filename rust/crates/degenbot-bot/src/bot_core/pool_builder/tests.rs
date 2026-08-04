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
use alloy::primitives::{Address, Bytes, I256, U256};
use async_trait::async_trait;
use degenbot_core::errors::ProviderError;
use degenbot_rpc::abi;

use super::choreography;
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
