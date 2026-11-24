//! Registration admission's offline fixtures, the two-adapter parity suite, the
//! R1 equivalence pin, the outcome parity table, and the loud-abort arms.
//!
//! Every fixture is offline: a `FakeRpc` answers the construction reads per
//! `(contract, selector)`, `NoDb`/`FixtureDb` answer the DB half, and a real
//! `Bot` + `EngineStages` pair owns the state and the registry, and the verify
//! home (`VerifyLifecycle`) owns the claim table. The fixture holds no session
//! driver: the admission's path writes and reads cross the stage seam alone.
//! Nothing here needs a node.

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use alloy::dyn_abi::DynSolValue;
use alloy::primitives::{address, Address, Bytes, I256, U256};
use async_trait::async_trait;
use degenbot_core::errors::ProviderError;
use degenbot_db::error::DbError;
use degenbot_db::rows::{
    Erc20TokenRow, ExchangeRow, LiquidityPoolRow, LiquidityPositionRow,
    ManagedPoolLiquidityPositionRow, PoolKindRow, PoolManagerRow, V4PoolRow,
};
use degenbot_pools::BuildFailure;
use degenbot_rpc::abi;
use degenbot_substrate::state_lock::LockSite;
use degenbot_uniswap::create2::compute_v2_address;
use degenbot_uniswap::dex_identity;
use hashbrown::HashMap;

use super::{run_crawl_unit, CrawlUnit, CrawlUnitHost};
use crate::arb_engine::crawl::admission::{
    AdmissionSettings, CrawlAbort, DiscoveredHop, RegistrationAdmission,
};
use crate::arb_engine::crawl::flights::CrawlFlights;
use crate::arb_engine::crawl::policy::{CrawlPathPolicy, TokenFilter};
use crate::arb_engine::crawl::receipt::{CrawlReceipt, UnitStage};
use crate::arb_engine::EngineStages;
use crate::bot_core::construction_io::{ConstructionIo, DbConstruction, NoDb, RpcConstruction};
use crate::bot_core::registration_ledger::{
    PipelineReport, PoolStanding, PreBuildIdentity, RegistrationLedgerHandle, RegistrationOutcome,
    RegistrationUnitOutcome, StandingDisposition, UnregistrableRecord,
};
use crate::bot_core::verification_retry::RetryPolicy;
use crate::bot_core::{Bot, ConstructionRefusal, ConstructionRoute, VerifyLifecycle};

// ── fixtures ────────────────────────────────────────────────────────────────

const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const DAI: Address = address!("6B175474E89094C44Da98b954EedeAC495271d0F");
const USDT: Address = address!("dAC17F958D2ee523a2206206994597C13D831ec7");
const V3_FACTORY: Address = address!("1111111111111111111111111111111111111111");
const V3_POOL: Address = address!("2222222222222222222222222222222222222222");
const V4_PM: Address = address!("3333333333333333333333333333333333333333");
const V4_POOL_ID: [u8; 32] = [0xcd; 32];
const V3_POOL_DAI: Address = address!("4444444444444444444444444444444444444444");
const V3_POOL_USDT: Address = address!("5555555555555555555555555555555555555555");
const BLOCK: u64 = 18_000_000;

/// The fixture chain (mainnet — the presets are mainnet's).
const CHAIN_ID: u64 = 1;

// ── the ABI encoders ────────────────────────────────────────────────────────

#[expect(
    clippy::needless_pass_by_value,
    reason = "test helper takes an owned value for inline call sites"
)]
fn enc(value: DynSolValue) -> Vec<u8> {
    value.abi_encode()
}

fn addr_word(a: Address) -> Vec<u8> {
    let mut v = vec![0u8; 12];
    v.extend_from_slice(a.as_slice());
    v
}

fn uint_word(value: u64, bits: usize) -> Vec<u8> {
    enc(DynSolValue::Uint(U256::from(value), bits))
}

fn int_word(value: i32, bits: usize) -> Vec<u8> {
    enc(DynSolValue::Int(I256::try_from(value).unwrap(), bits))
}

fn reserves_word(reserve0: u64, reserve1: u64) -> Vec<u8> {
    enc(DynSolValue::Tuple(vec![
        DynSolValue::Uint(U256::from(reserve0), 112),
        DynSolValue::Uint(U256::from(reserve1), 112),
        DynSolValue::Uint(U256::ZERO, 32),
    ]))
}

fn slot0_word() -> Vec<u8> {
    enc(DynSolValue::Tuple(vec![
        DynSolValue::Uint(U256::from(1u128 << 96), 160),
        DynSolValue::Int(I256::try_from(0i32).unwrap(), 24),
        DynSolValue::Uint(U256::ZERO, 16),
        DynSolValue::Uint(U256::ZERO, 16),
        DynSolValue::Uint(U256::ZERO, 16),
        DynSolValue::Uint(U256::ZERO, 8),
        DynSolValue::Bool(false),
    ]))
}

// ── the RPC double ──────────────────────────────────────────────────────────

/// A construction-RPC double keyed per `(contract, selector)`. A missing entry
/// REVERTS, which is what makes the family probes meaningful: an absent selector
/// is an absent getter.
#[derive(Default)]
struct FakeRpc {
    responses: HashMap<(Address, [u8; 4]), Vec<u8>>,
    block_number: u64,
}

impl FakeRpc {
    fn new() -> Self {
        Self {
            block_number: BLOCK,
            ..Self::default()
        }
    }

    fn set(&mut self, to: Address, signature: &[u8], bytes: Vec<u8>) {
        self.responses.insert((to, abi::selector(signature)), bytes);
    }
}

#[async_trait]
impl RpcConstruction for FakeRpc {
    async fn get_block_number(&self) -> Result<u64, ProviderError> {
        Ok(self.block_number)
    }

    async fn get_block(
        &self,
        _block: u64,
    ) -> Result<Option<degenbot_rpc::provider::EthBlock>, ProviderError> {
        Ok(None)
    }

    async fn get_block_timestamp(&self, _block: u64) -> Result<Option<u64>, ProviderError> {
        Ok(None)
    }

    async fn get_code(
        &self,
        _address: Address,
        _block: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        // Non-empty code: every fixture address is a contract.
        Ok(Bytes::from_static(&[0x60, 0x0a]))
    }

    async fn get_balance(
        &self,
        _address: Address,
        _block: Option<u64>,
    ) -> Result<U256, ProviderError> {
        Ok(U256::ZERO)
    }

    async fn call(
        &self,
        to: Address,
        data: Bytes,
        _block: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        match self.responses.get(&(to, data[..4].try_into().unwrap())) {
            Some(answer) => Ok(answer.clone().into()),
            None => Err(ProviderError::ExecutionReverted {
                code: -32000,
                message: "the fixture RPC has no answer for this call".to_owned(),
            }),
        }
    }
}

// ── the DB double (the V4 identity two-step) ───────────────────────────────

/// A `DbConstruction` double answering the V4 identity two-step: one manager
/// row, one V4 row, and the two currency token rows.
struct V4Db {
    /// The V4 row's static fee. `0x0080_0000` is the dynamic-fee flag, which the
    /// write door refuses at admission.
    fee: i64,
    hooks: Address,
}

#[async_trait]
impl DbConstruction for V4Db {
    async fn fetch_erc20_token(
        &self,
        _chain_id: i64,
        _address: Address,
    ) -> Result<Option<Erc20TokenRow>, DbError> {
        Ok(None)
    }

    async fn fetch_pool_row(
        &self,
        _chain_id: i64,
        _address: Address,
    ) -> Result<Option<LiquidityPoolRow>, DbError> {
        Ok(None)
    }

    async fn fetch_pool_kind(
        &self,
        _kind: &str,
        _pool_id: i64,
    ) -> Result<Option<PoolKindRow>, DbError> {
        Ok(None)
    }

    async fn fetch_token_by_id(&self, token_id: i64) -> Result<Option<Erc20TokenRow>, DbError> {
        let currency = match token_id {
            1 => USDC,
            2 => WETH,
            _ => return Ok(None),
        };
        Ok(Some(Erc20TokenRow {
            id: token_id,
            chain: 1,
            address: currency,
            name: None,
            symbol: None,
            decimals: Some(18),
        }))
    }

    async fn fetch_exchange(&self, _exchange_id: i64) -> Result<Option<ExchangeRow>, DbError> {
        Ok(None)
    }

    async fn fetch_liquidity_positions(
        &self,
        _pool_id: i64,
    ) -> Result<Vec<LiquidityPositionRow>, DbError> {
        Ok(Vec::new())
    }

    async fn fetch_pool_manager(
        &self,
        _chain_id: i64,
        address: Address,
    ) -> Result<Option<PoolManagerRow>, DbError> {
        Ok((address == V4_PM).then(|| PoolManagerRow {
            id: 1,
            address,
            chain: 1,
            kind: "v4".to_owned(),
            state_view: Some(V4_PM),
            exchange_id: 1,
        }))
    }

