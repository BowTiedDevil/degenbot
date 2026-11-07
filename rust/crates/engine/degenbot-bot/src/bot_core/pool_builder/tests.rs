//! Choreography decode unit tests.
//!
//! The V2/V3/V4 + ERC-20 + tick reads (plus the construction-policy wrappers
//! that compose them) moved core-side into this module. These tests drive each
//! read through a
//! `FakeRpc` (a [`crate::bot_core::construction_io::RpcConstruction`] double)
//! that returns pre-encoded response bytes per calldata selector, asserting the
//! decode — the coverage that previously lived in
//! `tests/builders/test_pybot_io.py` as Python fake-provider tests (removed
//! together with the Python-provider seam).

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use hashbrown::HashMap;

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{aliases::U112, aliases::U128, Address, Bytes, FixedBytes, I256, U256};
use alloy::sol_types::SolCall;
use async_trait::async_trait;
use degenbot_core::errors::ProviderError;
use degenbot_core::fee::{fee_ratio, fee_spec_retained};
use degenbot_pools::SpecRefusal;
use degenbot_rpc::abi::{self, ContractRead, Slot0Prefix, TickLiquidity, V2Reserves, V4Slot0};
use degenbot_rpc::multicall3::{AGGREGATE3_SELECTOR, MULTICALL3_ADDRESS};

use super::{builder, curve_choreography};
use crate::bot_core::construction_io::{
    ConstructionIo, NoDb, PacedRead, RpcConstruction, TickFetchPace,
};
use degenbot_pools::{DVariant, LendingRateStyle, SwapStyle};

const TO: Address = alloy::primitives::address!("0x1111111111111111111111111111111111111111");
const SV: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");
const POOL_ID: [u8; 32] = [0xab; 32];

/// ABI-encode a single return value → return bytes (inherent
/// `DynSolValue::abi_encode`, the same independent encoder path the crate's
/// `degenbot-rpc::abi` decoder-oracle tests use).
#[expect(clippy::needless_pass_by_value)] // test helper takes owned DynSolValue for inline call sites
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
    // Code returned by `get_code` (non-empty for the ERC-20 “contract present”
    // guard; empty by default so pool-family probes treat it as no-code).
    code: Bytes,
    // Selector-keyed hard provider failures (transport/timeout), distinct from
    // the no-response revert: checked before `responses`.
    errors: HashMap<[u8; 4], ProviderError>,
    // Yield to the executor before answering a call: the admission-race test
    // needs two concurrent constructions interleaved between their
    // get-or-register pre-checks and the one registration.
    yielding: bool,
    // Every `(target, calldata)` this double answered, in call order: the
    // batch-shape tests read the wire shape back from here.
    calls: std::sync::Mutex<Vec<(Address, Vec<u8>)>>,
}

impl FakeRpc {
    fn new() -> Self {
        Self {
            responses: HashMap::new(),
            responses_full: HashMap::new(),
            code: Bytes::new(),
            errors: HashMap::new(),
            yielding: false,
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn set(&mut self, sel: [u8; 4], bytes: Vec<u8>) {
        self.responses.insert(sel, bytes);
    }
    fn set_full(&mut self, data: Vec<u8>, bytes: Vec<u8>) {
        self.responses_full.insert(data, bytes);
    }
    fn set_code(&mut self, code: Bytes) {
        self.code = code;
    }

    fn set_error(&mut self, sel: [u8; 4], error: ProviderError) {
        self.errors.insert(sel, error);
    }

    fn set_yielding(&mut self) {
        self.yielding = true;
    }

    /// Every `(target, calldata)` answered so far, in call order.
    fn calls(&self) -> Vec<(Address, Vec<u8>)> {
        self.calls.lock().unwrap().clone()
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
        Ok(self.code.clone())
    }
    async fn get_balance(&self, _a: Address, _b: Option<u64>) -> Result<U256, ProviderError> {
        Ok(U256::ZERO)
    }
    async fn call(
        &self,
        to: Address,
        data: Bytes,
        _block: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        self.calls.lock().unwrap().push((to, data.to_vec()));
        if self.yielding {
            tokio::task::yield_now().await;
        }
        // A configured hard failure wins over any response: this models an
        // RPC/transport fault, not an EVM revert.
        if let Some(e) = self.errors.get(&data[..4]) {
            return Err(e.clone());
        }
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
    f.set(abi::selector(b"factory()"), addr_word(factory));
    let io = io_with(f);
    let got = abi::fetch_address_returning(&io, b"factory()", &TO, None)
        .await
        .unwrap();
    assert_eq!(got, factory);
}

#[tokio::test]
async fn build_erc20_metadata_resolves_on_chain_with_no_db() {
    // with NoDb the DB row is absent (all fields missing), so
    // build_erc20_metadata guards `get_code` then resolves all three on-chain
    // via the lower-spelling read; no write-back (no DB row).
    let mut f = FakeRpc::new();
    f.set_code(Bytes::from_static(&[0x60, 0x80]));
    f.set(abi::selector(b"name()"), str_ret("Wrapped Ether"));
    f.set(abi::selector(b"symbol()"), str_ret("WETH"));
    f.set(
        abi::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(18), 256)),
    );
    let io = io_with(f);
    let (name, symbol, decimals) = builder::build_erc20_metadata(&io, 1, TO, None)
        .await
        .unwrap();
    assert_eq!(name, "Wrapped Ether");
    assert_eq!(symbol, "WETH");
    assert_eq!(decimals, 18);
}

#[tokio::test]
async fn build_erc20_metadata_errors_on_empty_code() {
    // No contract deployed at the address → `get_code` is empty → the guard
    // raises `Decoding`, mirroring the Python "No contract deployed" error.
    let io = io_with(FakeRpc::new()); // code defaults to empty
    let err = builder::build_erc20_metadata(&io, 1, TO, None)
        .await
        .unwrap_err();
    match err {
        builder::PoolBuilderError::Decoding { message } => {
            assert_eq!(message, "No contract deployed at this address");
        }
        other => panic!("expected Decoding, got {other:?}"),
    }
}

#[tokio::test]
async fn build_erc20_metadata_unknown_sentinels_match_python() {
    // A present contract whose name()/symbol()/decimals() all revert resolves
    // the per-field UNKNOWN sentinels, matching the Python canonical values
    // (erc20.py): name="Unknown Token", symbol="UNKNOWN", decimals=18.
    let mut f = FakeRpc::new();
    f.set_code(Bytes::from_static(&[0x60, 0x80]));
    // No name()/symbol()/decimals() responses -> every eth_call reverts.
    let io = io_with(f);
    let (name, symbol, decimals) = builder::build_erc20_metadata(&io, 1, TO, None)
        .await
        .unwrap();
    assert_eq!(name, "Unknown Token");
    assert_eq!(symbol, "UNKNOWN");
    assert_eq!(decimals, 18);
}

#[tokio::test]
async fn build_erc20_metadata_uses_db_row_and_skips_on_chain() {
    // A DB-backed ConstructionIo whose row already has all three fields must
    // return them without any on-chain read. The FakeDb below returns a fully
    // populated row for the address; no RPC responses are configured, so any
    // on-chain read would revert.
    let io = ConstructionIo::new(
        std::sync::Arc::new(FakeDb { address: TO }),
        std::sync::Arc::new(FakeRpc::new()),
    );
    let (name, symbol, decimals) = builder::build_erc20_metadata(&io, 1, TO, None)
        .await
        .unwrap();
    assert_eq!(name, "Wrapped Ether");
    assert_eq!(symbol, "WETH");
    assert_eq!(decimals, 18);
}

/// One `aggregate3` return carrying per-sub-call `(success, returnData)` items.
fn aggregate3_ret(items: &[(bool, Vec<u8>)]) -> Vec<u8> {
    let tuples: Vec<DynSolValue> = items
        .iter()
        .map(|(success, data)| {
            DynSolValue::Tuple(vec![
                DynSolValue::Bool(*success),
                DynSolValue::Bytes(data.clone()),
            ])
        })
        .collect();
    DynSolValue::Array(tuples).abi_encode()
}

/// The `(target, sub_calldata)` pairs one `aggregate3` request carries.
fn decode_batch_calls(data: &[u8]) -> Vec<(Address, Vec<u8>)> {
    let ty: DynSolType = "(bool,address,bytes)[]".parse().unwrap();
    let decoded = ty.abi_decode(&data[4..]).unwrap();
    let DynSolValue::Array(items) = decoded else {
        panic!("aggregate3 params decode to an array");
    };
    items
        .into_iter()
        .map(|item| {
            let DynSolValue::Tuple(fields) = item else {
                panic!("each aggregate3 item is a tuple");
            };
            let (DynSolValue::Address(target), DynSolValue::Bytes(sub)) = (&fields[1], &fields[2])
            else {
                panic!("aggregate3 items carry (bool,address,bytes)");
            };
            (*target, sub.clone())
        })
        .collect()
}

/// The batch call shape over the one per-token core: a DB-complete token is
/// excluded from the network set, the network-missing tokens' eager reads
/// ride exactly ONE `aggregate3`, and a token whose eager sub-calls cannot
/// answer falls to the per-token alternate-prototype fallback — every
/// token's result in input order, no all-or-nothing collapse.
#[tokio::test]
async fn build_erc20_metadata_batch_is_one_aggregate3_over_the_network_missing_set() {
    let mut f = FakeRpc::new();
    f.set_code(Bytes::from_static(&[0x60, 0x80]));
    let db_complete: Address =
        alloy::primitives::address!("0xaac0000000000000000000000000000000000031");
    let network: Address =
        alloy::primitives::address!("0xaac0000000000000000000000000000000000032");
    let fallback_only: Address =
        alloy::primitives::address!("0xaac0000000000000000000000000000000000033");

    // ONE eager `aggregate3` for the two network-missing tokens, in input
    // order (three lower-spelling sub-calls each): `network` answers whole,
    // `fallback_only` fails every sub-call.
    f.set(
        AGGREGATE3_SELECTOR,
        aggregate3_ret(&[
            (true, str_ret("Beta")),
            (true, str_ret("BETA")),
            (true, enc(DynSolValue::Uint(U256::from(6u64), 256))),
            (false, Vec::new()),
            (false, Vec::new()),
            (false, Vec::new()),
        ]),
    );
    // The fallback leg's alternate spellings, per field.
    f.set(abi::selector(b"NAME()"), str_ret("Gamma"));
    f.set(abi::selector(b"SYMBOL()"), str_ret("GAMMA"));
    f.set(
        abi::selector(b"DECIMALS()"),
        enc(DynSolValue::Uint(U256::from(7u64), 256)),
    );

    let fake = std::sync::Arc::new(f);
    let rpc: std::sync::Arc<dyn RpcConstruction> = fake.clone();
    let io = ConstructionIo::new(
        std::sync::Arc::new(FakeDb {
            address: db_complete,
        }),
        rpc,
    );

    let out =
        builder::build_erc20_metadata_batch(&io, 1, &[db_complete, network, fallback_only], None)
            .await;

    // In input order: the DB row, the batched read, the per-token fallback.
    assert_eq!(
        out[0].as_ref().unwrap(),
        &("Wrapped Ether".to_owned(), "WETH".to_owned(), 18)
    );
    assert_eq!(
        out[1].as_ref().unwrap(),
        &("Beta".to_owned(), "BETA".to_owned(), 6)
    );
    assert_eq!(
        out[2].as_ref().unwrap(),
        &("Gamma".to_owned(), "GAMMA".to_owned(), 7)
    );

    // ONE aggregate3, covering exactly the network-missing set.
    let calls = fake.calls();
    let batches: Vec<&Vec<u8>> = calls
        .iter()
        .filter(|(to, data)| {
            *to == MULTICALL3_ADDRESS && data.len() >= 4 && data[..4] == AGGREGATE3_SELECTOR
        })
        .map(|(_, data)| data)
        .collect();
    assert_eq!(
        batches.len(),
        1,
        "the network set rides exactly ONE aggregate3"
    );
    let subs = decode_batch_calls(batches[0]);
    assert_eq!(
        subs.len(),
        6,
        "three eager spellings per network-missing token"
    );
    assert!(subs
        .iter()
        .all(|(target, _)| *target == network || *target == fallback_only));
    assert!(
        subs.iter().all(|(target, _)| *target != db_complete),
        "a DB-complete row is excluded from the network set"
    );

    // Outside the batch, only the fallback spellings were read — and only for
    // the token whose eager leg failed.
    let direct: Vec<(Address, [u8; 4])> = calls
        .iter()
        .filter(|(to, _)| *to != MULTICALL3_ADDRESS)
        .map(|(to, data)| (*to, data[..4].try_into().unwrap()))
        .collect();
    assert_eq!(
        direct,
        vec![
            (fallback_only, abi::selector(b"name()")),
            (fallback_only, abi::selector(b"NAME()")),
            (fallback_only, abi::selector(b"symbol()")),
            (fallback_only, abi::selector(b"SYMBOL()")),
            (fallback_only, abi::selector(b"decimals()")),
            (fallback_only, abi::selector(b"DECIMALS()")),
        ]
    );
}

/// A `DbConstruction` double returning a fully-populated `Erc20TokenRow` for a
/// single address (used to prove `build_erc20_metadata` prefers the DB row).
struct FakeDb {
    address: Address,
}

#[async_trait]
impl crate::bot_core::construction_io::DbConstruction for FakeDb {
    async fn fetch_erc20_token(
        &self,
        _chain_id: i64,
        address: Address,
    ) -> Result<Option<degenbot_db::rows::Erc20TokenRow>, degenbot_db::error::DbError> {
        if address == self.address {
            Ok(Some(degenbot_db::rows::Erc20TokenRow {
                id: 1,
                chain: 1,
                address: self.address,
                name: Some("Wrapped Ether".to_string()),
                symbol: Some("WETH".to_string()),
                decimals: Some(18),
            }))
        } else {
            Ok(None)
        }
    }
    async fn fetch_pool_row(
        &self,
        _chain_id: i64,
        _address: Address,
    ) -> Result<Option<degenbot_db::rows::LiquidityPoolRow>, degenbot_db::error::DbError> {
        Ok(None)
    }
    async fn fetch_pool_kind(
        &self,
        _kind: &str,
        _pool_id: i64,
    ) -> Result<Option<degenbot_db::rows::PoolKindRow>, degenbot_db::error::DbError> {
        Ok(None)
    }
    async fn fetch_token_by_id(
        &self,
        _token_id: i64,
    ) -> Result<Option<degenbot_db::rows::Erc20TokenRow>, degenbot_db::error::DbError> {
        Ok(None)
    }
    async fn fetch_exchange(
        &self,
        _exchange_id: i64,
    ) -> Result<Option<degenbot_db::rows::ExchangeRow>, degenbot_db::error::DbError> {
        Ok(None)
    }
    async fn fetch_liquidity_positions(
        &self,
        _pool_id: i64,
    ) -> Result<Vec<degenbot_db::rows::LiquidityPositionRow>, degenbot_db::error::DbError> {
        Ok(Vec::new())
    }
    async fn fetch_pool_manager(
        &self,
        _chain_id: i64,
        _address: Address,
    ) -> Result<Option<degenbot_db::rows::PoolManagerRow>, degenbot_db::error::DbError> {
        Ok(None)
    }
    async fn fetch_v4_pool_by_pool_hash(
        &self,
        _pool_hash_hex: &str,
    ) -> Result<Option<degenbot_db::rows::V4PoolRow>, degenbot_db::error::DbError> {
        Ok(None)
    }
    async fn fetch_managed_liquidity_positions(
        &self,
        _managed_pool_id: i64,
    ) -> Result<Vec<degenbot_db::rows::ManagedPoolLiquidityPositionRow>, degenbot_db::error::DbError>
    {
        Ok(Vec::new())
    }
    async fn update_erc20_token_metadata(
        &self,
        _chain_id: i64,
        _address: &str,
        _name: Option<&str>,
        _symbol: Option<&str>,
        _decimals: Option<i64>,
    ) -> Result<(), degenbot_db::error::DbError> {
        Ok(())
    }
}

#[tokio::test]
async fn fetch_v2_immutable_data_decodes() {
    let mut f = FakeRpc::new();
    let tok0: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000001");
    let tok1: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000002");
    f.set(abi::selector(b"factory()"), addr_word(tok0));
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    let io = io_with(f);
    let d = builder::fetch_v2_immutable_data(&io, &TO, None)
        .await
        .unwrap();
    assert_eq!(d.token0, tok0);
    assert_eq!(d.token1, tok1);
}

#[tokio::test]
async fn fetch_v2_reserves_decodes_two_uint256() {
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"getReserves()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(1_000_000u64), 256),
            DynSolValue::Uint(U256::from(2_000_000u64), 256),
            // `getReserves()` returns a 3-tuple (reserve0, reserve1,
            // blockTimestampLast); the decoder expects all three fields.
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    let io = io_with(f);
    let (r0, r1) = V2Reserves::read(&io, TO, None)
        .await
        .map(|r| (r.reserve0, r.reserve1))
        .unwrap();
    assert_eq!(r0, U256::from(1_000_000u64));
    assert_eq!(r1, U256::from(2_000_000u64));
}

#[tokio::test]
async fn fetch_v3_immutable_data_decodes_incl_negative_tick_spacing() {
    let mut f = FakeRpc::new();
    let tok0: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000001");
    let tok1: Address = alloy::primitives::address!("0xaaa0000000000000000000000000000000000002");
    f.set(abi::selector(b"factory()"), addr_word(tok0));
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(30), 24)),
    );
    f.set(
        abi::selector(b"tickSpacing()"),
        enc(DynSolValue::Int(I256::try_from(-10i128).unwrap(), 24)),
    );
    let io = io_with(f);
    let d = builder::fetch_v3_immutable_data(&io, &TO, None)
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
    f.set(abi::selector(b"slot0()"), slot0_ret(sqrt, -123));
    f.set(
        abi::selector(b"liquidity()"),
        enc(DynSolValue::Uint(U256::from(42), 128)),
    );
    let io = io_with(f);
    let slot0 = Slot0Prefix::read(&io, TO, None).await.unwrap();
    let liquidity = io
        .read(TO, abi::IUniswapV3Pool::liquidityCall {}, None)
        .await
        .unwrap();
    let (s, t, liq) = (slot0.sqrt_price_x96, slot0.tick, U256::from(liquidity));
    assert_eq!(s, sqrt);
    assert_eq!(t, I256::try_from(-123i128).unwrap());
    assert_eq!(liq, U256::from(42));
}

