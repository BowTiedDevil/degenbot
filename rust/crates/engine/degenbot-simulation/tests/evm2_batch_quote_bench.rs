//! The batch-quote pilot: EVM-native path quoting vs the white-box family
//! sims, on live mainnet state, at 10k variations.
//!
//! Two questions, one harness, both answered from the same pinned block:
//!
//! 1. **Cost.** What does a three-leg path quote cost when the EVM executes
//!    the real pool bytecode (warm state, warm code, JIT/AOT where the
//!    backend holds a program) versus the same path quoted by the white-box
//!    family sims (`simulate_swap`, the solver's byte-exact read)?
//! 2. **Parity.** Do the two agree to the wei, variation by variation?
//!
//! # The path
//!
//! `WETH --V3--> USDC --V4--> WETH --V2--> USDC`, every leg a REAL mainnet
//! pool of that family around the USDC/WETH pair, so each leg's output feeds
//! the next leg's input (a genuine composed path quote, not three disjoint
//! quotes). Every leg quotes the pool's LIQUID direction (the V4 pool's
//! WETH→USDC side carries no lower-bound liquidity at this block — a swap
//! there crashes the price to the floor in one step, which is real but not a
//! representative quote cost):
//!
//! - V3: Uniswap V3 USDC/WETH 0.05% pool `0x88e6…5640` (WETH = token1 in, so
//!   `zero_for_one = false`).
//! - V4: Uniswap V4 USDC/WETH 0.3% pool in PoolManager `0x…444c…8A90`
//!   (`poolId = keccak256(poolKey)`, USDC = currency0 in, so
//!   `zero_for_one = true`).
//! - V2: Uniswap V2 USDC/WETH pair `0xB4e1…c9dc` (WETH = token1 in, so
//!   `zero_for_one = false`).
//!
//! # The EVM side
//!
//! Each leg is ONE `eth_call`-shaped frame on the evm2 engine, in the
//! conservative sequential shape (one quote per call — no batch-quoter
//! contract amortizing N quotes, no multi-hop quoter folding the path into a
//! single frame):
//!
//! - V3: `QuoterV2.quoteExactInputSingle(...)` — the quoter drives the pool's
//!   real `swap()` (the full tick walk in pool bytecode) and captures the
//!   amount from the in-frame revert.
//! - V4: `Quoter.quoteExactInputSingle(...)` — the quoter drives
//!   `PoolManager.unlock` + the pool's real `swap()`.
//! - V2: `UniswapV2Router02.getAmountsOut(amountIn, [WETH, USDC])` — the
//!   router executes the pair's `getReserves()` + the constant-product math
//!   in EVM bytecode.
//!
//! # The white-box side
//!
//! The same three legs through `simulate_swap` (the `PoolEntry`-dispatched
//! stateless read `Pool::calculate_tokens_out` is a thin wrapper over): a V2
//! `V2PoolState`, a V3 `V3PoolState` and a V4 `V4PoolState`, each built from
//! the SAME pinned block the EVM reads, each hop's output feeding the next.
//!
//! # Warm state (the honest number)
//!
//! Both engines get one untimed pass over the identical variation stream
//! before the measured pass. The EVM pass's cold reads are counted through
//! `Evm2CountingFrameDb`: the warm pass reports how many forwarded reads it
//! needed to populate the layer caches, and the measured pass ASSERTS zero —
//! so "warm" is a checked claim, not an assumption. The warm pass's wall
//! time is reported as the cold number (it absorbs the one-time slot fetch).
//!
//! # Variations
//!
//! 10,000 deterministic amounts on a recorded seed, spread over
//! `[0.001 ETH, 1 ETH)` in WETH wei, asserted pairwise distinct so no
//! memoization can shortcut the loop.
//!
//! # Running it
//!
//! ```sh
//! cargo test -p degenbot-simulation --features evm2-jit --test evm2_batch_quote_bench \
//!   bench_batch_quote_path -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `DEGENBOT_RPC_HTTP_CHAINID_1` (or `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`)
//! selects the node; the pinned block needs archive state. Without the
//! `evm2-jit` feature the harness still runs and reports the INTERPRET row
//! only.

#![expect(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{address, keccak256, Address, Bytes, U256};
use degenbot_core::fee::fee_ratio;
use degenbot_pools::{
    decode_tick_word, decode_v2_reserves_word, decode_v3_slot0, decode_v4_slot0, simulate_swap,
    v4_liquidity_slot, v4_pool_id, v4_pool_state_base_slot, v4_slot0_slot,
    v4_tick_bitmap_word_slot, v4_tick_mapping_slot, PoolEntry, PoolTickCoverage,
    RegisterV2PoolParams, RegisterV3PoolParams, RegisterV4PoolParams, TickInfo, TickWordKey,
    TickWordSource, V2PoolState, V3PoolState, V4PoolKey, V4PoolState,
};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_rpc::AlloyTickWordSource;
use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_simulation::sim::evm2::frame_replay::{Evm2CountingFrameDb, Evm2ScratchEvm};
use degenbot_simulation::sim::frame_replay::{FrameRpcCounter, ReplayableTx, ScratchBlock};
use degenbot_simulation::sim::RunVerdict;
use degenbot_uniswap::dex_identity::DexVariant;