    async fn fetch_v4_pool_by_pool_hash(
        &self,
        pool_hash_hex: &str,
    ) -> Result<Option<V4PoolRow>, DbError> {
        let expected = format!("0x{}", alloy::hex::encode(V4_POOL_ID));
        Ok((pool_hash_hex == expected).then(|| V4PoolRow {
            managed_pool_id: 1,
            pool_hash: alloy::primitives::B256::from(V4_POOL_ID),
            hooks: self.hooks,
            currency0_id: 1,
            currency1_id: 2,
            fee_currency0: self.fee,
            fee_currency1: 0,
            fee_denominator: 1_000_000,
            tick_spacing: 1,
            liquidity_update_block: None,
            liquidity_update_log_index: None,
        }))
    }

    async fn fetch_managed_liquidity_positions(
        &self,
        _managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolLiquidityPositionRow>, DbError> {
        Ok(Vec::new())
    }

    async fn update_erc20_token_metadata(
        &self,
        _chain_id: i64,
        _address: &str,
        _name: Option<&str>,
        _symbol: Option<&str>,
        _decimals: Option<i64>,
    ) -> Result<(), DbError> {
        Ok(())
    }
}

// ── the fixture pool shapes ────────────────────────────────────────────────

/// The CREATE2 address of a V2-style pair for `dex`'s factory + init hash.
fn v2_address(dex: &dex_identity::DexIdentity, token0: Address, token1: Address) -> Address {
    compute_v2_address(
        dex.factory,
        token0,
        token1,
        dex.init_hash
            .expect("the fixture preset carries a CREATE2 init hash"),
    )
}

fn v2_hop() -> DiscoveredHop {
    DiscoveredHop::V2 {
        address: v2_address(&dex_identity::UNISWAP_V2, USDC, WETH),
    }
}

fn sushi_v2_hop() -> DiscoveredHop {
    DiscoveredHop::V2 {
        address: v2_address(&dex_identity::SUSHISWAP_V2, USDC, WETH),
    }
}

fn v3_hop() -> DiscoveredHop {
    DiscoveredHop::V3 { address: V3_POOL }
}

fn v4_hop() -> DiscoveredHop {
    DiscoveredHop::V4 {
        pool_manager: V4_PM,
        pool_hash: Some(V4_POOL_ID),
    }
}

/// The V2 reserve-pair reads (the DEX preset resolves from the factory alone).
fn set_v2(rpc: &mut FakeRpc, dex: &dex_identity::DexIdentity, token0: Address, token1: Address) {
    let pool = v2_address(dex, token0, token1);
    rpc.set(pool, b"factory()", addr_word(dex.factory));
    rpc.set(pool, b"token0()", addr_word(token0));
    rpc.set(pool, b"token1()", addr_word(token1));
    rpc.set(
        pool,
        b"getReserves()",
        reserves_word(1_500_000_000_000, 800_000_000_000_000_000),
    );
}

/// The V3 concentrated-liquidity reads.
fn set_v3(rpc: &mut FakeRpc, pool: Address, token0: Address, token1: Address) {
    rpc.set(pool, b"factory()", addr_word(V3_FACTORY));
    rpc.set(pool, b"token0()", addr_word(token0));
    rpc.set(pool, b"token1()", addr_word(token1));
    rpc.set(pool, b"fee()", uint_word(3_000, 24));
    rpc.set(pool, b"tickSpacing()", int_word(60, 24));
    rpc.set(pool, b"slot0()", slot0_word());
    rpc.set(pool, b"liquidity()", uint_word(1_000_000_000, 128));
    rpc.set(pool, b"tickBitmap(int16)", uint_word(0, 256));
}

/// The V4 manager-scoped reads.
fn set_v4(rpc: &mut FakeRpc) {
    rpc.set(V4_PM, b"getSlot0(bytes32)", {
        enc(DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(1u128 << 96), 160),
            DynSolValue::Int(I256::try_from(5i32).unwrap(), 24),
            DynSolValue::Uint(U256::from(300u32), 24),
            DynSolValue::Uint(U256::from(50u32), 24),
        ]))
    });
    rpc.set(
        V4_PM,
        b"getLiquidity(bytes32)",
        uint_word(2_000_000_000, 128),
    );
    rpc.set(V4_PM, b"getTickBitmap(bytes32,int16)", uint_word(0, 256));
}

/// The Camelot solidly-stable pair's reads: the shared volatile/stable factory
/// resolves the preset, and the pool's own `stableSwap()` picks the species
/// (which the V2 DB table cannot name — ADR-107 C7).
fn set_camelot_stable(rpc: &mut FakeRpc, token0: Address, token1: Address) {
    let dex = dex_identity::CAMELOT_V2_STABLE;
    let pool = v2_address(&dex, token0, token1);
    rpc.set(pool, b"factory()", addr_word(dex.factory));
    rpc.set(pool, b"token0()", addr_word(token0));
    rpc.set(pool, b"token1()", addr_word(token1));
    rpc.set(pool, b"stableSwap()", enc(DynSolValue::Bool(true)));
    rpc.set(pool, b"FEE_DENOMINATOR()", uint_word(100_000, 256));
    rpc.set(pool, b"token0FeePercent()", uint_word(300, 256));
    rpc.set(pool, b"token1FeePercent()", uint_word(300, 256));
    rpc.set(
        pool,
        b"getReserves()",
        reserves_word(1_500_000_000_000, 800_000_000_000_000_000),
    );
}

fn camelot_stable_hop() -> DiscoveredHop {
    DiscoveredHop::V2 {
        address: v2_address(&dex_identity::CAMELOT_V2_STABLE, USDC, WETH),
    }
}

/// The four-token cycle USDC → USDT → DAI → WETH → USDC over two V2 pairs and
/// two V3 pools: every hop family declares composition and no command-stream
/// producer expresses four mixed hops (ADR-107 C4).
fn four_hop_rpc() -> FakeRpc {
    let mut rpc = FakeRpc::new();
    set_v2(&mut rpc, &dex_identity::UNISWAP_V2, USDC, WETH);
    set_v3(&mut rpc, V3_POOL_DAI, WETH, DAI);
    set_v2(&mut rpc, &dex_identity::SUSHISWAP_V2, DAI, USDT);
    set_v3(&mut rpc, V3_POOL_USDT, USDT, USDC);
    rpc
}

fn four_hop_hops() -> Vec<DiscoveredHop> {
    vec![
        DiscoveredHop::V2 {
            address: v2_address(&dex_identity::UNISWAP_V2, USDC, WETH),
        },
        DiscoveredHop::V3 {
            address: V3_POOL_DAI,
        },
        DiscoveredHop::V2 {
            address: v2_address(&dex_identity::SUSHISWAP_V2, DAI, USDT),
        },
        DiscoveredHop::V3 {
            address: V3_POOL_USDT,
        },
    ]
}

/// The closed USDC→WETH→USDC cycle over a Uniswap V2 pair and a V3 pool.
fn cycle_rpc() -> FakeRpc {
    let mut rpc = FakeRpc::new();
    set_v2(&mut rpc, &dex_identity::UNISWAP_V2, USDC, WETH);
    set_v3(&mut rpc, V3_POOL, USDC, WETH);
    rpc
}

fn cycle_unit(seq: u64) -> CrawlUnit {
    CrawlUnit {
        seq,
        hops: vec![v2_hop(), v3_hop()],
        directions: None,
    }
}

// ── the harness ────────────────────────────────────────────────────────────

/// The admission's wide settings as a fixture value — no policy, the generic
/// construction rung, and this fixture's block.
fn default_settings() -> AdmissionSettings {
    AdmissionSettings {
        route: ConstructionRoute::generic_only(),
        policy: CrawlPathPolicy::default(),
        oracle: None,
        profit_token: USDC,
        snapshot_block: None,
        state_block: Some(BLOCK),
    }
}

struct Harness {
    bot: Arc<Bot>,
    /// The ONE engine seam the admission registers through: the fixture holds
    /// the stage surface itself, so no path operation here can reach the
    /// registry through a session object.
    stages: Arc<EngineStages>,
    /// The pose-independent verify home the admission asks, held directly.
    verify: Arc<VerifyLifecycle>,
    io: Arc<ConstructionIo>,
    /// The shared standings store the verify home and the admission settle in.
    ledger: RegistrationLedgerHandle,
    flights: Arc<CrawlFlights>,
    admission: RegistrationAdmission,
}

impl Harness {
    fn new(rpc: FakeRpc) -> Self {
        Self::with_db(rpc, Arc::new(NoDb))
    }

    fn with_db(rpc: FakeRpc, db: Arc<dyn DbConstruction + Send + Sync>) -> Self {
        Self::with_settings(rpc, db, default_settings())
    }