#[tokio::test]
async fn fetch_v4_slot0_liquidity_decodes() {
    let mut f = FakeRpc::new();
    let sqrt = U256::from(1u128 << 95);
    f.set(
        abi::selector(b"getSlot0(bytes32)"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(sqrt, 160),
            DynSolValue::Int(I256::try_from(7i128).unwrap(), 24),
            DynSolValue::Uint(U256::from(1), 24),
            DynSolValue::Uint(U256::from(2), 24),
        ])),
    );
    f.set(
        abi::selector(b"getLiquidity(bytes32)"),
        enc(DynSolValue::Uint(U256::from(99), 128)),
    );
    let io = io_with(f);
    let slot0 = V4Slot0::read(&io, SV, POOL_ID, None).await.unwrap();
    let liquidity = io
        .read(
            SV,
            abi::IUniswapV4StateView::getLiquidityCall {
                poolId: POOL_ID.into(),
            },
            None,
        )
        .await
        .unwrap();
    let (s, t, pf, lf, liq) = (
        slot0.sqrt_price_x96,
        slot0.tick,
        slot0.protocol_fee,
        slot0.lp_fee,
        U256::from(liquidity),
    );
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
        abi::selector(b"tickBitmap(int16)"),
        enc(DynSolValue::Uint(bitmap, 256)),
    );
    let io = io_with(f);
    assert_eq!(
        PacedRead::new(&io)
            .read(
                TO,
                abi::IUniswapV3Pool::tickBitmapCall { wordPosition: 3 },
                None
            )
            .await
            .unwrap(),
        bitmap
    );
}

/// The tick-RPC pace is a property of the construction transport, not of a
/// fetcher's discipline: every tick read crosses `PacedRead`, and a non-tick
/// read does not inherit the interval.
#[tokio::test]
async fn the_tick_pace_bounds_tick_reads_only() {
    let mut f = FakeRpc::new();
    let selector: [u8; 4] = abi::selector(b"tickBitmap(int16)");
    f.set(selector, enc(DynSolValue::Uint(U256::ZERO, 256)));
    let io = io_with(f).with_tick_fetch_pace(TickFetchPace::from_millis(40));

    // Generic construction reads claim no tick-fetch slot.
    let unpaced = std::time::Instant::now();
    abi::ContractRead::call(&io, TO, Bytes::from(selector.to_vec()), None)
        .await
        .unwrap();
    abi::ContractRead::call(&io, TO, Bytes::from(selector.to_vec()), None)
        .await
        .unwrap();
    assert!(
        unpaced.elapsed() < std::time::Duration::from_millis(30),
        "a non-tick read must not wait for the tick-RPC pace, got {:?}",
        unpaced.elapsed()
    );

    // Two tick reads are one interval apart.
    let paced = std::time::Instant::now();
    PacedRead::new(&io)
        .read(
            TO,
            abi::IUniswapV3Pool::tickBitmapCall { wordPosition: 3 },
            None,
        )
        .await
        .unwrap();
    PacedRead::new(&io)
        .read(
            TO,
            abi::IUniswapV3Pool::tickBitmapCall { wordPosition: 3 },
            None,
        )
        .await
        .unwrap();
    assert!(
        paced.elapsed() >= std::time::Duration::from_millis(30),
        "the second tick read waited for its paced slot, got {:?}",
        paced.elapsed()
    );
}

#[tokio::test]
async fn fetch_tick_data_decodes() {
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"ticks(int24)"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(111), 128),
            DynSolValue::Int(I256::try_from(-25i128).unwrap(), 128),
        ])),
    );
    let io = io_with(f);
    let (gross, net) = TickLiquidity::read(&PacedRead::new(&io), TO, 120, None)
        .await
        .map(|l| (l.gross, l.net))
        .unwrap();
    assert_eq!(gross.to_string(), "111");
    assert_eq!(net, -25i128);
}

#[tokio::test]
async fn fetch_v4_tick_bitmap_decodes() {
    let mut f = FakeRpc::new();
    let bitmap = U256::from(0x7u64) << 3;
    f.set(
        abi::selector(b"getTickBitmap(bytes32,int16)"),
        enc(DynSolValue::Uint(bitmap, 256)),
    );
    let io = io_with(f);
    assert_eq!(
        PacedRead::new(&io)
            .read(
                SV,
                abi::IUniswapV4StateView::getTickBitmapCall {
                    poolId: POOL_ID.into(),
                    wordPosition: -4
                },
                None
            )
            .await
            .unwrap(),
        bitmap
    );
}

#[tokio::test]
async fn fetch_v4_tick_data_decodes() {
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"getTickLiquidity(bytes32,int24)"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(7), 128),
            DynSolValue::Int(I256::try_from(3i128).unwrap(), 128),
        ])),
    );
    let io = io_with(f);
    let (gross, net) = TickLiquidity::read_v4(&PacedRead::new(&io), SV, POOL_ID, 5, None)
        .await
        .map(|l| (l.gross, l.net))
        .unwrap();
    assert_eq!(gross.to_string(), "7");
    assert_eq!(net, 3i128);
}

// ── PoolBuilder: probe → variant-resolution → build_v2  ──

#[tokio::test]
async fn probe_pool_type_dispatches() {
    // No responses → every probe reverts → unknown identity, never Curve.
    assert!(matches!(
        builder::probe_pool_type(&io_with(FakeRpc::new()), TO, None).await,
        Err(builder::PoolBuilderError::UnknownPoolIdentity { address }) if address == TO
    ));

    // coins(0) answers with an address → Curve.
    let mut f = FakeRpc::new();
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(TO),
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None)
            .await
            .unwrap(),
        builder::PoolFamily::Curve
    );

    // coins(0) answers with garbage → unknown identity, not Curve.
    let mut f = FakeRpc::new();
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        vec![0u8; 1],
    );
    assert!(matches!(
        builder::probe_pool_type(&io_with(f), TO, None).await,
        Err(builder::PoolBuilderError::UnknownPoolIdentity { address }) if address == TO
    ));

    // slot0() present → V3.
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"slot0()"),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None)
            .await
            .unwrap(),
        builder::PoolFamily::V3
    );

    // getReserves() present, no slot0 → V2.
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"getReserves()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::ZERO, 112),
            DynSolValue::Uint(U256::ZERO, 112),
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None)
            .await
            .unwrap(),
        builder::PoolFamily::V2
    );

    // getPoolId() present, no getNormalizedWeights → BalancerStable.
    let mut f = FakeRpc::new();
    f.set(abi::selector(b"getPoolId()"), addr_word(TO));
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None)
            .await
            .unwrap(),
        builder::PoolFamily::BalancerStable
    );

    // getPoolId() + getNormalizedWeights() → BalancerWeighted.
    let mut f = FakeRpc::new();
    f.set(abi::selector(b"getPoolId()"), addr_word(TO));
    f.set(
        abi::selector(b"getNormalizedWeights()"),
        enc(DynSolValue::FixedArray(vec![DynSolValue::Uint(
            U256::from(50),
            18,
        )])),
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None)
            .await
            .unwrap(),
        builder::PoolFamily::BalancerWeighted
    );
}

#[tokio::test]
async fn probe_pool_type_curve_leg_decides_when_presence_arms_answer_empty() {
    // Mainnet Curve registry pool 39/49,
    // 0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5, at the pinned registry
    // sweep block: a non-reverting fallback answers `slot0()` /
    // `getReserves()` / `getPoolId()` with empty returndata (the same artifact
    // an EOA's `eth_call` produces), while `coins(uint256)` answers with a
    // decodable address. Empty returndata declines every raw presence arm, so
    // the chain reaches the typed Curve leg and the pool is named by its coin
    // getter, never by the fallback artifact.
    let mut f = FakeRpc::new();
    f.set(abi::selector(b"slot0()"), vec![]);
    f.set(abi::selector(b"getReserves()"), vec![]);
    f.set(abi::selector(b"getPoolId()"), vec![]);
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(SV),
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None)
            .await
            .unwrap(),
        builder::PoolFamily::Curve
    );
}

#[tokio::test]
async fn probe_pool_type_accepts_the_legacy_int128_curve_shape() {
    // A legacy Curve registry pool exposes only `coins(int128)` — the other
    // supported coin-getter shape (`curve_choreography::CurvePrototype`);
    // `coins(uint256)` reverts on it (mainnet Curve registry pool 4/49,
    // 0x79a8C46DeA5aDa233ABaFFD40F3A0A2B1e5A4F27 at block 26204126). The
    // legacy shape is positive Curve identity: it resolves as Curve, never
    // into the unknown-identity refusal that broke that pool's construction.
    let mut f = FakeRpc::new();
    f.set_full(
        abi::ICurvePoolInt128::coinsCall { i: 0i128 }.abi_encode(),
        addr_word(SV),
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None)
            .await
            .unwrap(),
        builder::PoolFamily::Curve
    );

    // A garbage answer from the legacy getter is not identity either.
    let mut f = FakeRpc::new();
    f.set_full(
        abi::ICurvePoolInt128::coinsCall { i: 0i128 }.abi_encode(),
        vec![0u8; 1],
    );
    assert!(matches!(
        builder::probe_pool_type(&io_with(f), TO, None).await,
        Err(builder::PoolBuilderError::UnknownPoolIdentity { address }) if address == TO
    ));

    // A transport fault on the legacy leg is not absence: it propagates.
    let mut f = FakeRpc::new();
    f.set_error(
        abi::selector(b"coins(int128)"),
        ProviderError::ConnectionFailed {
            message: "rpc down".into(),
        },
    );
    match builder::probe_pool_type(&io_with(f), TO, None).await {
        Err(builder::PoolBuilderError::Rpc(ProviderError::ConnectionFailed { message })) => {
            assert_eq!(message, "rpc down");
        }
        other => panic!("expected ConnectionFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn probe_pool_type_propagates_transport_failure() {
    // A probe that fails with a transport/RPC error is NOT an absent selector:
    // it must surface, never degrade the pool to Curve.
    let mut f = FakeRpc::new();
    f.set_error(
        abi::selector(b"slot0()"),
        ProviderError::ConnectionFailed {
            message: "rpc down".into(),
        },
    );
    match builder::probe_pool_type(&io_with(f), TO, None).await {
        Err(builder::PoolBuilderError::Rpc(ProviderError::ConnectionFailed { message })) => {
            assert_eq!(message, "rpc down");
        }
        other => panic!("expected ConnectionFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn probe_pool_type_treats_empty_returndata_as_absence() {
    // The EOA / non-reverting-fallback artifact in full: every call "succeeds"
    // with empty returndata and neither coin getter answers with a decodable
    // address. Empty success is the absence of the selector, never its
    // presence — an address with no nonempty answer anywhere has no
    // recognized identity.
    let mut f = FakeRpc::new();
    f.set(abi::selector(b"slot0()"), vec![]);
    f.set(abi::selector(b"getReserves()"), vec![]);
    f.set(abi::selector(b"getPoolId()"), vec![]);
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        vec![],
    );
    f.set_full(
        abi::ICurvePoolInt128::coinsCall { i: 0i128 }.abi_encode(),
        vec![],
    );
    assert!(matches!(
        builder::probe_pool_type(&io_with(f), TO, None).await,
        Err(builder::PoolBuilderError::UnknownPoolIdentity { address }) if address == TO
    ));
}

#[tokio::test]
async fn probe_pool_type_classifies_v3_without_consulting_curve() {
    // `slot0()` answering with returndata decides the family without a single
    // Curve round trip — the coin getters are wired to hard-fault here, so
    // any consultation of the Curve leg surfaces as an error instead of a
    // family. The raw presence arms must be able to decide alone: paying for
    // Curve reads on every V3/V2/Balancer build exposes those builds to
    // endpoint faults on reads that can add no evidence.
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"slot0()"),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    f.set_error(
        abi::selector(b"coins(uint256)"),
        ProviderError::ConnectionFailed {
            message: "rpc down".into(),
        },
    );
    f.set_error(
        abi::selector(b"coins(int128)"),
        ProviderError::ConnectionFailed {
            message: "rpc down".into(),
        },
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None)
            .await
            .unwrap(),
        builder::PoolFamily::V3
    );
}