// ─────────────────────────────────────────────────────────────────────────
// The pinned block + the path
// ─────────────────────────────────────────────────────────────────────────

/// The archived block every read pins: the V4 pool's committed
/// chain-verified state block (`degenbot-pools`' V4 stale-state fixture), so
/// the V4 leg's state is independently corroborated.
const PIN: u64 = 25_635_461;

const USDC: Address = address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
const WETH: Address = address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

const V2_PAIR: Address = address!("0xB4e16d0168e52d35CACD2c6185b44281Ec28C9dc");
const V2_FACTORY: Address = address!("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");

const V3_POOL: Address = address!("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");
const V3_FACTORY: Address = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");
const V3_FEE: u32 = 500;
const V3_TICK_SPACING: i32 = 10;

const V4_MANAGER: Address = address!("0x000000000004444c5dc75cB358380D2e3dE08A90");
const V4_FEE: u32 = 3_000;
const V4_TICK_SPACING: i32 = 60;

/// The deployed quoters/routers the EVM legs execute (canonical mainnet).
const ROUTER_V2: Address = address!("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
const QUOTER_V3: Address = address!("0x61fFE014bA17989E743c5F6cB21bF9697530B21e");
const QUOTER_V4: Address = address!("0x52F0E24D1c21C8A0cB1e5a5dD6198556BD9E1203");

/// The contracts whose bytecode the path's frames execute — the AOT prep
/// set. Whatever is missed here (token implementations behind proxies,
/// delegatecall targets) compiles on first touch during the warm pass.
#[cfg(feature = "evm2-jit")]
const AOT_CONTRACTS: [Address; 8] = [
    V2_PAIR, V3_POOL, ROUTER_V2, QUOTER_V3, V4_MANAGER, QUOTER_V4, USDC, WETH,
];

/// The frame sender: a real funded EOA (the executor owner), so the frame's
/// fee accounting has a balance to draw on. Nonce is read from the pinned
/// parent state.
const SENDER: Address = address!("0x9C56a29c7231974c269E24F9FB3c29203039089E");

const GAS_LIMIT: u64 = 8_000_000;
const MAX_FEE_PER_GAS: u128 = 200_000_000_000;
const MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000;

// ─────────────────────────────────────────────────────────────────────────
// Variations
// ─────────────────────────────────────────────────────────────────────────

/// The recorded variation count.
const VARIATIONS: usize = 10_000;

/// The recorded PRNG seed (splitmix64-style LCG). Re-running with this seed
/// reproduces the exact 10,000-amount stream.
const VARIATION_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// 0.001 ETH in wei — the bottom of the spread.
const MIN_WEI: u64 = 1_000_000_000_000_000;

/// The spread width: `[0.001 ETH, 1 ETH)`.
const SPAN_WEI: u64 = 1_000_000_000_000_000_000 - MIN_WEI;

/// The deterministic variation stream: `VARIATIONS` distinct WETH-wei amounts
/// in `[0.001 ETH, 1 ETH)`.
fn variation_amounts() -> Vec<U256> {
    let mut state = VARIATION_SEED;
    let mut seen: HashSet<u64> = HashSet::with_capacity(VARIATIONS);
    let mut out = Vec::with_capacity(VARIATIONS);
    while out.len() < VARIATIONS {
        // splitmix64 step: full-period 64-bit stream.
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let amount = MIN_WEI + z % SPAN_WEI;
        if seen.insert(amount) {
            out.push(U256::from(amount));
        }
    }
    assert_eq!(
        seen.len(),
        VARIATIONS,
        "every variation must be distinct (no memoization may shortcut the loop)"
    );
    out
}

// ─────────────────────────────────────────────────────────────────────────
// ABI helpers (hand-encoded: one selector + flat word list per call)
// ─────────────────────────────────────────────────────────────────────────

fn selector(sig: &str) -> [u8; 4] {
    let hash = keccak256(sig.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

fn w_uint(v: U256) -> [u8; 32] {
    v.to_be_bytes()
}

fn w_addr(a: Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a.as_slice());
    w
}

/// A 32-byte word for a plain non-negative integer argument.
fn w_small(v: u64) -> [u8; 32] {
    w_uint(U256::from(v))
}

fn calldata(sel: [u8; 4], words: &[[u8; 32]]) -> Bytes {
    let mut out = Vec::with_capacity(4 + 32 * words.len());
    out.extend_from_slice(&sel);
    for word in words {
        out.extend_from_slice(word);
    }
    Bytes::from(out)
}

fn out_words(out: &Bytes) -> Vec<U256> {
    out.as_ref()
        .as_chunks::<32>()
        .0
        .iter()
        .map(|word| U256::from_be_bytes(*word))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────
// The three legs
// ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Leg {
    /// WETH -> USDC, Uniswap V2 (token1 in, so `zero_for_one = false`).
    V2,
    /// WETH -> USDC, Uniswap V3 (token1 in, so `zero_for_one = false`).
    V3,
    /// USDC -> WETH, Uniswap V4 (currency0 in, so `zero_for_one = true`).
    V4,
}

impl Leg {
    /// The path's leg order: the composition chain the quote walks.
    const ALL: [Self; 3] = [Self::V3, Self::V4, Self::V2];

    fn label(self) -> &'static str {
        match self {
            Self::V2 => "V2",
            Self::V3 => "V3",
            Self::V4 => "V4",
        }
    }

    fn zero_for_one(self) -> bool {
        match self {
            Self::V2 | Self::V3 => false,
            Self::V4 => true,
        }
    }

    /// The leg's `eth_call` target.
    fn target(self) -> Address {
        match self {
            Self::V2 => ROUTER_V2,
            Self::V3 => QUOTER_V3,
            Self::V4 => QUOTER_V4,
        }
    }

    /// The leg's quote calldata for `amount_in`.
    fn calldata(self, amount_in: U256) -> Bytes {
        match self {
            Self::V2 => calldata(
                selector("getAmountsOut(uint256,address[])"),
                &[
                    w_uint(amount_in),
                    w_small(0x40),
                    w_small(2),
                    w_addr(WETH),
                    w_addr(USDC),
                ],
            ),
            Self::V3 => calldata(
                selector("quoteExactInputSingle((address,address,uint256,uint24,uint160))"),
                &[
                    w_addr(WETH),
                    w_addr(USDC),
                    w_uint(amount_in),
                    w_small(u64::from(V3_FEE)),
                    w_uint(U256::ZERO),
                ],
            ),
            Self::V4 => calldata(
                selector(
                    "quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))",
                ),
                &[
                    w_small(0x20),
                    w_addr(USDC),
                    w_addr(WETH),
                    w_small(u64::from(V4_FEE)),
                    w_small(V4_TICK_SPACING.unsigned_abs().into()),
                    w_addr(Address::ZERO),
                    w_small(1),
                    w_uint(amount_in),
                    w_small(0x100),
                    w_small(0),
                ],
            ),
        }
    }

    /// The leg's quoted output amount from the call's return data.
    fn output(self, out: &Bytes) -> U256 {
        let words = out_words(out);
        match self {
            // `getAmountsOut` returns uint256[]: offset, length, then the
            // path's amounts ([amountIn, amountOut1, …]) — the quote is the
            // LAST element.
            Self::V2 => {
                assert!(words.len() >= 4, "getAmountsOut shape: {words:?}");
                let len = words[1].to::<usize>();
                assert_eq!(len, 2, "a 2-token path returns 2 amounts");
                words[1 + len]
            }
            // `QuoterV2`: (amountOut, sqrtPriceAfter, ticksCrossed, gasEstimate).
            // `Quoter` (V4): (amountOut, gasEstimate).
            Self::V3 | Self::V4 => {
                assert!(!words.is_empty(), "empty quoter return");
                words[0]
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Live state: one fetch, both engines read it
// ─────────────────────────────────────────────────────────────────────────

/// The three white-box entries built from the pinned block.
struct WhiteBox {
    entries: [PoolEntry; 3],
}

async fn fetch_v3_state(
    provider: &AlloyProvider,
    source: &AlloyTickWordSource,
    tick_spacing: i32,
) -> (U256, u128, i32, hashbrown::HashMap<i32, TickInfo>) {
    let slot0 = decode_v3_slot0(storage_word(
        provider
            .get_storage_at(&V3_POOL, U256::ZERO, Some(PIN))
            .await
            .unwrap(),
    ));
    let liquidity_word = storage_word(
        provider
            .get_storage_at(&V3_POOL, U256::from(4u64), Some(PIN))
            .await
            .unwrap(),
    );
    let liquidity = (liquidity_word & U256::from(u128::MAX)).to::<u128>();

    // The tick map around the current tick: the current word ± 1 (a ≤1 ETH
    // quote moves the price by a fraction of a tick in this pool, so three
    // words is orders of magnitude of headroom).
    let key = TickWordKey::V3 {
        address: V3_POOL,
        tick_spacing,
    };
    let mut tick_data: hashbrown::HashMap<i32, TickInfo> = hashbrown::HashMap::new();
    let center = degenbot_pools::word_of(slot0.tick, tick_spacing);
    for word in (center - 1)..=(center + 1) {
        if let Some(hit) = source.fetch_tick_word(&key, word, PIN).unwrap() {
            for (tick, info) in hit.ticks {
                tick_data.insert(tick, info);
            }
        }
    }
    (slot0.sqrt_price_x96, liquidity, slot0.tick, tick_data)
}

/// A storage read's `B256` answer as the `U256` word the layout decoders take.
fn storage_word(value: alloy::primitives::B256) -> U256 {
    U256::from_be_bytes(value.0)
}

/// One `PoolManager.extsload(bytes32)` word.
async fn v4_extsload(provider: &AlloyProvider, slot: U256) -> U256 {
    let data = calldata(selector("extsload(bytes32)"), &[w_uint(slot)]);
    let out = provider
        .eth_call(&V4_MANAGER, data, Some(PIN))
        .await
        .unwrap();
    out_words(&out)[0]
}

/// The V4 pool's scalars + the tick map around its current tick, read
/// straight from `PoolManager` storage (the same layout the V4 journal
/// extractors use — no `StateView` dependency).
async fn fetch_v4_state(
    provider: &AlloyProvider,
    pool_key: &V4PoolKey,
    tick_spacing: i32,
) -> (U256, u128, i32, u32, u32, hashbrown::HashMap<i32, TickInfo>) {
    let base = v4_pool_state_base_slot(v4_pool_id(pool_key));
    let slot0 = decode_v4_slot0(v4_extsload(provider, v4_slot0_slot(base)).await);
    let liquidity_word = v4_extsload(provider, v4_liquidity_slot(base)).await;
    let liquidity = (liquidity_word & U256::from(u128::MAX)).to::<u128>();

    let mut tick_data: hashbrown::HashMap<i32, TickInfo> = hashbrown::HashMap::new();
    let center = degenbot_pools::word_of(slot0.tick, tick_spacing);
    for word in (center - 1)..=(center + 1) {
        let word_pos = i16::try_from(word).expect("a bitmap word position fits i16");
        let bitmap = v4_extsload(provider, v4_tick_bitmap_word_slot(word_pos, base)).await;
        for bit in 0..256u32 {
            if bitmap.bit(bit as usize) {
                let compressed = word * 256 + i32::try_from(bit).unwrap();
                let tick = compressed * tick_spacing;
                let info_word = v4_extsload(provider, v4_tick_mapping_slot(tick, base)).await;
                let (liquidity_gross, liquidity_net) = decode_tick_word(info_word);
                tick_data.insert(
                    tick,
                    TickInfo {
                        liquidity_gross: alloy::primitives::U128::from(liquidity_gross),
                        liquidity_net,
                        block: PIN,
                    },
                );
            }
        }
    }
    (
        slot0.sqrt_price_x96,
        liquidity,
        slot0.tick,
        slot0.protocol_fee,
        slot0.lp_fee,
        tick_data,
    )
}

async fn fetch_white_box(provider: &AlloyProvider, source: &AlloyTickWordSource) -> WhiteBox {
    // V2: the pair's packed reserves word (slot 8) — the same word the pair's
    // own `getReserves()` reads.
    let reserves_word = storage_word(
        provider
            .get_storage_at(
                &V2_PAIR,
                U256::from(degenbot_pools::V2_RESERVES_SLOT),
                Some(PIN),
            )
            .await
            .unwrap(),
    );
    let reserves = decode_v2_reserves_word(reserves_word);
    println!(
        "  V2  reserves: {} USDC / {} WETH",
        reserves.reserve0, reserves.reserve1
    );
    let v2_params = RegisterV2PoolParams {
        address: V2_PAIR,
        token0: USDC,
        token1: WETH,
        reserve0: reserves.reserve0,
        reserve1: reserves.reserve1,
        fee_token0: fee_ratio(997, 1000),
        fee_token1: fee_ratio(997, 1000),
        factory: V2_FACTORY,
        update_block: PIN,
        variant: DexVariant::UniswapV2,
        deployer: Address::ZERO,
        init_hash: alloy::primitives::B256::ZERO,
        stable_swap: false,
    };
    let (v2_identity, v2_state) = V2PoolState::from_params(&v2_params, 8, (Some(6), Some(18)));

    // V3: slot0 + liquidity + the tick words around the current tick.
    let (v3_sqrt, v3_liquidity, v3_tick, v3_ticks) =
        fetch_v3_state(provider, source, V3_TICK_SPACING).await;
    println!(
        "  V3  sqrtPriceX96={v3_sqrt} tick={v3_tick} liquidity={v3_liquidity} ticks={}",
        v3_ticks.len()
    );
    let v3_params = RegisterV3PoolParams {
        address: V3_POOL,
        token0: USDC,
        token1: WETH,
        fee: V3_FEE,
        tick_spacing: V3_TICK_SPACING,
        factory: V3_FACTORY,
        sqrt_price_x96: v3_sqrt,
        liquidity: v3_liquidity,
        tick: v3_tick,
        tick_data: v3_ticks,
        update_block: PIN,
        tick_data_block: Some(PIN),
        coverage: PoolTickCoverage::Tracked,
        ..Default::default()
    };
    let (v3_identity, v3_state) = V3PoolState::from_params(v3_params, 8);

    // V4: PoolManager storage, protocols fee threaded (a byte-exact V4 hop
    // REQUIRES it — the lpFee alone over-predicts).
    let pool_key = V4PoolKey {
        currency0: USDC,
        currency1: WETH,
        fee: V4_FEE,
        tick_spacing: V4_TICK_SPACING,
        hooks: Address::ZERO,
    };
    let pool_id = v4_pool_id(&pool_key);
    let (v4_sqrt, v4_liquidity, v4_tick, v4_protocol_fee, v4_lp_fee, v4_ticks) =
        fetch_v4_state(provider, &pool_key, V4_TICK_SPACING).await;
    println!(
        "  V4  poolId=0x{} sqrtPriceX96={v4_sqrt} tick={v4_tick} liquidity={v4_liquidity} \
         protocolFee=0x{v4_protocol_fee:06x} lpFee={v4_lp_fee} ticks={}",
        alloy::hex::encode(pool_id),
        v4_ticks.len()
    );
    let v4_params = RegisterV4PoolParams {
        pool_manager: V4_MANAGER,
        pool_id: pool_id.0,
        pool_key: V4PoolKey {
            fee: v4_lp_fee,
            ..pool_key
        },
        state_view: Address::ZERO,
        hook_flags: 0,
        protocol_fee: v4_protocol_fee,
        sqrt_price_x96: v4_sqrt,
        liquidity: v4_liquidity,
        tick: v4_tick,
        tick_data: v4_ticks,
        update_block: PIN,
        tick_data_block: Some(PIN),
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: Vec::new(),
        fetcher: None,
    };
    let (v4_identity, v4_state) = V4PoolState::from_params(v4_params, 8);

    WhiteBox {
        entries: [
            PoolEntry::V2(Box::new((v2_identity, v2_state))),
            PoolEntry::V3(Box::new((v3_identity, v3_state))),
            PoolEntry::V4(Box::new((v4_identity, v4_state))),
        ],
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Passes
// ─────────────────────────────────────────────────────────────────────────

/// One measured pass: wall time, per-leg time, and the per-leg outputs.
struct QuotePass {
    total: Duration,
    legs: [Duration; 3],
    out: [Vec<U256>; 3],
    evm_gas: [u64; 3],
    reads: u64,
}

impl QuotePass {
    fn per_path_us(&self) -> f64 {
        self.total.as_secs_f64() * 1e6 / VARIATIONS as f64
    }

    fn leg_us(&self, leg: Leg) -> f64 {
        self.legs[leg as usize].as_secs_f64() * 1e6 / VARIATIONS as f64
    }
}

/// The white-box pass: the same three legs, composed hop by hop, through the
/// family sims. No I/O, no warm-up concept — but it takes one untimed pass so
/// the comparison is pass-2 vs pass-2.
fn white_box_pass(entries: &[PoolEntry; 3], amounts: &[U256]) -> QuotePass {
    let started = Instant::now();
    let mut legs = [Duration::ZERO; 3];
    let mut out: [Vec<U256>; 3] = [
        Vec::with_capacity(VARIATIONS),
        Vec::with_capacity(VARIATIONS),
        Vec::with_capacity(VARIATIONS),
    ];
    for amount in amounts {
        let mut input = *amount;
        for leg in Leg::ALL {
            let t = Instant::now();
            let quoted = simulate_swap(&entries[leg as usize], leg.zero_for_one(), input)
                .unwrap_or_else(|err| panic!("white-box {} quote refused: {err:?}", leg.label()));
            legs[leg as usize] += t.elapsed();
            input = quoted;
            out[leg as usize].push(quoted);
        }
    }
    QuotePass {
        total: started.elapsed(),
        legs,
        out,
        evm_gas: [0; 3],
        reads: 0,
    }
}

type BenchExt<'a> = ::evm2::evm::CacheDB<::evm2::evm::Db<Evm2CountingFrameDb<BlockingAsyncDb<'a>>>>;

/// One EVM pass: three sequential per-leg frames per variation, composed.
///
/// Every frame is a fresh EVM over the shared ext (transact + discard), so
/// the ext keeps read caches only and each variation quotes the identical
/// pinned state.
fn evm_pass(scratch: &mut Evm2ScratchEvm<BenchExt<'_>>, amounts: &[U256], nonce: u64) -> QuotePass {
    let started = Instant::now();
    let mut legs = [Duration::ZERO; 3];
    let mut evm_gas = [0u64; 3];
    let mut out: [Vec<U256>; 3] = [
        Vec::with_capacity(VARIATIONS),
        Vec::with_capacity(VARIATIONS),
        Vec::with_capacity(VARIATIONS),
    ];
    for amount in amounts {
        let mut input = *amount;
        for leg in Leg::ALL {
            let t = Instant::now();
            let tx = ReplayableTx {
                from: SENDER,
                to: Some(leg.target()),
                value: U256::ZERO,
                data: leg.calldata(input),
                gas_limit: GAS_LIMIT,
                max_fee_per_gas: MAX_FEE_PER_GAS,
                max_priority_fee_per_gas: MAX_PRIORITY_FEE_PER_GAS,
                nonce,
            };
            let outcome = scratch
                .replay(&tx)
                .unwrap_or_else(|err| panic!("{} frame refused: {err:?}", leg.label()));
            assert_eq!(
                outcome.status,
                RunVerdict::Success,
                "{} frame did not succeed: {:?} gas={} out=0x{}",
                leg.label(),
                outcome.status,
                outcome.gas_used,
                alloy::hex::encode(&outcome.output)
            );
            let quoted = leg.output(&outcome.output);
            legs[leg as usize] += t.elapsed();
            evm_gas[leg as usize] += outcome.gas_used;
            input = quoted;
            out[leg as usize].push(quoted);
        }
    }
    QuotePass {
        total: started.elapsed(),
        legs,
        out,
        evm_gas,
        reads: 0,
    }
}

/// The scratch over a fresh counted ext layer (the production layer order,
/// without the `BotState`-backed read cache: this benchmark's state is a
/// fixed pinned snapshot, so the plain `CacheDB` IS the warm layer).
fn make_scratch(
    provider: &AlloyProvider,
    block: ScratchBlock,
) -> Option<(Evm2ScratchEvm<BenchExt<'_>>, Arc<FrameRpcCounter>)> {
    let counter = Arc::new(FrameRpcCounter::default());
    let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(provider, PIN))?;
    let cold_path = Evm2CountingFrameDb::new(blocking, Arc::clone(&counter));
    let ext = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(cold_path));
    Some((
        Evm2ScratchEvm::with_counter(ext, block, Arc::clone(&counter)),
        counter,
    ))
}

// ─────────────────────────────────────────────────────────────────────────
// The measured run
// ─────────────────────────────────────────────────────────────────────────

/// Parity of one leg's quotes, EVM vs white-box.
struct Parity {
    mismatches: usize,
    max_abs_diff: U256,
}

fn parity(evm: &[U256], wb: &[U256]) -> Parity {
    assert_eq!(evm.len(), wb.len());
    let mut mismatches = 0usize;
    let mut max_abs_diff = U256::ZERO;
    for (a, b) in evm.iter().zip(wb) {
        if a != b {
            mismatches += 1;
            let diff = if a > b { *a - *b } else { *b - *a };
            if diff > max_abs_diff {
                max_abs_diff = diff;
            }
        }
    }
    Parity {
        mismatches,
        max_abs_diff,
    }
}

fn print_table(rows: &[(&str, &QuotePass)]) {
    println!();
    println!(
        "{:<22} {:>12} {:>12} {:>10} {:>10} {:>10}",
        "engine", "total ms", "per-path µs", "V3 µs", "V4 µs", "V2 µs"
    );
    for (name, pass) in rows {
        println!(
            "{name:<22} {:>12.1} {:>12.2} {:>10.2} {:>10.2} {:>10.2}",
            pass.total.as_secs_f64() * 1e3,
            pass.per_path_us(),
            pass.leg_us(Leg::V3),
            pass.leg_us(Leg::V4),
            pass.leg_us(Leg::V2),
        );
    }
    println!();
    println!("per-leg gas (sum over {VARIATIONS} variations):");
    for (name, pass) in rows {
        if pass.evm_gas[0] == 0 && pass.evm_gas[1] == 0 {
            continue;
        }
        println!(
            "  {name}: V3 {} / V4 {} / V2 {} (mean V3 {} V4 {} V2 {})",
            pass.evm_gas[Leg::V3 as usize],
            pass.evm_gas[Leg::V4 as usize],
            pass.evm_gas[Leg::V2 as usize],
            pass.evm_gas[Leg::V3 as usize] / VARIATIONS as u64,
            pass.evm_gas[Leg::V4 as usize] / VARIATIONS as u64,
            pass.evm_gas[Leg::V2 as usize] / VARIATIONS as u64,
        );
    }
}

fn print_parity(label: &str, evm: &QuotePass, wb: &QuotePass) {
    println!();
    println!("parity ({label}) — per-leg, {VARIATIONS} variations:");
    for leg in Leg::ALL {
        let p = parity(&evm.out[leg as usize], &wb.out[leg as usize]);
        println!(
            "  {}: mismatches {}/{VARIATIONS}  max |diff| {} wei",
            leg.label(),
            p.mismatches,
            p.max_abs_diff
        );
    }
    let last = Leg::ALL[Leg::ALL.len() - 1] as usize;
    let p = parity(&evm.out[last], &wb.out[last]);
    println!(
        "  composed path (WETH→USDC→WETH→USDC output): mismatches {}/{VARIATIONS}  max |diff| {} wei",
        p.mismatches, p.max_abs_diff
    );
}

/// A contract's runtime bytecode at the pinned block.
#[cfg(feature = "evm2-jit")]
async fn provider_code(provider: &AlloyProvider, address: Address) -> Bytes {
    provider.get_code(&address, Some(PIN)).await.unwrap()
}

fn rpc_url() -> String {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .expect(
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) \
             to an archive mainnet endpoint",
        )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC benchmark over archive mainnet state; run with --ignored"]
async fn bench_batch_quote_path() {
    let setup = Instant::now();
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    let chain_id = provider.get_chain_id().await.unwrap();
    assert_eq!(chain_id, 1, "the pinned path is a mainnet fixture");

    let block = provider
        .get_block(PIN)
        .await
        .unwrap()
        .expect("the pinned block is archived");
    let nonce = provider
        .get_transaction_count(&SENDER, Some(PIN))
        .await
        .unwrap();
    let base_fee_next = block.header.base_fee_per_gas.unwrap_or(0);
    let scratch_block = ScratchBlock {
        number: PIN + 1,
        timestamp: block.header.timestamp,
        base_fee_next: u128::from(base_fee_next),
    };

    println!("=== evm2 batch-quote pilot ===");
    println!("path      : WETH --V3--> USDC --V4--> WETH --V2--> USDC");
    println!(
        "block     : {PIN} (archive pin), frame block {}",
        scratch_block.number
    );
    println!(
        "variations: {VARIATIONS}, seed 0x{VARIATION_SEED:016x}, range [0.001, 1) ETH WETH wei"
    );

    println!("live state (fetch only, excluded from every timed loop):");
    let source = AlloyTickWordSource::new(Arc::clone(&provider));
    let white_box = fetch_white_box(&provider, &source).await;
    println!("  fetched in {} ms", setup.elapsed().as_millis());

    let amounts = variation_amounts();
    println!(
        "  variation range: {}..{} wei",
        amounts.iter().min().unwrap(),
        amounts.iter().max().unwrap()
    );

    // ── white-box ────────────────────────────────────────────────────────
    // Untimed pass first (symmetric with the EVM's warm pass), then measure.
    let _ = white_box_pass(&white_box.entries, &amounts);
    let wb = white_box_pass(&white_box.entries, &amounts);

    // ── EVM interpret ────────────────────────────────────────────────────
    let (mut scratch, counter) =
        make_scratch(&provider, scratch_block).expect("multi-threaded runtime is ambient");
    let cold = evm_pass(&mut scratch, &amounts, nonce);
    let cold_reads = counter.reads();
    let before = counter.reads();
    let mut interp = evm_pass(&mut scratch, &amounts, nonce);
    interp.reads = counter.reads() - before;
    assert_eq!(
        interp.reads, 0,
        "the measured pass must be WARM: {} cold reads reached the RPC leaf",
        interp.reads
    );

    let mut rows: Vec<(&str, &QuotePass)> = vec![("white-box (simulate_swap)", &wb)];
    rows.push(("EVM interpret", &interp));

    // ── EVM JIT (AOT store, pre-warmed) ──────────────────────────────────
    #[cfg(feature = "evm2-jit")]
    let mut jit_holder: Option<(QuotePass, u64)> = None;
    #[cfg(feature = "evm2-jit")]
    {
        let (backend, prep_ms, prepared) = jit::prepare(&provider).await;
        println!();
        match &backend {
            Some(_) => println!(
                "JIT backend: initialized; AOT prep {prep_ms} ms for {prepared} contract(s)"
            ),
            None => println!("JIT backend: UNAVAILABLE (LLVM init failed) — numbers are INTERPRET"),
        }
        if let Some(backend) = backend {
            let (mut scratch, counter) =
                make_scratch(&provider, scratch_block).expect("multi-threaded runtime is ambient");
            let runner: Arc<dyn ::evm2::evm::InterpreterRunner<::evm2::BaseEvmTypes>> = Arc::new(
                ::evm2_jit::evm2_evm::JitInterpreterRunner::new(backend.clone()),
            );
            scratch = scratch.with_jit(Some(runner));
            let warm = evm_pass(&mut scratch, &amounts, nonce);
            let warm_reads = counter.reads();
            let before = counter.reads();
            let mut jit_pass = evm_pass(&mut scratch, &amounts, nonce);
            jit_pass.reads = counter.reads() - before;
            assert_eq!(
                jit_pass.reads, 0,
                "the measured JIT pass must be WARM: {} cold reads reached the RPC leaf",
                jit_pass.reads
            );
            println!(
                "  warm pass: {:.1} ms, {warm_reads} cold reads (one-time slot/code fetch)",
                warm.total.as_secs_f64() * 1e3
            );
            jit_holder = Some((jit_pass, warm_reads));
        }
    }

    println!();
    println!(
        "EVM interpret warm pass (cold): {:.1} ms total, {cold_reads} cold reads",
        cold.total.as_secs_f64() * 1e3
    );
    println!(
        "EVM interpret measured pass: {:.1} ms total, {} cold reads (warm ✓)",
        interp.total.as_secs_f64() * 1e3,
        interp.reads
    );

    #[cfg(feature = "evm2-jit")]
    let jit_ref: Option<&QuotePass> = match &jit_holder {
        Some((pass, _)) => Some(pass),
        None => None,
    };
    #[cfg(not(feature = "evm2-jit"))]
    let jit_ref: Option<&QuotePass> = None;
    if let Some(pass) = jit_ref {
        rows.push(("EVM JIT (AOT, warm)", pass));
    }

    print_table(&rows);

    // The answer to the pilot's first question, in one line.
    let wb_us = wb.per_path_us();
    let interp_us = interp.per_path_us();
    println!();
    println!(
        "ratios (per path): white-box {wb_us:.2} µs = 1.00x | EVM interpret {interp_us:.2} µs          = {:.2}x | EVM JIT {:.2} µs = {:.2}x",
        interp_us / wb_us,
        jit_ref.map_or(f64::NAN, QuotePass::per_path_us),
        jit_ref.map_or(f64::NAN, |pass| pass.per_path_us() / wb_us),
    );

    print_parity("EVM interpret", &interp, &wb);
    if let Some(pass) = jit_ref {
        print_parity("EVM JIT", pass, &wb);
    }

    println!();
    println!("sample quotes (variation 0, {} wei in):", amounts[0]);
    for leg in Leg::ALL {
        println!(
            "  {}: EVM {}  white-box {}",
            leg.label(),
            interp.out[leg as usize][0],
            wb.out[leg as usize][0]
        );
    }

    // ── the brief's parity contract ──────────────────────────────────────
    // V2 and V3 are expected to match to the wei; a mismatch there is a real
    // divergence, not a benchmark result. V4 is reported (the pilot's
    // documented gap surface) but not asserted here.
    for leg in [Leg::V2, Leg::V3] {
        let p = parity(&interp.out[leg as usize], &wb.out[leg as usize]);
        assert_eq!(
            p.mismatches,
            0,
            "{}: {} of {VARIATIONS} EVM quotes diverged from the white-box sim (max {} wei)",
            leg.label(),
            p.mismatches,
            p.max_abs_diff
        );
    }
    let v4 = parity(&interp.out[Leg::V4 as usize], &wb.out[Leg::V4 as usize]);
    if v4.mismatches > 0 {
        println!(
            "V4 white-box parity gap (reported, not asserted): {}/{VARIATIONS} mismatches, \
             max |diff| {} wei",
            v4.mismatches, v4.max_abs_diff
        );
    }
    println!();
    println!(
        "total wall (setup + all passes): {:.1} s",
        setup.elapsed().as_secs_f64()
    );
}

// ─────────────────────────────────────────────────────────────────────────
// JIT / AOT preparation (feature-gated)
// ─────────────────────────────────────────────────────────────────────────

#[cfg(feature = "evm2-jit")]
mod jit {
    use super::{provider_code, AlloyProvider, PIN};
    use ::evm2_jit::runtime::{AotRequest, JitBackend, ProgramKind, RuntimeConfig, RuntimeTuning};
    use ::evm2_jit_runtime::OptimizationLevel;
    use degenbot_simulation::sim::evm2::aot_store::DurableArtifactStore;
    use degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC;
    use std::sync::Arc;
    use std::time::Instant;

    /// The optimization level for this run: `DEGENBOT_EVM2_OPT_LEVEL` =
    /// `none|less|default|aggressive` (unset = default). The bench's store is
    /// fresh per run, so each level's artifacts are compiled at that level.
    fn opt_level() -> OptimizationLevel {
        match std::env::var("DEGENBOT_EVM2_OPT_LEVEL").as_deref() {
            Ok("none") => OptimizationLevel::None,
            Ok("less") => OptimizationLevel::Less,
            Ok("default") | Err(_) => OptimizationLevel::Default,
            Ok("aggressive") => OptimizationLevel::Aggressive,
            Ok(other) => {
                panic!("unknown DEGENBOT_EVM2_OPT_LEVEL {other:?}: none|less|default|aggressive")
            }
        }
    }

    /// Runtime tuning with both opt-level knobs at this run's level.
    fn tuning() -> RuntimeTuning {
        RuntimeTuning {
            jit_opt_level: opt_level(),
            aot_opt_level: opt_level(),
            ..RuntimeTuning::default()
        }
    }

    /// Build the JIT backend over a durable AOT store and pre-compile every
    /// contract the path's frames execute. Returns `(backend, prep ms,
    /// contracts prepared)`; `None` when the LLVM backend cannot initialize
    /// (the run then reports INTERPRET).
    pub(super) async fn prepare(provider: &AlloyProvider) -> (Option<JitBackend>, u128, usize) {
        println!("JIT optimization level: {:?}", opt_level());
        let store_dir = std::env::temp_dir().join(format!("evm2-batch-quote-aot-{PIN}"));
        let _ = std::fs::remove_dir_all(&store_dir);
        let store = match DurableArtifactStore::new(store_dir.join("artifacts")) {
            Ok(store) => store,
            Err(err) => {
                println!("AOT store unavailable ({err}) — blocking compile-on-miss instead");
                return (
                    JitBackend::new(RuntimeConfig {
                        enabled: true,
                        blocking: true,
                        tuning: tuning(),
                        ..RuntimeConfig::default()
                    })
                    .ok(),
                    0,
                    0,
                );
            }
        };
        let backend = JitBackend::new(RuntimeConfig {
            enabled: true,
            blocking: true,
            tuning: tuning(),
            store: Some(Arc::new(store)),
            ..RuntimeConfig::default()
        });
        let Ok(backend) = backend else {
            return (None, 0, 0);
        };

        let started = Instant::now();
        let mut prepared = 0usize;
        let mut failures = Vec::new();
        for address in super::AOT_CONTRACTS {
            let code = provider_code(provider, address).await;
            if code.is_empty() {
                failures.push(format!("{address}: no code at {PIN}"));
                continue;
            }
            let code_hash = alloy::primitives::keccak256(&code);
            match backend.prepare_aot_sync(AotRequest {
                code_hash,
                code,
                spec_id: EVM2_PRODUCTION_SPEC,
            }) {
                Ok(()) => match backend.get_compiled(code_hash, EVM2_PRODUCTION_SPEC) {
                    Some(compiled) if compiled.kind == ProgramKind::Aot => prepared += 1,
                    other => failures.push(format!("{address}: {other:?} (expected Aot)")),
                },
                Err(err) => failures.push(format!("{address}: {err}")),
            }
        }
        if !failures.is_empty() {
            println!(
                "  AOT prep fallbacks (those contracts interpret or block-compile instead): {failures:?}"
            );
        }
        (Some(backend), started.elapsed().as_millis(), prepared)
    }
}