    fn with_settings(
        rpc: FakeRpc,
        db: Arc<dyn DbConstruction + Send + Sync>,
        settings: AdmissionSettings,
    ) -> Self {
        let io = Arc::new(ConstructionIo::new(db, Arc::new(rpc)));
        let bot = Arc::new(Bot::new(CHAIN_ID));
        let stages = Arc::new(EngineStages::with_core(bot.state_arc(), bot.active_delta()));
        // The fixture's ONE registration store: the verify home settles its
        // standings there and the admission reads them, exactly as the driver
        // composes them.
        let ledger = RegistrationLedgerHandle::new();
        // The verify home walks no retry sleeps in the suite: one attempt, no
        // delay, no jitter.
        let verify = Arc::new(VerifyLifecycle::new(
            stages.core(),
            crate::bot_core::VerifyLifecycleConfig {
                provider: None,
                state_view: None,
                retry: RetryPolicy {
                    max_attempts: 1,
                    base_delay: 0.0,
                    max_delay: 0.0,
                    jitter: 0.0,
                },
            },
            ledger.clone(),
        ));
        let flights = Arc::new(CrawlFlights::new());
        let admission = RegistrationAdmission::new(
            Arc::clone(&bot),
            Arc::clone(&io),
            Arc::clone(&stages),
            Arc::clone(&verify),
            ledger.clone(),
            Arc::clone(&flights),
            Arc::new(settings),
        );
        Self {
            bot,
            stages,
            verify,
            io,
            ledger,
            flights,
            admission,
        }
    }

    fn host(&self) -> CrawlUnitHost<'_> {
        CrawlUnitHost {
            admission: &self.admission,
            db: None,
        }
    }

    /// The CRAWL adapter: one submitted unit through the loop's entry.
    async fn run(&self, unit: &CrawlUnit) -> Result<CrawlReceipt, CrawlAbort> {
        run_crawl_unit(&self.host(), unit).await
    }

    /// The DIRECT adapter: the same admission, called with a candidate a caller
    /// already holds (the pure-Rust settlement bot's shape).
    async fn run_direct(&self, unit: &CrawlUnit) -> Result<CrawlReceipt, CrawlAbort> {
        self.admission
            .admit(unit.seq, &unit.hops, unit.directions.as_deref(), None)
            .await
    }

    /// Build one hop exactly as the admission's build stage does.
    async fn build_hop(&self, hop: &DiscoveredHop) -> Result<u64, ConstructionRefusal> {
        self.admission.build_hop(hop, BLOCK, None).await
    }

    /// The verify home the admission holds (the suite reads its claim counts).
    fn verify(&self) -> &Arc<VerifyLifecycle> {
        &self.verify
    }

    fn registered_family(&self, pool_id: u64) -> Option<degenbot_pools::RegisteredPoolFamily> {
        let state = self.bot.state_arc();
        let guard = state.read_at(LockSite::Registration);
        guard.registered_family(pool_id)
    }
}

// ── the parity table ───────────────────────────────────────────────────────

/// The outcome table these scenarios assert (`unit/parity_table.json`): per
/// case, the receipt's kind, breakdown tag, and answering stage.
const PARITY_TABLE: &str = include_str!("parity_table.json");

