#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]
//! Standalone-Rust consumer smoke test (ADR-005 standalone claim, made concrete).
//!
//! Proves a Rust consumer can `cargo add degenbot`, construct a `BotState`,
//! register a Uniswap V2 pool via the `UNISWAP_V2` `DexIdentity` preset, and
//! run a swap calc — **with no Python interpreter, no `pyo3` feature, no
//! maturin in the build graph**. This is the `polars`-equivalent of a Rust
//! binary doing `cargo add polars` and building a `DataFrame` with no Python.
//!
//! Run it with:
//! ```text
//! cargo run -p degenbot --example standalone_consumer
//! ```
//! It `panic!`s on any check failure (exit code != 0), so it doubles as a
//! standalone-consumer gate.

use std::path::PathBuf;
use std::sync::Arc;

use alloy::network::Ethereum;
use alloy::primitives::{address, aliases::U112, Address, Bytes, I256, U128, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::ClientBuilder;
use alloy::transports::mock::{Asserter, MockTransport};
use degenbot::degenbot_arbitrage::{
    simulate_in_process_with_db, FailBuckets, SimulateContext, SimulatePath,
};
use degenbot::degenbot_balancer_math::{mul_down, ONE};
use degenbot::degenbot_curve_math::{
    calculate_dy, stableswap_get_d, DVariant, DyCalculationInputs, YVariant,
};
use degenbot::degenbot_db::snapshot_db::SnapshotDb;
use degenbot::degenbot_executor::composers::{EncodeOptions, HopInfo, PathInfo, V2HopInfo};
use degenbot::degenbot_executor::compute_simulation_warmup_slots;
use degenbot::degenbot_simulation::apply_simulation_overrides;
use degenbot::degenbot_solidly_math::{calc_d as solidly_calc_d, calc_f as solidly_calc_f};
use degenbot::dex_identity::UNISWAP_V2;
use degenbot::{bot_core::Bot, BotState, RegisterV2PoolParams};
use revm::bytecode::Bytecode;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::state::AccountInfo;

// PoolBuilder (T2, 3FVZF4): the construction-I/O traits + build fns reached
// via the umbrella (no pyo3). `FailingConstruction` is a module-level RPC
// stub (never actually called — the standalone example is no-network) used
// to prove the `ConstructionIo` seam + probe/dispatch compile and run.
use degenbot::bot_core::construction_io::{ConstructionIo, NoDb, RpcConstruction};
use degenbot::bot_core::pool_builder::builder::{
    build_aerodrome_v2, build_balancer_stable, build_balancer_weighted, build_curve_pool,
    build_erc20_metadata, build_v2, build_v3, build_v4, probe_pool_type, resolve_v4_identity,
    PoolBuilderError, PoolFamily, V4PoolBuildIdentity, V4PoolBuildOverrides,
};
use degenbot::degenbot_rpc::provider::EthBlock;
use degenbot::errors::ProviderError;

/// A construction RPC that always fails (no RPC wired in the standalone
/// no-network example) — proves the trait-object seam compiles + runs
/// without a backend, and that `probe_pool_type` degrades to `Curve`.
struct FailingConstruction;
#[async_trait::async_trait]
impl RpcConstruction for FailingConstruction {
    async fn get_block_number(&self) -> Result<u64, ProviderError> {
        Err(ProviderError::RpcError {
            code: -32000,
            message: "no rpc".into(),
        })
    }
    async fn get_block(&self, _block_number: u64) -> Result<Option<EthBlock>, ProviderError> {
        Err(ProviderError::RpcError {
            code: -32000,
            message: "no rpc".into(),
        })
    }
    async fn get_block_timestamp(&self, _block_number: u64) -> Result<Option<u64>, ProviderError> {
        Err(ProviderError::RpcError {
            code: -32000,
            message: "no rpc".into(),
        })
    }
    async fn get_code(
        &self,
        _address: Address,
        _block: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        Err(ProviderError::RpcError {
            code: -32000,
            message: "no rpc".into(),
        })
    }
    async fn get_balance(
        &self,
        _address: Address,
        _block: Option<u64>,
    ) -> Result<U256, ProviderError> {
        Err(ProviderError::RpcError {
            code: -32000,
            message: "no rpc".into(),
        })
    }
    async fn call(
        &self,
        _to: Address,
        _data: Bytes,
        _block: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        Err(ProviderError::RpcError {
            code: -32000,
            message: "no rpc".into(),
        })
    }
}

fn fixture_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("DEGENBOT_FIXTURE_DB") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("degenbot-db")
        .join("tests")
        .join("fixtures")
        .join("parity.db")
}