#[tokio::test]
async fn probe_pool_type_classifies_v2_without_consulting_curve() {
    // The Camelot V2 shape: `getReserves()` answering with returndata decides
    // V2 with the Curve leg never consulted (coin getters hard-fault here).
    // A V2 build must neither pay for nor risk Curve probes.
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"getReserves()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::ZERO, 112),
            DynSolValue::Uint(U256::ZERO, 112),
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    f.set_error(
        abi::selector(b"coins(uint256)"),
        ProviderError::ConnectionFailed {
            message: "rpc down".into(),
        },
    );
    f.set_error(
        abi::selector(b"coins(int128)"),
        ProviderError::ConnectionFailed {
            message: "rpc down".into(),
        },
    );
    assert_eq!(
        builder::probe_pool_type(&io_with(f), TO, None)
            .await
            .unwrap(),
        builder::PoolFamily::V2
    );
}

#[tokio::test]
async fn probe_pool_type_treats_an_empty_curve_answer_as_absence() {
    // A coin getter "answering" with nothing is absence — not a transport
    // fault, and not identity: the typed read cannot decode an empty return,
    // and an undecodable answer never names a family.
    let mut f = FakeRpc::new();
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        vec![],
    );
    assert!(matches!(
        builder::probe_pool_type(&io_with(f), TO, None).await,
        Err(builder::PoolBuilderError::UnknownPoolIdentity { address }) if address == TO
    ));
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
    assert_eq!(id.fee_token0, fee_spec_retained(997, 1000));
}

#[tokio::test]
async fn resolve_v2_dex_reads_stable_flag_for_camelot_factory() {
    use degenbot_uniswap::dex_identity::{self, DexVariant};
    // Camelot volatile+stable share a factory; the pool's `stableSwap()` read
    // disambiguates. Give the read `true` → CamelotV2Stable.
    let factory = dex_identity::CAMELOT_V2_STABLE.factory;
    let mut f = FakeRpc::new();
    f.set(abi::selector(b"stableSwap()"), enc(DynSolValue::Bool(true)));
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
    f.set(abi::selector(b"stableSwap()"), enc(DynSolValue::Bool(true)));
    f.set(
        abi::selector(b"FEE_DENOMINATOR()"),
        enc(DynSolValue::Uint(U256::from(10_000u64), 256)),
    );
    f.set(
        abi::selector(b"token0FeePercent()"),
        enc(DynSolValue::Uint(U256::from(20u64), 16)),
    );
    f.set(
        abi::selector(b"token1FeePercent()"),
        enc(DynSolValue::Uint(U256::from(200u64), 16)),
    );
    let io = io_with(f);
    let s = builder::fetch_camelot_state(&io, &TO, None).await.unwrap();
    assert!(s.stable);
    assert_eq!(s.fee_denominator, U256::from(10_000u64));
    assert_eq!(s.token0_fee_percent, 20);
    assert_eq!(s.token1_fee_percent, 200);
}

/// A Camelot solidly-stable pool's fee is per-pool state, not a preset
/// constant: the build reads `token0FeePercent()` / `token1FeePercent()` over
/// the pool's own `FEE_DENOMINATOR()`.
///
/// The measured pool is the Arbitrum MIM/USDC stable pair (reserves and the
/// 0.04% fee as recorded on-chain: `getReserves()[2..4] = (40, 40)` over
/// `FEE_DENOMINATOR() = 100000`). Taking the `CAMELOT_V2_STABLE` preset's
/// `(0, denom)` "read the pool" marker instead would build a 100%-fee pool
/// that quotes zero for every input.
#[tokio::test]
async fn build_v2_reads_the_camelot_stable_fee_pair_from_the_pool() {
    use degenbot_uniswap::create2::compute_v2_address;
    use degenbot_uniswap::dex_identity::{self, DexVariant};

    let mim: Address = alloy::primitives::address!("0xFEa7a6a0B346362BF88A9e4A88416B77a57D6c2A");
    let usdc: Address = alloy::primitives::address!("0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8");
    let measured: Address =
        alloy::primitives::address!("0x68A0859de50B4Dfc6EFEbE981cA906D38Cdb0D1F");
    let pool = compute_v2_address(
        dex_identity::CAMELOT_V2_STABLE.factory,
        mim,
        usdc,
        dex_identity::CAMELOT_V2_STABLE.init_hash,
    );
    assert_eq!(
        pool, measured,
        "the recorded MIM/USDC pool is the CREATE2 address of its pair"
    );

    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"factory()"),
        addr_word(dex_identity::CAMELOT_V2_STABLE.factory),
    );
    f.set(abi::selector(b"token0()"), addr_word(mim));
    f.set(abi::selector(b"token1()"), addr_word(usdc));
    f.set(
        abi::selector(b"getReserves()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(234_608_881_288_298_988_978u128), 112),
            DynSolValue::Uint(U256::from(2_945_856u64), 112),
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    f.set(abi::selector(b"stableSwap()"), enc(DynSolValue::Bool(true)));
    f.set(
        abi::selector(b"FEE_DENOMINATOR()"),
        enc(DynSolValue::Uint(U256::from(100_000u64), 256)),
    );
    f.set(
        abi::selector(b"token0FeePercent()"),
        enc(DynSolValue::Uint(U256::from(40u64), 16)),
    );
    f.set(
        abi::selector(b"token1FeePercent()"),
        enc(DynSolValue::Uint(U256::from(40u64), 16)),
    );
    let io = io_with(f);

    let params = builder::build_v2(42_161, pool, &io, Some(477_785_000))
        .await
        .unwrap();
    assert_eq!(params.variant, DexVariant::CamelotV2Stable);
    assert!(params.stable_swap);
    // 0.04% fee = 40/100000 → retained gamma 99960/100000, both directions.
    assert_eq!(params.fee_token0, fee_ratio(99_960, 100_000));
    assert_eq!(params.fee_token1, fee_ratio(99_960, 100_000));

    // The registered identity carries the pool's own pair, not the marker's.
    let (identity, _state) =
        degenbot_pools::V2PoolState::from_params(&params, 8, (Some(18), Some(6)));
    assert_eq!(identity.fee_token0, fee_ratio(99_960, 100_000));
    assert_eq!(identity.fee_token1, fee_ratio(99_960, 100_000));
}

/// A stable fee configuration that retains nothing (a 100% fee, i.e. the
/// preset marker's own gamma) is refused BY ADDRESS rather than registered.
#[tokio::test]
async fn build_v2_refuses_a_camelot_stable_fee_that_retains_nothing() {
    use degenbot_uniswap::create2::compute_v2_address;
    use degenbot_uniswap::dex_identity;

    let tok0: Address = alloy::primitives::address!("0xFEa7a6a0B346362BF88A9e4A88416B77a57D6c2A");
    let tok1: Address = alloy::primitives::address!("0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8");
    let pool = compute_v2_address(
        dex_identity::CAMELOT_V2_STABLE.factory,
        tok0,
        tok1,
        dex_identity::CAMELOT_V2_STABLE.init_hash,
    );

    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"factory()"),
        addr_word(dex_identity::CAMELOT_V2_STABLE.factory),
    );
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"getReserves()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(1_000u64), 112),
            DynSolValue::Uint(U256::from(2_000u64), 112),
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    f.set(abi::selector(b"stableSwap()"), enc(DynSolValue::Bool(true)));
    f.set(
        abi::selector(b"FEE_DENOMINATOR()"),
        enc(DynSolValue::Uint(U256::from(100_000u64), 256)),
    );
    // A 100% fee on token0: gamma = denom - percent = 0.
    f.set(
        abi::selector(b"token0FeePercent()"),
        enc(DynSolValue::Uint(U256::from(100_000u64), 16)),
    );
    f.set(
        abi::selector(b"token1FeePercent()"),
        enc(DynSolValue::Uint(U256::from(40u64), 16)),
    );
    let io = io_with(f);

    match builder::build_v2(42_161, pool, &io, Some(477_785_000)).await {
        Err(builder::PoolBuilderError::CamelotStableFee { address, detail }) => {
            assert_eq!(address, pool, "the refusal names the pool");
            assert!(
                detail.contains("100000"),
                "the refusal names the offending percent: {detail}"
            );
        }
        other => panic!("expected a CamelotStableFee refusal, got {other:?}"),
    }
}

/// A failed fee read surfaces as the provider error — the build never falls
/// back to the preset's gamma-0 pair.
#[tokio::test]
async fn build_v2_surfaces_a_failed_camelot_fee_read() {
    use degenbot_uniswap::create2::compute_v2_address;
    use degenbot_uniswap::dex_identity;

    let tok0: Address = alloy::primitives::address!("0xFEa7a6a0B346362BF88A9e4A88416B77a57D6c2A");
    let tok1: Address = alloy::primitives::address!("0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8");
    let pool = compute_v2_address(
        dex_identity::CAMELOT_V2_STABLE.factory,
        tok0,
        tok1,
        dex_identity::CAMELOT_V2_STABLE.init_hash,
    );

    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"factory()"),
        addr_word(dex_identity::CAMELOT_V2_STABLE.factory),
    );
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"getReserves()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(1_000u64), 112),
            DynSolValue::Uint(U256::from(2_000u64), 112),
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    f.set(abi::selector(b"stableSwap()"), enc(DynSolValue::Bool(true)));
    f.set(
        abi::selector(b"FEE_DENOMINATOR()"),
        enc(DynSolValue::Uint(U256::from(100_000u64), 256)),
    );
    // The pool answers `token0FeePercent()` with a transport fault.
    f.set_error(
        abi::selector(b"token0FeePercent()"),
        ProviderError::Timeout {
            message: "fee read timed out".into(),
        },
    );
    f.set(
        abi::selector(b"token1FeePercent()"),
        enc(DynSolValue::Uint(U256::from(40u64), 16)),
    );
    let io = io_with(f);

    match builder::build_v2(42_161, pool, &io, Some(477_785_000)).await {
        Err(builder::PoolBuilderError::Rpc(ProviderError::Timeout { message })) => {
            assert_eq!(message, "fee read timed out");
        }
        other => panic!("expected the provider failure to surface, got {other:?}"),
    }
}

/// A volatile Camelot pool keeps the preset's deployment-constant fee and
/// issues no per-pool fee read at all.
#[tokio::test]
async fn build_v2_keeps_the_preset_pair_for_camelot_volatile() {
    use degenbot_uniswap::create2::compute_v2_address;
    use degenbot_uniswap::dex_identity::{self, DexVariant};

    let tok0: Address = alloy::primitives::address!("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1");
    let tok1: Address = alloy::primitives::address!("0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8");
    let pool = compute_v2_address(
        dex_identity::CAMELOT_V2_VOLATILE.factory,
        tok0,
        tok1,
        dex_identity::CAMELOT_V2_VOLATILE.init_hash,
    );

    // No `FEE_DENOMINATOR()` / `token*FeePercent()` responses: a read of any
    // of them reverts, so the pair below can only come from the preset.
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"factory()"),
        addr_word(dex_identity::CAMELOT_V2_VOLATILE.factory),
    );
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"getReserves()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(41_955_211_741_551_457_825u128), 112),
            DynSolValue::Uint(U256::from(66_259_753_282u64), 112),
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    f.set(
        abi::selector(b"stableSwap()"),
        enc(DynSolValue::Bool(false)),
    );
    let io = io_with(f);

    let params = builder::build_v2(42_161, pool, &io, Some(477_785_000))
        .await
        .unwrap();
    assert_eq!(params.variant, DexVariant::CamelotV2Volatile);
    assert!(!params.stable_swap);
    // The preset -> params boundary converted the declared ratio.
    assert_eq!(
        Some(params.fee_token0),
        dex_identity::CAMELOT_V2_VOLATILE.fee_token0.ratio()
    );
    assert_eq!(
        Some(params.fee_token1),
        dex_identity::CAMELOT_V2_VOLATILE.fee_token1.ratio()
    );
}

#[tokio::test]
async fn build_v2_assembles_register_params_from_onchain() {
    use degenbot_uniswap::create2::compute_v2_address;
    use degenbot_uniswap::dex_identity::{DexVariant, UNISWAP_V2};

    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool = compute_v2_address(UNISWAP_V2.factory, tok0, tok1, UNISWAP_V2.init_hash);

    let mut f = FakeRpc::new();
    f.set(abi::selector(b"factory()"), addr_word(UNISWAP_V2.factory));
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"getReserves()"),
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
    assert_eq!(params.fee_token0, fee_ratio(997, 1_000));
    assert_eq!(params.update_block, 18_000_000);
    assert!(!params.stable_swap);
    // Proxy preserves the resulting identity/state build.
    let (identity, state) = degenbot_pools::V2PoolState::from_params(&params, 8, (None, None));
    assert_eq!(identity.address, pool);
    assert_eq!(state.reserve0, U112::from(123_456_789u64));
}

/// The V2-reserve refusal names its reason, and it enforces the on-chain
/// `uint112` bound.
///
/// The arm is a DEFENSIVE guard, not a reachable refusal: the `getReserves()`
/// decode narrows to `uint112` (an over-bound word comes back masked — `2^112`
/// decodes as `0`), so no RPC hands the builder an out-of-bound reserve. The
/// name still has to be right: it is what a caller renders when the guard
/// fires.
#[test]
fn v2_reserve_refusal_names_its_reason() {
    assert!(
        degenbot_pools::narrow_v2_reserve(
            degenbot_pools::UINT112_MAX + U256::from(1u64),
            "reserve0",
        )
        .is_err(),
        "one past uint112(-1) is out of bounds"
    );
    assert_eq!(
        builder::PoolBuilderError::Spec(SpecRefusal::V2Reserve).to_string(),
        "out of spec (v2 reserve out of bounds)"
    );
}