/// Every case name the table carries, in table order.
fn parity_cases() -> Vec<(String, String, Option<String>, String)> {
    let table: serde_json::Value = serde_json::from_str(PARITY_TABLE).unwrap();
    table["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["case"].as_str().unwrap().to_owned(),
                row["kind"].as_str().unwrap().to_owned(),
                row["tag"].as_str().map(str::to_owned),
                row["stage"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Assert one receipt against its table row: kind, breakdown tag, and the stage
/// that answered it.
fn assert_parity(case: &str, receipt: &CrawlReceipt) {
    let rows = parity_cases();
    let (_, kind, tag, stage) = rows
        .iter()
        .find(|(name, ..)| name == case)
        .unwrap_or_else(|| panic!("{case} is not a row of the parity table"));
    assert_eq!(receipt.kind(), kind, "{case}: unit kind");
    assert_eq!(
        receipt.refusal_tag().map(str::to_owned),
        *tag,
        "{case}: breakdown tag"
    );
    assert_eq!(
        receipt.answering_stage().map(UnitStage::name),
        Some(stage.as_str()),
        "{case}: answering stage"
    );
}

/// The cases these tests exercise — every table row must appear here.
const EXERCISED: [&str; 15] = [
    "registered_cycle",
    "duplicate_cycle",
    "memoized_build_refusal",
    "v4_encoder_limit_refusal",
    "v4_encoder_limit_refusal_memoized",
    "v4_hop_without_pool_id",
    "pinned_direction_mismatch",
    "policy_denied",
    "policy_denied_memoized",
    "unsupported_family_post_build",
    "unsupported_family_memoized",
    "unsupported_route_shape",
    "unsupported_route_shape_memoized",
    "register_fail",
    "cap_stop",
];

#[test]
fn every_parity_row_is_exercised() {
    let mut table: Vec<String> = parity_cases().into_iter().map(|(case, ..)| case).collect();
    table.sort();
    let mut exercised: Vec<String> = EXERCISED.iter().map(|case| (*case).to_owned()).collect();
    exercised.sort();
    assert_eq!(
        table, exercised,
        "the parity table and the Rust scenarios must name the same cases"
    );
}

// ── the rows ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn registered_cycle_then_duplicate_cycle() {
    let harness = Harness::new(cycle_rpc());
    let first = harness.run(&cycle_unit(0)).await.unwrap();
    assert_parity("registered_cycle", &first);
    assert!(matches!(
        first.outcome,
        RegistrationUnitOutcome::Registered {
            created: true,
            v4_hops: 0
        }
    ));
    assert_eq!(first.split.len(), 5, "a registered unit runs every stage");
    assert_eq!(first.path_id, Some(1), "the engine allocated the first id");
    let sig = first
        .hop_sig
        .clone()
        .expect("a registered unit holds a sig");
    assert_eq!(sig.len(), 2);

    // Re-sighted: the registered-path memo's dup fast-path answers before
    // verify, with the engine-dedup'd shape.
    let second = harness.run(&cycle_unit(1)).await.unwrap();
    assert_parity("duplicate_cycle", &second);
    assert!(matches!(
        second.outcome,
        RegistrationUnitOutcome::Registered {
            created: false,
            v4_hops: 0
        }
    ));
    assert_eq!(second.hop_sig, Some(sig), "the dup answers the same sig");
    assert_eq!(harness.stages.path_count(), 1, "no second path registered");
}

#[tokio::test]
async fn a_memoized_build_refusal_answers_before_any_build() {
    let harness = Harness::new(cycle_rpc());
    // A pool fact: the V2 hop's stable refusal is already recorded.
    harness.ledger.with(|ledger| {
        assert_eq!(
            ledger.memoize_unregistrable(
                v2_hop()
                    .pre_build_identity()
                    .expect("a V2 hop carries its address"),
                RegistrationOutcome::BuildV3Refused,
                true,
            ),
            Ok(StandingDisposition::Settled)
        );
    });
    let receipt = harness.run(&cycle_unit(0)).await.unwrap();
    assert_parity("memoized_build_refusal", &receipt);
    assert_eq!(receipt.split.len(), 1, "the memo stage answered");
    assert_eq!(receipt.hop_sig, None, "no signature existed yet");
}

#[tokio::test]
async fn a_v4_encoder_limit_refusal_is_a_stable_counted_skip_and_memoizes() {
    let mut rpc = FakeRpc::new();
    set_v4(&mut rpc);
    set_v2(&mut rpc, &dex_identity::UNISWAP_V2, USDC, WETH);
    let harness = Harness::with_db(
        rpc,
        Arc::new(V4Db {
            // A static fee past the executor's 2-byte field: a pool-key fact,
            // so a STABLE refusal that memoizes (the dynamic-fee sentinel is
            // ADMITTED since F1 and no longer answers this scenario).
            fee: 320_000,
            hooks: Address::ZERO,
        }),
    );
    let unit = CrawlUnit {
        seq: 0,
        hops: vec![v4_hop(), v2_hop()],
        directions: None,
    };
    let receipt = harness.run(&unit).await.unwrap();
    assert_parity("v4_encoder_limit_refusal", &receipt);
    match &receipt.outcome {
        RegistrationUnitOutcome::Skip {
            label,
            counts_as_skip,
            ..
        } => {
            assert_eq!(
                label.as_label(),
                RegistrationOutcome::build_refused_for(degenbot_pathfinding::graph::PoolKind::V4)
                    .as_str(),
                "the encoder-limit refusal keeps the build-skip tag"
            );
            assert!(
                *counts_as_skip,
                "a static-fee refusal is a counted build skip"
            );
        }
        other => panic!("expected a skip, got {other:?}"),
    }

    // The stable refusal is now a pool fact, so a re-sighting answers at the
    // memo stage with the same label and the same counter class.
    let again = harness.run(&unit).await.unwrap();
    assert_parity("v4_encoder_limit_refusal_memoized", &again);
    assert_eq!(again.split.len(), 1);
    match &again.outcome {
        RegistrationUnitOutcome::Skip {
            label,
            counts_as_skip,
            ..
        } => {
            assert_eq!(
                label.as_label(),
                RegistrationOutcome::build_refused_for(degenbot_pathfinding::graph::PoolKind::V4)
                    .as_str()
            );
            assert!(*counts_as_skip);
        }
        other => panic!("expected a memoized skip, got {other:?}"),
    }
}

#[tokio::test]
async fn a_v4_hop_without_a_pool_id_is_a_bounded_skip() {
    let harness = Harness::new(FakeRpc::new());
    let unit = CrawlUnit {
        seq: 0,
        hops: vec![DiscoveredHop::V4 {
            pool_manager: V4_PM,
            pool_hash: None,
        }],
        directions: None,
    };
    let receipt = harness.run(&unit).await.unwrap();
    assert_parity("v4_hop_without_pool_id", &receipt);
    assert_eq!(receipt.split.len(), 2);
}

#[tokio::test]
async fn a_pinned_direction_mismatch_skips_before_verify() {
    let harness = Harness::new(cycle_rpc());
    let mut unit = cycle_unit(0);
    // One pinned direction for a two-hop path: the pin disagrees with the path.
    unit.directions = Some(vec![true]);
    let receipt = harness.run(&unit).await.unwrap();
    assert_parity("pinned_direction_mismatch", &receipt);
    assert_eq!(receipt.split.len(), 3, "the gate answered at eligibility");
    assert_eq!(
        harness.verify().verified_pool_count(),
        0,
        "a direction mismatch never reaches verify"
    );
    assert_eq!(harness.stages.path_count(), 0);
}

#[tokio::test]
async fn a_policy_denial_is_a_counted_reject_and_memoizes_the_path() {
    let harness = Harness::with_settings(
        cycle_rpc(),
        Arc::new(NoDb),
        AdmissionSettings {
            policy: CrawlPathPolicy {
                tokens: TokenFilter {
                    deny: [WETH].into_iter().collect(),
                    ..TokenFilter::default()
                },
                ..CrawlPathPolicy::default()
            },
            ..default_settings()
        },
    );
    let receipt = harness.run(&cycle_unit(0)).await.unwrap();
    assert_parity("policy_denied", &receipt);
    match &receipt.outcome {
        RegistrationUnitOutcome::Reject { detail } => {
            assert!(
                detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("TokenDenylisted")),
                "the refusal rendering rides the log detail: {detail:?}"
            );
        }
        other => panic!("expected a reject, got {other:?}"),
    }
    let sig = receipt.hop_sig.clone().expect("the candidate sig existed");
    assert!(
        harness.ledger.with(|ledger| ledger.path_rejected(&sig)),
        "a policy deny is deterministic per hop signature and must be memoized"
    );

    // The re-sighting answers from the rejected-path memo without re-running the
    // policy — and without a detail (the memo holds the fact, not the text).
    let again = harness.run(&cycle_unit(1)).await.unwrap();
    assert_parity("policy_denied_memoized", &again);
    match &again.outcome {
        RegistrationUnitOutcome::Reject { detail } => assert_eq!(*detail, None),
        other => panic!("expected a memoized reject, got {other:?}"),
    }
    assert_eq!(harness.stages.path_count(), 0);
}

#[tokio::test]
async fn the_cap_stop_answers_at_the_register_stage() {
    let harness = Harness::new(cycle_rpc());
    harness.stages.set_path_cap(Some(0));
    let receipt = harness.run(&cycle_unit(0)).await.unwrap();
    assert_parity("cap_stop", &receipt);
    assert_eq!(receipt.split.len(), 5, "the cap stop owns the whole climb");
    assert_eq!(receipt.path_id, None);
    assert_eq!(harness.stages.path_count(), 0);
}

/// A sub-2-hop candidate is MALFORMED, not uncomposable: the shape key is
/// defined over well-formed routes, so capability assessment passes it through
/// and registration answers its arity fault — the transient register failure,
/// never memoized and never reported as unsupported capability.
#[tokio::test]
async fn a_sub_two_hop_candidate_is_a_transient_register_failure() {
    let pool = v2_address(&dex_identity::UNISWAP_V2, USDC, USDC);
    let mut rpc = FakeRpc::new();
    rpc.set(
        pool,
        b"factory()",
        addr_word(dex_identity::UNISWAP_V2.factory),
    );
    rpc.set(pool, b"token0()", addr_word(USDC));
    rpc.set(pool, b"token1()", addr_word(USDC));
    rpc.set(pool, b"getReserves()", reserves_word(1, 1));
    let harness = Harness::new(rpc);
    let receipt = harness
        .run(&CrawlUnit {
            seq: 0,
            hops: vec![DiscoveredHop::V2 { address: pool }],
            directions: None,
        })
        .await
        .unwrap();
    assert_parity("register_fail", &receipt);
    assert_eq!(
        receipt.split.len(),
        5,
        "only the register stage answers a malformed candidate"
    );
    assert!(
        !harness
            .ledger
            .with(|ledger| ledger.path_rejected(&receipt.hop_sig.clone().unwrap_or_default())),
        "a transient register failure is never negatively memoized"
    );
    let key = PreBuildIdentity::Address(pool);
    assert!(
        harness
            .ledger
            .with(|ledger| ledger.unregistrable_record(Some(&key)).is_none()),
        "a malformed candidate never classifies its pool as unregistrable"
    );
    assert_eq!(harness.stages.path_count(), 0);
}

// ── R1: the edge kind is equivalent to the on-chain probe ──────────────────

/// Every fixture pool builds to the family its on-chain probe names.
///
/// The crawl trusts the discovery edge's `PoolKind` instead of re-probing the
/// chain (R1). This pins that the trust is equivalence for every fixture family:
/// the pool the edge-kind build registers is the family `probe_pool_type` answers
/// for the same contract. V4 is the one family the address-keyed probe cannot
/// name — a V4 pool lives under `(PoolManager, pool_id)`, and the manager's
/// getters take a pool id — so its row asserts the registration family directly.
#[tokio::test]
async fn the_edge_kind_is_equivalent_to_the_onchain_probe() {
    let harness = Harness::new(cycle_rpc());

    // V2: the reserve-pair reads.
    let v2 = harness.build_hop(&v2_hop()).await.unwrap();
    assert_eq!(
        harness.registered_family(v2),
        Some(degenbot_pools::RegisteredPoolFamily::V2)
    );
    assert_eq!(
        crate::bot_core::probe_pool_type(
            &harness.io,
            match v2_hop() {
                DiscoveredHop::V2 { address } => address,
                _ => unreachable!(),
            },
            Some(BLOCK)
        )
        .await
        .unwrap(),
        crate::bot_core::PoolFamily::V2,
        "the on-chain probe names the same family the edge kind built"
    );

    // V3: slot0() present.
    let v3 = harness.build_hop(&v3_hop()).await.unwrap();
    assert_eq!(
        harness.registered_family(v3),
        Some(degenbot_pools::RegisteredPoolFamily::V3)
    );
    assert_eq!(
        crate::bot_core::probe_pool_type(&harness.io, V3_POOL, Some(BLOCK))
            .await
            .unwrap(),
        crate::bot_core::PoolFamily::V3
    );

    // V4: keyed by (PoolManager, pool_id); the address-keyed probe cannot answer.
    let mut rpc = FakeRpc::new();
    set_v4(&mut rpc);
    let v4_harness = Harness::with_db(
        rpc,
        Arc::new(V4Db {
            fee: 500,
            hooks: Address::ZERO,
        }),
    );
    let v4 = v4_harness.build_hop(&v4_hop()).await.unwrap();
    assert_eq!(
        v4_harness.registered_family(v4),
        Some(degenbot_pools::RegisteredPoolFamily::V4)
    );
}

// ── the loud contract ──────────────────────────────────────────────────────

#[tokio::test]
async fn a_family_level_construction_refusal_aborts_the_crawl() {
    let harness = Harness::with_settings(
        cycle_rpc(),
        Arc::new(NoDb),
        AdmissionSettings {
            // No factory rung and no generic builder: the route serves no pool.
            route: ConstructionRoute {
                factories: Vec::new(),
                generic: false,
            },
            ..default_settings()
        },
    );
    let err = harness.run(&cycle_unit(0)).await.unwrap_err();
    assert!(
        matches!(
            err,
            CrawlAbort::Construction(
                crate::bot_core::registration_ledger::RefusalKind::UnsupportedFamily { .. }
            )
        ),
        "a family-level refusal is a loud abort, never a counted skip: {err}"
    );
    assert_eq!(harness.stages.path_count(), 0);
}

#[tokio::test]
async fn a_direction_invariant_violation_aborts_the_crawl() {
    let mut rpc = FakeRpc::new();
    set_v2(&mut rpc, &dex_identity::UNISWAP_V2, USDC, WETH);
    // A second pool on a disjoint pair: the chain cannot close.
    set_v3(&mut rpc, V3_POOL, DAI, USDC);
    let harness = Harness::new(rpc);
    let err = harness.run(&cycle_unit(0)).await.unwrap_err();
    assert!(
        matches!(err, CrawlAbort::Direction(_)),
        "an unclosed cycle is a direction invariant violation: {err}"
    );
    assert_eq!(harness.stages.path_count(), 0);
}

// ── the verify-once window ─────────────────────────────────────────────────

#[tokio::test]
async fn the_verify_memo_answers_one_lifecycle_per_pool() {
    // Two distinct candidate cycles sharing the V3 pool: the second sighting of
    // that pool must not run a second lifecycle.
    let mut rpc = FakeRpc::new();
    set_v2(&mut rpc, &dex_identity::UNISWAP_V2, USDC, WETH);
    set_v2(&mut rpc, &dex_identity::SUSHISWAP_V2, USDC, WETH);
    set_v3(&mut rpc, V3_POOL, USDC, WETH);
    let harness = Harness::new(rpc);

    let first = CrawlUnit {
        seq: 0,
        hops: vec![v2_hop(), v3_hop()],
        directions: None,
    };
    let second = CrawlUnit {
        seq: 1,
        hops: vec![sushi_v2_hop(), v3_hop()],
        directions: None,
    };
    harness.run(&first).await.unwrap();
    assert_eq!(harness.verify().verified_pool_count(), 1);
    harness.run(&second).await.unwrap();
    assert_eq!(
        harness.verify().verified_pool_count(),
        1,
        "a completed lifecycle is a pool fact: the V3 pool verifies once"
    );
    assert_eq!(
        harness.stages.path_count(),
        2,
        "two distinct cycles registered"
    );
}

// ── the build flights (R3) ─────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_identical_hops_share_one_construction() {
    let harness = Arc::new(Harness::new(cycle_rpc()));
    // Two units through the same hop, submitted concurrently.
    let a = {
        let harness = Arc::clone(&harness);
        tokio::spawn(async move {
            harness
                .run(&CrawlUnit {
                    seq: 0,
                    hops: vec![v2_hop(), v3_hop()],
                    directions: None,
                })
                .await
        })
    };
    let b = {
        let harness = Arc::clone(&harness);
        tokio::spawn(async move {
            harness
                .run(&CrawlUnit {
                    seq: 1,
                    hops: vec![v2_hop(), v3_hop()],
                    directions: None,
                })
                .await
        })
    };
    a.await.unwrap().unwrap();
    b.await.unwrap().unwrap();
    assert_eq!(
        harness.flights.live_count(),
        0,
        "every flight retires when its leader publishes"
    );
    assert_eq!(
        harness.stages.path_count(),
        1,
        "both units registered the same cycle once"
    );
}

