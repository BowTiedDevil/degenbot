//! Shared Tier-3 CL-family (V3 / Pancake-V3) byte-exact swap oracle driver.
//!
//! The Uniswap-V3 oracle (`tier3_v3_pool_swap_vs_revm.rs`) and the PancakeSwap
//! V3 fork oracle (`tier3_pancake_v3_swap_vs_revm.rs`) drive the same pipeline
//! against REAL deployed pool bytecode: the canonical pool contract is
//! deployed via a harness, its `slot0`/`liquidity`/`ticks`/`tickBitmap`
//! storage is seeded slot-for-slot from a `V3PoolState` via the fork's storage
//! encoders, then `pool.swap` is driven and the Rust `v3_simulate_swap` result
//! is compared BYTE-EXACT to the on-chain walk (amounts, post-sqrtPrice,
//! post-tick, post-liquidity).
//!
//! The driver is parameterized only by the fork's harness artifact name and
//! whether the fork harness exceeds EIP-170 ([`V3Fork`]) plus a fork-specific
//! storage seeder, so a CL fork is a one-line declaration and the V3 and
//! Pancake-V3 tests cannot drift (the HRT356 class). Fork-specific event-variant
//! assertions are NOT here: the probe returns the collected swap-tx logs so
//! each test owns its own decoder-level checks.
//!
//! [`ProbeOutcome`] is deliberately verdict-shaped (mirroring the V2 driver's
//! accept/revert/halt tri-state) so the tests can enforce rejection-reason
//! airtightness — see H1 in the ORACLE-HARDENING epic (CMORFZ): a Solidity
//! revert must be matched by an engine rejection, ONLY a verbless Halt (the
//! documented OOG trap, not a math verdict) is a legitimate skip.

#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
//! The module is compiled INTO each consuming test binary (V3, Pancake-V3, …),
//! so an item may legitimately be dead in one consumer but live in another;
//! dead-code warnings are therefore suppressed here rather than re-allowed at
//! every call site.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use alloy::primitives::{aliases::I256, keccak256, Address, Bytes, U128, U256};
use alloy::rpc::types::Log as RpcLog;
use revm::context::TxEnv;
use revm::context_interface::result::{ExecutionResult, Output};
use revm::context_interface::ContextTr;
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

use degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_pools::state_history::{ReorgJournal, V3BlockDelta};
use degenbot_pools::v3_state::{
    PoolTickCoverage, RegistrationLifecycle, TickRangeCache, V3PoolState,
};
use degenbot_pools::TickInfo;

/// Mask selecting the low 128 bits of a U256 (V3 `liquidity`/TickInfo fields).
pub const MASK_128: U256 = U256::from_limbs([u64::MAX, u64::MAX, 0, 0]);

/// Identifies one V3-family fork for the oracle. A fork differs only in its
/// harness artifact (the real forked pool bytecode) and whether the harness's
/// embedded pool-creation code exceeds EIP-170 (24.6KB) — the PancakeSwap fork
/// does; Uniswap's does not.
pub struct V3Fork {
    /// Harness artifact file name (under `tier3-oracle/artifacts/`).
    pub harness_sol: &'static str,
    /// Harness contract name within that artifact.
    pub harness_contract: &'static str,
    /// Raise revm's effective code-size limits (`usize::MAX`) so an
    /// oversized fork harness (mainnet-illegal, revm-runnable) deploys.
    pub raise_eip170: bool,
}

/// The state walk one on-chain swap produced, plus the swap-tx logs for the
/// test's own decoder-level event-variant assertions.
#[derive(Debug)]
pub struct OnChainSwapResult {
    pub amount0: U256,
    pub amount1: U256,
    pub post_sqrt: U256,
    pub post_tick: i32,
    pub post_liq: u128,
    /// Logs emitted by the swap transaction (the test decodes the fork's own
    /// `Swap` event variant from these). Consumed by the Pancake-V3 consumer;
    /// the V3 consumer reads only the state-walk fields, so this is allowed to
    /// be temporarily dead in one consumer.
    #[allow(dead_code)]
    pub logs: Vec<RpcLog>,
}