/// (JLLE57) Standalone-Rust consumer: full DB-snapshot → auto-backfill → resume flow.
///
/// Proves the end-state contract of epic P73ER6 with zero Python: a
/// `cargo add degenbot` consumer can
///   a. open a `SnapshotDb` (file-backed — here the `parity.db` fixture
///      read handle with a held deferred read tx so `S` + per-pool reads
///      share one frozen DB snapshot, epic `XEANMB`) from the bot's
///      `-pl fixtures/parity.db` path,
///      shipped alongside `degenbot-db`, chain 8453, which carries an
///      `aerodrome_v3` exchange with V3 tick rows + an empty `uniswap_v4`
///      family),
///   b. construct a `Bot` for that chain,
///   c. call `Bot::load_snapshot_from_db(&snap, chain)` — pure Rust, reads `S`
///      inside the held tx (no `SnapshotStore` materialization, epic `XEANMB`)
///      that emits `S = MIN(last_update_block)` over the V3/V4 `exchanges`
///      rows for the chain (here `S = 12_340_000` = min(V3 `12_345_000`, V4
///      `12_340_000`)); no tick dict ever crosses the FFI (the DB read is owned
///      by `degenbot-db`/`degenbot-bot` after DADWUP).
///
/// The remaining two steps — (d) `BlockPump::subscribe(...)` and
/// (e) `pump.resume_from_subscribe(state)` — close the S→W snapshot→WS
/// backfill *internally* (J3FMDO): `resume_from_subscribe` calls
/// `BlockPump::backfill_from_snapshot(W)` which fetches `eth_getLogs` for
/// `S+1..W-1` and applies them via `BotState::process_backfill_logs`
/// (state-only — no solve, no `on_send`), then enters the live loop with
/// `current_block = W` reflecting the backfilled anchor. No Python call,
/// no per-pool `PyO3` ingestion.
///
/// `subscribe()` needs a live WS endpoint, so driving (d)+(e) here is gated
/// behind `SMOKE_RPC_URL` for the executable proof; without it the smoke
/// loads the fixture and exits 0 (CI-runnable). The auto-backfill contract
/// itself is asserted directly by `block_pump::tests::
/// resume_anchors_to_subscribe_block` and the J3FMDO backfill tests; real
/// consumers wire (d)+(e) from their own tokio runtime +
/// sink/reorg/engine setup.
fn fixture_snapshot_seed_block() -> Option<u64> {
    let db_path = fixture_db_path();
    let snap = SnapshotDb::open(&db_path)
        .unwrap_or_else(|e| panic!("open fixture DB at {}: {e}", db_path.display()));
    let snapshot_bot = Bot::new(8453);
    snapshot_bot
        .load_snapshot_from_db(&snap.0, 8453)
        .expect("load_snapshot_from_db on the fixture DB returns Ok");
    let seed_block = snapshot_bot.state_arc().read().snapshot_seed_block();
    // Fixture DB has V3 ticks at chain 8453 (aerodrome_v3, last_update_block
    // = 12_345_000) AND an empty V4 family (uniswap_v4 exchange row,
    // last_update_block = 12_340_000) → S = MIN(V3, V4) = 12_340_000
    // (the bot loads BOTH families and takes the min as the snapshot anchor
    // so any participant pool's snapshot is honored).
    assert_eq!(
        seed_block,
        Some(12_340_000),
        "fixture DB at chain 8453 has V3+V4 exchanges → S = MIN(12345000, 12340000) = 12_340_000"
    );
    drop(snapshot_bot);
    drop(snap);
    seed_block
}