// ── the unit's own types ───────────────────────────────────────────────────

#[test]
fn hop_kinds_and_memo_identities_are_the_edge_values() {
    assert_eq!(v2_hop().kind(), degenbot_pathfinding::PoolKind::V2);
    assert_eq!(v3_hop().kind(), degenbot_pathfinding::PoolKind::V3);
    assert_eq!(v4_hop().kind(), degenbot_pathfinding::PoolKind::V4);
    assert_eq!(
        v2_hop().pre_build_identity(),
        Some(PreBuildIdentity::Address(match v2_hop() {
            DiscoveredHop::V2 { address } => address,
            _ => unreachable!(),
        }))
    );
    assert_eq!(
        v4_hop().pre_build_identity(),
        Some(PreBuildIdentity::V4(V4_POOL_ID))
    );
    assert_eq!(
        DiscoveredHop::V4 {
            pool_manager: V4_PM,
            pool_hash: None
        }
        .pre_build_identity(),
        None,
        "a hashless V4 hop is not memoizable"
    );
}

#[test]
fn the_build_failure_classification_keeps_the_taxonomy_class() {
    use crate::arb_engine::crawl::admission::{classify_construction_refusal, HopRefusal};
    use degenbot_pools::RegisterRefusal;

    // A dynamic-fee V4 pool crosses the write door as an admission refusal; the
    // classification must keep the taxonomy's class (stable, uncounted).
    let refusal = ConstructionRefusal::Registration(RegisterRefusal::Admission {
        failure: BuildFailure::DynamicFee,
        hook_flags: None,
        fee: Some(0x0080_0000),
    });
    match classify_construction_refusal(&refusal, v4_hop()) {
        HopRefusal::Skip(refusal) => {
            assert_eq!(refusal.outcome, RegistrationOutcome::V4DynamicFeeRejected);
            assert!(refusal.stable);
            assert!(!refusal.counts_as_skip);
        }
        HopRefusal::Abort(kind) => panic!("expected a classified skip, got {kind}"),
    }

    // A raced registration is transient: never memoized.
    let raced = ConstructionRefusal::Registration(RegisterRefusal::AlreadyRegistered {
        address: Some(USDC),
        pool_key: None,
        detail: "race".to_owned(),
    });
    match classify_construction_refusal(&raced, v2_hop()) {
        HopRefusal::Skip(refusal) => {
            assert!(!refusal.stable, "a raced registration self-heals");
        }
        HopRefusal::Abort(kind) => panic!("expected a classified skip, got {kind}"),
    }

    // The family-level refusal stays loud.
    let loud = ConstructionRefusal::UnsupportedFamily {
        address: USDC,
        detail: "no rung".to_owned(),
    };
    assert!(matches!(
        classify_construction_refusal(&loud, v2_hop()),
        HopRefusal::Abort(_)
    ));
}

// ── Registration admission: one implementation, two adapters ───────────────

/// Fold a receipt stream into the driver's report — the pure-Rust example's
/// fold (`PipelineReport::absorb`), which both adapters must feed identically.
fn fold(receipts: &[CrawlReceipt]) -> PipelineReport {
    let mut report = PipelineReport::default();
    for receipt in receipts {
        report.absorb(&receipt.outcome);
    }
    report
}

/// One candidate sequence — a fresh cycle, its duplicate, a second duplicate —
/// answers the same receipts through the crawl adapter and the direct adapter,
/// and folds to the same report. The stage order is one implementation, so the
/// only thing that can differ between the adapters is who submitted the
/// candidate.
#[tokio::test]
async fn the_two_adapters_answer_the_same_candidate_sequence() {
    let sequence = [cycle_unit(0), cycle_unit(1), cycle_unit(2)];
    let crawl = Harness::new(cycle_rpc());
    let direct = Harness::new(cycle_rpc());

    let mut crawl_receipts = Vec::new();
    let mut direct_receipts = Vec::new();
    for unit in &sequence {
        let submitted = crawl.run(unit).await.unwrap();
        let offered = direct.run_direct(unit).await.unwrap();
        assert_eq!(submitted.seq, unit.seq);
        assert_eq!(offered.seq, unit.seq);
        assert_eq!(
            submitted.outcome, offered.outcome,
            "seq {}: the same outcome",
            unit.seq
        );
        assert_eq!(submitted.hop_sig, offered.hop_sig, "seq {}", unit.seq);
        assert_eq!(submitted.path_id, offered.path_id, "seq {}", unit.seq);
        assert_eq!(
            submitted.split.len(),
            offered.split.len(),
            "seq {}: the same stage answered",
            unit.seq
        );
        crawl_receipts.push(submitted);
        direct_receipts.push(offered);
    }

    let crawl_report = fold(&crawl_receipts);
    assert_eq!(
        crawl_report,
        fold(&direct_receipts),
        "both adapters fold to the same report"
    );
    assert_eq!(crawl_report.path_count, 1, "one new path");
    assert_eq!(crawl_report.dup_count, 2, "two duplicates");
    assert!(
        crawl_report.fold_identities_hold(),
        "the fold's own identities hold"
    );
}

/// The skip arms agree too: a pinned-direction refusal and a hashless V4 hop
/// answer the same receipt through either adapter (kind, tag, stage).
#[tokio::test]
async fn the_two_adapters_answer_the_same_skips() {
    let mut pinned = cycle_unit(0);
    pinned.directions = Some(vec![true]);
    let hashless = CrawlUnit {
        seq: 1,
        hops: vec![DiscoveredHop::V4 {
            pool_manager: V4_PM,
            pool_hash: None,
        }],
        directions: None,
    };
    let crawl = Harness::new(cycle_rpc());
    let direct = Harness::new(cycle_rpc());
    for unit in [&pinned, &hashless] {
        let submitted = crawl.run(unit).await.unwrap();
        let offered = direct.run_direct(unit).await.unwrap();
        assert_eq!(submitted.kind(), offered.kind());
        assert_eq!(submitted.refusal_tag(), offered.refusal_tag());
        assert_eq!(
            submitted.answering_stage(),
            offered.answering_stage(),
            "seq {}: the same stage answered",
            unit.seq
        );
    }
    assert_eq!(crawl.stages.path_count(), 0);
    assert_eq!(direct.stages.path_count(), 0);
}

/// The benign registered-path cap is the register stage's answer in both
/// adapters, and it is the caller's BENIGN stop rather than an error.
#[tokio::test]
async fn the_benign_cap_answers_at_the_register_stage_for_both_adapters() {
    let crawl = Harness::new(cycle_rpc());
    let direct = Harness::new(cycle_rpc());
    crawl.stages.set_path_cap(Some(0));
    direct.stages.set_path_cap(Some(0));

    let submitted = crawl.run(&cycle_unit(0)).await.unwrap();
    let offered = direct.run_direct(&cycle_unit(0)).await.unwrap();
    assert_eq!(submitted.outcome, RegistrationUnitOutcome::Cap);
    assert_eq!(offered.outcome, RegistrationUnitOutcome::Cap);
    assert_eq!(submitted.split.len(), 5, "the cap owns the whole climb");
    assert_eq!(offered.split.len(), 5);
    let report = fold(&[submitted, offered]);
    assert!(report.capped, "the cap latches the benign witness");
    assert_eq!(report.cap_skip_count, 2);
    assert!(report.fold_identities_hold());
}