/// Outcome of a single pristine on-chain swap (deploy → setup → seed → swap →
/// read-back).
#[derive(Debug)]
pub enum ProbeOutcome {
    /// `pool.swap` succeeded; the results are byte-exact against the sealed
    /// state walk.
    Accepted(OnChainSwapResult),
    /// `pool.swap` reverted via Solidity `REVERT`; `reason` is the raw revert
    /// return-data (a math-level verdict — see H1).
    Reverted { reason: Bytes },
    /// The probe pipeline itself broke or the swap **Halted** (OOG / the
    /// documented empty-word-walk gas trap) — no EVM verdict was produced, so
    /// there is nothing to compare against the engine.
    Halted(String),
}

/// First 4 bytes of `keccak256(signature)` — the Solidity function selector.
pub fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    let mut out = [0u8; 4];
    out.copy_from_slice(&h.0[0..4]);
    out
}

/// Repo path to a built harness artifact (foundry `out/<File>.sol/<Contract>.json`).
pub fn harness_artifact_path(file: &str, contract: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tier3-oracle/artifacts")
        .join(file)
        .join(format!("{contract}.json"))
}

/// Load the creation (`bytecode.object`) hex for a harness.
pub fn load_creation_bytecode(file: &str, contract: &str) -> Vec<u8> {
    let path = harness_artifact_path(file, contract);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing harness artifact {}: {e}", path.display()));
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid harness JSON");
    let hex_str = v["bytecode"]["object"]
        .as_str()
        .expect("artifact has bytecode.object (creation)");
    hex::decode(hex_str.trim_start_matches("0x")).expect("hex creation bytecode")
}

/// Abi-encode the CL swap-harness constructor args `(uint24 fee, int24 tickSpacing)`.
pub fn harness_constructor_args(fee: u32, tick_spacing: i32) -> Vec<u8> {
    let mut args = Vec::with_capacity(64);
    args.extend_from_slice(&U256::from(fee).to_be_bytes::<32>());
    args.extend_from_slice(
        &I256::try_from(tick_spacing)
            .unwrap_or(I256::ZERO)
            .into_raw()
            .to_be_bytes::<32>(),
    );
    args
}

/// Abi-encode `swap(bool zeroForOne, int256 amountSpecified, uint160
/// sqrtPriceLimitX96)` for the harness entry (`uint160` is right-padded).
pub fn encode_swap_call(
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: u128,
) -> Vec<u8> {
    let mut call = selector("swap(bool,int256,uint160)").to_vec();
    call.extend_from_slice(&[0u8; 31]);
    call.push(u8::from(zero_for_one));
    call.extend_from_slice(&amount_specified.into_raw().to_be_bytes::<32>());
    call.extend_from_slice(&U256::from(sqrt_price_limit).to_be_bytes::<32>());
    call
}

/// Wrap an alloy primitive log (what revm hands back) into the rpc `Log` shape
/// the degenbot decoders consume (outer block/tx metadata absent in-process).
pub fn to_rpc_log(l: alloy::primitives::Log) -> RpcLog {
    RpcLog {
        inner: l,
        block_hash: None,
        block_number: None,
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    }
}

/// Decode a Solidity `Error(string)` revert payload to its message string.
///
/// Returns `None` for anything that isn't an `Error(string)` ABI encoding
/// (`0x08c379a0`, offset 0x20, length, zero-padded bytes) — e.g. a bare
/// reasonless revert or a `Panic(uint256)`.
pub fn decode_error_string(reason: &[u8]) -> Option<String> {
    if reason.len() < 4 || reason[..4] != [0x08, 0xc3, 0x79, 0xa0] {
        return None;
    }
    let data = &reason[4..];
    if data.len() < 64 {
        return None;
    }
    let len = U256::from_be_bytes::<32>(data[32..64].try_into().ok()?).to::<usize>();
    // Solidity's Error(string) uses offset 0x20, so the payload starts at byte
    // 64 and runs `len` bytes, zero-padded to a 32-byte boundary.
    if 64 + len > data.len() {
        return None;
    }
    Some(String::from_utf8_lossy(&data[64..64 + len]).into_owned())
}