#[tokio::test]
async fn build_v3_assembles_sparse_register_params_from_onchain() {
    use degenbot_pools::PoolTickCoverage;
    // Fake factory NOT in the JSON → CREATE2 verify is skipped (ad-hoc path).
    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");

    let mut f = FakeRpc::new();
    f.set(abi::selector(b"factory()"), addr_word(factory));
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(3000u32), 24)),
    );
    f.set(
        abi::selector(b"tickSpacing()"),
        enc(DynSolValue::Int(I256::try_from(60i32).unwrap(), 24)),
    );
    f.set(
        abi::selector(b"slot0()"),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    f.set(
        abi::selector(b"liquidity()"),
        enc(DynSolValue::Uint(U256::from(1_000_000_000u64), 128)),
    );
    // tick=0, spacing=60 → word 0; zero bitmap → sparse, no per-tick reads.
    f.set(
        abi::selector(b"tickBitmap(int16)"),
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
    use degenbot_pools::PoolTickCoverage;
    let pm: Address = alloy::primitives::address!("0x3333333333333333333333333333333333333333");
    let pid: [u8; 32] = [0xcd; 32];

    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"getSlot0(bytes32)"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(1u128 << 96), 160),
            DynSolValue::Int(I256::try_from(5i32).unwrap(), 24),
            DynSolValue::Uint(U256::from(300u32), 24), // protocol_fee
            DynSolValue::Uint(U256::from(50u32), 24),  // lp_fee
        ])),
    );
    f.set(
        abi::selector(b"getLiquidity(bytes32)"),
        enc(DynSolValue::Uint(U256::from(2_000_000_000u64), 128)),
    );
    // tick=5, spacing=1 -> word 0; zero bitmap -> sparse, no per-tick reads.
    f.set(
        abi::selector(b"getTickBitmap(bytes32,int16)"),
        enc(DynSolValue::Uint(U256::ZERO, 256)),
    );
    let io = io_with(f);

    let hook_addr = alloy::primitives::address!("0x99999999990a45c0ab9ec7e4b076dcfa05898f48");
    let result = builder::build_v4(
        builder::V4PoolBuildIdentity {
            pool_manager: pm,
            state_view: pm,
            pool_id: pid,
            currency0: TO,
            currency1: SV,
            fee: 500,
            tick_spacing: 1,
            hook_address: hook_addr,
        },
        None,
        &io,
        Some(11_000_000),
    )
    .await
    .unwrap();
    let params = &result.params;
    assert_eq!(params.pool_manager, pm);
    assert_eq!(params.pool_id, pid);
    assert_eq!(params.pool_key.currency0, TO);
    assert_eq!(params.pool_key.currency1, SV);
    assert_eq!(params.pool_key.fee, 500);
    assert_eq!(params.pool_key.tick_spacing, 1);
    // Regression (pool-ID mismatch incident): the registered pool key must
    // carry the REAL hook address, not Address::ZERO — the Python companion
    // re-derives pool_id = keccak(pool_key) from this identity.
    assert_eq!(params.pool_key.hooks, hook_addr);
    assert_eq!(params.hook_flags, builder::derive_hook_flags(hook_addr));
    assert_eq!(params.protocol_fee, 300);
    assert_eq!(params.tick, 5);
    assert_eq!(params.coverage, PoolTickCoverage::Sparse);
    assert!(params.tick_data.is_empty());
    // lp_fee is no longer discarded - returned from the SAME
    // head-stamped slot0 read as protocol_fee (mock slot0 has lp_fee=50).
    assert_eq!(result.lp_fee, 50);
}

/// A `TickMapDb` fake returning a canned `LiquidityMap` (V3 + V4 both query it).
struct FakeTickMapDb(degenbot_db::snapshot::LiquidityMap, Option<i64>);

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
    fn fetch_liquidity_update_block(
        &self,
        _address: Address,
    ) -> Result<Option<i64>, degenbot_db::error::DbError> {
        Ok(self.1)
    }
    fn fetch_liquidity_update_block_v4(
        &self,
        _pool_manager: Address,
        _pool_id_hash: alloy::primitives::B256,
    ) -> Result<Option<i64>, degenbot_db::error::DbError> {
        Ok(self.1)
    }
}

/// A DB hit gives a TRACKED pool: the builder favors the `TickMapDb` full tick
/// map (Tracked → feeds the verify lifecycle) over the chain-arm single-word
/// bootstrap, and ADMITS it only after the chain-truth intake gate reads the
/// pool's word 0 from chain and finds it agrees with the DB rows.
#[tokio::test]
async fn build_v3_db_hit_yields_tracked() {
    use degenbot_db::snapshot::{LiquidityAtTick, LiquidityMap};
    use degenbot_pools::PoolTickCoverage;
    use hashbrown::HashMap;

    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");

    let mut f = FakeRpc::new();
    f.set(abi::selector(b"factory()"), addr_word(factory));
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(3000u32), 24)),
    );
    f.set(
        abi::selector(b"tickSpacing()"),
        enc(DynSolValue::Int(I256::try_from(60i32).unwrap(), 24)),
    );
    f.set(
        abi::selector(b"slot0()"),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    f.set(
        abi::selector(b"liquidity()"),
        enc(DynSolValue::Uint(U256::from(1_000_000_000u64), 128)),
    );
    // The chain-truth intake gate reads the pool's word 0 from chain: bit 1
    // (tick 60 at spacing 60) must match the DB row below.
    f.set(
        abi::selector(b"tickBitmap(int16)"),
        enc(DynSolValue::Uint(U256::from(1u64) << 1, 256)),
    );
    let io = io_with(f);

    let db = FakeTickMapDb(
        LiquidityMap {
            tick_data: HashMap::from([(
                60,
                LiquidityAtTick {
                    liquidity_gross: U256::from(100u64),
                    liquidity_net: 100i128,
                },
            )]),
        },
        None,
    );

    let params = builder::build_v3(1, pool, Some(&db), &io, Some(9_000_000))
        .await
        .unwrap();
    assert_eq!(params.coverage, PoolTickCoverage::Tracked);
    assert_eq!(params.tick_data.len(), 1);
    assert_eq!(params.tick_data[&60].liquidity_gross, U128::from(100u64));
    assert_eq!(params.tick_data[&60].liquidity_net, 100i128);
}

/// a DB-seeded (`Tracked`) pool's LIQUIDITY clock
/// (`tick_data_block`) must be the DB `liquidity_update_block`, NOT the
/// caller-supplied head price clock. Before the fix the builder stamped
/// `tick_data_block = update_block = head`, so the seed/post-drain verify
/// compared stale DB seed data against on-chain at head and fabricated a
/// mismatch (regression 8c50e0cd).
#[tokio::test]
async fn build_v3_db_hit_stamps_tick_data_block_at_db_liquidity_update_block() {
    use degenbot_db::snapshot::{LiquidityAtTick, LiquidityMap};
    use degenbot_pools::PoolTickCoverage;
    use hashbrown::HashMap;

    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");

    let mut f = FakeRpc::new();
    f.set(abi::selector(b"factory()"), addr_word(factory));
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(3000u32), 24)),
    );
    f.set(
        abi::selector(b"tickSpacing()"),
        enc(DynSolValue::Int(I256::try_from(60i32).unwrap(), 24)),
    );
    f.set(
        abi::selector(b"slot0()"),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    f.set(
        abi::selector(b"liquidity()"),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 128)),
    );
    // The chain-truth intake gate reads word 0 from chain: bit 1 (tick 60 at
    // spacing 60) must match the DB row below.
    f.set(
        abi::selector(b"tickBitmap(int16)"),
        enc(DynSolValue::Uint(U256::from(1u64) << 1, 256)),
    );
    let io = io_with(f);

    // DB-seeded pool whose liquidity map is exact at block 123.
    let db = FakeTickMapDb(
        LiquidityMap {
            tick_data: HashMap::from([(
                60,
                LiquidityAtTick {
                    liquidity_gross: U256::from(100u64),
                    liquidity_net: 100i128,
                },
            )]),
        },
        Some(123),
    );

    // Caller-supplied build block (the live head price clock) = 9_000_000.
    let params = builder::build_v3(1, pool, Some(&db), &io, Some(9_000_000))
        .await
        .unwrap();
    assert_eq!(params.coverage, PoolTickCoverage::Tracked);
    assert_eq!(
        params.tick_data_block,
        Some(123),
        "DB-seeded pool's liquidity clock must be the DB liquidity_update_block, not the head"
    );
    assert_eq!(
        params.update_block, 9_000_000,
        "the PRICE clock stays at the fresh head read (two-stamp rule)"
    );
}

// --- Balancer choreography + builder decode tests -------------------

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
        abi::selector(b"getNormalizedWeights()"),
        u256_array_ret(&[500_000_000_000_000_000u64, 500_000_000_000_000_000u64]),
    );
    let io = io_with(f);
    let w = io
        .read(TO, abi::IBalancerPool::getNormalizedWeightsCall {}, None)
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
    assert!(builder::probe_balancer_type(&io, &TO, None).await.is_err());

    // getNormalizedWeights responds → Weighted.
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"getNormalizedWeights()"),
        u256_array_ret(&[500_000_000_000_000_000u64]),
    );
    let io = io_with(f);
    assert_eq!(
        builder::probe_balancer_type(&io, &TO, None).await.unwrap(),
        builder::BalancerFamily::Weighted
    );

    // Only getAmplificationParameter → Stable.
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"getAmplificationParameter()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(100u64), 256),
            DynSolValue::Bool(false),
            DynSolValue::Uint(U256::from(100u64), 256),
        ])),
    );
    let io = io_with(f);
    assert_eq!(
        builder::probe_balancer_type(&io, &TO, None).await.unwrap(),
        builder::BalancerFamily::Stable
    );
}

/// Factory membership is the Balancer V2 ownership answer (no enumerable
/// registry, no `factory()` getter on a deployed pool): the candidate that
/// answers `true` owns the pool, an all-`false` / silent candidate set
/// (unregistered factory revision) reports no owner.
#[tokio::test]
async fn fetch_balancer_factory_reports_the_owning_candidate() {
    let sel: [u8; 4] = abi::selector(b"isPoolFromFactory(address)");

    // Every candidate reverts (no response configured) → no owner.
    let io = io_with(FakeRpc::new());
    assert_eq!(
        builder::fetch_balancer_factory(&io, &TO, &[VAULT], None).await,
        None
    );

    // `true` → the candidate owns the pool.
    let mut f = FakeRpc::new();
    f.set(sel, enc(DynSolValue::Bool(true)));
    let io = io_with(f);
    assert_eq!(
        builder::fetch_balancer_factory(&io, &TO, &[VAULT], None).await,
        Some(VAULT)
    );

    // `false` → not the owner.
    let mut f = FakeRpc::new();
    f.set(sel, enc(DynSolValue::Bool(false)));
    let io = io_with(f);
    assert_eq!(
        builder::fetch_balancer_factory(&io, &TO, &[VAULT], None).await,
        None
    );
}

#[tokio::test]
async fn build_balancer_weighted_assembles_params() {
    let mut f = FakeRpc::new();
    let pool_id = [0xcd; 32];
    f.set(abi::selector(b"getPoolId()"), pool_id_ret(&pool_id));
    f.set(
        abi::selector(b"getPoolTokens(bytes32)"),
        pool_tokens_ret(&[T0, T1], &[U256::from(1_000u64), U256::from(2_000u64)]),
    );
    f.set(
        abi::selector(b"getSwapFeePercentage()"),
        enc(DynSolValue::Uint(U256::from(5_000_000_000_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"getNormalizedWeights()"),
        u256_array_ret(&[500_000_000_000_000_000u64, 500_000_000_000_000_000u64]),
    );
    // Token decimals: both 18 → scaling factor ONE (1e18) each. (The FakeRpc
    // keys responses by 4-byte selector only, so per-address decimals can't be
    // distinguished here — variance is covered by a dedicated scaling test.)
    f.set(
        abi::selector(b"decimals()"),
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
    f.set(abi::selector(b"getPoolId()"), pool_id_ret(&pool_id));
    f.set(
        abi::selector(b"getPoolTokens(bytes32)"),
        pool_tokens_ret(&[T0, T1], &[U256::from(100u64), U256::from(200u64)]),
    );
    f.set(
        abi::selector(b"getSwapFeePercentage()"),
        enc(DynSolValue::Uint(U256::from(5_000_000_000_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"getAmplificationParameter()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(100u64), 256),
            DynSolValue::Bool(false),
            DynSolValue::Uint(U256::from(100u64), 256),
        ])),
    );
    // Both tokens 18 decimals; one rate provider returning 1.02e18.
    f.set(
        abi::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(18), 256)),
    );
    f.set(
        abi::selector(b"getRateProviders()"),
        enc(DynSolValue::Array(vec![
            DynSolValue::Address(alloy::primitives::address!(
                "0x1111111111111111111111111111111111111111"
            )),
            DynSolValue::Address(Address::ZERO),
        ])),
    );
    f.set(
        abi::selector(b"getRate()"),
        enc(DynSolValue::Uint(
            U256::from(1_020_000_000_000_000_000u64),
            256,
        )),
    );
    let io = io_with(f);
    // No BPT among the Vault tokens → a non-composable stable pool → V2.
    let p = builder::build_balancer_stable(VAULT, TO, &io, None, None)
        .await
        .unwrap();
    assert_eq!(p.invariant_version, 2);
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

/// `FakeRpc` responses for one `build_balancer_stable` pass. `tokens` is the
/// Vault token list; a `ComposableStablePool` lists *itself* in it.
fn balancer_stable_fakes(tokens: &[Address]) -> FakeRpc {
    let mut f = FakeRpc::new();
    let pool_id = [0xef; 32];
    f.set(abi::selector(b"getPoolId()"), pool_id_ret(&pool_id));
    let balances: Vec<U256> = (0..tokens.len()).map(|_| U256::from(100u64)).collect();
    f.set(
        abi::selector(b"getPoolTokens(bytes32)"),
        pool_tokens_ret(tokens, &balances),
    );
    f.set(
        abi::selector(b"getSwapFeePercentage()"),
        enc(DynSolValue::Uint(U256::from(5_000_000_000_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"getAmplificationParameter()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(100u64), 256),
            DynSolValue::Bool(false),
            DynSolValue::Uint(U256::from(100u64), 256),
        ])),
    );
    f.set(
        abi::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(18), 256)),
    );
    // Every token carries the zero-address sentinel rate provider. Both stable
    // families answer `getRateProviders()` (measured on the mainnet
    // MetaStablePools), so its presence says nothing about the family.
    f.set(
        abi::selector(b"getRateProviders()"),
        enc(DynSolValue::Array(
            tokens
                .iter()
                .map(|_| DynSolValue::Address(Address::ZERO))
                .collect(),
        )),
    );
    f
}

