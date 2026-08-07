//! Tier-2 behavioral dual-driver parity — ERC-20 metadata resolution
//! (VK3YDM-S6).
//!
//! The VK3YDM-S2 port moved the ERC-20 *assembly* (DB-first metadata lookup,
//! on-chain read, UNKNOWN fallback, write-back, `BotState` registration) into
//! the Rust core as `build_erc20_metadata` + `PyBot.build_erc20_token`. That is
//! a capability crossing the FFI boundary, so AGENTS.md's Tier-2 rule applies:
//! a fixture must be driven through BOTH consumers and agree on the result.
//!
//! This seed asserts the **direct FFI-seam-lossless claim** for ERC-20: the
//! **same** canonical metadata fixture, driven through the **Rust consumer**
//! (`build_erc20_metadata` over a `ConstructionIo` with a fake `RpcConstruction`
//! serving `name()`/`symbol()`/`decimals()` + non-empty `get_code`, a `NoDb`
//! DB adapter so the DB-first lookup misses), resolves the **same**
//! `(name, symbol, decimals)` as the **Python consumer** (`PyBot.build_erc20_token`
//! over an offline alloy provider in `tests/standalone_parity/test_erc20_dual_driver.py`).
//!
//! ## Fixture (single source of truth — HRT356)
//!
//! The plain canonical metadata is loaded from the SHARED file
//! `tests/standalone_parity/fixtures/erc20_build.json`, which the Python twin
//! ALSO loads. Both sides ABI-encode the SAME `name`/`symbol`/`decimals` from
//! `fixture` into their provider doubles and assert the resolved output equals
//! `expected`. A fixture edit that drifts the metadata fails BOTH sides
//! mechanically (the shared-fixture contract that replaced copied constants).

#![allow(clippy::panic_in_result_fn, clippy::doc_markdown)]

use std::collections::HashMap;

use alloy::primitives::{Address, Bytes, U256};
use async_trait::async_trait;
use degenbot::bot_core::construction_io::{ConstructionIo, NoDb, RpcConstruction};
use degenbot::build_erc20_metadata;
use degenbot_core::errors::ProviderError;

/// Path to the shared ERC-20 fixture (loaded by both this Rust test and the
/// Python dual-driver test — the single source of truth).
const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/standalone_parity/fixtures/erc20_build.json"
);

/// The shared ERC-20 fixture, deserialized once per test.
#[derive(Debug, serde::Deserialize)]
struct Erc20FixtureFile {
    fixture: Erc20FixtureInputs,
    expected: Erc20Expected,
}

#[derive(Debug, serde::Deserialize)]
struct Erc20FixtureInputs {
    token: String,
    name: String,
    symbol: String,
    decimals: u64,
}

#[derive(Debug, serde::Deserialize)]
struct Erc20Expected {
    name: String,
    symbol: String,
    decimals: u64,
}

/// The 4-byte function selectors for the canonical no-arg metadata reads.
const SEL_NAME: [u8; 4] = [0x06, 0xfd, 0xde, 0x03];
const SEL_SYMBOL: [u8; 4] = [0x95, 0xd8, 0x9b, 0x41];
const SEL_DECIMALS: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];

/// ABI-encode a top-level `string` return: `[offset=0x20][len][data(32-padded)]`.
fn abi_string(s: &str) -> Vec<u8> {
    let n = s.len();
    let mut v = U256::from(32u64).to_be_bytes::<32>().to_vec();
    v.extend_from_slice(&U256::from(n).to_be_bytes::<32>());
    v.extend_from_slice(s.as_bytes());
    v.resize(v.len() + ((32 - (n % 32)) % 32), 0u8);
    v
}

/// ABI-encode a single `uint256` return.
fn abi_uint(v: u64) -> Vec<u8> {
    U256::from(v).to_be_bytes::<32>().to_vec()
}

/// A `RpcConstruction` double returning the fixture-encoded metadata reads and
/// non-empty code (mirrors the degenbot-bot unit-test `FakeRpc`, but local so
/// the parity test needs no private module access).
struct FakeRpc {
    responses: HashMap<[u8; 4], Vec<u8>>,
    code: Bytes,
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

#[tokio::test]
async fn erc20_metadata_dual_driver_matches_python() {
    let text = std::fs::read_to_string(FIXTURE_PATH).expect("read shared erc20_build.json fixture");
    let fx: Erc20FixtureFile =
        serde_json::from_str(&text).expect("parse shared erc20_build.json fixture");

    let token: Address = fx.fixture.token.parse().expect("valid token address");

    // A `NoDb` DB adapter so the DB-first lookup misses; the RPC double serves
    // only the on-chain metadata (with non-empty code to pass the guard).
    let mut fake = FakeRpc {
        responses: HashMap::new(),
        code: Bytes::from_static(&[0x60, 0x80]),
    };
    fake.responses
        .insert(SEL_NAME, abi_string(&fx.fixture.name));
    fake.responses
        .insert(SEL_SYMBOL, abi_string(&fx.fixture.symbol));
    fake.responses
        .insert(SEL_DECIMALS, abi_uint(fx.fixture.decimals));
    let io = ConstructionIo::new(std::sync::Arc::new(NoDb), std::sync::Arc::new(fake));

    let (name, symbol, decimals) = build_erc20_metadata(&io, 1, token, None)
        .await
        .expect("build_erc20_metadata over the fixture RPC");
    assert_eq!(name, fx.expected.name, "name must match the shared fixture");
    assert_eq!(
        symbol, fx.expected.symbol,
        "symbol must match the shared fixture"
    );
    assert_eq!(
        u64::from(decimals),
        fx.expected.decimals,
        "decimals must match the shared fixture"
    );
}