// ── the early answers, and the witness that must survive them ─────────────

/// A V4-bearing candidate over the fixture cycle: one V4 hop and one V2 hop on
/// USDC↔WETH, so the resolver closes the cycle and the candidate reaches
/// registration with a V4 hop in it.
fn v4_cycle_unit(seq: u64) -> CrawlUnit {
    CrawlUnit {
        seq,
        hops: vec![v4_hop(), v2_hop()],
        directions: None,
    }
}

fn v4_cycle_harness(settings: AdmissionSettings) -> Harness {
    let mut rpc = FakeRpc::new();
    set_v4(&mut rpc);
    set_v2(&mut rpc, &dex_identity::UNISWAP_V2, USDC, WETH);
    Harness::with_settings(
        rpc,
        Arc::new(V4Db {
            fee: 500,
            hooks: Address::ZERO,
        }),
        settings,
    )
}

/// The dup fast-path answers at ELIGIBILITY (before verify) and must still carry
/// the candidate's V4 witness: the fold counts `v4_pool_count` on created AND
/// duplicate outcomes, so a witness lost on the early answer under-counts V4
/// advertisement.
#[tokio::test]
async fn the_duplicate_fast_path_carries_the_v4_witness() {
    let harness = v4_cycle_harness(default_settings());
    let first = harness.run(&v4_cycle_unit(0)).await.unwrap();
    assert_eq!(first.answering_stage(), Some(UnitStage::Register));
    assert!(
        matches!(
            first.outcome,
            RegistrationUnitOutcome::Registered {
                created: true,
                v4_hops: 1
            }
        ),
        "a created registration carries the candidate's one V4 hop: {:?}",
        first.outcome
    );
    let sig = first
        .hop_sig
        .clone()
        .expect("a registered unit holds a sig");

    let dup = harness.run(&v4_cycle_unit(1)).await.unwrap();
    assert_eq!(
        dup.answering_stage(),
        Some(UnitStage::Eligibility),
        "the registered-path memo answers the dup before verify"
    );
    assert!(
        matches!(
            dup.outcome,
            RegistrationUnitOutcome::Registered {
                created: false,
                v4_hops: 1
            }
        ),
        "the early dup answer carries the candidate's one V4 hop: {:?}",
        dup.outcome
    );
    assert_eq!(dup.hop_sig, Some(sig), "the dup answers the admitted sig");
    assert_eq!(harness.stages.path_count(), 1, "no second path registered");

    let report = fold(&[first, dup]);
    assert_eq!(
        report.v4_pool_count, 2,
        "the witness folds on the created AND the duplicate outcome"
    );
    assert_eq!(report.path_count, 1);
    assert_eq!(report.dup_count, 1);
}

/// The cap stop is the register stage's benign answer: it submits the admitted
/// signature and folds no V4 witness (a cap stop is not a registration).
#[tokio::test]
async fn the_cap_stop_submits_the_admitted_signature() {
    let harness = v4_cycle_harness(default_settings());
    harness.stages.set_path_cap(Some(0));
    let receipt = harness.run(&v4_cycle_unit(0)).await.unwrap();
    assert_eq!(receipt.answering_stage(), Some(UnitStage::Register));
    assert_eq!(receipt.outcome, RegistrationUnitOutcome::Cap);
    assert_eq!(
        receipt.hop_sig.as_ref().map(Vec::len),
        Some(2),
        "the cap stop reached register with the two-hop admitted signature"
    );
    let report = fold(&[receipt]);
    assert!(report.capped);
    assert_eq!(
        report.v4_pool_count, 0,
        "only a completed registration contributes the V4 witness"
    );
}

/// A policy denial answers at ELIGIBILITY and never reaches verify or register,
/// and the deny is memoized for the next sighting of the same signature.
#[tokio::test]
async fn a_policy_denial_answers_before_verify() {
    let harness = v4_cycle_harness(AdmissionSettings {
        policy: CrawlPathPolicy {
            tokens: TokenFilter {
                deny: [WETH].into_iter().collect(),
                ..TokenFilter::default()
            },
            ..CrawlPathPolicy::default()
        },
        ..default_settings()
    });
    let receipt = harness.run(&v4_cycle_unit(0)).await.unwrap();
    assert_eq!(receipt.answering_stage(), Some(UnitStage::Eligibility));
    assert!(
        matches!(receipt.outcome, RegistrationUnitOutcome::Reject { .. }),
        "a policy deny is a counted reject: {:?}",
        receipt.outcome
    );
    assert_eq!(
        harness.verify().verified_pool_count(),
        0,
        "an eligibility answer never reaches verify"
    );
    assert_eq!(harness.stages.path_count(), 0);

    let again = harness.run(&v4_cycle_unit(1)).await.unwrap();
    assert_eq!(again.answering_stage(), Some(UnitStage::Eligibility));
    assert!(
        matches!(
            again.outcome,
            RegistrationUnitOutcome::Reject { detail: None }
        ),
        "the rejected-path memo answers without the policy's detail: {:?}",
        again.outcome
    );
}

/// A STABLE build refusal is a pool fact (the second sighting answers at the
/// memo stage); a TRANSIENT one is never memoized (the second sighting rebuilds).
#[tokio::test]
async fn a_stable_refusal_is_a_pool_fact_and_a_transient_one_stays_retryable() {
    // Stable: a V4 static fee past the executor's field is a pool-key fact.
    let mut rpc = FakeRpc::new();
    set_v4(&mut rpc);
    let stable = Harness::with_db(
        rpc,
        Arc::new(V4Db {
            fee: 320_000,
            hooks: Address::ZERO,
        }),
    );
    let unit = CrawlUnit {
        seq: 0,
        hops: vec![v4_hop()],
        directions: None,
    };
    let first = stable.run_direct(&unit).await.unwrap();
    assert_eq!(first.split.len(), 2, "the build stage answered");
    let again = stable.run_direct(&unit).await.unwrap();
    assert_eq!(again.split.len(), 1, "the memo answered before any build");
    match (&first.outcome, &again.outcome) {
        (
            RegistrationUnitOutcome::Skip {
                label: first_label,
                counts_as_skip: first_counts,
                ..
            },
            RegistrationUnitOutcome::Skip {
                label: again_label,
                counts_as_skip: again_counts,
                ..
            },
        ) => {
            assert_eq!(first_label, again_label, "the same memoized label");
            assert_eq!(first_counts, again_counts, "the same counter class");
        }
        other => panic!("expected two skips, got {other:?}"),
    }
    let key = v4_hop().pre_build_identity();
    assert!(
        stable
            .ledger
            .with(|ledger| ledger.unregistrable_record(key.as_ref()).is_some()),
        "a stable refusal is memoized"
    );

    // Transient: an unreadable V3 pool re-runs the build stage every time.
    let transient = Harness::new(FakeRpc::new());
    let unit = CrawlUnit {
        seq: 0,
        hops: vec![v3_hop()],
        directions: None,
    };
    let first = transient.run_direct(&unit).await.unwrap();
    assert_eq!(first.split.len(), 2, "the build stage answered");
    let again = transient.run_direct(&unit).await.unwrap();
    assert_eq!(
        again.split.len(),
        2,
        "a transient refusal re-runs the build stage"
    );
    let key = v3_hop().pre_build_identity();
    assert!(
        transient
            .ledger
            .with(|ledger| ledger.unregistrable_record(key.as_ref()).is_none()),
        "a transient failure is never memoized"
    );
}

/// One verify home, two adapters: a pool the direct adapter verified is not
/// verified again when the crawl adapter offers a second candidate through it.
#[tokio::test]
async fn the_verify_fact_is_joined_across_the_two_adapters() {
    let mut rpc = FakeRpc::new();
    set_v2(&mut rpc, &dex_identity::UNISWAP_V2, USDC, WETH);
    set_v2(&mut rpc, &dex_identity::SUSHISWAP_V2, USDC, WETH);
    set_v3(&mut rpc, V3_POOL, USDC, WETH);
    let harness = Harness::new(rpc);

    let offered = CrawlUnit {
        seq: 0,
        hops: vec![v2_hop(), v3_hop()],
        directions: None,
    };
    let submitted = CrawlUnit {
        seq: 1,
        hops: vec![sushi_v2_hop(), v3_hop()],
        directions: None,
    };
    harness.run_direct(&offered).await.unwrap();
    assert_eq!(
        harness.verify().verified_pool_count(),
        1,
        "the direct adapter ran the V3 pool's lifecycle"
    );
    harness.run(&submitted).await.unwrap();
    assert_eq!(
        harness.verify().verified_pool_count(),
        1,
        "the crawl adapter joined the completed lifecycle"
    );
    assert_eq!(
        harness.stages.path_count(),
        2,
        "two distinct cycles registered"
    );
}