/// Seed a revm `CacheDB`'s pool account from a `V3PoolState` using the fork's
/// storage-slot encoders (a fork-specific responsibility — Uniswap and
/// PancakeSwap differ in their `slot0`/`liquidity`/`ticks`/`tickBitmap` slot
/// indices). The probe calls this right after resolving the pool address.
pub type PoolSeeder = fn(&mut CacheDB<EmptyDB>, Address, &V3PoolState, i32);

/// Drive one on-chain CL `pool.swap` end-to-end against a fresh, self-contained
/// revm `CacheDB`: deploy the harness + real pool, seed storage via `seeder`,
/// call `harness.swap`, and read back the (absolute) flow tuple plus the
/// post-swap `(sqrtPriceX96, tick, liquidity)` and the swap-tx logs. Each call
/// rebuilds the evm + harness so storage is pristine.
///
/// Revert vs halt is kept distinct: a Solidity `Revert` is a math-level verdict
/// (the caller must match it against an engine rejection), a `Halt` (OOG) is
/// the documented fixture gas trap with no verdict.
#[allow(clippy::too_many_arguments)] // fork, seeder, state, fee, spacing, zfo, amount, limit
#[allow(clippy::too_many_lines)] // one logical deploy → setup → seed → swap → read pipeline
#[allow(clippy::match_wildcard_for_single_variants)] // `other` covers only the lone `Success{Create}`
pub fn run_onchain_swap(
    fork: &V3Fork,
    seeder: PoolSeeder,
    state: &V3PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: u128,
) -> ProbeOutcome {
    let mut init_code = load_creation_bytecode(fork.harness_sol, fork.harness_contract);
    init_code.extend_from_slice(&harness_constructor_args(fee, tick_spacing));
    let db = CacheDB::new(EmptyDB::default());
    let mut evm = revm::context::Context::mainnet()
        .with_db(db)
        .build_mainnet();
    evm.ctx.cfg.disable_nonce_check = true;
    if fork.raise_eip170 {
        evm.ctx.cfg.limit_contract_code_size = Some(usize::MAX);
        evm.ctx.cfg.limit_contract_initcode_size = Some(usize::MAX);
    }

    // 1. Deploy harness (mock tokens + the deployer/callback roles).
    let deploy_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Create)
                .gas_limit(16_700_000)
                .data(Bytes::from(init_code))
                .build()
                .expect("deploy tx"),
        )
        .expect("deploy transact");
    let harness = match &deploy_res.result {
        ExecutionResult::Success {
            output: Output::Create(_, Some(addr)),
            ..
        } => *addr,
        other => return ProbeOutcome::Halted(format!("harness deploy failed: {other:?}")),
    };
    evm.commit(deploy_res.state);

    // 2. setupPool -> real pool (a separate CALL so the code-deposit gas isn't
    //    starved by the constructor's 63/64 forwarding).
    let setup_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(16_700_000)
                .data(Bytes::from(selector("setupPool()").to_vec()))
                .build()
                .expect("setupPool tx"),
        )
        .expect("setupPool transact");
    match &setup_res.result {
        ExecutionResult::Success { .. } => {}
        other => return ProbeOutcome::Halted(format!("setupPool failed: {other:?}")),
    }
    evm.commit(setup_res.state);

    // 3. Resolve + seed the pool address.
    let pool_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(2_000_000)
                .data(Bytes::from(selector("pool()").to_vec()))
                .build()
                .expect("pool() tx"),
        )
        .expect("pool() transact");
    let pool = match &pool_res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&b.as_ref()[0..32]);
            Address::from_slice(&buf[12..32])
        }
        other => return ProbeOutcome::Halted(format!("pool() failed: {other:?}")),
    };
    evm.commit(pool_res.state);
    seeder(evm.ctx.db_mut(), pool, state, tick_spacing);

    // 4. Drive the swap.
    let data = encode_swap_call(zero_for_one, amount_specified, sqrt_price_limit);
    let res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(harness))
                .gas_limit(16_700_000)
                .data(Bytes::from(data))
                .build()
                .expect("swap tx"),
        )
        .expect("swap transact");

    let out = match res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            logs,
            ..
        } => {
            evm.commit(res.state);
            (b, logs)
        }
        ExecutionResult::Revert { output, .. } => {
            evm.commit(res.state);
            return ProbeOutcome::Reverted { reason: output };
        }
        ExecutionResult::Halt { reason, .. } => {
            evm.commit(res.state);
            return ProbeOutcome::Halted(format!("swap halted: {reason:?}"));
        }
        other => {
            evm.commit(res.state);
            return ProbeOutcome::Halted(format!("swap returned unexpected result: {other:?}"));
        }
    };

    let (out, logs) = out;
    let mut w = [0u8; 32];
    w.copy_from_slice(&out.as_ref()[0..32]);
    let amount0 = I256::from_raw(U256::from_be_bytes(w)).unsigned_abs();
    w.copy_from_slice(&out.as_ref()[32..64]);
    let amount1 = I256::from_raw(U256::from_be_bytes(w)).unsigned_abs();

    // 5. Post-swap slot0 -> sqrtPriceX96 (word0), tick (word1, sign-extended).
    let s0 = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(pool))
                .gas_limit(2_000_000)
                .data(Bytes::from(selector("slot0()").to_vec()))
                .build()
                .expect("slot0 tx"),
        )
        .expect("slot0 transact");
    let s0_out = match &s0.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => return ProbeOutcome::Halted(format!("slot0() reverted: {other:?}")),
    };
    evm.commit(s0.state);
    let word_at = |i: usize| -> U256 {
        let mut buf = [0u8; 32];
        buf.copy_from_slice(&s0_out.as_ref()[i * 32..(i + 1) * 32]);
        U256::from_be_bytes(buf)
    };
    let post_sqrt = word_at(0) & U256::from_limbs([u64::MAX, u64::MAX, 0xFF, 0]);
    let tick_bits = (word_at(1) & U256::from(u32::MAX)).to::<u32>();
    let post_tick = tick_bits as i32;

    // 6. Post-swap liquidity() -> uint128.
    let liq_res = evm
        .transact(
            TxEnv::builder()
                .kind(TxKind::Call(pool))
                .gas_limit(2_000_000)
                .data(Bytes::from(selector("liquidity()").to_vec()))
                .build()
                .expect("liquidity tx"),
        )
        .expect("liquidity transact");
    let liq_out = match &liq_res.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => b.clone(),
        other => return ProbeOutcome::Halted(format!("liquidity() reverted: {other:?}")),
    };
    evm.commit(liq_res.state);
    let mut lb = [0u8; 32];
    lb.copy_from_slice(&liq_out.as_ref()[0..32]);
    let post_liq = (U256::from_be_bytes(lb) & MASK_128).to::<u128>();

    let rpc_logs = logs.into_iter().map(to_rpc_log).collect();
    ProbeOutcome::Accepted(OnChainSwapResult {
        amount0,
        amount1,
        post_sqrt,
        post_tick,
        post_liq,
        logs: rpc_logs,
    })
}