/// A `ComposableStablePool` lists its own BPT among the Vault tokens
/// (`bpt_idx` = `Some`). Composable pools ship the monorepo `StableMath` —
/// always-roundDown `D_P` — so the shape resolves to `INVARIANT_V1`.
#[tokio::test]
async fn build_balancer_stable_derives_v1_for_a_composable_pool() {
    let io = io_with(balancer_stable_fakes(&[TO, T0, T1]));
    let p = builder::build_balancer_stable(VAULT, TO, &io, None, None)
        .await
        .unwrap();
    assert_eq!(p.bpt_idx, Some(0), "composable shape: the BPT is token 0");
    assert_eq!(p.invariant_version, 1);
}

/// A non-composable stable pool (`MetaStablePool` / the 2021 `StablePool`) carries
/// no BPT and ships the 2021 deployed `StableMath` — roundUp `P_D` — so the
/// shape resolves to `INVARIANT_V2`.
///
/// The wstETH/WETH `MetaStablePool`'s pool-id specialization is `0x0002`
/// (`TwoToken` — a token-count artifact of the Vault's `_getSpecialization`,
/// not a family signal), which is why the retired
/// specialization-derived rule got every `MetaStablePool` wrong.
#[tokio::test]
async fn build_balancer_stable_derives_v2_for_a_non_composable_pool() {
    let io = io_with(balancer_stable_fakes(&[T0, T1]));
    let p = builder::build_balancer_stable(VAULT, TO, &io, None, None)
        .await
        .unwrap();
    assert_eq!(p.bpt_idx, None);
    assert_eq!(p.invariant_version, 2);
}

/// An explicit override outranks the shape derivation in both directions.
#[tokio::test]
async fn build_balancer_stable_override_outranks_the_shape() {
    for (tokens, override_version) in [(&[TO, T0, T1][..], 2u8), (&[T0, T1][..], 1u8)] {
        let io = io_with(balancer_stable_fakes(tokens));
        let p = builder::build_balancer_stable(VAULT, TO, &io, None, Some(override_version))
            .await
            .unwrap();
        assert_eq!(p.invariant_version, override_version);
    }
}

/// A known-broken Balancer V2 pool is refused before any RPC: the `FakeRpc`
/// carries no responses because the gate fires before the first read.
#[tokio::test]
async fn build_balancer_weighted_rejects_a_known_broken_pool() {
    let io = io_with(FakeRpc::new());
    for broken in builder::BROKEN_BALANCER_V2_POOLS {
        assert!(
            matches!(
                builder::build_balancer_weighted(VAULT, *broken, &io, Some(1)).await,
                Err(builder::PoolBuilderError::Spec(
                    SpecRefusal::BalancerBrokenPool
                ))
            ),
            "known-broken pool built: {broken}"
        );
    }
}

/// The same known-broken pool is refused on the stable arm too — the gate is a
/// property of the pool, not of the family the caller guessed.
#[tokio::test]
async fn build_balancer_stable_rejects_a_known_broken_pool() {
    let io = io_with(FakeRpc::new());
    for broken in builder::BROKEN_BALANCER_V2_POOLS {
        assert!(
            matches!(
                builder::build_balancer_stable(VAULT, *broken, &io, Some(1), None).await,
                Err(builder::PoolBuilderError::Spec(
                    SpecRefusal::BalancerBrokenPool
                ))
            ),
            "known-broken pool built: {broken}"
        );
    }
}

/// A token whose `decimals()` answers nothing (no runtime code — self-destructed
/// after its pool was registered — or an empty return) cannot be scaled, so the
/// pool is refused by name instead of surfacing as an RPC decode failure.
#[tokio::test]
async fn build_balancer_weighted_refuses_a_pool_with_an_unreadable_coin() {
    let mut f = FakeRpc::new();
    let pool_id = [0xac; 32];
    f.set(abi::selector(b"getPoolId()"), pool_id_ret(&pool_id));
    f.set(
        abi::selector(b"getPoolTokens(bytes32)"),
        pool_tokens_ret(&[T0, T1], &[U256::from(1_000u64), U256::from(2_000u64)]),
    );
    f.set(
        abi::selector(b"getSwapFeePercentage()"),
        enc(DynSolValue::Uint(U256::from(5_000_000_000_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"getNormalizedWeights()"),
        u256_array_ret(&[500_000_000_000_000_000u64, 500_000_000_000_000_000u64]),
    );
    // `decimals()` returns no data at all → the ABI word cannot decode.
    f.set(abi::selector(b"decimals()"), Vec::new());
    let io = io_with(f);
    assert!(matches!(
        builder::build_balancer_weighted(VAULT, TO, &io, None).await,
        Err(builder::PoolBuilderError::Spec(SpecRefusal::NoCodeCoin))
    ));
}

/// A composable weighted pool (`ManagedPool`) lists its own BPT as a Vault
/// token while `getNormalizedWeights()` covers only the underlying tokens, so
/// the weight vector is one short of the token vector. The weighted state holds
/// one weight per token; the shape refuses by name here instead of panicking in
/// the registration arity check.
#[tokio::test]
async fn build_balancer_weighted_refuses_a_composable_weighted_pool() {
    let mut f = FakeRpc::new();
    let pool_id = [0xab; 32];
    f.set(abi::selector(b"getPoolId()"), pool_id_ret(&pool_id));
    // `TO` is Vault token 0 — the pool's own BPT — and the weights cover the
    // two underlying tokens only.
    f.set(
        abi::selector(b"getPoolTokens(bytes32)"),
        pool_tokens_ret(
            &[TO, T0, T1],
            &[U256::ZERO, U256::from(1_000u64), U256::from(2_000u64)],
        ),
    );
    f.set(
        abi::selector(b"getNormalizedWeights()"),
        u256_array_ret(&[500_000_000_000_000_000u64, 500_000_000_000_000_000u64]),
    );
    let io = io_with(f);
    assert!(matches!(
        builder::build_balancer_weighted(VAULT, TO, &io, None).await,
        Err(builder::PoolBuilderError::Spec(
            SpecRefusal::BalancerComposableWeighted
        ))
    ));
}

/// A rate provider that reverts (a nested pool killed or in recovery mode)
/// leaves the parent pool unable to price its own tokens: a pool fact, so the
/// stable arm refuses it by name rather than surfacing an RPC error.
#[tokio::test]
async fn build_balancer_stable_refuses_a_pool_whose_rate_provider_reverts() {
    let mut f = balancer_stable_fakes(&[T0, T1]);
    // A live (non-sentinel) provider and no `getRate()` response → the read
    // reverts.
    f.set(
        abi::selector(b"getRateProviders()"),
        enc(DynSolValue::Array(vec![
            DynSolValue::Address(alloy::primitives::address!(
                "0x2222222222222222222222222222222222222222"
            )),
            DynSolValue::Address(Address::ZERO),
        ])),
    );
    let io = io_with(f);
    assert!(matches!(
        builder::build_balancer_stable(VAULT, TO, &io, None, None).await,
        Err(builder::PoolBuilderError::Spec(
            SpecRefusal::BalancerRateProviderUnreadable
        ))
    ));
}

// ---------------------------------------------------------------------------
// Curve detection choreography
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
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(coin0),
    );
    f.set_full(
        abi::ICurvePool::coinsCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        addr_word(Address::ZERO),
    );
    f.set_full(
        abi::ICurvePool::balancesCall { i: U256::ZERO }.abi_encode(),
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
    f.set_full(
        abi::ICurvePoolInt128::coinsCall { i: 0i128 }.abi_encode(),
        addr_word(coin0),
    );
    f.set_full(
        abi::ICurvePoolInt128::balancesCall { i: 0i128 }.abi_encode(),
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
        abi::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(1000u64), 256)),
    );
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"admin_fee()"),
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
async fn fetch_curve_balances_loops_count_indices() {
    let mut f = FakeRpc::new();
    for (i, expected) in [(0u8, 111u64), (1, 222), (2, 333)] {
        f.set_full(
            abi::ICurvePool::balancesCall { i: U256::from(i) }.abi_encode(),
            enc(DynSolValue::Uint(U256::from(expected), 256)),
        );
    }
    let io = io_with(f);
    let b = curve_choreography::fetch_curve_balances(&io, TO, 3, None)
        .await
        .unwrap();
    assert_eq!(
        b,
        vec![U256::from(111u64), U256::from(222u64), U256::from(333u64)]
    );
}