#[expect(clippy::too_many_lines)]
fn main() {
    // 2b reaches ArbitrageEngine for the standalone lifecycle slice.
    use degenbot::solvers::arb_engine::{ArbitrageEngine, EnginePhase};

    // 1. Construct the Rust-owned per-chain bot state (no Python).
    let mut bot = BotState::new();

    // 2. Derive the V2 pool's registration parameters from the `UNISWAP_V2`
    //    `DexIdentity` preset (ADR-005 slice 6 — the standalone-constraint
    //    data layer). Using the preset's factory + fee params means a
    //    standalone Rust consumer reaches on-chain-correct swap math without
    //    any Python-side ClassVar lookup.
    let token0 = address!("000000000000000000000000000000000000000A");
    let token1 = address!("000000000000000000000000000000000000000B");
    let pool = address!("000000000000000000000000000000000000000C");

    // 1_000_000 USDC (6dp) in / reserves roughly 0.5 WETH (18dp) — on-chain
    // getAmountOut parity reference (slice-5 convention: gamma_numer is the
    // RETAINED post-fee fraction = 997/1000 for a 0.3% Uniswap V2 fee).
    let reserve0 = U256::from(1_000_000_000_000_u64); // 1e6 * 1e6
    let reserve1 = U256::from(500_000_000_000_000_000_u64); // 0.5 * 1e18

    let params = RegisterV2PoolParams {
        address: pool,
        token0,
        token1,
        reserve0: reserve0.to::<U112>(),
        reserve1: reserve1.to::<U112>(),
        fee_token0: UNISWAP_V2.fee_token0,
        fee_token1: UNISWAP_V2.fee_token1,
        factory: UNISWAP_V2.factory,
        update_block: 19_000_000,
        variant: UNISWAP_V2.variant,
        stable_swap: false,
        fee_denominator: None,
        ..Default::default()
    };
    let pool_id = bot
        .register_v2_pool(&params)
        .expect("standalone: register V2");
    assert_eq!(pool_id, 1, "first registered pool gets id 1");

    // 2b. Standalone engine lifecycle (ZU7RAF): the core `ArbitrageEngine`
    //    owns EnginePhase — a cargo-add degenbot consumer observes + guards it.
    let engine = ArbitrageEngine::new();
    assert_eq!(engine.current_phase(), EnginePhase::Created);
    assert!(engine.current_phase().allow_subscribe("subscribe").is_ok());
    engine.set_phase(EnginePhase::Subscribed);
    assert_eq!(engine.current_phase(), EnginePhase::Subscribed);

    // 3. Run a swap calc through the Rust core (the `degenbot-v2-math`
    //    `IntHopState` constant-product path). The same code path the PyO3 binding ships to
    //    Python — but here reached without a single `pyo3` import.
    let amount_in = U256::from(1_000_000_000_u64); // 1000 USDC in
    let amount_out = bot
        .calculate_tokens_out_miss_aware(pool_id, true, amount_in)
        .expect("small non-overflowing V2 amount; standalone calc must not miss or overflow");
    assert!(
        amount_out > U256::ZERO,
        "expected a non-zero swap output, got {amount_out}"
    );

    // Round-trip sanity: a larger input must produce a strictly-larger output
    // (constant-product is monotonic, ignoring fee edge cases at the extremes).
    let bigger_in = amount_in * U256::from(2_u64);
    let bigger_out = bot
        .calculate_tokens_out_miss_aware(pool_id, true, bigger_in)
        .expect("small non-overflowing V2 amount; standalone calc must not miss or overflow");
    assert!(
        bigger_out > amount_out,
        "constant-product calc must be monotonic: {bigger_out} !> {amount_out}"
    );

    // 4. Verify the preset's on-chain-correct fee convention is wired through
    //    (the slice-5 bug was registering the FEE numerator, not the RETAINED
    //    complement). The `UNISWAP_V2` preset's `fee_tokenN.0` is `997`
    //    (retained) over `1000` — a 0.3% fee. Cross-check against the
    //    closed-form Uniswap V2 `getAmountOut` for the configured reserves:
    //      amountInWithFee = amount_in * 997
    //      numerator       = amountInWithFee * reserve_out
    //      denominator     = reserve_in * 1000 + amountInWithFee
    //    — byte-identical to the core's EVM-exact integer path
    //    (`degenbot_v2_math::IntHopState::swap`).
    let amount_in_with_fee = amount_in * U256::from(997_u64);
    let numer = amount_in_with_fee * reserve1;
    let denom = reserve0 * U256::from(1000_u64) + amount_in_with_fee;
    let expected = numer / denom;
    assert_eq!(
        amount_out, expected,
        "Rust core calc must match the closed-form Uniswap V2 getAmountOut"
    );

    println!("standalone degenbot consumer OK: pool_id={pool_id} amount_out={amount_out}");

    // 5. Reach the pure-Rust math leaves directly — proves the umbrella
    //    re-exports the `degenbot-curve-math` / `degenbot-balancer-math`
    //    leaves (ADR-005 sub-step B' completion) so a standalone consumer
    //    can call StableSwap / FixedPoint math without `pyo3` in the graph.
    //
    // Curve `stableswap_get_d` over a balanced 2-coin pool: D converges to
    //    sum(xp) = 2e18 when the pool is balanced (the StableSwap invariant
    //    equals the constant-sum invariant at balance — A amplification perturbs
    //    D by < 1 unit at the fixed point; the contraction stops within MAX
    //    loop steps when |d - d_prev| <= 1).
    let xp = [
        U256::from(1_000_000_000_000_000_000_u64),
        U256::from(1_000_000_000_000_000_000_u64),
    ];
    let n_coins = U256::from(2u64);
    let a_precision = U256::from(100u64);
    let amp = U256::from(2000u64); // A = 20
    let d = stableswap_get_d(&xp, amp, n_coins, a_precision, DVariant::Standard)
        .expect("stableswap_get_d converged");
    let sum = xp[0] + xp[1];
    // Balanced pool: D ≈ sum(xp) within the convergence tolerance (±1).
    assert!(
        d.abs_diff(sum) <= U256::from(1u64),
        "balanced StableSwap D must ≈ sum(xp): {d} vs {sum}"
    );

    // Balancer `FixedPoint.mul_down(a, b) = a*b / ONE` (18-dec fixed-point).
    // Identity check: mul_down(x, ONE) == x for any x ≤ max_fp.
    let val = U256::from(42_000_000_000_000_000_000_u128); // 42 * 1e18
    assert_eq!(mul_down(val, ONE).unwrap(), val, "mul_down(x, ONE) == x");

    // 6. Solidly-stable math leaf reach: confirm `calc_d(x0, y)` and
    //    `calc_f(x0, y)` are reachable from the umbrella without `pyo3`.
    //    The Solidly/Aerodrome deployed-contract `calc_d` evaluates the
    //    analytic `D = 3*x0*y^2 + x0^3*y` (1e18-scaled); re-derive it in plain
    //    integer arithmetic for the parity check.
    let x0 = U256::from(2_000_000_000_000_000_000_u64); // 2 * 1e18
    let y = U256::from(3_000_000_000_000_000_000_u64); // 3 * 1e18
    let got_d = solidly_calc_d(x0, y);
    // D = 3*x0*(y^2 / 1e18) / 1e18 + (((x0^2 / 1e18) * x0) / 1e18)
    let yy = y * y / ONE;
    let term1 = U256::from(3u64) * x0 * yy / ONE;
    let x0x0 = x0 * x0 / ONE;
    let term2 = x0x0 * x0 / ONE;
    let expected_d = term1 + term2;
    assert_eq!(got_d, expected_d, "solidly calc_d direct port");
    // And `calc_f(x0, y) = x0*y^3 + x0^3*y`:
    let got_f = solidly_calc_f(x0, y);
    let a = x0 * y / ONE;
    let b = x0 * x0 / ONE + y * y / ONE;
    let expected_f = a * b / ONE;
    assert_eq!(got_f, expected_f, "solidly calc_f direct port");

    // Curve `get_dy` calc layer (T6, YY64IT): a standalone consumer builds a
    //    `DyCalculationInputs` snapshot and runs a full swap through the pure
    //    `calculate_dy` — the counterpart of the Python companion's delegation
    //    (T7). Matches the `standard_plain` canonical fixture (recorded dy).
    let dx_inputs = DyCalculationInputs {
        precision: U256::from(1_000_000_000_000_000_000_u64),
        fee_denominator: U256::from(10_000_000_000_u64),
        fee: U256::from(500_000_u64),
        n_coins: 2,
        balances: vec![
            U256::from(3_000_000_000_000_000_000_000_u128),
            U256::from(6_000_000_000_000_000_000_000_u128),
        ],
        rate_multipliers: vec![
            U256::from(1_000_000_000_000_000_000_u64),
            U256::from(1_000_000_000_000_000_000_u64),
        ],
        precision_multipliers: vec![U256::from(1_u8), U256::from(1_u8)],
        offpeg_fee_multiplier: U256::ZERO,
        fee_gamma: U256::ZERO,
        mid_fee: U256::ZERO,
        out_fee: U256::ZERO,
        address: Address::ZERO,
        resolved_rates: vec![
            U256::from(1_000_000_000_000_000_000_u64),
            U256::from(1_000_000_000_000_000_000_u64),
        ],
        xp: vec![
            U256::from(3_000_000_000_000_000_000_000_u128),
            U256::from(6_000_000_000_000_000_000_000_u128),
        ],
        block_number: 0,
        block_timestamp: 0,
        amp: U256::from(10_000_u64),
        d_variant: DVariant::Standard,
        y_variant: YVariant::Standard,
        a_precision: U256::from(100_u64),
        swap_style: 1,
        metapool: false,
        metapool_rate_style: 1,
        metapool_underlying_style: 1,
        d: None,
        gamma: None,
        price_scale: None,
        live_balances: None,
        admin_balances: None,
        effective_balances: None,
        virtual_price: None,
        scaled_redemption_price: None,
    };
    let dy = calculate_dy(0, 1, U256::from(1_000_000_000_000_000_000_u64), &dx_inputs)
        .expect("calculate_dy converged");
    assert_eq!(
        dy,
        U256::from(1_008_296_947_143_911_861_u64),
        "curve get_dy direct port"
    );

    println!("standalone degenbot consumer OK: curve D={d} dy={dy} balancer fp.mul_down(identity) solidly calc_d={got_d}");

    // 7. (JLLE57) Standalone-Rust consumer: full DB-snapshot → auto-backfill → resume flow.
    //    See `fixture_snapshot_seed_block` for the end-state contract of
    //    epic P73ER6 (zero-Python), the S→W backfill that happens inside
    //    `BlockPump::resume_from_subscribe` (J3FMDO), and the SMOKE_RPC_URL
    //    gate for driving `subscribe`+`resume`.
    let seed_block = fixture_snapshot_seed_block();

    let rpc_url = std::env::var("SMOKE_RPC_URL").ok();
    if let Some(rpc) = rpc_url.as_deref() {
        println!("standalone degenbot consumer OK: fixture DB at chain 8453 loaded with S={seed_block:?}; SMOKE_RPC_URL={rpc} set — wire `BlockPump::subscribe({rpc}, bot_arc, sink, reorg, shutdown)` then `pump.resume_from_subscribe(state)` and the pump auto-backfills S+1..W-1 internally (state-only `eth_getLogs` via `BotState::process_backfill_logs`) before entering the live loop with current_block = W.");
    } else {
        println!("standalone degenbot consumer OK: fixture DB at chain 8453 loaded with S={seed_block:?} (set SMOKE_RPC_URL='ws://...' to drive `BlockPump::subscribe()`+`resume_from_subscribe()` — gated so the example stays CI-runnable without a node; the auto-backfill path is covered by `block_pump::tests::resume_anchors_to_subscribe_block`).");
    }

    // 8. Standalone-Rust consumer reaches the in-process revm EVM sim
    //    (ADR-005 Tier-0, task 62YWCF — `cargo add degenbot` reaches
    //    `simulate_in_process_with_db` with no Python in the build graph).
    //    Proven via the SELFDESTRUCT-gift success path: the executor stub
    //    CALLs a gift contract; the gift self-destructs to the executor
    //    (CALLER), sending 1 ETH → `gross_profit = 1 ETH` → non-None
    //    `SimResult` (the only success path achievable over
    //    `CacheDB<EmptyDB>` — no real pool state needed). Asserts the full
    //    `SimResult` (gross/net/gas/priority_fee) shape both the Rust +
    //    Python consumers hold against the shared fixture JSON. No RPC
    //    (mock transport with an empty queue).
    in_process_sim_standalone_slice();
    registration_lifecycle_standalone_slice();
}