/// Verification and admission settle in ONE store: the completed lifecycle is a
/// `Verified` pool standing the admission's own ledger handle reads, so a
/// re-sighted candidate sees the same fact and no second table can contradict
/// it.
#[tokio::test]
async fn verification_and_admission_settle_one_ledger() {
    let harness = Harness::new(cycle_rpc());
    harness.run_direct(&cycle_unit(0)).await.unwrap();

    let key = PreBuildIdentity::Address(V3_POOL);
    assert_eq!(
        harness
            .ledger
            .with(|ledger| ledger.pool_standing(&key).cloned()),
        Some(PoolStanding::Verified),
        "the admission reads the verify home's standing from its own ledger"
    );
    assert!(
        harness
            .verify()
            .ledger()
            .with(|ledger| ledger.pool_verified(&key)),
        "the verify home hands back the same store"
    );
    assert!(
        harness
            .ledger
            .with(|ledger| ledger.unregistrable_record(Some(&key)).is_none()),
        "a verified pool holds no refusal standing"
    );
}

/// A verified pool is a REGISTERED pool, so its construction is
/// get-or-register and can never refuse stably. The regression pins the
/// admission's order: re-offering a built-and-verified hop reaches the cohort
/// again and answers the dup fast-path, so the build stage's
/// verified-vs-refusal conflict stays unreachable and its abort arm is a
/// corruption tripwire rather than a path a candidate can take.
#[tokio::test]
async fn a_registered_verified_pool_is_never_memoized_unregistrable() {
    let harness = Harness::new(cycle_rpc());
    harness.run(&cycle_unit(0)).await.unwrap();
    let verified = PreBuildIdentity::Address(V3_POOL);
    assert_eq!(
        harness
            .ledger
            .with(|ledger| ledger.pool_standing(&verified).cloned()),
        Some(PoolStanding::Verified)
    );

    let again = harness.run(&cycle_unit(1)).await.unwrap();
    assert_eq!(
        again.outcome,
        RegistrationUnitOutcome::Registered {
            created: false,
            v4_hops: 0
        },
        "the re-offer is the engine's dup, never a build refusal"
    );
    for hop in [v2_hop(), v3_hop()] {
        let key = hop
            .pre_build_identity()
            .expect("a V2/V3 hop carries its address");
        assert!(
            harness
                .ledger
                .with(|ledger| ledger.unregistrable_record(Some(&key)).is_none()),
            "a registered pool acquires no refusal standing"
        );
    }
}

/// A fatal verify failure is the loud arm: the admission stops rather than
/// counting a skip, and it records no verified fact.
#[tokio::test]
async fn a_fatal_verify_failure_aborts_the_admission() {
    use crate::bot_core::registration_ledger::{RegistrationPhase, VerifyOrigin};

    let harness = Harness::new(cycle_rpc());
    let mismatch = crate::bot_core::registration_ledger::RefusalKind::verify_failed(
        RegistrationPhase::Verify,
        true,
        VerifyOrigin::Mismatch,
        "snapshot mismatch".to_owned(),
    );
    harness
        .verify()
        .script_attempts_for_test(vec![Err(mismatch.clone())]);

    let err = harness.run_direct(&cycle_unit(0)).await.unwrap_err();
    assert!(
        matches!(&err, CrawlAbort::Verify(kind) if *kind == mismatch),
        "a fatal mismatch is a loud abort, never a counted skip: {err}"
    );
    assert_eq!(harness.verify().verified_pool_count(), 0);
    assert_eq!(harness.stages.path_count(), 0, "no path registered");
}

/// A transient verify failure is retried INSIDE one admission under the verify
/// home's policy, so one candidate still registers without the caller looping.
#[tokio::test]
async fn a_transient_verify_failure_is_retried_inside_one_admission() {
    use crate::bot_core::registration_ledger::{RegistrationPhase, VerifyOrigin};

    let harness = Harness::new(cycle_rpc());
    harness
        .verify()
        .configure(crate::bot_core::VerifyLifecycleConfig {
            provider: None,
            state_view: None,
            retry: RetryPolicy {
                max_attempts: 2,
                base_delay: 0.0,
                max_delay: 0.0,
                jitter: 0.0,
            },
        });
    let blip = crate::bot_core::registration_ledger::RefusalKind::verify_failed(
        RegistrationPhase::Verify,
        false,
        VerifyOrigin::Transport,
        "transport blip".to_owned(),
    );
    harness.verify().script_attempts_for_test(vec![Err(blip)]);

    let receipt = harness.run_direct(&cycle_unit(0)).await.unwrap();
    assert_eq!(
        receipt.outcome,
        RegistrationUnitOutcome::Registered {
            created: true,
            v4_hops: 0
        },
        "the retry recovered inside one admission"
    );
    assert_eq!(harness.verify().verified_pool_count(), 1);
}

/// A policy denial is a counted reject that memoizes the hop signature, and a
/// re-sighting answers from the memo without a detail — through the direct
/// adapter exactly as through the crawl.
#[tokio::test]
async fn a_policy_denial_rejects_and_memoizes_through_the_direct_adapter() {
    let harness = Harness::with_settings(
        cycle_rpc(),
        Arc::new(NoDb),
        AdmissionSettings {
            policy: CrawlPathPolicy {
                tokens: TokenFilter {
                    deny: [WETH].into_iter().collect(),
                    ..TokenFilter::default()
                },
                ..CrawlPathPolicy::default()
            },
            ..default_settings()
        },
    );
    let receipt = harness.run_direct(&cycle_unit(0)).await.unwrap();
    match &receipt.outcome {
        RegistrationUnitOutcome::Reject { detail } => assert!(
            detail
                .as_deref()
                .is_some_and(|detail| detail.contains("TokenDenylisted")),
            "the refusal rendering rides the log detail: {detail:?}"
        ),
        other => panic!("expected a reject, got {other:?}"),
    }
    let sig = receipt.hop_sig.clone().expect("the candidate sig existed");
    assert!(
        harness.ledger.with(|ledger| ledger.path_rejected(&sig)),
        "a policy deny memoizes the hop signature"
    );
    let again = harness.run_direct(&cycle_unit(1)).await.unwrap();
    match &again.outcome {
        RegistrationUnitOutcome::Reject { detail } => assert_eq!(*detail, None),
        other => panic!("expected a memoized reject, got {other:?}"),
    }
    assert_eq!(harness.stages.path_count(), 0);
}

// ── the capability gates (ADR-107 D4) ─────────────────────────────────────

/// The bounded capability refusal a receipt carries, when it carries one.
fn capability_skip(receipt: &CrawlReceipt) -> Option<&Option<String>> {
    match &receipt.outcome {
        RegistrationUnitOutcome::Skip {
            label,
            counts_as_skip,
            detail,
        } => {
            assert_eq!(
                label.as_label(),
                RegistrationOutcome::UnsupportedCapability.as_str(),
                "the refusal keeps the bounded unsupported-capability vocabulary"
            );
            assert!(*counts_as_skip, "a capability refusal is a counted skip");
            Some(detail)
        }
        _ => None,
    }
}

/// A constructed hop whose family declares no settlement composition is refused
/// at the earliest stage that knows the family — after construction, before the
/// verify lifecycle or any registration — and the pool is memoized as
/// unregistrable, so a later route through it spends nothing.
///
/// The Camelot solidly-stable pair lives in the V2 DB table, which is why the
/// family is only knowable post-build (ADR-107 C7: the graph vocabulary is
/// `PoolKind::{V2,V3,V4}`). Its `SolidlyStable` hop is the live refusal today.
#[tokio::test]
async fn an_unsupported_built_family_refuses_before_verify_and_memoizes_the_pool() {
    let mut rpc = FakeRpc::new();
    set_camelot_stable(&mut rpc, USDC, WETH);
    set_v3(&mut rpc, V3_POOL, USDC, WETH);
    let harness = Harness::new(rpc);

    let receipt = harness
        .run(&CrawlUnit {
            seq: 0,
            hops: vec![camelot_stable_hop(), v3_hop()],
            directions: None,
        })
        .await
        .unwrap();
    assert_parity("unsupported_family_post_build", &receipt);
    let detail = capability_skip(&receipt).expect("a capability skip");
    assert!(
        detail
            .as_deref()
            .is_some_and(|detail| detail.contains("solidly-stable")),
        "the structured refusal names the offending family in the log detail: {detail:?}"
    );
    assert_eq!(
        receipt.answering_stage(),
        Some(UnitStage::Eligibility),
        "the refusal answers right after construction"
    );
    assert_eq!(
        harness.verify().verified_pool_count(),
        0,
        "a refused candidate spends no verify work"
    );
    assert_eq!(harness.stages.path_count(), 0, "and registers no path");

    let key = PreBuildIdentity::Address(match camelot_stable_hop() {
        DiscoveredHop::V2 { address } => address,
        _ => unreachable!(),
    });
    assert_eq!(
        harness
            .ledger
            .with(|ledger| ledger.unregistrable_record(Some(&key)).cloned()),
        Some(crate::bot_core::registration_ledger::UnregistrableRecord {
            outcome: RegistrationOutcome::UnsupportedCapability,
            counts_as_skip: true,
        }),
        "the refused pool is a stable pool fact"
    );

    // A later, differently-shaped route through the same pool answers at the
    // memo stage: no second construction, no second evaluation.
    let later = harness
        .run(&CrawlUnit {
            seq: 1,
            hops: vec![v3_hop(), camelot_stable_hop()],
            directions: None,
        })
        .await
        .unwrap();
    assert_parity("unsupported_family_memoized", &later);
    assert_eq!(
        later.split.len(),
        1,
        "the pool fact answered before any build or eligibility work"
    );
    assert!(
        capability_skip(&later).is_some_and(Option::is_none),
        "the memoized record carries the fact, not the first refusal's text"
    );
    assert_eq!(harness.stages.path_count(), 0);

    // One bounded admission outcome for the mixed candidate, no registered
    // path, no solve-eligible count — the report reads the census label.
    let report = fold(std::slice::from_ref(&receipt));
    assert_eq!(report.skip_count, 1, "one bounded skip");
    assert_eq!(report.path_count, 0, "no registered path");
    assert_eq!(
        report
            .skip_reasons
            .get(RegistrationOutcome::UnsupportedCapability.as_str()),
        Some(&1),
        "the breakdown carries the bounded label"
    );
    assert!(report.fold_identities_hold());
}