#[tokio::test]
async fn fetch_curve_pool_params_propagates_missing_read() {
    // fee() not configured → whole fetch errors (required, not optional).
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"A()"),
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
        abi::selector(b"initial_A()"),
        enc(DynSolValue::Uint(U256::from(1_000u64), 256)),
    );
    f.set(
        abi::selector(b"initial_A_time()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"future_A()"),
        enc(DynSolValue::Uint(U256::from(5_000u64), 256)),
    );
    f.set(
        abi::selector(b"future_A_time()"),
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
        abi::selector(b"future_A()"),
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
        abi::selector(b"fee_gamma()"),
        enc(DynSolValue::Uint(
            U256::from(500_000_000_000_000_000u64),
            256,
        )),
    );
    f.set(
        abi::selector(b"mid_fee()"),
        enc(DynSolValue::Uint(U256::from(3_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"out_fee()"),
        enc(DynSolValue::Uint(U256::from(30_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"gamma()"),
        enc(DynSolValue::Uint(U256::from(145_000_000_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"offpeg_fee_multiplier()"),
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
        abi::selector(b"offpeg_fee_multiplier()"),
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
    f.set(abi::selector(b"isCToken()"), enc(DynSolValue::Bool(true)));
    f.set(abi::selector(b"underlying()"), addr_word(underlying));
    f.set(
        abi::selector(b"decimals()"),
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
    f.set(abi::selector(b"token()"), addr_word(underlying));
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
        abi::ICurveRegistry::get_lp_tokenCall { pool: TO }.abi_encode(),
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
    f.set_full(
        abi::ICurveRegistry::get_lp_tokenCall { pool: TO }.abi_encode(),
        addr_word(lp),
    );
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
    f.set_full(
        abi::ICurveRegistry::is_metaCall { pool: TO }.abi_encode(),
        enc(DynSolValue::Bool(true)),
    );
    f.set_full(
        abi::ICurveRegistry::get_underlying_coinsCall { pool: TO }.abi_encode(),
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
    f.set(abi::selector(b"base_pool()"), addr_word(base));
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
        abi::ICurveRegistry::is_metaCall { pool: TO }.abi_encode(),
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
    f.set_full(
        abi::ICurveRegistry::is_metaCall { pool: TO }.abi_encode(),
        enc(DynSolValue::Bool(true)),
    );
    f.set_full(
        abi::ICurveRegistry::get_underlying_coinsCall { pool: TO }.abi_encode(),
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
// Curve build assembly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn build_curve_pool_assembles_plain_pool_params() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000001");
    let coin1: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000002");

    // Coins (uint256 prototype) + balances.
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(coin0),
    );
    f.set_full(
        abi::ICurvePool::coinsCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        addr_word(coin1),
    );
    // coin index 2 reverts (no response) → discovery stops at 2 coins.
    f.set_full(
        abi::ICurvePool::balancesCall { i: U256::ZERO }.abi_encode(),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set_full(
        abi::ICurvePool::balancesCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        enc(DynSolValue::Uint(U256::from(2_000_000u64), 256)),
    );

    // Pool params.
    f.set(
        abi::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(2000u64), 256)),
    );
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"admin_fee()"),
        enc(DynSolValue::Uint(U256::from(500_000_000u64), 256)),
    );

    // Both coins have 6 decimals → rate_multiplier 10^(36-6)=10^30.
    f.set(
        abi::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(6u64), 256)),
    );

    let io = io_with(f);
    let p = builder::build_curve_pool(1, TO, &[REGISTRY], &io, Some(123))
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
    assert_eq!(p.swap_style, SwapStyle::Standard);
    assert_eq!(p.lending_rate_style, LendingRateStyle::None);
    assert_eq!(p.d_variant, DVariant::Standard);

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
async fn build_curve_pool_rejects_a_known_broken_v1_pool() {
    // A known-broken V1 pool is refused before any RPC: the FakeRpc needs no
    // responses because discovery never runs.
    let io = io_with(FakeRpc::new());
    for broken in builder::BROKEN_CURVE_V1_POOLS {
        assert!(
            matches!(
                builder::build_curve_pool(1, *broken, &[REGISTRY], &io, Some(1)).await,
                Err(builder::PoolBuilderError::Spec(
                    SpecRefusal::CurveBrokenPool
                ))
            ),
            "known-broken pool built: {broken}"
        );
    }
}

#[tokio::test]
async fn build_curve_pool_rejects_fewer_than_two_coins() {
    // Only one coin discovered → Spec error (BrokenPool guard).
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000003");
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(coin0),
    );
    f.set_full(
        abi::ICurvePool::coinsCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        addr_word(Address::ZERO),
    );
    // Params must succeed (they're fetched before the min-tokens guard).
    f.set(
        abi::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"admin_fee()"),
        enc(DynSolValue::Uint(U256::from(0u64), 256)),
    );
    let io = io_with(f);
    assert!(matches!(
        builder::build_curve_pool(1, TO, &[REGISTRY], &io, Some(1)).await,
        Err(builder::PoolBuilderError::Spec(
            SpecRefusal::CurveTooFewCoins
        ))
    ));
}

/// A required read that reverts (`A()`/`fee()`/`admin_fee()`/a coin's
/// `decimals()`) is the contract's own verdict that it does not implement
/// the interface — a broken pool, refused with `Spec`, not a transport `Rpc`.
#[tokio::test]
async fn build_curve_pool_rejects_a_pool_whose_required_getter_reverts() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000007");
    let coin1: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000008");
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(coin0),
    );
    f.set_full(
        abi::ICurvePool::coinsCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        addr_word(coin1),
    );
    f.set_full(
        abi::ICurvePool::balancesCall { i: U256::ZERO }.abi_encode(),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set_full(
        abi::ICurvePool::balancesCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        enc(DynSolValue::Uint(U256::from(2_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    // `fee()` reverts — a decommissioned pool.
    f.set_error(
        abi::selector(b"fee()"),
        ProviderError::ExecutionReverted {
            code: 3,
            message: "execution reverted".to_string(),
        },
    );

    let io = io_with(f);
    assert!(matches!(
        builder::build_curve_pool(1, TO, &[REGISTRY], &io, Some(1)).await,
        Err(builder::PoolBuilderError::Spec(
            SpecRefusal::CurveRequiredReadReverted
        ))
    ));
}

/// A discovered coin with no runtime code (a junk `coins()` return, or a
/// token that self-destructed after its pool registered) cannot be built: its
/// ERC-20 reads will never succeed, so the build refuses with `Spec` rather
/// than dying on the decimals fetch. Deliberately a NON-sentinel address: the
/// all-Es coin of the old Vyper pools is the Curve spelling of the native
/// asset and translates to it instead (see
/// [`build_curve_pool_translates_the_all_es_coin_to_the_native_asset`]).
#[tokio::test]
async fn build_curve_pool_rejects_a_coin_with_no_code() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000006");
    let codeless: Address =
        alloy::primitives::address!("0xaac000000000000000000000000000000000dEaD");

    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(coin0),
    );
    f.set_full(
        abi::ICurvePool::coinsCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        addr_word(codeless),
    );
    f.set_full(
        abi::ICurvePool::balancesCall { i: U256::ZERO }.abi_encode(),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set_full(
        abi::ICurvePool::balancesCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        enc(DynSolValue::Uint(U256::from(2_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"admin_fee()"),
        enc(DynSolValue::Uint(U256::from(0u64), 256)),
    );
    // The codeless coin's `decimals()` returns nothing (no runtime code).
    f.set(abi::selector(b"decimals()"), Vec::new());

    let io = io_with(f);
    assert!(matches!(
        builder::build_curve_pool(1, TO, &[REGISTRY], &io, Some(1)).await,
        Err(builder::PoolBuilderError::Spec(SpecRefusal::NoCodeCoin))
    ));
}

/// The all-Es coin of the old Vyper deployments is the Curve protocol's own
/// spelling of the native asset (ADR-097 D3): the build TRANSLATES it — the
/// intrinsic chain-facts decimals, no ERC-20 read for the coin — instead of
/// refusing it as a codeless coin. The canned `decimals()` answer below is 6
/// for every address that IS read, so a rate multiplier of 10^18 for the
/// all-Es coin proves its decimals came from the chain-facts manifest (18),
/// not from an ERC-20 call.
#[tokio::test]
async fn build_curve_pool_translates_the_all_es_coin_to_the_native_asset() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000021");
    let native_coin: Address =
        alloy::primitives::address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");

    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(coin0),
    );
    f.set_full(
        abi::ICurvePool::coinsCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        addr_word(native_coin),
    );
    f.set_full(
        abi::ICurvePool::balancesCall { i: U256::ZERO }.abi_encode(),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set_full(
        abi::ICurvePool::balancesCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        enc(DynSolValue::Uint(U256::from(2_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"admin_fee()"),
        enc(DynSolValue::Uint(U256::from(0u64), 256)),
    );
    // The ERC-20 answer every READ coin gets: 6 decimals. The native coin
    // must not read it.
    f.set(
        abi::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(6u64), 256)),
    );

    let io = io_with(f);
    let p = builder::build_curve_pool(1, TO, &[REGISTRY], &io, Some(123))
        .await
        .unwrap();

    // The Curve wire view keeps the all-Es coin; the translation is not a
    // rewrite of the protocol's own record.
    assert_eq!(p.tokens, vec![coin0, native_coin]);
    // coin0 read `decimals()` = 6 -> rate 10^(36-6), precision 10^(18-6);
    // the native coin took the chain-facts 18 -> rate 10^(36-18), precision 1.
    let pow = |exp: u32| U256::from(10u64).pow(U256::from(exp));
    assert_eq!(p.rate_multipliers, vec![pow(30), pow(18)]);
    assert_eq!(p.precision_multipliers, vec![pow(12), U256::from(1u64)]);
}

/// A discovered coin whose `decimals()` reverts is the same class of refusal
/// as the no-code sentinel (a coin that cannot serve an ERC-20 read), but a
/// distinct reason: the contract answered, and refused.
#[tokio::test]
async fn build_curve_pool_rejects_a_coin_whose_decimals_read_reverts() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000011");
    let coin1: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000012");

    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(coin0),
    );
    f.set_full(
        abi::ICurvePool::coinsCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        addr_word(coin1),
    );
    f.set_full(
        abi::ICurvePool::balancesCall { i: U256::ZERO }.abi_encode(),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set_full(
        abi::ICurvePool::balancesCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        enc(DynSolValue::Uint(U256::from(2_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"admin_fee()"),
        enc(DynSolValue::Uint(U256::from(0u64), 256)),
    );
    f.set_error(
        abi::selector(b"decimals()"),
        ProviderError::ExecutionReverted {
            code: 3,
            message: "execution reverted".to_string(),
        },
    );

    let io = io_with(f);
    assert!(matches!(
        builder::build_curve_pool(1, TO, &[REGISTRY], &io, Some(1)).await,
        Err(builder::PoolBuilderError::Spec(
            SpecRefusal::CurveCoinDecimalsReverted
        ))
    ));
}

/// A pool that answers `coins(1)` but reverts on `balances(1)` is
/// non-standard: discovery stops with more coins than balances, and the
/// registration layer asserts N-consistency. The build must refuse with the
/// same `Spec` taxonomy as the `BrokenPool` guard rather than assembling
/// params that panic downstream.
#[tokio::test]
async fn build_curve_pool_rejects_a_coin_whose_balance_read_fails() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000004");
    let coin1: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000005");

    // Two coins, but only coin 0's balance answers; coin 1's balance read
    // reverts (no FakeRpc response).
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(coin0),
    );
    f.set_full(
        abi::ICurvePool::coinsCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        addr_word(coin1),
    );
    f.set_full(
        abi::ICurvePool::balancesCall { i: U256::ZERO }.abi_encode(),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );

    // Params must succeed (they're fetched before the guards).
    f.set(
        abi::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(100u64), 256)),
    );
    f.set(
        abi::selector(b"admin_fee()"),
        enc(DynSolValue::Uint(U256::from(0u64), 256)),
    );
    let io = io_with(f);
    assert!(matches!(
        builder::build_curve_pool(1, TO, &[REGISTRY], &io, Some(1)).await,
        Err(builder::PoolBuilderError::Spec(
            SpecRefusal::CurveCoinsBalancesMismatch
        ))
    ));
}

/// The three ERC-20 token reads (`balanceOf`/`allowance`/`totalSupply`) decode
/// a `uint256` return losslessly through the core choreography (SUB-TASK: ERC-20
/// token-balance family). Selector-keyed [`FakeRpc`] responses.
#[tokio::test]
async fn fetch_token_balance_supply_allowance_decode_uint256() {
    // balanceOf(address) returns 1234... (a 64-bit value).
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"balanceOf(address)"),
        U256::from(1_000_000_000u64).to_be_bytes::<32>().to_vec(),
    );
    let io = io_with(f);
    let bal = io
        .read(TO, abi::IERC20::balanceOfCall { account: SV }, None)
        .await
        .expect("balanceOf decodes");
    assert_eq!(bal, U256::from(1_000_000_000u64));

    // totalSupply() returns a large value (must survive lossless: > u64 to
    // prove the u256 path).
    let total: U256 = (U256::from(1u64) << 100) | U256::from(0xDEAD_BEEFu64);
    let mut f2 = FakeRpc::new();
    f2.set(
        abi::selector(b"totalSupply()"),
        total.to_be_bytes::<32>().to_vec(),
    );
    let io2 = io_with(f2);
    let sup = io2
        .read(TO, abi::IERC20::totalSupplyCall {}, None)
        .await
        .expect("totalSupply decodes");
    assert_eq!(sup, total);

    // allowance(owner,spender) returns 4321.
    let mut f3 = FakeRpc::new();
    f3.set(
        abi::selector(b"allowance(address,address)"),
        U256::from(4_321u64).to_be_bytes::<32>().to_vec(),
    );
    let io3 = io_with(f3);
    let allowance = io3
        .read(
            TO,
            abi::IERC20::allowanceCall {
                owner: SV,
                spender: TO,
            },
            None,
        )
        .await
        .expect("allowance decodes");
    assert_eq!(allowance, U256::from(4_321u64));
}

#[tokio::test]
async fn fetch_erc20_uint_field_decodes_a_declared_word() {
    // decimals() dynamic-signature uint read (the name()/symbol() field
    // reads moved to `degenbot_rpc::erc20` with the metadata family).
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(18u64), 256)),
    );
    let io = io_with(f);
    let d = abi::fetch_uint256_field(&io, b"decimals()", &TO, None)
        .await
        .unwrap();
    assert_eq!(d, U256::from(18u64));
}

#[tokio::test]
async fn resolve_v4_identity_orders_currencies_and_derives_hook_flags() {
    // the override (kwargs) path of the core identity resolver —
    // over a NoDb io it must order currency0/1 by ascending address, carry
    // fee/tick_spacing, and derive hook_flags from the hook address (low 16
    // bits).
    let first = alloy::primitives::address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let second = alloy::primitives::address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let io = io_with(FakeRpc::new());
    let overrides = builder::V4PoolBuildOverrides {
        // deliberately reversed — resolver must order currency0 < currency1.
        currency0: Some(second),
        currency1: Some(first),
        fee: Some(3000),
        tick_spacing: Some(60),
        hook_address: Some(SV),
        state_view: Some(SV),
    };
    let id = builder::resolve_v4_identity(1, TO, POOL_ID, &overrides, &io)
        .await
        .unwrap();
    assert_eq!(id.currency0, first);
    assert_eq!(id.currency1, second);
    assert_eq!(id.fee, 3000);
    assert_eq!(id.tick_spacing, 60);
    // The FULL hook address must survive identity resolution (the pool_id
    // round-trip + the ADR-037 hook guards read it) — not just its 16-bit
    // flag mask.
    assert_eq!(id.hook_address, SV);
    assert_eq!(
        builder::derive_hook_flags(id.hook_address),
        u16::from_be_bytes([SV[18], SV[19]])
    );
    assert_eq!(id.state_view, SV);
}

#[tokio::test]
async fn resolve_v4_identity_empty_overrides_is_missing_identity() {
    // over a no-DB io with empty overrides the resolver must
    // degrade to the typed `MissingIdentity` error (never a panic).
    let io = io_with(FakeRpc::new());
    let overrides = builder::V4PoolBuildOverrides::default();
    let err = builder::resolve_v4_identity(1, TO, POOL_ID, &overrides, &io)
        .await
        .unwrap_err();
    assert!(
        matches!(err, builder::PoolBuilderError::MissingIdentity { .. }),
        "expected MissingIdentity, got {err:?}"
    );
}

// ── Construction through the door: route order + get-or-register + typed refusals ──
//
// Every observation below crosses the ONE door verb (`Bot::construct_pool`)
// over a family-tagged `PoolConstruction` request — the same value the route
// dispatches on, so the verdict fold is exercised wherever route behavior is.

use crate::bot_core::pool_builder::construction_door::{ConstructionVerdict, PoolConstruction};
use crate::bot_core::pool_builder::route;
use crate::bot_core::Bot;
use degenbot_pools::RegisteredPoolFamily;
use degenbot_substrate::state_lock::LockSite;

/// The V3 construction fixture: the whole immutable batch a served rung reads
/// (fee 3000 / tick spacing 60 / one-word tick map), so a route test answers
/// from one set instead of hand-building a fake per case.
fn v3_route_fake(factory: Address, token0: Address, token1: Address) -> FakeRpc {
    let mut f = FakeRpc::new();
    f.set(abi::selector(b"factory()"), addr_word(factory));
    f.set(abi::selector(b"token0()"), addr_word(token0));
    f.set(abi::selector(b"token1()"), addr_word(token1));
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(3000u32), 24)),
    );
    f.set(
        abi::selector(b"tickSpacing()"),
        enc(DynSolValue::Int(I256::try_from(60i32).unwrap(), 24)),
    );
    f.set(
        abi::selector(b"slot0()"),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    f.set(
        abi::selector(b"liquidity()"),
        enc(DynSolValue::Uint(U256::from(1_000_000_000u64), 128)),
    );
    f.set(
        abi::selector(b"tickBitmap(int16)"),
        enc(DynSolValue::Uint(U256::ZERO, 256)),
    );
    f
}

#[tokio::test]
async fn construction_door_v3_transient_rpc_failure_is_a_counted_skip() {
    // A transient RPC failure ENDS the route — it is a typed skip, never a
    // bare exception swallowed into the next rung (the retired driver chain's
    // bug re-ran a full build per surviving rung).
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");
    let mut f = FakeRpc::new();
    f.set_error(
        abi::selector(b"factory()"),
        ProviderError::ConnectionFailed {
            message: "blip".to_owned(),
        },
    );
    let io = io_with(f);
    let bot = Bot::new(1);

    let verdict = bot
        .construct_pool(
            &route::ConstructionRoute::generic_only(),
            PoolConstruction::V3 {
                address: pool,
                db: None,
                fetcher: None,
                slot_layout: None,
            },
            &io,
            Some(9_000_000),
        )
        .await;
    assert!(
        matches!(
            verdict,
            ConstructionVerdict::Refused {
                family: RegisteredPoolFamily::V3,
                refusal: route::ConstructionRefusal::Skipped(
                    crate::bot_core::registration_ledger::BuildFailure::Transient(_)
                ),
            }
        ),
        "transient RPC classifies as a counted skip, got {verdict:?}"
    );
}

#[tokio::test]
async fn construction_door_v3_unserved_factory_refuses_loud() {
    // ADR-055 D4: a factory no route rung serves is a family-level stable
    // refusal — the LOUD typed abort, never a silent skip.
    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");
    let mut f = FakeRpc::new();
    f.set(abi::selector(b"factory()"), addr_word(factory));
    let io = io_with(f);
    let bot = Bot::new(1);

    let served: Address = alloy::primitives::address!("0x9999999999999999999999999999999999999999");
    let verdict = bot
        .construct_pool(
            &route::ConstructionRoute {
                factories: vec![served],
                generic: false,
            },
            PoolConstruction::V3 {
                address: pool,
                db: None,
                fetcher: None,
                slot_layout: None,
            },
            &io,
            Some(9_000_000),
        )
        .await;
    assert!(
        matches!(
            verdict,
            ConstructionVerdict::Refused {
                family: RegisteredPoolFamily::V3,
                refusal: route::ConstructionRefusal::UnsupportedFamily { .. },
            }
        ),
        "unserved factory is the loud-abort class, got {verdict:?}"
    );
}