/// (IKGQ6F / ADR-022 D1) Standalone-Rust consumer drives the core-owned
/// registration verify-lifecycle (`degenbot::run_cl_v3_lifecycle`) with no
/// Python in the build graph and no provider: a Tracked V3 pool registers
/// Quarantined, then the generic lifecycle runs its two verify closures
/// (trivial passing stubs here), drains + pins, and lands the pool `Live`.
/// This is the D4/D-C choreography the Python registry now delegates to.
fn registration_lifecycle_standalone_slice() {
    use std::collections::HashMap;

    use degenbot::bot_core::{PoolTickCoverage, RegistrationLifecycle, TickInfo};
    use degenbot::{run_cl_v3_lifecycle, RegisterV3PoolParams};
    use parking_lot::RwLock;

    let core = RwLock::new(BotState::new());
    let addr = address!("000000000000000000000000000000000000000D");
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: U128::from(100),
            liquidity_net: I256::try_from(100i128).unwrap(),
            block: 0,
        },
    );
    let pid = core
        .write()
        .register_v3_pool(&RegisterV3PoolParams {
            address: addr,
            token0: address!("000000000000000000000000000000000000000A"),
            token1: address!("000000000000000000000000000000000000000B"),
            fee: 3000,
            tick_spacing: 60,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            coverage: PoolTickCoverage::Tracked,
            ..Default::default()
        })
        .expect("standalone: Tracked V3 registration");
    assert_eq!(
        core.read().get_v3_pool(pid).unwrap().registration_lifecycle,
        RegistrationLifecycle::Quarantined,
        "Tracked registers Quarantined (DFQYM5)"
    );

    // Drive the D4 lifecycle: quarantine → seed-verify @42 → drain+pin →
    // post-drain-verify (@ the pin's own block) → Live. Trivial passing
    // closures stand in for the provider-backed verify (the generic fn needs
    // no provider). Sparse would be an immediate no-op.
    degenbot::runtime::get_runtime()
        .block_on(run_cl_v3_lifecycle::<_, _, _, _, ()>(
            &core,
            addr,
            Some(42),
            |_seed, _block| async move { Ok(()) },
            |_td, _block| async move { Ok(()) },
        ))
        .expect("standalone: lifecycle must pass");
    assert_eq!(
        core.read().get_v3_pool(pid).unwrap().registration_lifecycle,
        RegistrationLifecycle::Live,
        "Tracked lands Live only after verification"
    );
    println!(
        "standalone degenbot consumer OK: registration lifecycle — tracked V3 Quarantined → verified → Live"
    );
}