/// Build a dense multi-position `V3PoolState` at `current_tick`. `k_positions`
/// overlapping positions centered on `current_tick`:
/// `[current_tick - k*spacing, current_tick + k*spacing]` for k=1..=K, each
/// contributing liquidity `liq`. Active liquidity at `current_tick` is
/// `K*liq`; every boundary is a DISTINCT initialized tick, so a swap sinks
/// deterministically into the band instead of chasing an empty word edge.
///
/// `current_tick` must be chosen MID-WORD (its compressed value not at a word
/// boundary) so the first `nextInitializedTickWithinOneWord` lookup finds a
/// same-word initialized tick in the swap direction — this is what avoids the
/// isolated-word-edge degenerate walk (a band anchored at tick 0 still OOGs).
pub fn dense_state(liq: u128, spacing: i32, k_positions: i32, current_tick: i32) -> V3PoolState {
    let sp = U256::from(get_sqrt_ratio_at_tick_internal(current_tick).unwrap());
    let mut tick_data = HashMap::new();
    for k in 1..=k_positions {
        let lower = current_tick - (k * spacing);
        let upper = current_tick + (k * spacing);
        // Lower boundary: crossing downward (zfo) removes this position's liq
        // (net is +liq, so a zfo down-cross subtracts it).
        let entry = tick_data.entry(lower).or_insert_with(|| TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        entry.liquidity_gross = U128::from(entry.liquidity_gross.to::<u128>() + liq);
        entry.liquidity_net = I256::try_from(
            i128::try_from(entry.liquidity_net).unwrap() + i128::try_from(liq).unwrap(),
        )
        .unwrap();
        // Upper boundary: crossing upward adds this position's liq (net -liq).
        let entry = tick_data.entry(upper).or_insert_with(|| TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        entry.liquidity_gross = U128::from(entry.liquidity_gross.to::<u128>() + liq);
        entry.liquidity_net = I256::try_from(
            i128::try_from(entry.liquidity_net).unwrap() - i128::try_from(liq).unwrap(),
        )
        .unwrap();
    }
    V3PoolState {
        sqrt_price_x96: sp,
        liquidity: (k_positions as u128) * liq,
        tick: current_tick,
        update_block: 0,
        tick_data_block: 0,
        initial_state_block: 0,
        tick_data,
        snapshot_seed: None,
        post_drain_snapshot: None,
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: HashSet::new(),
        fetcher: None,
        journal: ReorgJournal::<V3BlockDelta>::new(8),
        state_nonce: 0,
        registration_lifecycle: RegistrationLifecycle::default(),
        cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
    }
}

/// Build a SPARSE V3 state: initialized tick boundaries separated by far more
/// than one bitmap word (`256 * spacing` ticks), so a swap crossing from the
/// current tick to a distant boundary must walk through MANY EMPTY bitmap
/// words. This is the exact topology the Tier-3b oracle exists for — the
/// `compute_tick_ranges` word-boundary flooring divergence manifests only when
/// a range spans uninitialized word boundaries. Two overlapping wide positions
/// around `current_tick` (mid-word, away from a word edge) provide real
/// liquidity the swap sinks into while its boundaries sit in far-apart words.
/// One randomized position in an arbitrary-liquidity-distribution topology.
/// `lower`/`upper` are initialized-tick bounds on the `tick_spacing` grid; the
/// position contributes `liquidity` while price is in `[lower, upper)`.
#[derive(Debug, Clone)]
pub struct ArbV3Position {
    pub lower: i32,
    pub upper: i32,
    pub liquidity: u128,
}

/// Build a `V3PoolState` from an **arbitrary** liquidity distribution: any set
/// of initialized-tick boundaries (clustered, isolated, spanning empty bitmap
/// words) expressed as overlapping/adjacent positions. This is the general
/// builder behind [`dense_state`]/[`sparse_state`] — the concrete topologies
/// are just specific `positions` vectors. The active `liquidity` is derived as
/// the sum of position liquidity covering `current_tick`, and `tick_data` is
/// folded from each position's `lower` (+liq gross/net) / `upper` (+gross,
/// −net) boundaries, so the seeded storage is always internally consistent.
///
/// **On-chain consistency:** a V3 position's boundaries must lie on the
/// `tick_spacing` grid — the on-chain `tickBitmap` compresses ticks via
/// `tick / tickSpacing` (floored), so an off-grid boundary in the submitted
/// `positions` would floor to a different on-chain tick and the pool would
/// walk past it without updating liquidity (a state that cannot exist
/// on-chain). This builder therefore snaps every boundary to a grid multiple
/// (`lower` floored, `upper` ceiled) and drops any position that collapses to
/// zero width after snapping. `current_tick` itself may be off-grid (real
/// pools end swaps at an arbitrary tick) — only boundaries must be on-grid.
///
/// The swap-oracle proptest fuzzes topology through this builder: initialized
/// ticks crossing the current tick, empty regions between far-apart words, and
/// one-sided ranges that force a swap to run to a price limit / bitmap end.
pub fn build_arbitrary_v3_state(
    current_tick: i32,
    tick_spacing: i32,
    positions: &[ArbV3Position],
) -> V3PoolState {
    let sp = U256::from(get_sqrt_ratio_at_tick_internal(current_tick).unwrap());
    // Snap a boundary to the spacing grid: `lower` floors (toward −inf),
    // `upper` ceils, so a snapped position still contains its original span
    // and `lower < upper` is preserved wherever the source span was ≥ spacing.
    let snap_floor = |x: i32| x.div_euclid(tick_spacing) * tick_spacing;
    let snap_ceil = |x: i32| {
        let bumped = x + (tick_spacing - 1);
        bumped.div_euclid(tick_spacing) * tick_spacing
    };
    let mut tick_data = HashMap::new();
    let mut active: u128 = 0;
    for p in positions {
        let lower = snap_floor(p.lower);
        let upper = snap_ceil(p.upper);
        if lower >= upper {
            // Too thin to survive snapping — drop (contributes no real range).
            continue;
        }
        if lower <= current_tick && current_tick < upper {
            active = active.saturating_add(p.liquidity);
        }
        let lo = tick_data.entry(lower).or_insert_with(|| TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        lo.liquidity_gross =
            U128::from(lo.liquidity_gross.to::<u128>().saturating_add(p.liquidity));
        lo.liquidity_net = I256::try_from(
            i128::try_from(lo.liquidity_net)
                .unwrap()
                .saturating_add(i128::try_from(p.liquidity).unwrap()),
        )
        .unwrap();
        let hi = tick_data.entry(upper).or_insert_with(|| TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        hi.liquidity_gross =
            U128::from(hi.liquidity_gross.to::<u128>().saturating_add(p.liquidity));
        hi.liquidity_net = I256::try_from(
            i128::try_from(hi.liquidity_net)
                .unwrap()
                .saturating_sub(i128::try_from(p.liquidity).unwrap()),
        )
        .unwrap();
    }
    V3PoolState {
        sqrt_price_x96: sp,
        liquidity: active,
        tick: current_tick,
        update_block: 0,
        tick_data_block: 0,
        initial_state_block: 0,
        tick_data,
        snapshot_seed: None,
        post_drain_snapshot: None,
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: HashSet::new(),
        fetcher: None,
        journal: ReorgJournal::<V3BlockDelta>::new(8),
        state_nonce: 0,
        registration_lifecycle: RegistrationLifecycle::default(),
        cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
    }
}

pub fn sparse_state(liq: u128, spacing: i32, current_tick: i32) -> V3PoolState {
    let sp = U256::from(get_sqrt_ratio_at_tick_internal(current_tick).unwrap());
    // Word boundary = 256 compressed ticks. Place the two positions' bounds far
    // beyond ±2 words so the walk between them crosses several empty words.
    let far = 300 * spacing; // ~1.2 words past load-bearing range on each side
    let mut tick_data = HashMap::new();
    for (lower, upper, amount) in [
        (current_tick - far, current_tick + far, liq),
        (
            current_tick - far + spacing,
            current_tick + far - spacing,
            liq,
        ),
    ] {
        let entry = tick_data.entry(lower).or_insert_with(|| TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        entry.liquidity_gross = U128::from(entry.liquidity_gross.to::<u128>() + amount);
        entry.liquidity_net = I256::try_from(
            i128::try_from(entry.liquidity_net).unwrap() + i128::try_from(amount).unwrap(),
        )
        .unwrap();
        let entry = tick_data.entry(upper).or_insert_with(|| TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: I256::ZERO,
            block: 0,
        });
        entry.liquidity_gross = U128::from(entry.liquidity_gross.to::<u128>() + amount);
        entry.liquidity_net = I256::try_from(
            i128::try_from(entry.liquidity_net).unwrap() - i128::try_from(amount).unwrap(),
        )
        .unwrap();
    }
    V3PoolState {
        sqrt_price_x96: sp,
        // Active liquidity = both positions cover current_tick.
        liquidity: 2 * liq,
        tick: current_tick,
        update_block: 0,
        tick_data_block: 0,
        initial_state_block: 0,
        tick_data,
        snapshot_seed: None,
        post_drain_snapshot: None,
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: HashSet::new(),
        fetcher: None,
        journal: ReorgJournal::<V3BlockDelta>::new(8),
        state_nonce: 0,
        registration_lifecycle: RegistrationLifecycle::default(),
        cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
    }
}