#[tokio::test]
async fn construction_door_v3_matched_rung_builds_and_registers() {
    // The route walk answers a served factory with a CONSTRUCTED, REGISTERED
    // pool: one immutable batch, the seeded build, one registration.
    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");

    let io = io_with(v3_route_fake(factory, tok0, tok1));
    let bot = Bot::new(1);

    let verdict = bot
        .construct_pool(
            &route::ConstructionRoute::generic_only(),
            PoolConstruction::V3 {
                address: pool,
                db: None,
                fetcher: None,
                slot_layout: None,
            },
            &io,
            Some(9_000_000),
        )
        .await;
    assert_eq!(verdict.family(), RegisteredPoolFamily::V3);
    let built = verdict
        .into_result()
        .expect("a served factory constructs and registers");
    let identity = built.built.expect("a fresh build carries its identity");
    assert_eq!(identity.address, pool);
    assert_eq!(identity.pair(), Some((tok0, tok1)));
    assert_eq!(identity.factory, Some(factory));
    // The variant echo is derived core-side from the built factory (the
    // deployment table's DEX name, "uniswap-v3" for an unknown deployment).
    assert_eq!(identity.variant, "uniswap-v3");
    assert_eq!(
        bot.state_arc()
            .read_at(LockSite::Core)
            .registered_pool_by_address(&pool),
        Some((built.pool_id, RegisteredPoolFamily::V3)),
        "the entry registers into session state"
    );
    // The construction seeded the reorg genesis anchor in the SAME write
    // region as the registration: one `before == after` delta at the
    // construction block, so a reorg restore targeting the registration seed
    // finds a known state boundary (an unseeded Sparse pool journals nothing).
    assert_eq!(
        bot.state_arc()
            .read_at(LockSite::Core)
            .pool_journal_len(built.pool_id),
        Some(1),
        "the genesis anchor is the journal's only delta"
    );
    assert_eq!(
        bot.state_arc()
            .read_at(LockSite::Core)
            .newest_journal_block(built.pool_id),
        Some(9_000_000),
        "the anchor lands at the construction block"
    );

    // Get-or-register: a second request answers from the registry with NO
    // I/O — an all-erroring transport still succeeds with the same pool id.
    let io_down = io_with(FakeRpc::new());
    let again = bot
        .construct_pool(
            &route::ConstructionRoute::generic_only(),
            PoolConstruction::V3 {
                address: pool,
                db: None,
                fetcher: None,
                slot_layout: None,
            },
            &io_down,
            Some(9_000_000),
        )
        .await
        .into_result()
        .expect("a registered pool answers from the registry");
    assert_eq!(again.pool_id, built.pool_id);
    assert!(again.built.is_none(), "the GET half re-derives no identity");
    // A reused registration is never re-seeded: the first registrant's anchor
    // is the pool's state boundary, and a second delta at the same block would
    // only lengthen the journal.
    assert_eq!(
        bot.state_arc()
            .read_at(LockSite::Core)
            .pool_journal_len(built.pool_id),
        Some(1),
        "the reuse path appends no second anchor"
    );
}

#[tokio::test]
async fn construction_door_v3_seeds_the_anchor_only_from_a_positive_block() {
    // The seed mirrors the driver's `block is not None and block > 0` gate: a
    // construction with no block (or at genesis) has nothing to anchor to, so
    // the journal stays empty instead of carrying a delta at a made-up block.
    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let io = io_with(v3_route_fake(factory, tok0, tok1));
    let bot = Bot::new(1);

    for (pool, block) in [
        (
            alloy::primitives::address!("0x3333333333333333333333333333333333333333"),
            None,
        ),
        (
            alloy::primitives::address!("0x4444444444444444444444444444444444444444"),
            Some(0),
        ),
    ] {
        let built = bot
            .construct_pool(
                &route::ConstructionRoute::generic_only(),
                PoolConstruction::V3 {
                    address: pool,
                    db: None,
                    fetcher: None,
                    slot_layout: None,
                },
                &io,
                block,
            )
            .await
            .into_result()
            .expect("a served factory constructs and registers without an anchor");
        assert_eq!(
            bot.state_arc()
                .read_at(LockSite::Core)
                .pool_journal_len(built.pool_id),
            Some(0),
            "block {block:?} seeds no anchor"
        );
    }
}

#[tokio::test]
async fn construction_identity_parity_guard_reads_registered_identity_and_refuses_a_wrong_echo() {
    // The core-side identity-parity guard against REAL registered state: build
    // + register a V3 pool, then read the registered identity back and prove
    // (a) the minted echo matches and (b) a wrong (swapped) echo is refused —
    // the address-keyed red path, exercised where the identity is minted.
    use crate::bot_core::pool_builder::parity;

    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");

    let mut f = FakeRpc::new();
    f.set(abi::selector(b"factory()"), addr_word(factory));
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(3000u32), 24)),
    );
    f.set(
        abi::selector(b"tickSpacing()"),
        enc(DynSolValue::Int(I256::try_from(60i32).unwrap(), 24)),
    );
    f.set(
        abi::selector(b"slot0()"),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    f.set(
        abi::selector(b"liquidity()"),
        enc(DynSolValue::Uint(U256::from(1_000_000_000u64), 128)),
    );
    f.set(
        abi::selector(b"tickBitmap(int16)"),
        enc(DynSolValue::Uint(U256::ZERO, 256)),
    );
    let io = io_with(f);
    let bot = Bot::new(1);

    let built = bot
        .construct_pool(
            &route::ConstructionRoute::generic_only(),
            PoolConstruction::V3 {
                address: pool,
                db: None,
                fetcher: None,
                slot_layout: None,
            },
            &io,
            Some(9_000_000),
        )
        .await
        .into_result()
        .expect("a served factory constructs and registers");

    let registered = parity::registered_address_identity(&bot, built.pool_id)
        .expect("the registered pool carries its identity");

    // Happy path: the minted echo equals the registered identity.
    let minted = parity::AddressIdentity {
        token0: tok0,
        token1: tok1,
        address: pool,
    };
    assert!(parity::verify_address_identity(minted, registered).is_ok());

    // Fault injection: a swapped minted identity must be refused LOUD.
    let wrong = parity::AddressIdentity {
        token0: tok1,
        token1: tok0,
        address: pool,
    };
    let err = parity::verify_address_identity(wrong, registered)
        .expect_err("a divergent minted identity is refused core-side");
    match err {
        parity::IdentityDivergence::MintedVsRegistered { address, .. } => {
            assert_eq!(address, pool);
        }
        other @ parity::IdentityDivergence::Create2(_) => {
            panic!("a minted-vs-registered divergence: {other:?}")
        }
    }
}

// ── The construction door (ADR-078 D2 / ADR-085 D1): every family through the one request ──
//
// `Bot::construct_pool` hands its family-tagged `PoolConstruction` straight to
// `route::construct_pool` — route.rs keeps every piece of depth
// (get-or-register, identity resolution, the route walk, the race fold,
// refusal classification). Each family's request variant is exercised through
// the door on its own fixture: one request definition, one route arm.

#[test]
fn construction_door_requests_name_their_family() {
    assert_eq!(
        PoolConstruction::V2 { address: TO }.family(),
        RegisteredPoolFamily::V2
    );
    assert_eq!(
        PoolConstruction::V3 {
            address: TO,
            db: None,
            fetcher: None,
            slot_layout: None,
        }
        .family(),
        RegisteredPoolFamily::V3
    );
    assert_eq!(
        PoolConstruction::V4 {
            pool_manager: TO,
            pool_id: POOL_ID,
            overrides: builder::V4PoolBuildOverrides::default(),
            db: None,
        }
        .family(),
        RegisteredPoolFamily::V4
    );
}

#[tokio::test]
async fn construction_door_v2_constructs_registers_and_reuses() {
    use degenbot_uniswap::create2::compute_v2_address;
    use degenbot_uniswap::dex_identity::UNISWAP_V2;

    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool = compute_v2_address(UNISWAP_V2.factory, tok0, tok1, UNISWAP_V2.init_hash);

    let mut f = FakeRpc::new();
    f.set(abi::selector(b"factory()"), addr_word(UNISWAP_V2.factory));
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"getReserves()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(123_456_789u64), 112),
            DynSolValue::Uint(U256::from(987_654_321u64), 112),
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    let io = io_with(f);

    let plan = route::ConstructionRoute::generic_only();
    let bot = Bot::new(1);
    let door = bot
        .construct_pool(
            &plan,
            PoolConstruction::V2 { address: pool },
            &io,
            Some(18_000_000),
        )
        .await;

    assert_eq!(door.family(), RegisteredPoolFamily::V2);
    let built = door
        .clone()
        .into_result()
        .expect("a constructed V2 verdict carries its pool");
    assert_eq!(
        bot.state_arc()
            .read_at(LockSite::Core)
            .registered_pool_by_address(&pool),
        Some((built.pool_id, RegisteredPoolFamily::V2)),
        "the door registers into session state"
    );

    // Get-or-register through the door: the registry GET answers with NO I/O
    // (an all-erroring transport still succeeds with the same pool id).
    let io_down = io_with(FakeRpc::new());
    let again = bot
        .construct_pool(
            &plan,
            PoolConstruction::V2 { address: pool },
            &io_down,
            Some(18_000_000),
        )
        .await;
    assert_eq!(again.pool_id(), door.pool_id());
    assert!(
        matches!(again, ConstructionVerdict::Constructed { pool, .. } if pool.built.is_none()),
        "the door's GET half re-derives no identity"
    );
}

/// The V4 door fixture: the `StateView` reads a served `(manager, id)` pair
/// builds through.
fn v4_door_fake() -> FakeRpc {
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"getSlot0(bytes32)"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(1u128 << 96), 160),
            DynSolValue::Int(I256::try_from(5i32).unwrap(), 24),
            DynSolValue::Uint(U256::from(300u32), 24),
            DynSolValue::Uint(U256::from(50u32), 24),
        ])),
    );
    f.set(
        abi::selector(b"getLiquidity(bytes32)"),
        enc(DynSolValue::Uint(U256::from(2_000_000_000u64), 128)),
    );
    f.set(
        abi::selector(b"getTickBitmap(bytes32,int16)"),
        enc(DynSolValue::Uint(U256::ZERO, 256)),
    );
    f
}

/// The caller-supplied V4 identity overrides filling every gap (the shape the
/// DB-resolution fallback leaves to the caller).
fn v4_full_overrides(pm: Address) -> builder::V4PoolBuildOverrides {
    builder::V4PoolBuildOverrides {
        currency0: Some(TO),
        currency1: Some(SV),
        fee: Some(500),
        tick_spacing: Some(1),
        hook_address: Some(Address::ZERO),
        state_view: Some(pm),
    }
}

#[tokio::test]
async fn construction_door_v4_constructs_registers_and_reuses() {
    let pm: Address = alloy::primitives::address!("0x3333333333333333333333333333333333333333");
    let pid: [u8; 32] = [0xcd; 32];
    let io = io_with(v4_door_fake());

    let plan = route::ConstructionRoute::generic_only();
    let bot = Bot::new(1);
    let door = bot
        .construct_pool(
            &plan,
            PoolConstruction::V4 {
                pool_manager: pm,
                pool_id: pid,
                overrides: v4_full_overrides(pm),
                db: None,
            },
            &io,
            Some(11_000_000),
        )
        .await;

    assert_eq!(door.family(), RegisteredPoolFamily::V4);
    let built = door
        .clone()
        .into_result()
        .expect("a constructed V4 verdict carries its pool");
    let identity = built.built.expect("a fresh V4 build carries its identity");
    assert_eq!(identity.v4_pool_id, Some(pid));
    assert_eq!(identity.address, pm);
    assert_eq!(
        bot.state_arc()
            .read_at(LockSite::Core)
            .try_registered(pm, &pid)
            .map(|registered| registered.pool_id),
        Some(built.pool_id),
    );

    // The door's `(manager, pool_id)`-keyed get half answers without I/O.
    let io_down = io_with(FakeRpc::new());
    let again = bot
        .construct_pool(
            &plan,
            PoolConstruction::V4 {
                pool_manager: pm,
                pool_id: pid,
                overrides: v4_full_overrides(pm),
                db: None,
            },
            &io_down,
            Some(11_000_000),
        )
        .await;
    assert_eq!(again.pool_id(), door.pool_id());
    assert!(
        matches!(again, ConstructionVerdict::Constructed { pool, .. } if pool.built.is_none()),
        "the keyed GET half re-derives no identity"
    );
}

#[tokio::test]
async fn construction_door_v4_registers_the_state_view_and_tick_word_source() {
    // The door owns the StateView registration (the solver-state gate's
    // registry) and the sparse-backfill tick-word source — both
    // driver-side responsibilities under the retired verb.
    let pm: Address = alloy::primitives::address!("0x3333333333333333333333333333333333333333");
    let pid: [u8; 32] = [0xcd; 32];
    let io = io_with(v4_door_fake());
    let bot = Bot::new(1);

    let verdict = bot
        .construct_pool(
            &route::ConstructionRoute::generic_only(),
            PoolConstruction::V4 {
                pool_manager: pm,
                pool_id: pid,
                overrides: v4_full_overrides(pm),
                db: None,
            },
            &io,
            Some(11_000_000),
        )
        .await;
    let pool_id = verdict.pool_id().expect("the V4 arm constructs");

    let session = bot.state_arc();
    let core = session.read_at(LockSite::Core);
    assert_eq!(
        core.state_view_for_pool(pool_id),
        Some(pm),
        "the door registers the StateView for the solver-state gate"
    );
    let fetcher_attached = core
        .pool_slice::<degenbot_pools::V4PoolState>(pool_id)
        .is_some_and(|state| state.fetcher.is_some());
    assert!(fetcher_attached, "the door attaches the tick-word source");
}

// --- The construction door's family arms (ADR-085 D1) ---