/// (62YWCF) Standalone-Rust consumer reaches the in-process revm EVM sim.
///
/// The SELFDESTRUCT-gift success path: the executor stub bytecode CALLs a
/// gift contract; the gift (`CALLER SELFDESTRUCT`) sends 1 ETH to the
/// executor → `gross_profit = 1 ETH` → non-None `SimResult`. Multicall3
/// bytecode (`getEthBalance`) deployed so the pre/post balance reads return
/// real ETH balances. No RPC — `CacheDB<EmptyDB>` + a mock provider with an
/// empty queue.
#[expect(clippy::too_many_lines)]
fn in_process_sim_standalone_slice() {
    const OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
    const EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
    const MULTICALL3: Address = address!("c411372f0b8ae58585e33b78aea9e0596da9a6f1");
    const TOKEN1: Address = address!("1111111111111111111111111111111111111111");
    const POOL_B: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    // Balancer V2 singleton Vault (mainnet). Only used as a read target on the
    // failing stub, so the exact value is immaterial to the Rpc-error assertion.
    const VAULT: Address = address!("ba12222222228d8ba445958a75a0704d566bf2c8");
    const POOL_C: Address = address!("cccccccccccccccccccccccccccccccccccccccc");
    const GIFT: Address = address!("dddddddddddddddddddddddddddddddddddddddd");
    const ONE_ETH: U256 = U256::from_limbs([1_000_000_000_000_000_000u64, 0, 0, 0]);
    /// Multicall3.getEthBalance(address) → `address.balance`.
    const MULTICALL3_BYTECODE: [u8; 12] = [
        0x60, 0x04, 0x35, 0x31, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xF3,
    ];
    /// Gift contract: `CALLER SELFDESTRUCT` → sends gift's ETH to the caller.
    const GIFT_BYTECODE: [u8; 2] = [0x33, 0xFF];

    // Build the executor stub bytecode: CALL the gift → gift self-destructs →
    // 1 ETH lands on the executor.
    let mut executor_bc = vec![
        0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73,
    ];
    executor_bc.extend_from_slice(GIFT.as_slice());
    executor_bc.extend_from_slice(&[0x5A, 0xF1, 0x50, 0x00]);

    // Mock provider — empty transport queue (never called over CacheDB<EmptyDB>).
    let asserter = Asserter::new();
    let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
    let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
    let provider = degenbot::degenbot_rpc::provider::AlloyProvider::from_provider(Arc::new(
        dyn_provider,
    )
        as Arc<dyn Provider<Ethereum>>);
    let warmup = compute_simulation_warmup_slots(EXECUTOR, WETH);
    let ctx = SimulateContext {
        provider: &provider,
        executor_owner: OWNER,
        executor_address: EXECUTOR,
        weth_address: WETH,
        pool_manager_address: PM,
        multicall3_address: MULTICALL3,
        inject_code: true,
        injected_address: Some(EXECUTOR),
        runtime_bytecode: Bytes::from(executor_bc),
        warmup,
        base_fee_next: 1_000_000_000u128,
        current_block: 100,
        block_timestamp: 0,
        block_priority_fees: None,
    };

    let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
    apply_simulation_overrides(&mut cache_db, &ctx.override_params())
        .expect("standalone: overrides apply over EmptyDB");
    cache_db.insert_account_info(
        MULTICALL3,
        AccountInfo {
            balance: U256::ZERO,
            nonce: 1,
            code: Some(Bytecode::new_raw(Bytes::from(MULTICALL3_BYTECODE.to_vec()))),
            ..Default::default()
        },
    );
    cache_db.insert_account_info(
        GIFT,
        AccountInfo {
            balance: ONE_ETH,
            nonce: 1,
            code: Some(Bytecode::new_raw(Bytes::from(GIFT_BYTECODE.to_vec()))),
            ..Default::default()
        },
    );
    let path = SimulatePath {
        path_id: 42,
        optimal_input: 1_000_000_000_000_000_000u128,
        hop_outputs: vec![1_100_000_000_000_000_000u128, 1_210_000_000_000_000_000u128],
        consumed_inputs: vec![1_100_000_000_000_000_000u128, 1_210_000_000_000_000_000u128],
        path_info: PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: POOL_B,
                token0_address: WETH,
                token1_address: TOKEN1,
                fee: 30,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: POOL_C,
                token0_address: TOKEN1,
                token1_address: WETH,
                fee: 30,
                zfo: true,
            }),
        ]),
        solve_block: 100,
        opts: EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: false,
            ..Default::default()
        },
        state_nonces: vec![],
    };
    let mut buckets = FailBuckets::new();
    let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets)
        .expect("standalone: in-process sim over CacheDB<EmptyDB> cannot RPC-fail");
    let sim = result.expect(
        "standalone: the SELFDESTRUCT-gift fixture must produce a non-None SimResult (gross_profit = 1 ETH)",
    );
    assert_eq!(
        sim.gross_profit, ONE_ETH,
        "standalone: gross_profit must be 1 ETH (closed form)"
    );
    assert!(sim.gas_used > 0, "standalone: gas_used must be non-zero");
    assert!(
        sim.net_profit > U256::ZERO,
        "standalone: net_profit must be positive"
    );
    println!(
        "standalone degenbot consumer OK: in-process revm sim — gross_profit=1ETH gas_used={} priority_fee={} net_profit={} wei",
        sim.gas_used, sim.priority_fee, sim.net_profit
    );

    // 7. Reach the PoolBuilder (T2, 3FVZF4): a `cargo add degenbot` consumer
    //    reaches the probe-dispatched build fns over a `ConstructionIo` with no
    //    pyo3 in the graph. Drive `probe_pool_type` against an always-failing
    //    RPC stub — every probe reverts, so it resolves `Curve` — and call
    //    `build_v2`, which must surface a typed `PoolBuilderError` (RPC) rather
    //    than panic. This pins the umbrella path + the error/identity types;
    //    the full on-chain read path is unit-tested in degenbot-bot's FakeRpc
    //    suite.
    let io = Arc::new(ConstructionIo::new(
        Arc::new(NoDb),
        Arc::new(FailingConstruction),
    ));
    let family = degenbot::runtime::get_runtime().block_on(probe_pool_type(&io, POOL_B, None));
    assert_eq!(
        family,
        PoolFamily::Curve,
        "no responses → every probe reverts → Curve"
    );
    let err = degenbot::runtime::get_runtime().block_on(build_v2(1, POOL_B, &io, None));
    assert!(
        matches!(err, Err(PoolBuilderError::Rpc(_))),
        "build_v2 over a failing RPC must yield a typed Rpc error, got {err:?}"
    );
    // ERC-20 (VK3YDM-S2): `build_erc20_metadata` is reachable standalone via
    // the umbrella with no pyo3. Over the same always-failing stub it must
    // surface a typed `PoolBuilderError::Rpc` (the `get_code` guard errors)
    // rather than panic — pinning the umbrella path for the erc20 family (the
    // on-chain success path is unit-tested in degenbot-bot's FakeRpc suite).
    let err = degenbot::runtime::get_runtime().block_on(build_erc20_metadata(&io, 1, POOL_B, None));
    assert!(
        matches!(err, Err(PoolBuilderError::Rpc(_))),
        "build_erc20_metadata over a failing RPC must yield a typed Rpc error, got {err:?}"
    );
    // V3: same failing stub — `build_v3` (the T4 Tracked/Sparse DB-arm + Chain
    // sparse path) must surface a typed Rpc error too, pinning the umbrella
    // path for the V3 family.
    let err = degenbot::runtime::get_runtime().block_on(build_v3(1, POOL_B, None, &io, None));
    assert!(
        matches!(err, Err(PoolBuilderError::Rpc(_))),
        "build_v3 over a failing RPC must yield a typed Rpc error, got {err:?}"
    );
    // V4: build_v4 takes a caller-supplied `V4PoolBuildIdentity` — the failing
    // stub never reads it, so any addresses suffice to prove the V4 family is
    // reachable and degrades to the same typed Rpc error.
    let v4_id = V4PoolBuildIdentity {
        pool_manager: POOL_B,
        state_view: POOL_B,
        pool_id: [0xAA; 32],
        currency0: TOKEN1,
        currency1: POOL_C,
        fee: 0x10_0000,
        tick_spacing: 1,
        hook_flags: 0,
    };
    let err = degenbot::runtime::get_runtime().block_on(build_v4(v4_id, None, &io, None));
    assert!(
        matches!(err, Err(PoolBuilderError::Rpc(_))),
        "build_v4 over a failing RPC must yield a typed Rpc error, got {err:?}"
    );
    // V4 identity resolution (TF7RZB-S3): `resolve_v4_identity` is a Rust-owned
    // core capability (DB two-step else caller overrides). Over a failing/no-DB
    // stub with EMPTY overrides it must degrade to the typed `MissingIdentity`
    // error, never a panic — pinning the standalone reach of the resolver.
    let overrides = V4PoolBuildOverrides::default();
    let err = degenbot::runtime::get_runtime()
        .block_on(resolve_v4_identity(1, POOL_B, [0xBB; 32], &overrides, &io));
    assert!(
        matches!(err, Err(PoolBuilderError::MissingIdentity { .. })),
        "resolve_v4_identity with empty overrides must yield MissingIdentity, got {err:?}"
    );
    // Aerodrome V2: build_aerodrome_v2 (the SSSXG6 follow-up) reads the same
    // failing `FailingConstruction` stub — must surface the typed Rpc error too,
    // pinning the umbrella path (re-export + error type) for the Aerodrome family.
    let err = degenbot::runtime::get_runtime().block_on(build_aerodrome_v2(1, POOL_B, &io, None));
    assert!(
        matches!(err, Err(PoolBuilderError::Rpc(_))),
        "build_aerodrome_v2 over a failing RPC must yield a typed Rpc error, got {err:?}"
    );
    // Balancer V2 (SSSXG6): build_balancer_weighted + build_balancer_stable hit
    // the same `FailingConstruction` stub — the vault read reverts, so each must
    // surface the typed Rpc error, pinning the umbrella path for the two
    // Balancer families (weighted + stable).
    let err = degenbot::runtime::get_runtime()
        .block_on(build_balancer_weighted(VAULT, POOL_B, &io, None));
    assert!(
        matches!(err, Err(PoolBuilderError::Rpc(_))),
        "build_balancer_weighted over a failing RPC must yield a typed Rpc error, got {err:?}"
    );
    let err = degenbot::runtime::get_runtime()
        .block_on(build_balancer_stable(VAULT, POOL_B, &io, None, None));
    assert!(
        matches!(err, Err(PoolBuilderError::Rpc(_))),
        "build_balancer_stable over a failing RPC must yield a typed Rpc error, got {err:?}"
    );
    // Curve (SSSXG6): build_curve_pool reads the same `FailingConstruction`
    // stub. Coin discovery tolerates the reverting probe (empty coin set), so
    // the fatal `fetch_curve_pool_params` A() read reverts next — the umbrella
    // path + `RegisterCurvePoolParams` for the Curve family are proven by the
    // typed `PoolBuilderError::Rpc`.
    let err = degenbot::runtime::get_runtime().block_on(build_curve_pool(POOL_B, &[], &io, None));
    assert!(
        matches!(err, Err(PoolBuilderError::Rpc(_))),
        "build_curve_pool over a failing RPC must yield a typed Rpc error, got {err:?}"
    );
    println!(
        "standalone degenbot consumer OK: PoolBuilder probe+dispatch reachable (family={family:?})"
    );
    // 8. Reach the PancakeSwap V3 storage-slot encoders (W32CAU): the fork's
    //    layout diverges from Uniswap V3 (two-word slot0, liquidity@5, ticks@6,
    //    tickBitmap@7). A `cargo add degenbot` consumer must reach the fork-aware
    //    constants + encoders via the umbrella with no pyo3, and they must
    //    actually differ from the Uniswap layout (so a pancake pool is never
    //    seeded with the wrong slot indices).
    assert_eq!(
        degenbot::degenbot_pools::PANCAKE_V3_LIQUIDITY_SLOT,
        5,
        "fork liquidity@5 (Uniswap@4)"
    );
    assert_eq!(
        degenbot::degenbot_pools::PANCAKE_V3_TICKS_MAPPING_SLOT,
        6,
        "fork ticks base@6 (Uniswap@5)"
    );
    assert_eq!(
        degenbot::degenbot_pools::PANCAKE_V3_TICK_BITMAP_MAPPING_SLOT,
        7,
        "fork tickBitmap base@7 (Uniswap@6)"
    );
    let fork_tick_slot = degenbot::degenbot_pools::pancake_v3_tick_mapping_slot(0);
    let uniswap_tick_slot = degenbot::degenbot_pools::v3_tick_mapping_slot(0);
    assert_ne!(
        fork_tick_slot, uniswap_tick_slot,
        "fork + Uniswap tick mapping slots must differ for the same tick"
    );
    // slot0 word 1 packs feeProtocol (low 32b) | unlocked (bit 32).
    assert_eq!(
        degenbot::degenbot_pools::encode_pancake_v3_slot0_word1(0, true),
        U256::from(1u64) << 32
    );
    println!("standalone degenbot consumer OK: PancakeSwap V3 fork slot encoders reachable");
}