/// A pool that already completed its verify lifecycle still acquires the
/// permanent capability refusal: verification and composition answer different
/// questions, so the refusal SUPERSEDES the verified standing (the ledger holds
/// ONE standing per key, resolved by evidence) and the pool becomes memoizable.
/// The displaced standing is reported where it happens, and the next candidate
/// through that pool answers at the pre-build memo with no further work.
#[tokio::test]
async fn a_capability_refusal_supersedes_a_verified_standing_and_then_memoizes() {
    let mut rpc = FakeRpc::new();
    set_camelot_stable(&mut rpc, USDC, WETH);
    set_v3(&mut rpc, V3_POOL, USDC, WETH);
    let harness = Harness::new(rpc);

    let key = PreBuildIdentity::Address(match camelot_stable_hop() {
        DiscoveredHop::V2 { address } => address,
        _ => unreachable!(),
    });
    // An operator-driven verify lifecycle already ran over the pool.
    assert_eq!(
        harness
            .ledger
            .with(|ledger| ledger.memoize_verified_pool(key)),
        Ok(StandingDisposition::Settled)
    );
    assert_eq!(
        harness.ledger.with(|ledger| ledger.verified_pool_count()),
        1
    );

    let receipt = harness
        .run(&CrawlUnit {
            seq: 0,
            hops: vec![camelot_stable_hop(), v3_hop()],
            directions: None,
        })
        .await
        .unwrap();
    assert_parity("unsupported_family_post_build", &receipt);
    assert_eq!(
        harness
            .ledger
            .with(|ledger| ledger.pool_standing(&key).cloned()),
        Some(PoolStanding::Unregistrable(UnregistrableRecord {
            outcome: RegistrationOutcome::UnsupportedCapability,
            counts_as_skip: true,
        })),
        "the permanent refusal is evidence that outranks the verified standing"
    );
    assert_eq!(
        harness.ledger.with(|ledger| ledger.verified_pool_count()),
        0,
        "the displaced verified standing is gone"
    );

    // The next, differently-shaped candidate through that pool spends nothing.
    let later = harness
        .run(&CrawlUnit {
            seq: 1,
            hops: vec![v3_hop(), camelot_stable_hop()],
            directions: None,
        })
        .await
        .unwrap();
    assert_parity("unsupported_family_memoized", &later);
    assert_eq!(
        later.split.len(),
        1,
        "the pool fact answered at the memo stage"
    );
    assert_eq!(harness.verify().verified_pool_count(), 0);
    assert_eq!(harness.stages.path_count(), 0);
}

/// ADR-107 C7: today's DB graph yields `PoolKind::{V2,V3,V4}` only, and every
/// one of those families declares settlement composition, so no discovered
/// family can be refused in production. This drives the PRODUCTION
/// pre-construction stage with the taxonomy a future graph family would carry —
/// the real evaluator, no fabricated edge and no second capability registry —
/// and proves the refusal lands before any construction or I/O, settles the
/// pool as a fact, and answers a later candidate from that fact.
#[tokio::test]
async fn the_pre_build_family_gate_refuses_a_future_graph_family_before_any_build() {
    use crate::arb_engine::crawl::admission::PreBuildHop;

    let harness = Harness::new(FakeRpc::new());
    let solidly = PreBuildHop {
        identity: Some(PreBuildIdentity::Address(USDC)),
        hop_type: degenbot_pools::solve::HopType::SolidlyStable,
    };

    let receipt = harness
        .admission
        .pre_build_refusal(0, std::slice::from_ref(&solidly))
        .expect("a solve-only family is refused before construction");
    assert_eq!(receipt.kind(), "skip");
    assert_eq!(receipt.refusal_tag(), Some("unsupported-capability"));
    assert_eq!(
        receipt.answering_stage(),
        Some(UnitStage::Memo),
        "the pre-construction stage answered: no build, verify, register, or solve"
    );
    assert_eq!(receipt.hop_sig, None, "no direction-resolved signature yet");
    assert_eq!(harness.stages.path_count(), 0);
    assert_eq!(
        harness.ledger.with(|ledger| ledger
            .unregistrable_record(Some(&PreBuildIdentity::Address(USDC)))
            .cloned()),
        Some(crate::bot_core::registration_ledger::UnregistrableRecord {
            outcome: RegistrationOutcome::UnsupportedCapability,
            counts_as_skip: true,
        }),
        "the refused hop's pre-build identity is the memo key"
    );

    // A later candidate through that same identity answers from the memo — the
    // pool fact, whatever taxonomy the new edge would carry — before any build.
    let supported_route = [
        PreBuildHop {
            identity: Some(PreBuildIdentity::Address(USDC)),
            hop_type: degenbot_pools::solve::HopType::V2,
        },
        PreBuildHop {
            identity: Some(PreBuildIdentity::Address(WETH)),
            hop_type: degenbot_pools::solve::HopType::V3,
        },
    ];
    let memoized = harness
        .admission
        .pre_build_refusal(1, &supported_route)
        .expect("the pool fact answers before the taxonomy is consulted");
    assert_eq!(memoized.split.len(), 1);
    assert_eq!(
        memoized.refusal_tag(),
        Some("unsupported-capability"),
        "the memo answers the bounded label, without the first refusal's text"
    );

    // A candidate whose hops hold no refused fact proceeds to construction.
    let fresh = [
        PreBuildHop {
            identity: Some(PreBuildIdentity::Address(WETH)),
            hop_type: degenbot_pools::solve::HopType::V2,
        },
        PreBuildHop {
            identity: Some(PreBuildIdentity::Address(DAI)),
            hop_type: degenbot_pools::solve::HopType::V3,
        },
    ];
    assert_eq!(harness.admission.pre_build_refusal(2, &fresh), None);
}

/// A four-hop mixed V2/V3 route: every hop family declares composition, and the
/// command stream has no producer for the sequence (ADR-107 C4). The refusal
/// lands after construction and before verify, and it memoizes the EXACT hop
/// signature — a different route through the same pools is evaluated on its own.
#[tokio::test]
async fn a_route_without_a_producer_refuses_and_memoizes_its_signature() {
    let harness = Harness::new(four_hop_rpc());
    let unit = CrawlUnit {
        seq: 0,
        hops: four_hop_hops(),
        directions: None,
    };
    let receipt = harness.run(&unit).await.unwrap();
    assert_parity("unsupported_route_shape", &receipt);
    let detail = capability_skip(&receipt).expect("a capability skip");
    assert!(
        detail
            .as_deref()
            .is_some_and(|detail| detail.contains("4-hop")),
        "the structured refusal names the route shape: {detail:?}"
    );
    assert_eq!(receipt.answering_stage(), Some(UnitStage::Eligibility));
    assert_eq!(harness.verify().verified_pool_count(), 0);
    assert_eq!(harness.stages.path_count(), 0);
    let sig = receipt
        .hop_sig
        .clone()
        .expect("the candidate held a direction-resolved signature");
    assert!(
        harness.ledger.with(|ledger| ledger.path_rejected(&sig)),
        "the exact hop signature is rejected"
    );

    // The same route re-sighted: the rejected-path memo answers, before verify.
    let again = harness
        .run(&CrawlUnit {
            seq: 1,
            hops: four_hop_hops(),
            directions: None,
        })
        .await
        .unwrap();
    assert_parity("unsupported_route_shape_memoized", &again);
    assert_eq!(again.answering_stage(), Some(UnitStage::Eligibility));

    // The reverse cycle over the same four pools is a different route with a
    // different signature: evaluated independently, so it answers with the
    // bounded refusal rather than the memo's generic deny.
    let mut reversed = four_hop_hops();
    reversed.reverse();
    let other = harness
        .run(&CrawlUnit {
            seq: 2,
            hops: reversed,
            directions: None,
        })
        .await
        .unwrap();
    assert!(capability_skip(&other).is_some(), "{:?}", other.outcome);
    assert_ne!(
        other.hop_sig, receipt.hop_sig,
        "a different route is a different signature"
    );
    assert_eq!(harness.stages.path_count(), 0);
}