#[tokio::test]
async fn construction_door_curve_constructs_registers_and_reuses() {
    let mut f = FakeRpc::new();
    let coin0: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000001");
    let coin1: Address = alloy::primitives::address!("0xaac0000000000000000000000000000000000002");
    f.set_full(
        abi::ICurvePool::coinsCall { i: U256::ZERO }.abi_encode(),
        addr_word(coin0),
    );
    f.set_full(
        abi::ICurvePool::coinsCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        addr_word(coin1),
    );
    f.set_full(
        abi::ICurvePool::balancesCall { i: U256::ZERO }.abi_encode(),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set_full(
        abi::ICurvePool::balancesCall {
            i: U256::from(1u64),
        }
        .abi_encode(),
        enc(DynSolValue::Uint(U256::from(2_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"A()"),
        enc(DynSolValue::Uint(U256::from(2000u64), 256)),
    );
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(1_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"admin_fee()"),
        enc(DynSolValue::Uint(U256::from(500_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(6u64), 256)),
    );
    let io = io_with(f);
    let bot = Bot::new(1);
    let plan = route::ConstructionRoute::generic_only();
    let request = || PoolConstruction::Curve {
        address: TO,
        registries: vec![REGISTRY],
    };
    let first = bot
        .construct_pool(&plan, request(), &io, Some(123))
        .await
        .into_result()
        .expect("the curve arm constructs and registers");
    let built = first.built.expect("a fresh build carries its identity");
    assert_eq!(built.tokens, vec![coin0, coin1]);
    assert_eq!(built.pair(), Some((coin0, coin1)));
    assert_eq!(built.variant, "stableswap");
    // Get-or-register: the second attempt answers the winner's id with no
    // rebuild (`built: None`) — the door's contract, never the direct
    // entries' rebuild-then-refuse race.
    let second = bot
        .construct_pool(&plan, request(), &io, Some(123))
        .await
        .into_result()
        .expect("the door reuses the registered pool");
    assert_eq!(second.pool_id, first.pool_id);
    assert!(second.built.is_none());
}

// --- Every remaining family through the Construction door (ADR-085 D1) ---

#[tokio::test]
async fn construction_door_aerodrome_v2_constructs_registers_and_reuses() {
    // The fixture is the on-chain Base AERO/WETH volatile pair, so the
    // EIP-1167 CREATE2 verification must agree with the shipped deployment row.
    let factory: Address =
        alloy::primitives::address!("0x420DD381b31aEf6683db6B902084cB0FFECe40Da");
    let tok0: Address = alloy::primitives::address!("0x4200000000000000000000000000000000000006");
    let tok1: Address = alloy::primitives::address!("0x940181a94a35a4569e4529a3cdfb74e38fd98631");
    let pool: Address = alloy::primitives::address!("0x7f670f78B17dEC44d5Ef68a48740b6f8849cc2e6");

    let mut f = FakeRpc::new();
    f.set(abi::selector(b"factory()"), addr_word(factory));
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"getReserves()"),
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(123_456_789u64), 112),
            DynSolValue::Uint(U256::from(987_654_321u64), 112),
            DynSolValue::Uint(U256::ZERO, 32),
        ])),
    );
    f.set(abi::selector(b"stable()"), enc(DynSolValue::Bool(false)));
    f.set(
        abi::selector(b"getFee(address,bool)"),
        enc(DynSolValue::Uint(U256::from(30u64), 256)),
    );
    f.set(
        abi::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(18), 256)),
    );
    let io = io_with(f);

    let plan = route::ConstructionRoute::generic_only();
    let bot = Bot::new(8453);
    let door = bot
        .construct_pool(
            &plan,
            PoolConstruction::AerodromeV2 { address: pool },
            &io,
            Some(123),
        )
        .await;

    assert_eq!(door.family(), RegisteredPoolFamily::AerodromeV2);
    let built = door
        .clone()
        .into_result()
        .expect("the Aerodrome arm constructs and registers");
    let identity = built.built.expect("a fresh build carries its identity");
    assert_eq!(identity.tokens, vec![tok0, tok1]);
    assert_eq!(identity.variant, "aerodrome");
    assert_eq!(identity.factory, Some(factory));
    assert_eq!(
        bot.state_arc()
            .read_at(LockSite::Core)
            .registered_pool_by_address(&pool),
        Some((built.pool_id, RegisteredPoolFamily::AerodromeV2)),
    );

    // Get-or-register: the second attempt answers the winner's id with no
    // rebuild (`built: None`).
    let io_down = io_with(FakeRpc::new());
    let again = bot
        .construct_pool(
            &plan,
            PoolConstruction::AerodromeV2 { address: pool },
            &io_down,
            Some(123),
        )
        .await;
    assert_eq!(again.pool_id(), door.pool_id());
    assert!(
        matches!(again, ConstructionVerdict::Constructed { pool, .. } if pool.built.is_none()),
        "the door's GET half re-derives no identity"
    );
}

#[tokio::test]
async fn construction_door_balancer_weighted_constructs_registers_and_reuses() {
    let pool_id_bytes = [0xcd; 32];
    let mut f = FakeRpc::new();
    f.set(abi::selector(b"getPoolId()"), pool_id_ret(&pool_id_bytes));
    f.set(
        abi::selector(b"getPoolTokens(bytes32)"),
        pool_tokens_ret(&[T0, T1], &[U256::from(1_000u64), U256::from(2_000u64)]),
    );
    f.set(
        abi::selector(b"getSwapFeePercentage()"),
        enc(DynSolValue::Uint(U256::from(5_000_000_000_000_000u64), 256)),
    );
    f.set(
        abi::selector(b"getNormalizedWeights()"),
        u256_array_ret(&[500_000_000_000_000_000u64, 500_000_000_000_000_000u64]),
    );
    f.set(
        abi::selector(b"decimals()"),
        enc(DynSolValue::Uint(U256::from(18), 256)),
    );
    let io = io_with(f);

    let plan = route::ConstructionRoute::generic_only();
    let bot = Bot::new(1);
    let door = bot
        .construct_pool(
            &plan,
            PoolConstruction::BalancerWeighted {
                vault: VAULT,
                address: TO,
            },
            &io,
            Some(123),
        )
        .await;

    assert_eq!(door.family(), RegisteredPoolFamily::BalancerWeighted);
    let built = door
        .clone()
        .into_result()
        .expect("the weighted arm constructs and registers");
    let identity = built.built.expect("a fresh build carries its identity");
    assert_eq!(identity.tokens, vec![T0, T1]);
    assert_eq!(identity.variant, "balancer_weighted");
    assert_eq!(
        bot.state_arc()
            .read_at(LockSite::Core)
            .registered_pool_by_address(&TO),
        Some((built.pool_id, RegisteredPoolFamily::BalancerWeighted)),
    );

    // Get-or-register: the second attempt answers the winner's id with no
    // rebuild (`built: None`).
    let io_down = io_with(FakeRpc::new());
    let again = bot
        .construct_pool(
            &plan,
            PoolConstruction::BalancerWeighted {
                vault: VAULT,
                address: TO,
            },
            &io_down,
            Some(123),
        )
        .await;
    assert_eq!(again.pool_id(), door.pool_id());
    assert!(
        matches!(again, ConstructionVerdict::Constructed { pool, .. } if pool.built.is_none()),
        "the door's GET half re-derives no identity"
    );
}

#[tokio::test]
async fn construction_door_balancer_stable_constructs_registers_and_reuses() {
    let io = io_with(balancer_stable_fakes(&[T0, T1]));
    let plan = route::ConstructionRoute::generic_only();
    let bot = Bot::new(1);
    let request = || PoolConstruction::BalancerStable {
        vault: VAULT,
        address: TO,
        invariant_version: Some(1),
    };
    let door = bot.construct_pool(&plan, request(), &io, Some(123)).await;

    assert_eq!(door.family(), RegisteredPoolFamily::BalancerStable);
    let built = door
        .clone()
        .into_result()
        .expect("the stable arm constructs and registers");
    let identity = built.built.expect("a fresh build carries its identity");
    assert_eq!(identity.tokens, vec![T0, T1]);
    assert_eq!(identity.variant, "balancer_stable");
    // The override rides the request variant and outranks the shape-derived
    // version (the shape here would derive V2).
    assert_eq!(
        bot.state_arc()
            .read_at(LockSite::Core)
            .pool_slice::<degenbot_pools::BalancerStablePoolIdentity>(built.pool_id)
            .map(|identity| identity.invariant_version),
        Some(1),
    );

    // Get-or-register: the second attempt answers the winner's id with no
    // rebuild (`built: None`).
    let io_down = io_with(FakeRpc::new());
    let again = bot
        .construct_pool(&plan, request(), &io_down, Some(123))
        .await;
    assert_eq!(again.pool_id(), door.pool_id());
    assert!(
        matches!(again, ConstructionVerdict::Constructed { pool, .. } if pool.built.is_none()),
        "the door's GET half re-derives no identity"
    );
}

/// A tick-word source stub answering nothing — the V3 family-inputs test only
/// asserts WHICH fetcher the door stored, never a fetch.
#[derive(Debug)]
struct NeverWordSource;

impl degenbot_pools::TickWordSource for NeverWordSource {
    fn fetch_tick_word(
        &self,
        _key: &degenbot_pools::TickWordKey,
        _word: i32,
        _block: u64,
    ) -> Result<Option<degenbot_pools::FetchedTickWord>, degenbot_pools::TickWordFetchError> {
        Err(degenbot_pools::TickWordFetchError::FetchFailed)
    }
}

#[tokio::test]
async fn construction_door_v3_rides_its_family_only_inputs() {
    // The V3 request variant carries the handles only the V3 arm reads: the
    // DB tick-map (a hit seeds `Tracked`), the caller's tick-word fetcher, and
    // the fork slot-layout override. All three must land on the registered
    // pool — no side request value survives the dispatch.
    use degenbot_db::snapshot::{LiquidityAtTick, LiquidityMap};

    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");

    let mut f = FakeRpc::new();
    f.set(abi::selector(b"factory()"), addr_word(factory));
    f.set(abi::selector(b"token0()"), addr_word(tok0));
    f.set(abi::selector(b"token1()"), addr_word(tok1));
    f.set(
        abi::selector(b"fee()"),
        enc(DynSolValue::Uint(U256::from(3000u32), 24)),
    );
    f.set(
        abi::selector(b"tickSpacing()"),
        enc(DynSolValue::Int(I256::try_from(60i32).unwrap(), 24)),
    );
    f.set(
        abi::selector(b"slot0()"),
        slot0_ret(U256::from(1u128 << 96), 0),
    );
    f.set(
        abi::selector(b"liquidity()"),
        enc(DynSolValue::Uint(U256::from(1_000_000_000u64), 128)),
    );
    // The chain-truth intake gate reads word 0 from chain: bit 1 (tick 60 at
    // spacing 60) must agree with the DB row below.
    f.set(
        abi::selector(b"tickBitmap(int16)"),
        enc(DynSolValue::Uint(U256::from(1u64) << 1, 256)),
    );
    let io = io_with(f);

    let db = FakeTickMapDb(
        LiquidityMap {
            tick_data: HashMap::from([(
                60,
                LiquidityAtTick {
                    liquidity_gross: U256::from(100u64),
                    liquidity_net: 100i128,
                },
            )]),
        },
        None,
    );
    let fetcher: std::sync::Arc<dyn degenbot_pools::TickWordSource> =
        std::sync::Arc::new(NeverWordSource);

    let bot = Bot::new(1);
    let verdict = bot
        .construct_pool(
            &route::ConstructionRoute::generic_only(),
            PoolConstruction::V3 {
                address: pool,
                db: Some(&db),
                fetcher: Some(std::sync::Arc::clone(&fetcher)),
                slot_layout: Some(degenbot_pools::ConcentratedLiquiditySlotLayout::PancakeV3),
            },
            &io,
            Some(9_000_000),
        )
        .await;
    let pool_id = verdict
        .pool_id()
        .expect("the V3 arm constructs with its family inputs");

    let session = bot.state_arc();
    let core = session.read_at(LockSite::Core);
    let state = core
        .pool_slice::<degenbot_pools::V3PoolState>(pool_id)
        .expect("the registered V3 pool carries its state");
    assert!(
        state.snapshot_seed.is_some(),
        "the DB hit seeds a Tracked pool"
    );
    assert_eq!(state.tick_data.len(), 1, "the DB rows seed the tick map");
    assert!(
        std::sync::Arc::ptr_eq(
            state.fetcher.as_ref().expect("the door stores a fetcher"),
            &fetcher
        ),
        "the caller's tick-word fetcher is the stored one"
    );
    assert_eq!(
        core.pool_slice::<degenbot_pools::V3PoolIdentity>(pool_id)
            .map(|identity| identity.slot_layout),
        Some(degenbot_pools::ConcentratedLiquiditySlotLayout::PancakeV3),
        "the slot-layout override lands on the registered identity"
    );
}

#[tokio::test]
async fn construction_door_folds_an_admission_race_to_the_winners_pool_id() {
    // Two concurrent constructions of one pool both pass the
    // get-or-register pre-check before either registers (the fake yields at
    // every call): the loser's write-door `AlreadyRegistered` is a race
    // artifact, so it folds to the winner's pool id — never a second pool,
    // never a refusal. Only the winner seeds the genesis anchor.
    let factory: Address =
        alloy::primitives::address!("0x1111111111111111111111111111111111111111");
    let tok0: Address = alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let tok1: Address = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    let pool: Address = alloy::primitives::address!("0x2222222222222222222222222222222222222222");

    let mut f = v3_route_fake(factory, tok0, tok1);
    f.set_yielding();
    let io = io_with(f);
    let bot = Bot::new(1);
    let plan = route::ConstructionRoute::generic_only();
    let request = || PoolConstruction::V3 {
        address: pool,
        db: None,
        fetcher: None,
        slot_layout: None,
    };
    let (first, second) = tokio::join!(
        bot.construct_pool(&plan, request(), &io, Some(9_000_000)),
        bot.construct_pool(&plan, request(), &io, Some(9_000_000)),
    );
    for verdict in [&first, &second] {
        assert!(
            matches!(
                verdict,
                ConstructionVerdict::Constructed {
                    family: RegisteredPoolFamily::V3,
                    ..
                }
            ),
            "the race fold is never a refusal, got {verdict:?}"
        );
    }
    assert_eq!(
        first.pool_id(),
        second.pool_id(),
        "the loser folds to the winner's pool id"
    );
    let winner = first.pool_id().expect("the winner carries its pool id");
    assert_eq!(
        bot.state_arc()
            .read_at(LockSite::Core)
            .pool_journal_len(winner),
        Some(1),
        "the loser's fold seeds no second anchor"
    );
}

// --- The Tick-word source (GLOSSARY): the live-miss backfill is core-owned ---

#[test]
fn an_unbound_word_source_reads_a_checked_empty_word() {
    // An all-zero bitmap is a KNOWN word with no ticks — per the trait
    // contract the source reports it as `Ok(None)` (a known-empty word the
    // caller records in `known_bitmap_words` and stops re-fetching), never
    // as a failure.
    let mut f = FakeRpc::new();
    f.set(
        abi::selector(b"tickBitmap(int16)"),
        enc(DynSolValue::Uint(U256::ZERO, 256)),
    );
    let io = io_with(f);
    let source = super::tick_source::attached(&io);
    let key = degenbot_pools::TickWordKey::V3 {
        address: TO,
        tick_spacing: 1,
    };
    let fetched = source
        .fetch_tick_word(&key, 0, 100)
        .expect("the source reads through the construction io seam");
    assert!(
        fetched.is_none(),
        "an all-zero bitmap is the documented known-empty verdict"
    );
}

#[test]
fn an_unbound_word_source_refuses_an_overrunning_word() {
    // The word -> tick mapping (`(word << 8) * spacing`) is checked before any
    // RPC: an overrunning word is a typed refusal, never a wrapped bogus tick.
    let io = io_with(FakeRpc::new());
    let source = super::tick_source::attached(&io);
    let key = degenbot_pools::TickWordKey::V3 {
        address: TO,
        tick_spacing: 1,
    };
    assert_eq!(
        source.fetch_tick_word(&key, i32::MAX, 1).unwrap_err(),
        degenbot_pools::TickWordFetchError::OutOfRange
    );
}
