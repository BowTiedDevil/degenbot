//! The gas decomposition of a swap leg under evm2 — the measured input for a
//! gas-aware admission floor (`docs/gas-estimation.md` carries the coefficient
//! table and the error analysis this rig prints).
//!
//! One question: **what does a real swap cost, and what drives the cost?**
//! The floor has to be path-shaped, so this rig measures the coefficients of
//!
//! ```text
//! gas(leg) = a_family + b_cross * initialized_ticks_crossed
//!                     + b_word  * bitmap_words_walked
//!                     + b_transfer * token_transfers
//! ```
//!
//! by executing real pool swaps on evm2 over archive state, and it settles two
//! further questions the floor's accuracy depends on:
//!
//! 1. **The chain's own crossing count.** The deployed `QuoterV2` returns
//!    `initializedTicksCrossed`; the rig compares it against its own bitmap
//!    census between the pre-swap and post-swap tick. (The two differ by the
//!    endpoint convention — measured, not assumed.) V4 has no quoter census,
//!    so the V4 axes come from the frame's touched slot set: one bitmap-word
//!    slot per word marched, one tick-info slot per `cross()`.
//! 2. **Which crossing source admission can afford.** Option (a) carries the
//!    count on the walk's outcome, (b) recomputes it from the in-memory tick
//!    map, (c) approximates it as `|tick_final − tick_initial| / spacing`. The
//!    rig measures (c)'s drift against the census on the phantom corpus.
//!
//! # The frames
//!
//! Every leg is executed through the *pool's own bytecode*, never through the
//! white-box walk under test:
//!
//! - **V3 (and any V3 fork):** a quote caller (`v3_caller_code`) that invokes
//!   `pool.swap` and answers `uniswapV3SwapCallback` by REVERTING with
//!   `(amount0Delta, amount1Delta, sqrtPriceX96After, tickAfter)`. The frame
//!   ends in a revert, so nothing settles — which is what makes it
//!   address-agnostic (Pancake/Sushi/fork pools all answer `swap`), and it is
//!   the thinnest frame that still runs the real crossing loop. The post-swap
//!   words come from a `slot0()` staticcall *inside* the callback: the pool has
//!   already written its new price by then (the rig asserts this — see the
//!   `slot0` self-check line).
//! - **V3 canonical cross-check:** the deployed `QuoterV2`'s
//!   `quoteExactInputSingle`, which answers `(amountOut, sqrtPriceX96After,
//!   initializedTicksCrossed, gasEstimate)` — the chain's own count. The rig
//!   quotes the same point through both frames so the quoter's own overhead is
//!   a measured constant rather than a guess.
//! - **V2:** the canonical router's `getAmountsOut` (the same frame the phantom
//!   rig measures V2 legs with).
//! - **V4:** the black-box quoter primitive
//!   ([`V4BlackBoxQuoter`](degenbot_simulation::V4BlackBoxQuoter)), whose quote
//!   carries the frame's `gas_used` — the whole unlock/delta path per leg.
//!
//! # Method
//!
//! For each V3 pool and direction the rig first reads the pool's real state
//! (`slot0`, `liquidity`, `tickSpacing`), scales an amount ladder to the
//! in-range reserve `L·2^96/√P` (the only amount scale that means the same
//! thing across pools), then **bisects the amount for each crossing count**
//! `k = 0, 1, 2, …` and samples the midpoint of each `k`-window. The x-axis is
//! the census the pool's own tick map produces, so a fit's slope is gas per
//! *actually crossed* tick, not per tick-gap.
//!
//! `b_word` is a separate regressor because the swap pays a cold `SLOAD` per
//! bitmap word it touches, whether or not the word holds an initialized tick:
//! a swap that leaves the in-range liquidity and marches through empty words
//! (the `EMPTY-HALT` shape) burns gas in proportion to *words*, not crossings.
//!
//! The V4 word term needs its own sweep, not V3's borrowed rate: every V4
//! pool's bitmap is a mapping inside the singleton `PoolManager`'s storage, so
//! a V4 word is one `SLOAD` of an account every leg already touches. That
//! side's ladder bisects the amount for each word count *on the chain's own
//! word count* — the contiguous run of the pool's bitmap-word slots in the
//! frame's touched set — and its shared-account experiment prices two V4 legs
//! in one frame against two frames. The white-box walk's `landed` /
//! `word_boundary_prices` projection is printed beside the chain's counts as a
//! cross-check, and it does not agree with them on V4 (see the word report):
//! the coefficient is fitted on the chain's counts, and a floor that wants the
//! word axis must carry it, not project it.
//!
//! # Running it
//!
//! ```sh
//! cargo test -p degenbot-simulation --test evm2_gas_decomposition \
//!   gas_decomposition_census -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `DEGENBOT_RPC_HTTP_CHAINID_1` (or `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`)
//! selects the node; the anchor needs archive state. Minutes of live-RPC work.

#![expect(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::similar_names
)]

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use ::evm2::evm::{AccountInfo, CacheDB, Db};
use alloy::primitives::{address, keccak256, Address, Bytes, U256};

use degenbot_pools::{
    decode_tick_word, decode_v4_slot0, v4_liquidity_slot, v4_pool_id, v4_pool_state_base_slot,
    v4_slot0_slot, v4_tick_bitmap_word_slot, v4_tick_mapping_slot, word_of,
    ConcentratedLiquidityPathSim, PoolTickCoverage, PreparedConcentratedLiquidityHop,
    RegisterV4PoolParams, TickInfo, V4PoolKey, V4PoolState,
};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_simulation::sim::evm2::frame_replay::Evm2ScratchEvm;
use degenbot_simulation::sim::evm2::v4_black_box::{
    decode_quote_return, quote_calldata, V4BlackBoxQuoter, V4Leg, V4QuoteError, V4QuoterConfig,
    V4_POOL_MANAGER,
};
use degenbot_simulation::sim::frame_replay::{ReplayableTx, ScratchBlock};
use degenbot_simulation::sim::RunVerdict;

// ─────────────────────────────────────────────────────────────────────────
// The anchor and the mainnet constants
// ─────────────────────────────────────────────────────────────────────────

/// The phantom corpus's anchor: every corpus hop is measured at the block the
/// rig's ACTUAL-gas figures were taken at, so the estimator's error is measured
/// on the state the candidates were published against.
const PIN: u64 = 26_335_759;

const QUOTER_V3: Address = address!("0x61fFE014bA17989E743c5F6cB21bF9697530B21e");
const ROUTER_V2: Address = address!("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
const ROUTER_SUSHI: Address = address!("0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F");

const SENDER: Address = address!("0x9C56a29c7231974c269E24F9FB3c29203039089E");

/// The quote caller: the contract that invokes `pool.swap` and answers the
/// pool's `uniswapV3SwapCallback`.
const CALLBACK: Address = address!("0x00000000000000000000000000000000c0ffee01");

/// The batching quote caller: forwards one or two `quoteExactInputSingle`
/// calldatas to the canonical quoter inside ONE frame — the "two legs, one
/// unlock" shape a settled path pays for its V4 hops.
const V4_BATCHER: Address = address!("0x00000000000000000000000000000000c0ffee02");

/// The same caller with a single leg, the frame-overhead baseline for the
/// batching shape (a quote routed through a contract rather than an EOA).
const V4_BATCHER_ONE: Address = address!("0x00000000000000000000000000000000c0ffee03");

/// The batching caller's calldata plumbing, echoed: the self-check that the
/// memory the caller forwards is byte-identical to a plain quote's calldata.
const V4_ECHO: Address = address!("0x00000000000000000000000000000000c0ffee04");

/// Generous enough for a walk that leaves the liquidity and marches to the
/// price limit (the `EMPTY-HALT` shape) — the shape the floor exists to price.
const GAS_LIMIT: u64 = 16_000_000;
const MAX_FEE_PER_GAS: u128 = 200_000_000_000;
const MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000;

// ─────────────────────────────────────────────────────────────────────────
// ABI helpers (hand-encoded, the phantom rig's idiom)
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

fn w_int(v: i32) -> [u8; 32] {
    let mut w = if v < 0 { [0xffu8; 32] } else { [0u8; 32] };
    w[28..].copy_from_slice(&v.to_be_bytes());
    w
}

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

/// `int24` from an ABI word (sign-extended).
fn i24_of(word: U256) -> i32 {
    let bits = (word & U256::from(0x00FF_FFFFu32)).to::<u32>();
    if (bits & 0x80_0000) != 0 {
        i32::try_from(bits).expect("24 bits fit i32") - (1 << 24)
    } else {
        i32::try_from(bits).expect("24 bits fit i32")
    }
}

fn ten_pow(exp: u32) -> U256 {
    let mut v = U256::ONE;
    for _ in 0..exp {
        v *= U256::from(10u64);
    }
    v
}

/// `L·2^96/√P`: the in-range reserve (raw units) — the amount scale every pool
/// shares.
fn reserve0(liquidity: u128, sqrt_price_x96: U256) -> U256 {
    if sqrt_price_x96.is_zero() {
        return U256::ZERO;
    }
    U256::from(liquidity) * (U256::ONE << 96u32) / sqrt_price_x96
}

fn reserve1(liquidity: u128, sqrt_price_x96: U256) -> U256 {
    U256::from(liquidity) * sqrt_price_x96 / (U256::ONE << 96u32)
}

// ─────────────────────────────────────────────────────────────────────────
// The quote caller: `pool.swap` + a callback that answers with the deltas AND
// the pool's post-swap tick
// ─────────────────────────────────────────────────────────────────────────

/// The caller's bytecode. Forward arm: calldata is `[pool address (32B)][swap
/// calldata]`; it copies the swap calldata to memory, CALLs the pool, and
/// returns the pool's 64-byte delta payload. Both callback selectors jump to
/// the quote arm, which reverts with
/// `(amount0Delta, amount1Delta, sqrtPriceX96After, tickAfter)` — the last two
/// words come from a `slot0()` STATICCALL whose `CALLER` is the pool.
///
/// The pool has already written its new `sqrtPriceX96`/`tick` when it calls the
/// callback, so the frame reports the exact post-swap price without any
/// white-box math. A quote frame therefore measures the real crossing loop and
/// nothing else: no settlement, no token balances, no factory route.
fn v3_caller_code() -> Vec<u8> {
    let mut c: Vec<u8> = vec![
        0x60, 0x00, 0x35, 0x60, 0xe0, 0x1c, // selector = calldataload(0) >> 224
        0x80, // DUP1
        0x63, 0xfa, 0x46, 0x1e, 0x33, // PUSH4 uniswapV3SwapCallback(int256,int256,bytes)
        0x14, // EQ
        0x61, 0x00, 0x4e, // PUSH2 78 (the quote arm)
        0x57, // JUMPI
        0x80, // DUP1
        0x63, 0x23, 0xa6, 0x9e, 0x75, // PUSH4 pancakeV3SwapCallback(int256,int256,bytes)
        0x14, // EQ
        0x61, 0x00, 0x4e, // PUSH2 78 (the quote arm)
        0x57, // JUMPI
        0x50, // POP (the unmatched selector)
    ];
    assert_eq!(c.len(), 29, "the forward arm starts at 29");

    // Forward arm: call the pool with calldata[0x20..] and capture its 64-byte
    // return at memory[0x100..0x140]; a failed call bubbles its payload.
    c.extend_from_slice(&[
        0x60, 0x20, 0x36, 0x03, // size = calldatasize - 0x20
        0x60, 0x20, // offset = 0x20
        0x60, 0x00, // destOffset
        0x37, // CALLDATACOPY
        0x60, 0x40, // outSize = 64 (the two int256 deltas)
        0x61, 0x01, 0x00, // outOffset = 0x100
        0x60, 0x20, 0x36, 0x03, // inSize = calldatasize - 0x20
        0x60, 0x00, // inOffset
        0x60, 0x00, // value
        0x60, 0x00, 0x35, // addr = calldataload(0) (the pool)
        0x5a, // GAS
        0xf1, // CALL
        0x15, // ISZERO
        0x61, 0x00, 0x43, // PUSH2 67 (the bubble arm)
        0x57, // JUMPI
        0x60, 0x40, 0x61, 0x01, 0x00, 0xf3, // RETURN(0x100, 64)
    ]);
    assert_eq!(c.len(), 67, "the bubble arm starts at 67");

    // Bubble arm: re-revert with the callee's payload.
    c.extend_from_slice(&[
        0x5b, // JUMPDEST
        0x3d, // RETURNDATASIZE
        0x60, 0x00, // PUSH1 0
        0x60, 0x00, // PUSH1 0
        0x3e, // RETURNDATACOPY
        0x3d, // RETURNDATASIZE
        0x60, 0x00, // PUSH1 0
        0xfd, // REVERT
    ]);
    assert_eq!(c.len(), 78, "the quote arm starts at 78");

    // Quote arm: the deltas, then the pool's own post-swap `slot0()`. The pool
    // address rides in the swap's `bytes data` argument (the pool passes it
    // through verbatim), so the frame never guesses at who the caller is.
    c.extend_from_slice(&[
        0x5b, // JUMPDEST
        0x60, 0x04, 0x35, // PUSH1 4 CALLDATALOAD (amount0Delta)
        0x60, 0x00, 0x52, // MSTORE(0x00, amount0Delta)
        0x60, 0x24, 0x35, // PUSH1 0x24 CALLDATALOAD (amount1Delta)
        0x60, 0x20, 0x52, // MSTORE(0x20, amount1Delta)
        // memory[0x100] = slot0()'s selector, left-aligned (the getter takes no
        // arguments, so the staticcall's calldata is those four bytes).
        0x63, 0x38, 0x50, 0xc7, 0xbd, // PUSH4 slot0()
        0x60, 0xe0, // PUSH1 0xe0
        0x1b, // SHL
        0x61, 0x01, 0x00, // PUSH2 0x100
        0x52, // MSTORE(0x100, selector << 224)
        0x60, 0x40, // retSize = 0x40 (sqrtPriceX96, tick)
        0x60, 0x40, // retOffset = 0x40
        0x60, 0x04, // argsSize = 4
        0x61, 0x01, 0x00, // argsOffset = 0x100
        // addr = calldataload(4 + calldataload(0x44) + 32) — the first word of
        // the callback's `bytes data` argument, which is the pool address.
        0x60, 0x44, 0x35, 0x60, 0x04, 0x01, 0x60, 0x20, 0x01, 0x35, 0x5a, // GAS
        0xfa, // STATICCALL
        0x60, 0x80, 0x52, // MSTORE(0x80, success flag)
        0x60, 0x44, 0x35, 0x60, 0x04, 0x01, 0x60, 0x20, 0x01, 0x35, // addr again
        0x60, 0xa0, 0x52, // MSTORE(0xa0, addr) — the self-check word
        0x33, 0x60, 0xc0, 0x52, // MSTORE(0xc0, CALLER)
        0x60, 0xe0, // size = 224
        0x60, 0x00, // offset = 0
        0xfd, // REVERT — the payload IS the measurement
    ]);
    c
}

/// A V3 leg's measurement, read from the caller's 128-byte revert payload.
#[derive(Clone, Copy, Debug)]
struct V3Frame {
    gas: u64,
    amount0_delta: U256,
    amount1_delta: U256,
    sqrt_price_after: U256,
    tick_after: i32,
    /// Whether the in-callback `slot0()` staticcall succeeded.
    slot0_call_ok: bool,
    /// The pool address the callback resolved from its `bytes data` argument.
    pool: Address,
    /// The address `CALLER` reported inside the swap callback (diagnostic).
    caller: Address,
}

fn parse_v3_frame(out: &Bytes, gas: u64) -> Option<V3Frame> {
    let words = out_words(out);
    if words.len() != 7 {
        return None;
    }
    let addr_of = |word: U256| {
        let mut raw = [0u8; 20];
        raw.copy_from_slice(&word.to_be_bytes::<32>()[12..]);
        Address::from(raw)
    };
    Some(V3Frame {
        gas,
        amount0_delta: words[0],
        amount1_delta: words[1],
        sqrt_price_after: words[2] & ((U256::ONE << 160u32) - U256::ONE),
        tick_after: i24_of(words[3]),
        slot0_call_ok: !words[4].is_zero(),
        pool: addr_of(words[5]),
        caller: addr_of(words[6]),
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Node reads (cached: one read per (target, calldata) per run)
// ─────────────────────────────────────────────────────────────────────────

struct Reads<'p> {
    provider: &'p AlloyProvider,
    block: u64,
    calls: HashMap<(Address, Vec<u8>), Bytes>,
    storages: HashMap<(Address, U256), U256>,
    /// Distinct bitmap words read (the cache's growth is the rig's RPC cost).
    bitmap_words: usize,
    /// Censuses whose walk spanned more words than the scan bound.
    truncated_censuses: usize,
}

impl<'p> Reads<'p> {
    fn new(provider: &'p AlloyProvider, block: u64) -> Self {
        Self {
            provider,
            block,
            calls: HashMap::new(),
            storages: HashMap::new(),
            bitmap_words: 0,
            truncated_censuses: 0,
        }
    }

    async fn call(&mut self, to: Address, data: Bytes) -> Bytes {
        let key = (to, data.to_vec());
        if let Some(hit) = self.calls.get(&key) {
            return hit.clone();
        }
        let out = self
            .provider
            .eth_call(&to, data, Some(self.block))
            .await
            .unwrap_or_default();
        self.calls.insert(key, out.clone());
        out
    }

    async fn storage(&mut self, to: Address, slot: U256) -> U256 {
        if let Some(hit) = self.storages.get(&(to, slot)) {
            return *hit;
        }
        let value = self
            .provider
            .get_storage_at(&to, slot, Some(self.block))
            .await
            .map_or(U256::ZERO, |b| U256::from_be_bytes(b.0));
        self.storages.insert((to, slot), value);
        value
    }

    async fn word(&mut self, to: Address, sig: &str) -> U256 {
        let out = self.call(to, calldata(selector(sig), &[])).await;
        out_words(&out).first().copied().unwrap_or(U256::ZERO)
    }

    async fn tick_spacing(&mut self, pool: Address) -> i32 {
        i24_of(self.word(pool, "tickSpacing()").await)
    }

    async fn liquidity(&mut self, pool: Address) -> u128 {
        self.word(pool, "liquidity()").await.to::<u128>()
    }

    /// `(sqrtPriceX96, tick)` from the pool's own `slot0()` getter (not the raw
    /// storage word, so a fork's layout cannot skew it).
    async fn v3_slot0(&mut self, pool: Address) -> (U256, i32) {
        let words = out_words(&self.call(pool, calldata(selector("slot0()"), &[])).await);
        assert!(
            words.len() >= 2,
            "slot0() returns the packed struct as words"
        );
        (
            words[0] & ((U256::ONE << 160u32) - U256::ONE),
            i24_of(words[1]),
        )
    }

    async fn bitmap_word(&mut self, pool: Address, word: i16) -> U256 {
        self.bitmap_words += 1;
        let out = self
            .call(
                pool,
                calldata(selector("tickBitmap(int16)"), &[w_int(i32::from(word))]),
            )
            .await;
        out_words(&out).first().copied().unwrap_or(U256::ZERO)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The tick-map census: the pool's own initialised-tick layout
// ─────────────────────────────────────────────────────────────────────────

/// One directional census between the pre-swap and post-swap tick.
#[derive(Clone, Debug, Default)]
struct Census {
    /// Initialised ticks strictly inside the travelled range — the ticks the
    /// pool's `cross()` runs on. This is the gas driver.
    crossed_exclusive: usize,
    /// The same census including both endpoints (the `QuoterV2` convention —
    /// see the census cross-check in the report).
    crossed_inclusive: usize,
    /// Distinct bitmap words the walk reads (initialised or not).
    words: usize,
    /// The crossed ticks, ascending (for the warm/cold split).
    ticks: Vec<i32>,
}

impl Census {
    /// Distinct bitmap words among the crossed ticks — a crossing that lands in
    /// an already-loaded word skips the cold `SLOAD` the first one paid.
    fn distinct_tick_words(&self, spacing: i32) -> usize {
        let mut words: Vec<i32> = self.ticks.iter().map(|t| (t / spacing) >> 8).collect();
        words.sort_unstable();
        words.dedup();
        words.len()
    }
}

/// Count initialised ticks and bitmap words between `tick_before` and
/// `tick_after` in the swap direction, straight from the pool's tick bitmap.
async fn census(
    reads: &mut Reads<'_>,
    pool: Address,
    spacing: i32,
    tick_before: i32,
    tick_after: i32,
    zero_for_one: bool,
) -> Census {
    let (lo, hi) = if zero_for_one {
        (tick_after, tick_before)
    } else {
        (tick_before, tick_after)
    };
    let spacing = spacing.max(1);
    let lo_c = lo.div_euclid(spacing);
    let hi_c = hi.div_euclid(spacing);
    let w_lo = lo_c >> 8;
    let w_hi = hi_c >> 8;
    let span = usize::try_from(w_hi - w_lo + 1).unwrap_or(usize::MAX);
    let truncated = span > 2_000;
    let w_hi = if truncated { w_lo + 1_999 } else { w_hi };

    if truncated {
        reads.truncated_censuses += 1;
    }
    let mut census = Census {
        words: span,
        ..Census::default()
    };
    for w in w_lo..=w_hi {
        let Ok(word) = i16::try_from(w) else {
            continue;
        };
        let bitmap = reads.bitmap_word(pool, word).await;
        if bitmap.is_zero() {
            continue;
        }
        for bit in 0..256usize {
            if !bitmap.bit(bit) {
                continue;
            }
            let compressed = i64::from(w) * 256 + i64::try_from(bit).unwrap();
            let tick = i32::try_from(compressed * i64::from(spacing)).expect("tick fits i32");
            if lo < tick && tick <= hi {
                census.crossed_exclusive += 1;
                census.ticks.push(tick);
            }
            if lo <= tick && tick <= hi {
                census.crossed_inclusive += 1;
            }
        }
    }
    census.ticks.sort_unstable();
    census
}

// ─────────────────────────────────────────────────────────────────────────
// The evm2 scratch
// ─────────────────────────────────────────────────────────────────────────

type RigExt<'p> = CacheDB<Db<BlockingAsyncDb<'p>>>;

struct FrameOut {
    output: Bytes,
    gas: u64,
    status: RunVerdict,
}

struct Chain<'p> {
    scratch: Evm2ScratchEvm<RigExt<'p>>,
    nonce: u64,
    /// How many unparseable frames have been reported (bounded diagnostics).
    diagnostics: usize,
}

fn huge_balance() -> U256 {
    ten_pow(60)
}

impl<'p> Chain<'p> {
    fn new(provider: &'p AlloyProvider, block: ScratchBlock, nonce: u64) -> Self {
        let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(provider, block.number - 1))
            .expect("multi-threaded runtime is ambient");
        let mut ext = CacheDB::new(Db::new(blocking));
        ext.insert_account_info(&SENDER, AccountInfo::default().with_balance(huge_balance()));
        ext.insert_account_info(
            &CALLBACK,
            AccountInfo::default()
                .with_balance(huge_balance())
                .with_code(::evm2::bytecode::Bytecode::new_raw(Bytes::from(
                    v3_caller_code(),
                ))),
        );
        let quoter = V4QuoterConfig::default().quoter;
        for (address, code) in [
            (V4_BATCHER, v4_batcher_code(quoter)),
            (V4_BATCHER_ONE, v4_single_quote_caller_code(quoter)),
            (V4_ECHO, v4_echo_code()),
        ] {
            ext.insert_account_info(
                &address,
                AccountInfo::default()
                    .with_balance(huge_balance())
                    .with_code(::evm2::bytecode::Bytecode::new_raw(Bytes::from(code))),
            );
        }
        Self {
            scratch: Evm2ScratchEvm::new(ext, block),
            nonce,
            diagnostics: 0,
        }
    }

    fn frame(&mut self, from: Address, to: Address, data: Bytes) -> FrameOut {
        let tx = ReplayableTx {
            from,
            to: Some(to),
            value: U256::ZERO,
            data,
            gas_limit: GAS_LIMIT,
            max_fee_per_gas: MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: MAX_PRIORITY_FEE_PER_GAS,
            nonce: self.nonce,
        };
        match self.scratch.replay(&tx) {
            Ok(outcome) => FrameOut {
                output: outcome.output.clone(),
                gas: outcome.gas_used,
                status: outcome.status,
            },
            Err(err) => panic!("frame refused: {err:?}"),
        }
    }

    /// One V3 leg through the pool's own `swap()` bytecode.
    fn quote_v3(&mut self, pool: Address, zero_for_one: bool, amount_in: U256) -> Option<V3Frame> {
        let limit = if zero_for_one {
            U256::from(degenbot_math::concentrated_liquidity::tick_math::MIN_SQRT_RATIO) + U256::ONE
        } else {
            U256::from(degenbot_math::concentrated_liquidity::tick_math::MAX_SQRT_RATIO) - U256::ONE
        };
        // The swap's `bytes data` argument carries the pool address, so the
        // callback can read the pool's post-swap `slot0()` without guessing at
        // `CALLER`: four static head words, then the `data` offset (0xa0), its
        // length word and the address.
        let swap = calldata(
            selector("swap(address,bool,int256,uint160,bytes)"),
            &[
                w_addr(CALLBACK),
                w_small(u64::from(zero_for_one)),
                w_uint(amount_in),
                w_uint(limit),
                w_small(0xa0),
                w_small(32),
                w_addr(pool),
            ],
        );
        let mut forwarded = Vec::with_capacity(32 + swap.len());
        forwarded.extend_from_slice(&w_addr(pool));
        forwarded.extend_from_slice(swap.as_ref());
        let out = self.frame(SENDER, CALLBACK, Bytes::from(forwarded));
        let parsed = parse_v3_frame(&out.output, out.gas);
        if parsed.is_none() && self.diagnostics < 6 {
            self.diagnostics += 1;
            println!(
                "   [diag] pool {pool} zfo={zero_for_one} amount={amount_in}: payload {} bytes                  status {:?} gas {} head 0x{}",
                out.output.len(),
                out.status,
                out.gas,
                alloy::hex::encode(&out.output[..out.output.len().min(40)])
            );
        }
        parsed
    }

    /// The same leg through the deployed `QuoterV2`: `(amountOut, crossings,
    /// gasEstimate)` — the chain's own crossing count.
    fn quote_v3_canonical(
        &mut self,
        token_in: Address,
        token_out: Address,
        fee: u32,
        amount_in: U256,
    ) -> Option<(U256, usize, U256, u64)> {
        let data = calldata(
            selector("quoteExactInputSingle((address,address,uint256,uint24,uint160))"),
            &[
                w_addr(token_in),
                w_addr(token_out),
                w_uint(amount_in),
                w_small(u64::from(fee)),
                w_uint(U256::ZERO),
            ],
        );
        let out = self.frame(SENDER, QUOTER_V3, data);
        let words = out_words(&out.output);
        if words.len() < 4 {
            return None;
        }
        Some((
            words[0],
            usize::try_from(words[2].to::<u64>()).unwrap_or(usize::MAX),
            words[3],
            out.gas,
        ))
    }

    /// A V2 leg through the canonical router.
    fn quote_v2(
        &mut self,
        router: Address,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> (u64, Option<U256>) {
        let data = calldata(
            selector("getAmountsOut(uint256,address[])"),
            &[
                w_uint(amount_in),
                w_small(0x40),
                w_small(2),
                w_addr(token_in),
                w_addr(token_out),
            ],
        );
        let out = self.frame(SENDER, router, data);
        let words = out_words(&out.output);
        let amount_out = if words.len() >= 4 {
            Some(words[3])
        } else {
            None
        };
        (out.gas, amount_out)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The sample ledger
// ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Family {
    V2,
    V3,
    V4,
}

impl Family {
    fn label(self) -> &'static str {
        match self {
            Self::V2 => "V2",
            Self::V3 => "V3",
            Self::V4 => "V4",
        }
    }
}

/// One measured leg.
#[derive(Clone, Debug)]
struct Sample {
    family: Family,
    /// Which pool/frame the sample came from (the grouping key).
    group: String,
    amount_in: U256,
    /// The frame's own gas — the ACTUAL figure the estimator is scored against.
    gas: u64,
    /// True initialised crossings (the pool's `cross()` count).
    crossings: usize,
    /// Bitmap words the walk touched.
    words: usize,
    /// `|tick_after − tick_before| / spacing` — option (c)'s estimate.
    drift: usize,
    /// The pre-swap tick.
    tick_before: i32,
    tick_after: i32,
    /// The deployed `QuoterV2`'s `initializedTicksCrossed` for the same point
    /// (`None` where no canonical quoter serves the pool).
    quoter_crossings: Option<usize>,
}

// ─────────────────────────────────────────────────────────────────────────
// Least squares + quantiles (pure — unit-tested below)
// ─────────────────────────────────────────────────────────────────────────

/// `y = a + b·x`.
fn fit2(xs: &[f64], ys: &[f64]) -> (f64, f64) {
    assert_eq!(xs.len(), ys.len());
    let n = xs.len() as f64;
    assert!(n >= 2.0, "fit2 needs two points");
    let sx: f64 = xs.iter().sum();
    let sy: f64 = ys.iter().sum();
    let sxx: f64 = xs.iter().map(|x| x * x).sum();
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| x * y).sum();
    let denom = n * sxx - sx * sx;
    let b = if denom.abs() < f64::EPSILON {
        0.0
    } else {
        (n * sxy - sx * sy) / denom
    };
    (sy / n - b * sx / n, b)
}

/// `y = a + b·x1 + c·x2` (normal equations, Gaussian elimination with partial
/// pivoting).
fn fit3(rows: &[(f64, f64, f64)]) -> (f64, f64, f64) {
    let n = rows.len() as f64;
    assert!(rows.len() >= 3, "fit3 needs three points");
    let (mut s1, mut s2, mut sy) = (0.0, 0.0, 0.0);
    let (mut s11, mut s12, mut s22) = (0.0, 0.0, 0.0);
    let (mut s1y, mut s2y) = (0.0, 0.0);
    for (x1, x2, y) in rows {
        s1 += x1;
        s2 += x2;
        sy += y;
        s11 += x1 * x1;
        s12 += x1 * x2;
        s22 += x2 * x2;
        s1y += x1 * y;
        s2y += x2 * y;
    }
    let mut m = [[n, s1, s2, sy], [s1, s11, s12, s1y], [s2, s12, s22, s2y]];
    // Forward elimination with partial pivoting.
    for col in 0..3 {
        let mut pivot = col;
        for (offset, row) in m.iter().enumerate().skip(col + 1) {
            if row[col].abs() > m[pivot][col].abs() {
                pivot = offset;
            }
        }
        m.swap(col, pivot);
        let d = m[col][col];
        if d.abs() < 1e-12 {
            continue;
        }
        let pivot_row = m[col];
        for row in m.iter_mut().skip(col + 1) {
            let factor = row[col] / d;
            for (cell, pivot) in row.iter_mut().zip(pivot_row.iter()).skip(col) {
                *cell -= factor * pivot;
            }
        }
    }
    // Back substitution.
    let mut out = [0.0f64; 3];
    for row in (0..3).rev() {
        let mut acc = m[row][3];
        for k in row + 1..3 {
            acc -= m[row][k] * out[k];
        }
        out[row] = if m[row][row].abs() < 1e-12 {
            0.0
        } else {
            acc / m[row][row]
        };
    }
    (out[0], out[1], out[2])
}

/// The 3×3 inverse by the adjugate (the covariance diagonal `fit3_se` needs).
/// A singular matrix yields zeros, and the caller's standard errors read zero.
fn invert3(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let (a, b, c) = (m[0][0], m[0][1], m[0][2]);
    let (d, e, f) = (m[1][0], m[1][1], m[1][2]);
    let (g, h, i) = (m[2][0], m[2][1], m[2][2]);
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return [[0.0; 3]; 3];
    }
    let scale = 1.0 / det;
    [
        [
            (e * i - f * h) * scale,
            (c * h - b * i) * scale,
            (b * f - c * e) * scale,
        ],
        [
            (f * g - d * i) * scale,
            (a * i - c * g) * scale,
            (c * d - a * f) * scale,
        ],
        [
            (d * h - e * g) * scale,
            (b * g - a * h) * scale,
            (a * e - b * d) * scale,
        ],
    ]
}

/// `y = a + b·x1 + c·x2` with the OLS standard errors of the coefficients and
/// R²: the fit3 coefficients plus the confidence the report quotes.
fn fit3_se(rows: &[(f64, f64, f64)]) -> (f64, f64, f64, (f64, f64, f64), f64) {
    let n = rows.len();
    assert!(n >= 4, "fit3_se needs four points for a residual variance");
    let mut gram = [[0.0f64; 3]; 3];
    let mut rhs = [0.0f64; 3];
    for (x1, x2, y) in rows {
        let x = [1.0, *x1, *x2];
        for row in 0..3 {
            rhs[row] += x[row] * y;
            for column in 0..3 {
                gram[row][column] += x[row] * x[column];
            }
        }
    }
    let inverse = invert3(gram);
    let mut beta = [0.0f64; 3];
    for row in 0..3 {
        for column in 0..3 {
            beta[row] += inverse[row][column] * rhs[column];
        }
    }
    let ybar = rows.iter().map(|(_, _, y)| y).sum::<f64>() / n as f64;
    let (mut ss_res, mut ss_tot) = (0.0f64, 0.0f64);
    for (x1, x2, y) in rows {
        let predicted = beta[0] + beta[1] * x1 + beta[2] * x2;
        ss_res += (y - predicted) * (y - predicted);
        ss_tot += (y - ybar) * (y - ybar);
    }
    let sigma2 = ss_res / (n as f64 - 3.0);
    let standard_error = |index: usize| (sigma2 * inverse[index][index]).max(0.0).sqrt();
    let r2 = if ss_tot > 0.0 {
        1.0 - ss_res / ss_tot
    } else {
        1.0
    };
    (
        beta[0],
        beta[1],
        beta[2],
        (standard_error(0), standard_error(1), standard_error(2)),
        r2,
    )
}

/// R² of `y = a + b·x` on `xs`/`ys`.
fn r2_of2(xs: &[f64], ys: &[f64], a: f64, b: f64) -> f64 {
    let n = ys.len() as f64;
    let ybar = ys.iter().sum::<f64>() / n;
    let (mut ss_res, mut ss_tot) = (0.0f64, 0.0f64);
    for (x, y) in xs.iter().zip(ys) {
        ss_res += (y - (a + b * x)).powi(2);
        ss_tot += (y - ybar).powi(2);
    }
    if ss_tot > 0.0 {
        1.0 - ss_res / ss_tot
    } else {
        1.0
    }
}

/// Linear-interpolated quantile (`q` in `[0, 1]`) of an unsorted slice.
fn quantile(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let pos = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

// ─────────────────────────────────────────────────────────────────────────
// Part 1: the V3 crossing sweep
// ─────────────────────────────────────────────────────────────────────────

struct SweepPool {
    label: &'static str,
    pool: Address,
    token0: Address,
    token1: Address,
    fee: u32,
    /// Whether the canonical factory serves this pool (the `QuoterV2`
    /// cross-check only answers for those).
    canonical: bool,
}

const SWEEP_POOLS: &[SweepPool] = &[
    SweepPool {
        label: "usdc/weth-500",
        pool: address!("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"),
        token0: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        token1: address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        fee: 500,
        canonical: true,
    },
    SweepPool {
        label: "dai/usdc-100",
        pool: address!("0x5777d92f208679DB4b9778590Fa3CAB3aC9e2168"),
        token0: address!("0x6B175474E89094C44Da98b954EedeAC495271d0F"),
        token1: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        fee: 100,
        canonical: true,
    },
    SweepPool {
        label: "usdc/usdt-100",
        pool: address!("0x3416cF6C708Da44DB2624D63ea0AAef7113527C6"),
        token0: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        token1: address!("0xdAC17F958D2ee523a2206206994597C13D831ec7"),
        fee: 100,
        canonical: true,
    },
    SweepPool {
        label: "dai/usdc-500",
        pool: address!("0x6c6Bc977E13Df9b0de53b251522280BB72383700"),
        token0: address!("0x6B175474E89094C44Da98b954EedeAC495271d0F"),
        token1: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        fee: 500,
        canonical: true,
    },
    SweepPool {
        label: "fork-usdc/weth-100",
        pool: address!("0x1445F32D1A74872bA41f3D8cF4022E9996120b31"),
        token0: address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        token1: address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        fee: 100,
        canonical: false,
    },
];

/// How deep a single pool's sweep goes (`k = 0..=MAX_K`).
const MAX_K: usize = 8;

/// The static shape of one side of a pool: what a census or a bisection needs.
#[derive(Clone, Copy, Debug)]
struct LegShape {
    pool: Address,
    spacing: i32,
    zero_for_one: bool,
    tick_before: i32,
}

/// The minimal amount whose swap crosses at least `k` initialised ticks, by
/// bisection on the census (monotone: more input never crosses fewer ticks).
async fn amount_for_crossings(
    reads: &mut Reads<'_>,
    chain: &mut Chain<'_>,
    leg: LegShape,
    k: usize,
    hi: U256,
) -> Option<U256> {
    let mut lo = U256::ONE;
    let mut hi = hi;
    // Confirm the bracket is reachable at all.
    let top = chain.quote_v3(leg.pool, leg.zero_for_one, hi)?;
    let top_census = census(
        reads,
        leg.pool,
        leg.spacing,
        leg.tick_before,
        top.tick_after,
        leg.zero_for_one,
    )
    .await;
    if top_census.crossed_exclusive < k {
        return None;
    }
    for _ in 0..48 {
        if hi.saturating_sub(lo) <= U256::ONE {
            break;
        }
        let mid = lo + (hi - lo) / U256::from(2u64);
        let frame = chain.quote_v3(leg.pool, leg.zero_for_one, mid)?;
        let c = census(
            reads,
            leg.pool,
            leg.spacing,
            leg.tick_before,
            frame.tick_after,
            leg.zero_for_one,
        )
        .await;
        if c.crossed_exclusive >= k {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(hi)
}

/// Part 1: one pool/direction's `k → gas` sweep, plus the frame cross-checks.
async fn sweep_pool_direction(
    reads: &mut Reads<'_>,
    chain: &mut Chain<'_>,
    spec: &SweepPool,
    zero_for_one: bool,
    samples: &mut Vec<Sample>,
    notes: &mut Vec<String>,
) {
    let spacing = reads.tick_spacing(spec.pool).await;
    let liquidity = reads.liquidity(spec.pool).await;
    let (sqrt_price, tick_before) = reads.v3_slot0(spec.pool).await;
    let reserve = if zero_for_one {
        reserve0(liquidity, sqrt_price)
    } else {
        reserve1(liquidity, sqrt_price)
    };
    let (token_in, token_out) = if zero_for_one {
        (spec.token0, spec.token1)
    } else {
        (spec.token1, spec.token0)
    };
    println!();
    println!("-- {} zfo={zero_for_one} --", spec.label);
    println!(
        "   spacing={spacing} liq={liquidity} tick={tick_before} sqrt={sqrt_price} \
         reserve_in={:.3e}",
        reserve.to::<u128>() as f64
    );
    println!(
        "   {:>4}  {:>26}  {:>10}  {:>5}  {:>5}  {:>6}  {:>4}  {:>10}  {:>6}",
        "k", "amount_in", "gas", "cen", "warm", "drift", "wds", "tick_after", "quoter"
    );
    if reserve.is_zero() {
        println!("   no in-range liquidity at the anchor — no crossing sweep");
        return;
    }

    // The usable upper bound: the largest fraction of the in-range reserve whose
    // frame still completes. Past it the walk leaves the liquidity and marches
    // to the price limit — the `EMPTY-HALT` shape, priced separately.
    let mut hi = U256::ZERO;
    for frac in [1e-6f64, 3e-6, 1e-5, 3e-5, 1e-4, 3e-4, 1e-3, 3e-3, 1e-2] {
        let amount = U256::from((reserve.to::<u128>() as f64 * frac) as u128);
        if amount.is_zero() {
            continue;
        }
        if chain.quote_v3(spec.pool, zero_for_one, amount).is_some() {
            hi = amount;
        } else {
            notes.push(format!(
                "EMPTY-HALT {} zfo={zero_for_one}: a swap of {:.2e} (fraction {frac}) exhausts the \
                 {GAS_LIMIT} frame gas limit — the walk left the liquidity and marched to the \
                 price limit",
                spec.label,
                amount.to::<u128>() as f64
            ));
            break;
        }
    }
    if hi.is_zero() {
        println!("   no frame completed: the smallest ladder amount already exhausts the book");
        return;
    }

    let leg = LegShape {
        pool: spec.pool,
        spacing,
        zero_for_one,
        tick_before,
    };
    // The crossing boundary for each k, then the midpoint of the k-window.
    let mut boundary = U256::ONE;
    let mut written = 0usize;
    for k in 0..=MAX_K {
        let amount = if k == 0 {
            match amount_for_crossings(reads, chain, leg, 1, hi).await {
                Some(b) if b > U256::from(2u64) => b / U256::from(2u64),
                _ => hi / U256::from(1_000_000u64),
            }
        } else {
            match amount_for_crossings(reads, chain, leg, k + 1, hi).await {
                Some(next) if next > boundary => boundary + (next - boundary) / U256::from(2u64),
                _ => boundary + (hi - boundary) / U256::from(2u64),
            }
        };
        if amount.is_zero() {
            continue;
        }
        let Some(frame) = chain.quote_v3(spec.pool, zero_for_one, amount) else {
            println!("   {k:>4}  frame exhausted at {amount} — sweep stops");
            break;
        };
        if k == 0 {
            println!(
                "   self-check: slot0() in callback ok={} pool={} caller={} | deltas {} / {}                  sqrt_after {} (pre {sqrt_price})",
                frame.slot0_call_ok,
                frame.pool,
                frame.caller,
                frame.amount0_delta,
                frame.amount1_delta,
                frame.sqrt_price_after
            );
        }
        let cen = census(
            reads,
            spec.pool,
            spacing,
            tick_before,
            frame.tick_after,
            zero_for_one,
        )
        .await;
        let distinct_words = cen.distinct_tick_words(spacing);
        let warm = cen.crossed_exclusive.saturating_sub(distinct_words);
        let drift = usize::try_from((frame.tick_after - tick_before).unsigned_abs())
            .unwrap_or(usize::MAX)
            / usize::try_from(spacing).unwrap_or(1);
        let quoter = if spec.canonical {
            chain
                .quote_v3_canonical(token_in, token_out, spec.fee, amount)
                .map(|(_, crossings, _, gas)| (crossings, gas))
        } else {
            None
        };
        println!(
            "   {k:>4}  {:>26}  {:>10}  {:>5}  {:>5}  {:>6}  {:>4}  {:>10}  {:>6}",
            amount.to_string(),
            frame.gas,
            cen.crossed_exclusive,
            warm,
            drift,
            cen.words,
            frame.tick_after,
            quoter.map_or("-".to_string(), |(c, _)| c.to_string())
        );

        samples.push(Sample {
            family: Family::V3,
            group: format!(
                "{}-{}",
                spec.label,
                if zero_for_one { "zfo" } else { "ofz" }
            ),
            amount_in: amount,
            gas: frame.gas,
            crossings: cen.crossed_exclusive,
            words: cen.words.max(1),
            drift,
            tick_before,
            tick_after: frame.tick_after,
            quoter_crossings: quoter.map(|(c, _)| c),
        });
        written += 1;
        if k >= 1 {
            boundary = amount;
        }
        if boundary >= hi {
            break;
        }
    }
    println!("   ({written} samples; usable bound {hi})");
}

// ─────────────────────────────────────────────────────────────────────────
// Part 2: V2 and per-token-transfer coefficients
// ─────────────────────────────────────────────────────────────────────────

const V2_LEGS: &[(Address, Address, Address, &str)] = &[
    (
        ROUTER_SUSHI,
        address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        "sushi usdc/weth",
    ),
    (
        ROUTER_V2,
        address!("0x408e41876cccdc0f92210600ef50372656052a38"),
        address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        "uni ren/usdc",
    ),
];

fn v2_legs(chain: &mut Chain<'_>, samples: &mut Vec<Sample>) {
    println!();
    println!("---- V2 legs (the constant) ----");
    for (router, token_in, token_out, label) in V2_LEGS {
        let mut gases = Vec::new();
        for exp in [15u32, 18, 21] {
            let amount = ten_pow(exp);
            let (gas, amount_out) = chain.quote_v2(*router, *token_in, *token_out, amount);
            println!(
                "   {label:<18} in=10^{exp:<2} gas={gas:<8} out={}",
                amount_out.map_or("-".to_string(), |v| v.to_string())
            );
            gases.push(gas);
        }
        let gas = *gases.iter().min().unwrap_or(&0);
        samples.push(Sample {
            family: Family::V2,
            group: format!("{label} (config route)"),
            amount_in: ten_pow(18),
            gas,
            crossings: 0,
            words: 0,
            drift: 0,
            tick_before: 0,
            tick_after: 0,
            quoter_crossings: None,
        });
    }
}

/// A holder of `token` at the anchor, plus the storage slot its `balanceOf`
/// mapping lives at — probed by matching the token's own `balanceOf(holder)`
/// against `keccak(holder ‖ slot)` for `slot = 0..=24`. The probe is
/// self-checking: a slot either reproduces the chain's balance exactly or is
/// rejected.
async fn probe_balance_slot(
    reads: &mut Reads<'_>,
    token: Address,
    holder: Address,
) -> Option<U256> {
    let out = reads
        .call(
            token,
            calldata(selector("balanceOf(address)"), &[w_addr(holder)]),
        )
        .await;
    let balance = out_words(&out).first().copied()?;
    if balance.is_zero() {
        return None;
    }
    let mut holder_word = [0u8; 32];
    holder_word[12..].copy_from_slice(holder.as_slice());
    for slot in 0u64..=24 {
        let mut preimage = Vec::with_capacity(64);
        preimage.extend_from_slice(&holder_word);
        preimage.extend_from_slice(&w_small(slot));
        let hashed = U256::from_be_bytes(keccak256(&preimage).0);
        if reads.storage(token, hashed).await == balance {
            return Some(U256::from(slot));
        }
    }
    None
}

/// The marginal cost of one ERC-20 `transfer`, measured on the real token: a
/// funded `transfer` frame's gas minus the transaction's intrinsic 21 000.
/// `zero_to_nonzero` is the pool→executor leg (the executor's balance starts at
/// zero), `nonzero_to_nonzero` the settlement leg into a pool that already
/// holds a balance.
async fn transfer_cost(reads: &mut Reads<'_>, chain: &mut Chain<'_>) {
    println!();
    println!("---- the per-token-transfer marginal (real USDC) ----");
    let usdc = address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    let holder = address!("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");
    let Some(slot) = probe_balance_slot(reads, usdc, holder).await else {
        println!("   USDC balanceOf slot not found — transfer cost not measured");
        return;
    };
    println!("   USDC balanceOf mapping slot = {slot} (probed against the pool's own balance)");
    let payer = SENDER;
    let recipient = CALLBACK;
    let payer_slot = {
        let mut preimage = Vec::with_capacity(64);
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(payer.as_slice());
        preimage.extend_from_slice(&w);
        preimage.extend_from_slice(&w_small(slot.to::<u64>()));
        U256::from_be_bytes(keccak256(&preimage).0)
    };
    let recipient_slot = {
        let mut preimage = Vec::with_capacity(64);
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(recipient.as_slice());
        preimage.extend_from_slice(&w);
        preimage.extend_from_slice(&w_small(slot.to::<u64>()));
        U256::from_be_bytes(keccak256(&preimage).0)
    };
    chain
        .scratch
        .ext_mut()
        .insert_account_storage(&usdc, &payer_slot, &ten_pow(30));
    let amount = ten_pow(6);

    // Cold: the recipient has no balance (0 -> nonzero SSTORE).
    chain
        .scratch
        .ext_mut()
        .insert_account_storage(&usdc, &recipient_slot, &U256::ZERO);
    let data = calldata(
        selector("transfer(address,uint256)"),
        &[w_addr(recipient), w_uint(amount)],
    );
    let cold = chain.frame(payer, usdc, data.clone());
    // Warm: the recipient already holds a balance (nonzero -> nonzero SSTORE).
    chain
        .scratch
        .ext_mut()
        .insert_account_storage(&usdc, &recipient_slot, &ten_pow(20));
    let warm = chain.frame(payer, usdc, data);
    println!(
        "   transfer frame gas: cold-recipient {} | warm-recipient {} => transfer marginal \
         (frame - 21 000 intrinsic): {} / {}",
        cold.gas,
        warm.gas,
        cold.gas.saturating_sub(21_000),
        warm.gas.saturating_sub(21_000)
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Part 3: the V4 leg (the unlock/delta path)
// ─────────────────────────────────────────────────────────────────────────

struct V4LegSpec {
    label: &'static str,
    key: V4PoolKey,
    zero_for_one: bool,
}

fn v4_corpus_legs() -> Vec<V4LegSpec> {
    let dai = address!("0x6B175474E89094C44Da98b954EedeAC495271d0F");
    let usdc = address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    let weth = address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    vec![
        V4LegSpec {
            label: "v4 dai/usdc-100-s1",
            key: V4PoolKey {
                currency0: dai,
                currency1: usdc,
                fee: 100,
                tick_spacing: 1,
                hooks: Address::ZERO,
            },
            zero_for_one: false,
        },
        V4LegSpec {
            label: "v4 usdc/weth-500-s10",
            key: V4PoolKey {
                currency0: usdc,
                currency1: weth,
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            zero_for_one: false,
        },
    ]
}

/// The swap-direction V4 tick map, read straight out of `PoolManager` storage.
async fn v4_tick_map(
    reads: &mut Reads<'_>,
    base: U256,
    spacing: i32,
    current_tick: i32,
    zero_for_one: bool,
    max_words: usize,
) -> HashMap<i32, TickInfo> {
    let center = word_of(current_tick, spacing);
    let lo = word_of(
        degenbot_math::concentrated_liquidity::tick_math::MIN_TICK,
        spacing,
    );
    let hi = word_of(
        degenbot_math::concentrated_liquidity::tick_math::MAX_TICK,
        spacing,
    );
    let mut words: Vec<i32> = if zero_for_one {
        (lo..=center).rev().collect()
    } else {
        (center..=hi).collect()
    };
    words.truncate(max_words);
    let mut map: HashMap<i32, TickInfo> = HashMap::new();
    for word in words {
        let Ok(pos) = i16::try_from(word) else {
            continue;
        };
        let bitmap = reads
            .storage(V4_POOL_MANAGER, v4_tick_bitmap_word_slot(pos, base))
            .await;
        if bitmap.is_zero() {
            continue;
        }
        for bit in 0..256usize {
            if !bitmap.bit(bit) {
                continue;
            }
            let compressed = i64::from(word) * 256 + i64::try_from(bit).unwrap();
            let tick = i32::try_from(compressed * i64::from(spacing)).expect("tick fits i32");
            let info = reads
                .storage(V4_POOL_MANAGER, v4_tick_mapping_slot(tick, base))
                .await;
            let (gross, net) = decode_tick_word(info);
            map.insert(
                tick,
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(gross),
                    liquidity_net: net,
                    block: PIN,
                },
            );
        }
    }
    map
}

/// A V4 hop's chain-derived white-box state: the pool's own `slot0`, its
/// in-range liquidity, and the walk's projection (the crossing source the sweep
/// uses as its x-axis).
struct V4Hop {
    slot0: degenbot_pools::V4Slot0Parts,
    liquidity: u128,
    reserve: U256,
    prepared: PreparedConcentratedLiquidityHop,
    /// The initialized ticks the pool's own bitmap carries inside the tracked
    /// horizon, ascending — the layout the word sweep engineers its ladder from.
    ticks: Vec<i32>,
}

async fn v4_hop(
    reads: &mut Reads<'_>,
    key: &V4PoolKey,
    zero_for_one: bool,
    horizon: usize,
) -> Option<V4Hop> {
    let pool_id = v4_pool_id(key);
    let base = v4_pool_state_base_slot(pool_id);
    let slot0 = decode_v4_slot0(reads.storage(V4_POOL_MANAGER, v4_slot0_slot(base)).await);
    let liquidity = (reads
        .storage(V4_POOL_MANAGER, v4_liquidity_slot(base))
        .await
        & U256::from(u128::MAX))
    .to::<u128>();
    let spacing = key.tick_spacing;
    let reserve = if zero_for_one {
        reserve0(liquidity, slot0.sqrt_price_x96)
    } else {
        reserve1(liquidity, slot0.sqrt_price_x96)
    };
    if liquidity == 0 {
        return None;
    }
    let tick_data = v4_tick_map(reads, base, spacing, slot0.tick, zero_for_one, horizon).await;
    let mut ticks: Vec<i32> = tick_data.keys().copied().collect();
    ticks.sort_unstable();
    let params = RegisterV4PoolParams {
        pool_manager: V4_POOL_MANAGER,
        pool_id: pool_id.0,
        pool_key: V4PoolKey {
            fee: slot0.lp_fee,
            ..key.clone()
        },
        state_view: Address::ZERO,
        hook_flags: 0,
        protocol_fee: slot0.protocol_fee,
        sqrt_price_x96: slot0.sqrt_price_x96,
        liquidity,
        tick: slot0.tick,
        tick_data: tick_data.into_iter().collect(),
        update_block: PIN,
        tick_data_block: Some(PIN),
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: Vec::new(),
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    let seq = state.build_int_v4_sequence(spacing, slot0.lp_fee, zero_for_one)?;
    Some(V4Hop {
        slot0,
        liquidity,
        reserve,
        prepared: PreparedConcentratedLiquidityHop::derive(Arc::new(seq)),
        ticks,
    })
}

async fn v4_legs(
    reads: &mut Reads<'_>,
    chain: &mut Chain<'_>,
    samples: &mut Vec<Sample>,
    notes: &mut Vec<String>,
) {
    println!();
    println!("---- V4 legs (the unlock/delta path) ----");
    let mut quoter = V4BlackBoxQuoter::new(V4QuoterConfig::default());
    for spec in v4_corpus_legs() {
        let Some(hop) = v4_hop(reads, &spec.key, spec.zero_for_one, 400).await else {
            println!("\n   {}: no usable V4 state at the anchor", spec.label);
            continue;
        };
        let liquidity = hop.liquidity;
        let slot0 = hop.slot0;
        let spacing = spec.key.tick_spacing;
        let reserve = hop.reserve;
        println!(
            "\n   {} tick={} spacing={spacing} liq={liquidity} reserve_in={:.3e}",
            spec.label,
            slot0.tick,
            reserve.to::<u128>() as f64
        );
        if liquidity == 0 {
            println!("      no in-range liquidity at the anchor — nothing to price");
            continue;
        }
        let prepared = hop.prepared;
        let sim = ConcentratedLiquidityPathSim::new(
            &[&prepared.int_seq],
            Some(std::slice::from_ref(&prepared)),
        );
        let leg = V4Leg::canonical(spec.key.clone(), spec.zero_for_one);
        println!(
            "      {:>26}  {:>10}  {:>5}  {:>5}  {:>10}",
            "amount_in", "gas", "xing", "words", "gasEst"
        );
        for frac in [1e-6f64, 1e-5, 1e-4, 3e-4, 1e-3, 3e-3] {
            let amount = (reserve.to::<u128>() as f64 * frac) as u128;
            if amount == 0 {
                continue;
            }
            let amount = U256::from(amount);
            let outcome = sim.output(amount);
            let landed = outcome.landed.first().copied().unwrap_or(0);
            let words = words_walked(&prepared, landed);
            match quoter.quote(&mut chain.scratch, &leg, amount) {
                Ok(quote) => {
                    println!(
                        "      {:>26}  {:>10}  {:>5}  {:>5}  {:>10}",
                        amount.to_string(),
                        quote.gas_used,
                        landed,
                        words,
                        quote.gas_estimate
                    );
                    samples.push(Sample {
                        family: Family::V4,
                        group: format!("{} (black-box frame)", spec.label),
                        amount_in: amount,
                        gas: quote.gas_used,
                        crossings: landed,
                        words,
                        drift: 0,
                        tick_before: slot0.tick,
                        tick_after: slot0.tick,
                        quoter_crossings: None,
                    });
                }
                Err(
                    V4QuoteError::Reverted { gas_used, .. } | V4QuoteError::Halted { gas_used, .. },
                ) => println!(
                    "      {:>26}  frame refused (gas {gas_used})",
                    amount.to_string()
                ),
                Err(other) => println!("      {amount}: frame error {other}"),
            }
        }
        // The V4 quote's white-box figure is only a cross-check: the pool's own
        // amountOut must equal the walk's.
        let probe = (reserve.to::<u128>() as f64 * 1e-4) as u128;
        if probe > 0 {
            let amount = U256::from(probe);
            let sim_out = sim.output(amount).final_output;
            if let Ok(quote) = quoter.quote(&mut chain.scratch, &leg, amount) {
                let equal = sim_out == quote.amount_out;
                println!(
                    "      parity: walk {sim_out} vs chain {} (equal: {equal})",
                    quote.amount_out,
                );
                if !equal {
                    notes.push(format!(
                        "PARITY {}: the walk answers {sim_out} where the chain answers {} — the                          sweep's crossing axis is not trustworthy for this pool",
                        spec.label, quote.amount_out
                    ));
                }
            }
        }
    }
}

/// Bitmap words a swap marches, by the walk's own projection: one for the range
/// the price starts in, plus every empty-word boundary the landed ranges carry.
fn words_walked(prepared: &PreparedConcentratedLiquidityHop, landed: usize) -> usize {
    1 + prepared
        .int_seq
        .ranges
        .iter()
        .take(landed + 1)
        .map(|r| r.word_boundary_prices.len())
        .sum::<usize>()
}

// ─────────────────────────────────────────────────────────────────────────
// Part 3b: the V4 bitmap-word sweep (the floor's last unvalidated term)
// ─────────────────────────────────────────────────────────────────────────

/// A hookless candidate the word sweep probes. V4's tick map lives in the
/// singleton `PoolManager`'s storage, so one bitmap word is one `SLOAD` of the
/// manager's account however many pools a path touches — the account-warmth
/// question the shared-frame experiment answers. Hooks are excluded: a hook's
/// own `beforeSwap`/`afterSwap` is gas the floor prices separately and would
/// only add noise to the word term.
///
/// The candidates come from the registration DB
/// (`~/.local/state/degenbot/db/degenbot.db`): hookless pools with live
/// mid-anchor liquidity, one per tick layout —
///
/// ```sql
/// SELECT p.tick_spacing, t0.address, t1.address, p.fee_currency0,
///        group_concat(l.tick)
///   FROM uniswap_v4_pools p
///   JOIN erc20_tokens t0 ON t0.id = p.currency0_id
///   JOIN erc20_tokens t1 ON t1.id = p.currency1_id
///   JOIN managed_pool_liquidity_positions l
///     ON l.managed_pool_id = p.managed_pool_id
///  WHERE lower(p.hooks) = '0x0000000000000000000000000000000000000000'
///    AND p.fee_denominator = 1000000
///  GROUP BY p.managed_pool_id;
/// ```
///
/// A spacing-1 pair whose words above the price are sparse, a spacing-10 pool
/// with a dense map (the other extreme), a deep stable pair, and the corpus's
/// own two hookless V4 legs. The rig prints each pool's bitmap layout before it
/// sweeps, and a pool with no live liquidity at the anchor drops out with a
/// note rather than a silent zero.
struct V4WordPool {
    label: &'static str,
    key: V4PoolKey,
}

fn v4_word_pools() -> Vec<V4WordPool> {
    let usdc = address!("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    let usdt = address!("0xdAC17F958D2ee523a2206206994597C13D831ec7");
    let dai = address!("0x6B175474E89094C44Da98b954EedeAC495271d0F");
    let weth = address!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    vec![
        V4WordPool {
            label: "v4 dai/usdc-100-s1",
            key: V4PoolKey {
                currency0: dai,
                currency1: usdc,
                fee: 100,
                tick_spacing: 1,
                hooks: Address::ZERO,
            },
        },
        V4WordPool {
            label: "v4 usdc/weth-500-s10",
            key: V4PoolKey {
                currency0: usdc,
                currency1: weth,
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
        },
        V4WordPool {
            label: "v4 eth/usdc-100-s1",
            key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: usdc,
                fee: 100,
                tick_spacing: 1,
                hooks: Address::ZERO,
            },
        },
        V4WordPool {
            label: "v4 usdc/usdt-100-s1",
            key: V4PoolKey {
                currency0: usdc,
                currency1: usdt,
                fee: 100,
                tick_spacing: 1,
                hooks: Address::ZERO,
            },
        },
    ]
}

/// How many bitmap words one side of the ladder marches.
const V4_WORD_MAX: usize = 10;

/// Bisection steps for a word boundary (the amount range is wide; 14 halvings
/// land inside one wei of the boundary on any leg reached here).
const V4_WORD_BISECT: usize = 14;

/// The tick-map horizon the walk's projection tracks, in bitmap words. The
/// sweep's x-axis is the CHAIN's word count (see [`v4_bitmap_words`]); this
/// only bounds the white-box projection the report cross-checks against.
const V4_WORD_HORIZON: usize = 96;

/// One side of a pool's word ladder: the leg plus the static shape its word
/// count needs (the pool's `Pool.State` base, the current bitmap word, the
/// swapped direction, and the tracked initialized ticks).
struct WordLadder {
    leg: V4Leg,
    base: U256,
    current_word: i32,
    zero_for_one: bool,
    ticks: Vec<i32>,
}

/// One black-box V4 quote frame's measurement.
struct V4Probe {
    gas: u64,
    amount_out: U256,
    /// Bitmap words the pool's own bytecode read (see [`v4_bitmap_words`]).
    words: usize,
    /// Initialized ticks the pool's own bytecode crossed: every `cross()` reads
    /// the tick's `Tick.Info` word, so the tracked ticks whose slots the frame
    /// touched ARE the crossing count.
    crossings: usize,
}

/// The chain's own word count, read out of the frame's touched slot set: the
/// swap loop reads one `tickBitmap` word per step, so the contiguous run of the
/// pool's bitmap-word slots in `touched` IS the number of words marched. The
/// white-box walk is the projection under test; this is the pool's bytecode
/// answering the same question, exactly as the V3 sweep's census does for
/// crossings.
fn v4_bitmap_words(
    touched: &[(Address, Vec<U256>)],
    base: U256,
    current_word: i32,
    zero_for_one: bool,
) -> usize {
    let slots = touched
        .iter()
        .find(|(address, _)| *address == V4_POOL_MANAGER)
        .map_or(&[][..], |(_, slots)| slots.as_slice());
    let step = if zero_for_one { -1i32 } else { 1 };
    let mut words = 0usize;
    let mut word = current_word;
    while let Ok(pos) = i16::try_from(word) {
        if !slots.contains(&v4_tick_bitmap_word_slot(pos, base)) {
            break;
        }
        words += 1;
        word += step;
    }
    words
}

/// Execute one V4 quote frame at the canonical quoter (the frame the corpus's
/// V4 legs are priced in) and keep the touched set the word count needs.
fn v4_quote_frame(chain: &mut Chain<'_>, ladder: &WordLadder, amount: U256) -> Option<V4Probe> {
    let config = V4QuoterConfig::default();
    let tx = ReplayableTx {
        from: config.from,
        to: Some(config.quoter),
        value: U256::ZERO,
        data: quote_calldata(&ladder.leg, amount),
        gas_limit: config.gas_limit,
        max_fee_per_gas: config.max_fee_per_gas,
        max_priority_fee_per_gas: config.max_priority_fee_per_gas,
        nonce: config.nonce,
    };
    let outcome = chain.scratch.replay(&tx).ok()?;
    if outcome.status != RunVerdict::Success {
        return None;
    }
    let amount_out = decode_quote_return(&outcome.output)?.0;
    let slots = outcome
        .touched
        .iter()
        .find(|(address, _)| *address == V4_POOL_MANAGER)
        .map_or(&[][..], |(_, slots)| slots.as_slice());
    let crossings = ladder
        .ticks
        .iter()
        .filter(|tick| slots.contains(&v4_tick_mapping_slot(**tick, ladder.base)))
        .count();
    Some(V4Probe {
        gas: outcome.gas_used,
        amount_out,
        words: v4_bitmap_words(
            &outcome.touched,
            ladder.base,
            ladder.current_word,
            ladder.zero_for_one,
        ),
        crossings,
    })
}

/// The smallest amount whose frame marches at least `target` bitmap words, by
/// bisection on the chain's own count (monotone: more input never marches
/// fewer words). `None` when even the top of the ladder cannot reach `target`
/// — the `EMPTY-HALT` shape jumps the count instead of stepping it.
fn amount_for_words(
    chain: &mut Chain<'_>,
    ladder: &WordLadder,
    target: usize,
    hi: U256,
) -> Option<U256> {
    let top = v4_quote_frame(chain, ladder, hi)?;
    if top.words < target {
        return None;
    }
    let (mut lo, mut hi) = (U256::ZERO, hi);
    for _ in 0..V4_WORD_BISECT {
        if hi.saturating_sub(lo) <= U256::ONE {
            break;
        }
        let mid = lo + (hi - lo) / U256::from(2u64);
        match v4_quote_frame(chain, ladder, mid) {
            Some(probe) if probe.words >= target => hi = mid,
            _ => lo = mid,
        }
    }
    Some(hi)
}

/// One swept V4 leg point: the frame's gas against the chain's word count and
/// the walk's crossing count.
#[derive(Clone, Debug)]
struct WordSample {
    group: String,
    amount_in: U256,
    gas: u64,
    /// Initialized ticks the frame crossed, from the touched tick-info slots.
    crossings: usize,
    /// The white-box walk's landing index for the same amount (cross-check).
    crossings_walk: usize,
    words_chain: usize,
    words_walk: usize,
}

/// Part 3b: one V4 pool/direction's `words → gas` ladder.
async fn v4_word_sweep(
    reads: &mut Reads<'_>,
    chain: &mut Chain<'_>,
    spec: &V4WordPool,
    zero_for_one: bool,
    samples: &mut Vec<WordSample>,
    notes: &mut Vec<String>,
) {
    let spacing = spec.key.tick_spacing;
    let Some(hop) = v4_hop(reads, &spec.key, zero_for_one, V4_WORD_HORIZON).await else {
        println!("\n   {}: no usable V4 state at the anchor", spec.label);
        return;
    };
    let liquidity = hop.liquidity;
    let slot0 = hop.slot0;
    let reserve = hop.reserve;
    let base = v4_pool_state_base_slot(v4_pool_id(&spec.key));
    let current_word = word_of(slot0.tick, spacing);
    println!();
    println!(
        "-- {} zfo={zero_for_one} (the bitmap-word ladder) --",
        spec.label
    );
    if liquidity == 0 {
        println!("   no in-range liquidity at the anchor — nothing to price");
        return;
    }
    println!(
        "   tick={} spacing={spacing} liq={liquidity} word={current_word} reserve_in={:.3e}",
        slot0.tick,
        reserve.to::<u128>() as f64
    );
    // The layout, empirically: the pool's own initialized ticks in the swap
    // direction and the words they sit in. The gaps between those words are
    // what the ladder has to step through, so they set the ladder's range.
    {
        let mut next: Vec<i32> = hop
            .ticks
            .iter()
            .copied()
            .filter(|tick| {
                if zero_for_one {
                    *tick < slot0.tick
                } else {
                    *tick > slot0.tick
                }
            })
            .collect();
        if zero_for_one {
            next.reverse();
        }
        let upcoming: Vec<String> = next
            .iter()
            .take(5)
            .map(|tick| format!("{tick} (word {})", word_of(*tick, spacing)))
            .collect();
        println!(
            "   layout: {} initialized ticks tracked; next in the swap direction: {}",
            hop.ticks.len(),
            upcoming.join(" ")
        );
    }
    let ladder = WordLadder {
        leg: V4Leg::canonical(spec.key.clone(), zero_for_one),
        base,
        current_word,
        zero_for_one,
        ticks: hop.ticks.clone(),
    };

    // The usable upper bound: the largest amount on a log ladder whose frame
    // completes. Past it the walk leaves the book and marches to the price
    // limit — the `EMPTY-HALT` shape, recorded as a finding.
    let mut hi = U256::ZERO;
    for frac in [
        1e-9f64, 1e-8, 1e-7, 3e-7, 1e-6, 3e-6, 1e-5, 3e-5, 1e-4, 3e-4, 1e-3, 3e-3, 1e-2, 3e-2, 1e-1,
    ] {
        let amount = U256::from((reserve.to::<u128>() as f64 * frac) as u128);
        if amount.is_zero() {
            continue;
        }
        if v4_quote_frame(chain, &ladder, amount).is_some() {
            hi = amount;
        } else {
            notes.push(format!(
                "V4 EMPTY-HALT {} zfo={zero_for_one}: a swap of {amount} (fraction {frac} of the \
                 in-range reserve) exhausts the {GAS_LIMIT} frame gas limit — the walk left the \
                 book and marched to the price limit",
                spec.label
            ));
            break;
        }
    }
    if hi.is_zero() {
        println!("   no frame completed: the smallest ladder amount already exhausts the book");
        return;
    }

    let prepared = hop.prepared;
    let sim = ConcentratedLiquidityPathSim::new(
        &[&prepared.int_seq],
        Some(std::slice::from_ref(&prepared)),
    );
    // The word boundaries: `edges[k]` is the smallest amount whose frame marches
    // at least `k` words. A zero edge means the ladder's top cannot reach it.
    let mut edges: Vec<U256> = vec![U256::ZERO; V4_WORD_MAX + 2];
    edges[1] = U256::ONE;
    for (target, edge) in edges.iter_mut().enumerate().skip(2) {
        *edge = amount_for_words(chain, &ladder, target, hi).unwrap_or(U256::ZERO);
    }
    println!(
        "   {:>4}  {:>26}  {:>9}  {:>5}  {:>4}  {:>5}  {:>5}  {:>8}",
        "k", "amount_in", "gas", "words", "walk", "xing", "xwalk", "marg/wd"
    );
    let mut previous: Option<(u64, usize)> = None;
    for k in 1..=V4_WORD_MAX {
        let lower = edges[k];
        if lower.is_zero() {
            // The ladder's top could not reach `k` words: the count jumps past
            // it (a swap that leaves the book marches every empty word of the
            // stretch in one frame), so the rest of the ladder is unreachable.
            break;
        }
        let upper = edges[k + 1];
        let amount = if upper.is_zero() || upper <= lower {
            if lower >= hi {
                continue;
            }
            lower + (hi - lower) / U256::from(2u64)
        } else {
            lower + (upper - lower) / U256::from(2u64)
        };
        if amount.is_zero() {
            continue;
        }
        let Some(probe) = v4_quote_frame(chain, &ladder, amount) else {
            break;
        };
        let outcome = sim.output(amount);
        let landed = outcome.landed.first().copied().unwrap_or(0);
        let walk_words = words_walked(&prepared, landed);
        let marginal = previous.map_or(f64::NAN, |(gas, words)| {
            let delta_words = probe.words.saturating_sub(words).max(1) as f64;
            (probe.gas as f64 - gas as f64) / delta_words
        });
        println!(
            "   {k:>4}  {:>26}  {:>9}  {:>5}  {:>4}  {:>5}  {:>5}  {marginal:>8.0}",
            amount.to_string(),
            probe.gas,
            probe.words,
            walk_words,
            probe.crossings,
            landed
        );
        // The walk must answer the chain's amountOut, or the walk's own axes
        // are not the pool's and only the chain-derived counts are usable.
        if landed > 0 && outcome.final_output != probe.amount_out && notes.len() < 200 {
            notes.push(format!(
                "V4 WORD PARITY {} zfo={zero_for_one} at {amount}: walk {} vs chain {}",
                spec.label, outcome.final_output, probe.amount_out
            ));
        }
        samples.push(WordSample {
            group: format!("{} zfo={zero_for_one}", spec.label),
            amount_in: amount,
            gas: probe.gas,
            crossings: probe.crossings,
            crossings_walk: landed,
            words_chain: probe.words,
            words_walk: walk_words,
        });
        previous = Some((probe.gas, probe.words));
    }
}

/// The V4 word coefficient: `gas = a + b_cross·crossings + b_word·words`,
/// fitted per crossing level (the cleanest identification — words move while
/// crossings do not) and pooled with its standard errors.
fn v4_word_report(samples: &[WordSample], notes: &mut Vec<String>) {
    println!();
    println!("=== the V4 bitmap-word coefficient ===");
    if samples.is_empty() {
        println!("   (no V4 word samples: every candidate lacked live liquidity)");
        return;
    }
    let mismatched_words = samples
        .iter()
        .filter(|s| s.words_walk != s.words_chain)
        .count();
    let mismatched_crossings = samples
        .iter()
        .filter(|s| s.crossings_walk != s.crossings)
        .count();
    println!(
        "   {} samples over {} groups; the walk's projection disagrees with the chain's touched \
         set on {mismatched_words} words / {mismatched_crossings} crossings — the fits below use \
         the chain's own counts",
        samples.len(),
        {
            let mut groups: Vec<&String> = samples.iter().map(|s| &s.group).collect();
            groups.sort();
            groups.dedup();
            groups.len()
        }
    );
    {
        let mut ladder_groups: Vec<&String> = samples.iter().map(|s| &s.group).collect();
        ladder_groups.sort();
        ladder_groups.dedup();
        for group in &ladder_groups {
            let group_rows: Vec<&WordSample> =
                samples.iter().filter(|s| &s.group == *group).collect();
            let low = group_rows
                .iter()
                .map(|s| s.amount_in)
                .min()
                .unwrap_or_default();
            let high = group_rows
                .iter()
                .map(|s| s.amount_in)
                .max()
                .unwrap_or_default();
            println!(
                "   {group}: {} samples, amount_in {:.3e}…{:.3e}, words {}…{}",
                group_rows.len(),
                low.to::<u128>() as f64,
                high.to::<u128>() as f64,
                group_rows
                    .iter()
                    .map(|s| s.words_chain)
                    .min()
                    .unwrap_or_default(),
                group_rows
                    .iter()
                    .map(|s| s.words_chain)
                    .max()
                    .unwrap_or_default()
            );
        }
    }
    println!(
        "   {:>32}  {:>4}  {:>10}  {:>10}  {:>4}  {:>7}",
        "group", "xing", "fixed", "per_word", "n", "r2"
    );
    let mut levels: Vec<&String> = samples.iter().map(|s| &s.group).collect();
    levels.sort();
    levels.dedup();
    let mut per_word: Vec<f64> = Vec::new();
    let mut per_level_intercept: Vec<f64> = Vec::new();
    // The pooled constant-crossing slope: each level's (words, gas) pairs are
    // demeaned inside the level, so only the word variation identifies b_word
    // and the level's own intercept (a pool's fixed cost at that crossing
    // count) cannot leak into it.
    let (mut sxy, mut sxx) = (0.0f64, 0.0f64);
    let mut demeaned: Vec<(f64, f64)> = Vec::new();
    for group in levels {
        let mut crossing_levels: Vec<usize> = samples
            .iter()
            .filter(|s| &s.group == group)
            .map(|s| s.crossings)
            .collect();
        crossing_levels.sort_unstable();
        crossing_levels.dedup();
        for level in crossing_levels {
            let rows: Vec<&WordSample> = samples
                .iter()
                .filter(|s| &s.group == group && s.crossings == level)
                .collect();
            let mut word_levels: Vec<usize> = rows.iter().map(|s| s.words_chain).collect();
            word_levels.sort_unstable();
            word_levels.dedup();
            if rows.len() < 3 || word_levels.len() < 3 {
                println!(
                    "   {group:>32}  {level:>4}  (degenerate: {} samples, {} word level(s))",
                    rows.len(),
                    word_levels.len()
                );
                continue;
            }
            let xs: Vec<f64> = rows.iter().map(|s| s.words_chain as f64).collect();
            let ys: Vec<f64> = rows.iter().map(|s| s.gas as f64).collect();
            let (a, b) = fit2(&xs, &ys);
            per_word.push(b);
            per_level_intercept.push(a);
            println!(
                "   {group:>32}  {level:>4}  {a:>10.0}  {b:>10.1}  {:>4}  {:>7.4}",
                rows.len(),
                r2_of2(&xs, &ys, a, b)
            );
            let mean_words = xs.iter().sum::<f64>() / xs.len() as f64;
            let mean_gas = ys.iter().sum::<f64>() / ys.len() as f64;
            for (x, y) in xs.iter().zip(&ys) {
                let dx = x - mean_words;
                let dy = y - mean_gas;
                sxy += dx * dy;
                sxx += dx * dx;
                demeaned.push((dx, dy));
            }
        }
    }
    // The pooled constant-crossing slope: the clean identification of b_word.
    // Pooled over every level, `b_word = Σdx·dy / Σdx²` on the demeaned pairs,
    // with the through-origin residual as its standard error.
    let mut pooled_word: Option<(f64, f64)> = None;
    if sxx > 0.0 && per_word.len() >= 2 {
        let b_word = sxy / sxx;
        let sse: f64 = demeaned.iter().map(|(x, y)| (y - b_word * x).powi(2)).sum();
        let ss_tot: f64 = demeaned.iter().map(|(_, y)| y * y).sum();
        let dof = (demeaned.len() as f64 - 1.0).max(1.0);
        let se_word = (sse / dof / sxx).sqrt();
        let r2 = if ss_tot > 0.0 {
            1.0 - sse / ss_tot
        } else {
            1.0
        };
        let abs: Vec<f64> = demeaned
            .iter()
            .map(|(x, y)| (y - b_word * x).abs())
            .collect();
        println!();
        println!(
            "   pooled constant-crossing fit (gas = a_level + b_word·words, n={} over {} levels):",
            demeaned.len(),
            per_word.len()
        );
        println!("      b_word  = {b_word:>10.1}  ± {se_word:>7.1}   (R² {r2:.4})");
        println!(
            "      residual |gas| p50 {:.0}  p90 {:.0}  max {:.0}",
            quantile(&abs, 0.5),
            quantile(&abs, 0.9),
            quantile(&abs, 1.0)
        );
        println!(
            "      the per-level intercepts (a pool's fixed cost at that crossing count) span \
             {:.0}…{:.0}",
            quantile(&per_level_intercept, 0.0),
            quantile(&per_level_intercept, 1.0)
        );
        pooled_word = Some((b_word, se_word));
    }
    if let Some((b_word, se_word)) = pooled_word {
        println!();
        println!(
            "   the floor borrows the V3 rate 7,373 gas/word: the measured V4 rate is {b_word:.0} \
             ± {se_word:.0} ({:.0}% of it), {:.0} at the median constant-crossing level",
            b_word / 7_373.0 * 100.0,
            quantile(&per_word, 0.5)
        );
        if (b_word - 7_373.0).abs() > 0.05 * 7_373.0 {
            notes.push(format!(
                "V4 WORD: the measured per-word coefficient {b_word:.0} (±{se_word:.0}) differs from \
                 the V3 7,373 the floor borrows — the floor needs a per-family b_word"
            ));
        }
    }

    // The within-group three-parameter fit, as a collinearity cross-check: when
    // a ladder's crossings and words move together the two slopes trade off, so
    // its b_word is the WEAKER estimate and is quoted only beside the pooled one.
    let mut groups: Vec<&String> = samples.iter().map(|s| &s.group).collect();
    groups.sort();
    groups.dedup();
    let mut rows: Vec<(f64, f64, f64)> = Vec::new();
    let mut intercepts: Vec<f64> = Vec::new();
    for group in &groups {
        let group_rows: Vec<&WordSample> = samples.iter().filter(|s| &s.group == *group).collect();
        if group_rows.len() < 3 {
            continue;
        }
        let mean_crossings =
            group_rows.iter().map(|s| s.crossings as f64).sum::<f64>() / group_rows.len() as f64;
        let mean_words =
            group_rows.iter().map(|s| s.words_chain as f64).sum::<f64>() / group_rows.len() as f64;
        let mean_gas =
            group_rows.iter().map(|s| s.gas as f64).sum::<f64>() / group_rows.len() as f64;
        for sample in &group_rows {
            rows.push((
                sample.crossings as f64 - mean_crossings,
                sample.words_chain as f64 - mean_words,
                sample.gas as f64 - mean_gas,
            ));
        }
        // The group's own level: a + b_cross·mean_crossings + b_word·mean_words.
        intercepts.push(mean_gas);
    }
    if rows.len() >= 4 {
        let (offset, b_cross, b_word, (_, se_cross, se_word), r2) = fit3_se(&rows);
        println!();
        println!(
            "   within-group fit (gas = a_pool + b_cross·crossings + b_word·words, n={} over {} \
             groups):",
            rows.len(),
            groups.len()
        );
        println!("      b_cross = {b_cross:>10.1}  ± {se_cross:>7.1}");
        println!("      b_word  = {b_word:>10.1}  ± {se_word:>7.1}   (R² {r2:.4})");
        println!(
            "      group level (a + marginals at the group's mean) = {offset:>9.0} ± the pool \
             fixed costs {:.0}…{:.0}",
            quantile(&intercepts, 0.0),
            quantile(&intercepts, 1.0)
        );
        let residuals: Vec<f64> = rows
            .iter()
            .map(|(crossings, words, gas)| gas - (offset + b_cross * crossings + b_word * words))
            .collect();
        let abs: Vec<f64> = residuals.iter().map(|r| r.abs()).collect();
        println!(
            "      residual |gas| p50 {:.0}  p90 {:.0}  max {:.0}",
            quantile(&abs, 0.5),
            quantile(&abs, 0.9),
            quantile(&abs, 1.0)
        );
    }
    if per_word.is_empty() {
        println!("   no constant-crossing level had three word points: b_word unidentified");
    } else {
        println!(
            "   per-word marginal at constant crossings: min {:.0} | p50 {:.0} | max {:.0} ({} levels)",
            quantile(&per_word, 0.0),
            quantile(&per_word, 0.5),
            quantile(&per_word, 1.0),
            per_word.len()
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The V4 shared-account experiment: one frame vs two
// ─────────────────────────────────────────────────────────────────────────

/// The quote calldata's fixed length, from [`quote_calldata`]: the selector,
/// the tuple's head offset, the five pool-key words, the direction flag, the
/// amount, the `hookData` offset, and its (zero) length — ten words.
const V4_QUOTE_CALLDATA_LEN: usize = 4 + 10 * 32;

/// `CALLDATACOPY(dest, offset, V4_QUOTE_CALLDATA_LEN)` — size, offset, dest
/// pushed in CALLDATACOPY's pop order.
fn emit_calldata_copy(code: &mut Vec<u8>, dest: u16, offset: u16) {
    let len = u16::try_from(V4_QUOTE_CALLDATA_LEN).expect("the quote calldata fits a word count");
    for value in [len, offset, dest] {
        code.push(0x61);
        code.extend_from_slice(&value.to_be_bytes());
    }
    code.push(0x37);
}

/// `CALL(gas, quoter, 0, offset, V4_QUOTE_CALLDATA_LEN, 0, 0)` — retSize,
/// retOffset, argsSize, argsOffset, value, address, gas in CALL's pop order.
fn emit_quoter_call(code: &mut Vec<u8>, quoter: Address, args_offset: u16) {
    let len = u16::try_from(V4_QUOTE_CALLDATA_LEN).expect("the quote calldata fits a word count");
    code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00]);
    for value in [len, args_offset] {
        code.push(0x61);
        code.extend_from_slice(&value.to_be_bytes());
    }
    code.extend_from_slice(&[0x60, 0x00, 0x73]);
    code.extend_from_slice(quoter.as_slice());
    code.extend_from_slice(&[0x5a, 0xf1]);
}

/// The two-leg batch caller: `selector ‖ quote(A) ‖ quote(B)` in, one frame,
/// where the quoter is called once per leg. The legs ride one transaction, so
/// the singleton `PoolManager`'s account and code are already warm for the
/// second leg — the shape a path's V4 hops pay in a settled transaction.
fn v4_batcher_code(quoter: Address) -> Vec<u8> {
    let mut code: Vec<u8> = Vec::new();
    emit_calldata_copy(&mut code, 0x0000, 4);
    let second = u16::try_from(4 + V4_QUOTE_CALLDATA_LEN).expect("the second calldata fits");
    emit_calldata_copy(&mut code, 0x0200, second);
    let mut fail_patches: Vec<usize> = Vec::new();
    for args_offset in [0x0000u16, 0x0200] {
        emit_quoter_call(&mut code, quoter, args_offset);
        code.push(0x15); // ISZERO
        fail_patches.push(code.len() + 1);
        code.extend_from_slice(&[0x61, 0x00, 0x00]); // PUSH2 <fail>
        code.push(0x57); // JUMPI
    }
    emit_return_last(&mut code);
    let fail = u16::try_from(code.len()).expect("the bytecode fits");
    code.push(0x5b); // JUMPDEST
    code.extend_from_slice(&[0x3d, 0x60, 0x00, 0x60, 0x00, 0x3e, 0x3d, 0x60, 0x00, 0xfd]);
    for patch in fail_patches {
        code[patch..patch + 2].copy_from_slice(&fail.to_be_bytes());
    }
    code
}

/// The one-leg shape through the same caller: the batching machinery's own
/// overhead, so the two-frame/two-leg comparison nets it out.
fn v4_single_quote_caller_code(quoter: Address) -> Vec<u8> {
    let mut code: Vec<u8> = Vec::new();
    emit_calldata_copy(&mut code, 0x0000, 4);
    emit_quoter_call(&mut code, quoter, 0x0000);
    code.push(0x15); // ISZERO
    let patch = code.len() + 1;
    code.extend_from_slice(&[0x61, 0x00, 0x00]);
    code.push(0x57);
    emit_return_last(&mut code);
    let fail = u16::try_from(code.len()).expect("the bytecode fits");
    code.push(0x5b);
    code.extend_from_slice(&[0x3d, 0x60, 0x00, 0x60, 0x00, 0x3e, 0x3d, 0x60, 0x00, 0xfd]);
    code[patch..patch + 2].copy_from_slice(&fail.to_be_bytes());
    code
}

/// `RETURN(0, 292)` over the copied calldata — the batch caller's plumbing
/// self-check (what it forwards IS what a plain quote would send).
fn v4_echo_code() -> Vec<u8> {
    let mut code: Vec<u8> = Vec::new();
    emit_calldata_copy(&mut code, 0x0000, 4);
    let len = u16::try_from(V4_QUOTE_CALLDATA_LEN).expect("the quote calldata fits a word count");
    code.push(0x61);
    code.extend_from_slice(&len.to_be_bytes());
    code.extend_from_slice(&[0x60, 0x00, 0xf3]);
    code
}

/// `RETURN(0, RETURNDATASIZE)` — the last quoter call's `(amountOut,
/// gasEstimate)` pair, so the batch shape's answer is comparable to a quote.
fn emit_return_last(code: &mut Vec<u8>) {
    code.extend_from_slice(&[0x3d, 0x60, 0x00, 0x60, 0x00, 0x3e, 0x3d, 0x60, 0x00, 0xf3]);
}

/// A raw frame over the scratch: `(gas, output)` on success. The batching
/// callers are hand-assembled bytecode, so a refusal prints its own verdict and
/// payload rather than vanishing into a `None`.
fn raw_frame(chain: &mut Chain<'_>, to: Address, data: Bytes) -> Option<(u64, Bytes)> {
    let tx = ReplayableTx {
        from: SENDER,
        to: Some(to),
        value: U256::ZERO,
        data,
        gas_limit: GAS_LIMIT,
        max_fee_per_gas: MAX_FEE_PER_GAS,
        max_priority_fee_per_gas: MAX_PRIORITY_FEE_PER_GAS,
        nonce: chain.nonce,
    };
    match chain.scratch.replay(&tx) {
        Ok(outcome) if outcome.status == RunVerdict::Success => {
            Some((outcome.gas_used, outcome.output))
        }
        Ok(outcome) => {
            println!(
                "   [diag] frame to {to} {:?} at gas {}: 0x{}",
                outcome.status,
                outcome.gas_used,
                alloy::hex::encode(&outcome.output[..outcome.output.len().min(96)])
            );
            None
        }
        Err(err) => {
            println!("   [diag] frame to {to} refused: {err:?}");
            None
        }
    }
}

/// `selector ‖ quote(A) ‖ quote(B)`.
fn batch_calldata(first: &V4Leg, second: &V4Leg, amount_a: U256, amount_b: U256) -> Bytes {
    let mut data = Vec::with_capacity(4 + 2 * V4_QUOTE_CALLDATA_LEN);
    data.extend_from_slice(&[0x00; 4]);
    for (leg, amount) in [(first, amount_a), (second, amount_b)] {
        let quote = quote_calldata(leg, amount);
        assert_eq!(
            quote.len(),
            V4_QUOTE_CALLDATA_LEN,
            "the batching caller's fixed layout assumes one quote calldata length"
        );
        data.extend_from_slice(quote.as_ref());
    }
    Bytes::from(data)
}

/// Does the singleton manager's warmth amortize across a path's V4 legs?
///
/// The floor prices a path's V4 legs at 72,232 gas each, a figure measured one
/// leg per frame. Two legs in one transaction share the manager's account
/// access, its code, and the transient `CurrencyDelta` slots — the warmth a
/// settled path gets for free. Measured three ways: two frames vs one frame for
/// two pools, the same pool twice (the slot-warmth discriminator, since two
/// pools' `Pool.State` slots never alias), and one leg through the batch caller
/// (the caller machinery's own overhead, netted out of the other two).
async fn v4_shared_account(reads: &mut Reads<'_>, chain: &mut Chain<'_>, notes: &mut Vec<String>) {
    println!();
    println!("=== the V4 shared-account experiment (the singleton's warmth) ===");
    let pools = v4_word_pools();
    let (Some(first), Some(second)) = (pools.first(), pools.get(1)) else {
        return;
    };
    let Some(first_hop) = v4_hop(reads, &first.key, false, 8).await else {
        println!("   {}: no usable V4 state at the anchor", first.label);
        return;
    };
    let Some(second_hop) = v4_hop(reads, &second.key, false, 8).await else {
        println!("   {}: no usable V4 state at the anchor", second.label);
        return;
    };
    let leg_a = V4Leg::canonical(first.key.clone(), false);
    let leg_b = V4Leg::canonical(second.key.clone(), false);
    let amount_a = U256::from((first_hop.reserve.to::<u128>() as f64 * 1e-3) as u128);
    let amount_b = U256::from((second_hop.reserve.to::<u128>() as f64 * 1e-3) as u128);
    println!(
        "   A = {} (in {}), B = {} (in {})",
        first.label, amount_a, second.label, amount_b
    );
    let expected = quote_calldata(&leg_a, amount_a);
    let copied = raw_frame(
        chain,
        V4_ECHO,
        batch_calldata(&leg_a, &leg_a, amount_a, amount_a),
    )
    .map(|(_, output)| output);
    println!(
        "   self-check: the batch caller forwards a byte-identical quote calldata (ok={})",
        copied
            .as_ref()
            .is_some_and(|bytes| bytes.as_ref() == expected.as_ref())
    );
    let (Some((gas_a, _)), Some((gas_b, out_b_answer))) = (
        raw_frame(
            chain,
            V4QuoterConfig::default().quoter,
            quote_calldata(&leg_a, amount_a),
        ),
        raw_frame(
            chain,
            V4QuoterConfig::default().quoter,
            quote_calldata(&leg_b, amount_b),
        ),
    ) else {
        println!("   a single-leg quote frame refused — the experiment needs live pools");
        return;
    };
    let Some((gas_one_in_batcher, out_one)) = raw_frame(
        chain,
        V4_BATCHER_ONE,
        batch_calldata(&leg_a, &leg_a, amount_a, amount_a),
    ) else {
        println!("   the batch caller's one-leg shape refused");
        return;
    };
    let Some((gas_ab, out_ab)) = raw_frame(
        chain,
        V4_BATCHER,
        batch_calldata(&leg_a, &leg_b, amount_a, amount_b),
    ) else {
        println!("   the two-leg batch frame refused");
        return;
    };
    let Some((gas_aa, out_aa)) = raw_frame(
        chain,
        V4_BATCHER,
        batch_calldata(&leg_a, &leg_a, amount_a, amount_a),
    ) else {
        println!("   the same-pool batch frame refused");
        return;
    };
    println!("   {:>38}  {:>10}", "shape", "frame gas");
    println!("   {:>38}  {gas_a:>10}", "A alone (own frame)");
    println!("   {:>38}  {gas_ab:>10}", "A + B (one frame)");
    println!("   {:>38}  {gas_aa:>10}", "A + A (one frame)");
    println!("   {:>38}  {gas_b:>10}", "B alone (own frame)");
    println!(
        "   {:>38}  {gas_one_in_batcher:>10}",
        "A alone (batch caller, one leg)"
    );
    // Execution-only comparison: strip the intrinsic from every frame and the
    // caller's own plumbing from the batching shapes, so what is left is the
    // quoter+manager work the legs actually run.
    let intrinsic = 21_000f64;
    let overhead = gas_one_in_batcher as f64 - gas_a as f64;
    let exec_two_frames = (gas_a + gas_b) as f64 - 2.0 * intrinsic;
    let exec_one_frame = gas_ab as f64 - intrinsic - overhead;
    let exec_same_pool_twice = 2.0 * (gas_a as f64 - intrinsic);
    let exec_aa_one_frame = gas_aa as f64 - intrinsic - overhead;
    let warmth_ab = exec_two_frames - exec_one_frame;
    let warmth_aa = exec_same_pool_twice - exec_aa_one_frame;
    println!();
    println!(
        "   caller plumbing (a quote routed through the batch caller, one leg) = {overhead:.0} gas; \
         every batching shape carries it once"
    );
    println!(
        "   execution, two frames A+B = {exec_two_frames:.0}; one frame A+B = {exec_one_frame:.0} \
         -> the pair is {warmth_ab:.0} gas cheaper in one frame ({:.0} per leg, {:.1}% of a V4 leg)",
        warmth_ab / 2.0,
        warmth_ab / 2.0 / (exec_two_frames / 2.0) * 100.0
    );
    println!(
        "   the same pool twice: {exec_same_pool_twice:.0} in two frames vs \
         {exec_aa_one_frame:.0} in one frame -> {warmth_aa:.0} cheaper, of which \
         {:.0} gas beyond the cross-pool term is storage-slot warmth",
        warmth_aa - warmth_ab
    );
    let answer_aa = decode_quote_return(&out_aa).map(|(amount_out, _)| amount_out);
    let answer_ab = decode_quote_return(&out_ab).map(|(amount_out, _)| amount_out);
    println!(
        "   the batched legs answer a plain frame exactly: A+A second leg {}, A+B second leg {}",
        answer_aa == decode_quote_return(&out_one).map(|(amount_out, _)| amount_out),
        answer_ab == decode_quote_return(&out_b_answer).map(|(amount_out, _)| amount_out)
    );
    println!(
        "   verdict: the singleton manager amortizes its ACCOUNT and CODE only — the per-pool \
         `Pool.State` slots (bitmap words included) never alias, and the per-leg quote reverts \
         its own swap, so the slot warmth it paid does not survive into the next leg"
    );
    notes.push(format!(
        "V4 SHARED ACCOUNT: one frame of two V4 legs is {warmth_ab:.0} gas cheaper than two \
         frames beyond the intrinsic ({:.0}/leg); the same-pool pair is {warmth_aa:.0} cheaper, \
         so storage-slot sharing adds {:.0} gas — nothing to model in the floor",
        warmth_ab / 2.0,
        warmth_aa - warmth_ab
    ));
}

// ─────────────────────────────────────────────────────────────────────────
// Part 4: the phantom corpus (ACTUAL gas vs the estimator)
// ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    V2,
    V3,
    V4,
}

#[derive(Clone, Debug)]
struct HopSpec {
    kind: Kind,
    pool: Address,
    token0: Address,
    token1: Address,
    fee: u32,
    tick_spacing: Option<i32>,
    zero_for_one: bool,
    dex: &'static str,
}

#[derive(Clone, Debug)]
struct PathSpec {
    id: String,
    anchor: u64,
    hops: Vec<HopSpec>,
}

fn load_corpus() -> Vec<PathSpec> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phantom_paths.tsv");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("phantom corpus {} unreadable: {e}", path.display()));
    let mut paths: Vec<PathSpec> = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') || line.starts_with("path_id") {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        assert_eq!(cols.len(), 11, "corpus line {} shape: {line}", index + 1);
        let kind = match cols[3] {
            "V2" => Kind::V2,
            "V3" => Kind::V3,
            "V4" => Kind::V4,
            other => panic!("unknown hop kind {other:?}"),
        };
        let dex = match cols[10] {
            "sushi" => "sushi",
            _ => "uniswap",
        };
        let hop = HopSpec {
            kind,
            pool: Address::from_str(cols[4]).expect("pool"),
            token0: Address::from_str(cols[5]).expect("token0"),
            token1: Address::from_str(cols[6]).expect("token1"),
            fee: cols[7].parse().expect("fee"),
            tick_spacing: cols[8].parse().ok(),
            zero_for_one: matches!(cols[9], "True" | "true"),
            dex,
        };
        match paths.iter_mut().find(|p| p.id == cols[0]) {
            Some(existing) => existing.hops.push(hop),
            None => paths.push(PathSpec {
                id: cols[0].to_string(),
                anchor: cols[1].parse().expect("anchor"),
                hops: vec![hop],
            }),
        }
    }
    paths
}

/// Part 4's ACTUAL gas: the same thin V3 frame, the router frame for V2, the
/// black-box quoter for V4 — the shapes a leg is simulated with.
async fn corpus_samples(
    reads: &mut Reads<'_>,
    chain: &mut Chain<'_>,
    samples: &mut Vec<Sample>,
    drift_rows: &mut Vec<(String, usize, usize)>,
    notes: &mut Vec<String>,
) {
    println!();
    println!("---- the phantom corpus: ACTUAL gas per hop ----");
    let corpus = load_corpus();
    let mut anchors: Vec<u64> = corpus.iter().map(|p| p.anchor).collect();
    anchors.sort_unstable();
    anchors.dedup();
    println!("   {} path(s), anchor block(s) {anchors:?}", corpus.len());
    println!(
        "   {:>18} {:>3} {:>5} {:>8} {:>26} {:>10} {:>5} {:>6} {:>6}",
        "path", "hop", "kind", "fee", "amount_in", "gas", "xing", "drift", "words"
    );
    let mut v4_quoter = V4BlackBoxQuoter::new(V4QuoterConfig::default());
    for path in &corpus {
        for (index, hop) in path.hops.iter().enumerate() {
            match hop.kind {
                Kind::V3 => {
                    let spacing = match hop.tick_spacing {
                        Some(s) => s,
                        None => reads.tick_spacing(hop.pool).await,
                    };
                    let liquidity = reads.liquidity(hop.pool).await;
                    let (sqrt_price, tick_before) = reads.v3_slot0(hop.pool).await;
                    let reserve = if hop.zero_for_one {
                        reserve0(liquidity, sqrt_price)
                    } else {
                        reserve1(liquidity, sqrt_price)
                    };
                    let ladder = if reserve.is_zero() {
                        vec![ten_pow(15), ten_pow(18), ten_pow(21)]
                    } else {
                        [1e-6f64, 1e-5, 1e-4, 3e-4, 1e-3, 3e-3]
                            .into_iter()
                            .map(|f| U256::from((reserve.to::<u128>() as f64 * f) as u128))
                            .filter(|a| !a.is_zero())
                            .collect()
                    };
                    let mut gases = Vec::new();
                    let mut censuses = Vec::new();
                    let mut ticks = Vec::new();
                    for amount in &ladder {
                        if let Some(frame) = chain.quote_v3(hop.pool, hop.zero_for_one, *amount) {
                            let cen = census(
                                reads,
                                hop.pool,
                                spacing,
                                tick_before,
                                frame.tick_after,
                                hop.zero_for_one,
                            )
                            .await;
                            let drift =
                                usize::try_from((frame.tick_after - tick_before).unsigned_abs())
                                    .unwrap_or(usize::MAX)
                                    / usize::try_from(spacing).unwrap_or(1);
                            gases.push(frame.gas);
                            censuses.push(cen.crossed_exclusive);
                            ticks.push((*amount, frame.gas, cen, drift, frame.tick_after));
                        }
                    }
                    let _ = (gases, censuses);
                    let mut out = Vec::new();
                    for (amount, gas, cen, drift, tick_after) in ticks {
                        out.push((
                            amount,
                            gas,
                            cen.crossed_exclusive,
                            cen.words,
                            drift,
                            tick_before,
                            tick_after,
                        ));
                    }
                    for (amount, gas, crossings, words, drift, tb, ta) in out {
                        println!(
                            "   {:>18} {:>3} {:>5} {:>8} {:>26} {:>10} {:>5} {:>6} {:>6}",
                            path.id,
                            index,
                            hop.kind_label(),
                            hop.fee,
                            amount.to_string(),
                            gas,
                            crossings,
                            drift,
                            words
                        );
                        drift_rows.push((format!("{} hop{index}", path.id), drift, crossings));
                        samples.push(Sample {
                            family: Family::V3,
                            group: format!("corpus:{}", hop.pool),
                            amount_in: amount,
                            gas,
                            crossings,
                            words,
                            drift,
                            tick_before: tb,
                            tick_after: ta,
                            quoter_crossings: None,
                        });
                    }
                }
                Kind::V2 => {
                    let router = if hop.dex == "sushi" {
                        ROUTER_SUSHI
                    } else {
                        ROUTER_V2
                    };
                    let mut gases = Vec::new();
                    let mut amounts = Vec::new();
                    for exp in [15u32, 18, 21] {
                        let amount = ten_pow(exp);
                        let (gas, _) = chain.quote_v2(router, hop.token0, hop.token1, amount);
                        gases.push(gas);
                        amounts.push(amount);
                    }
                    for (amount, gas) in amounts.into_iter().zip(gases) {
                        println!(
                            "   {:>18} {:>3} {:>5} {:>8} {:>26} {:>10} {:>5} {:>6} {:>6}",
                            path.id,
                            index,
                            hop.kind_label(),
                            hop.fee,
                            amount.to_string(),
                            gas,
                            0,
                            0,
                            0
                        );
                        samples.push(Sample {
                            family: Family::V2,
                            group: format!("corpus:{}", hop.dex),
                            amount_in: amount,
                            gas,
                            crossings: 0,
                            words: 0,
                            drift: 0,
                            tick_before: 0,
                            tick_after: 0,
                            quoter_crossings: None,
                        });
                    }
                }
                Kind::V4 => {
                    let spacing = hop.tick_spacing.expect("V4 hops carry their spacing");
                    let key = V4PoolKey {
                        currency0: hop.token0,
                        currency1: hop.token1,
                        fee: hop.fee,
                        tick_spacing: spacing,
                        hooks: Address::ZERO,
                    };
                    let Some(v4) = v4_hop(reads, &key, hop.zero_for_one, 400).await else {
                        println!(
                            "   {:>18} {:>3} {:>5} {:>8}  no usable V4 state at the anchor",
                            path.id,
                            index,
                            hop.kind_label(),
                            hop.fee
                        );
                        continue;
                    };
                    let slot0 = v4.slot0;
                    let reserve = v4.reserve;
                    let sim = ConcentratedLiquidityPathSim::new(
                        &[&v4.prepared.int_seq],
                        Some(std::slice::from_ref(&v4.prepared)),
                    );
                    let leg = V4Leg::canonical(key.clone(), hop.zero_for_one);
                    let ladder: Vec<U256> = if reserve.is_zero() {
                        vec![ten_pow(15), ten_pow(18), ten_pow(21)]
                    } else {
                        [1e-6f64, 1e-5, 1e-4, 3e-4, 1e-3]
                            .into_iter()
                            .map(|f| U256::from((reserve.to::<u128>() as f64 * f) as u128))
                            .filter(|a| !a.is_zero())
                            .collect()
                    };
                    for amount in ladder {
                        let outcome = sim.output(amount);
                        let landed = outcome.landed.first().copied().unwrap_or(0);
                        let words: usize = 1 + v4
                            .prepared
                            .int_seq
                            .ranges
                            .iter()
                            .take(landed + 1)
                            .map(|r| r.word_boundary_prices.len())
                            .sum::<usize>();
                        if let Ok(quote) = v4_quoter.quote(&mut chain.scratch, &leg, amount) {
                            let walk_out = outcome.final_output;
                            if walk_out != quote.amount_out {
                                notes.push(format!(
                                    "PARITY {} hop{index} (fee {}): the V4 walk answers {walk_out} \
                                     where the chain answers {} — this leg's crossing axis is the \
                                     walk's, so its estimate is not trustworthy",
                                    path.id, hop.fee, quote.amount_out
                                ));
                            }
                            println!(
                                "   {:>18} {:>3} {:>5} {:>8} {:>26} {:>10} {:>5} {:>6} {:>6}",
                                path.id,
                                index,
                                hop.kind_label(),
                                hop.fee,
                                amount.to_string(),
                                quote.gas_used,
                                landed,
                                "-",
                                words
                            );
                            samples.push(Sample {
                                family: Family::V4,
                                group: format!("corpus:{}", hop.pool),
                                amount_in: amount,
                                gas: quote.gas_used,
                                crossings: landed,
                                words,
                                drift: 0,
                                tick_before: slot0.tick,
                                tick_after: slot0.tick,
                                quoter_crossings: None,
                            });
                        }
                    }
                }
            }
        }
    }
}

impl HopSpec {
    fn kind_label(&self) -> &'static str {
        match self.kind {
            Kind::V2 => "V2",
            Kind::V3 => "V3",
            Kind::V4 => "V4",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Part 5: the fits, the estimate, and the error analysis
// ─────────────────────────────────────────────────────────────────────────

/// The measured coefficient table. One row per family: the fixed per-leg cost
/// at zero crossings and the marginal per crossed tick / bitmap word, fitted
/// over that family's sweep samples.
#[derive(Clone, Copy, Debug)]
struct Coefficients {
    fixed: f64,
    per_crossing: f64,
    per_word: f64,
    samples: usize,
}

fn estimate(coeff: Coefficients, crossings: f64, words: f64) -> f64 {
    coeff.fixed + coeff.per_crossing * crossings + coeff.per_word * words
}

// ─────────────────────────────────────────────────────────────────────────
// The run
// ─────────────────────────────────────────────────────────────────────────

fn rpc_url() -> String {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .expect(
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) to an \
             archive mainnet endpoint",
        )
}

/// The anchor ritual both live-RPC tests share: the pinned archive block, the
/// scratch block it replays as, and the quote caller's anchor nonce.
async fn anchor_setup() -> (Arc<AlloyProvider>, ScratchBlock, u64) {
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    assert_eq!(
        provider.get_chain_id().await.unwrap(),
        1,
        "the sweep pools are mainnet fixtures"
    );
    let block = provider
        .get_block(PIN)
        .await
        .unwrap()
        .expect("the anchor block is archived");
    let scratch_block = ScratchBlock {
        number: PIN + 1,
        timestamp: block.header.timestamp,
        base_fee_next: u128::from(block.header.base_fee_per_gas.unwrap_or(0)),
    };
    let nonce = provider
        .get_transaction_count(&SENDER, Some(PIN))
        .await
        .unwrap();
    (provider, scratch_block, nonce)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC gas decomposition over archive mainnet state; run with --ignored"]
async fn gas_decomposition_census() {
    let started = Instant::now();
    let (provider, scratch_block, nonce) = anchor_setup().await;
    let provider: &AlloyProvider = &provider;

    println!("=== the per-op gas decomposition (evm2) ===");
    println!(
        "anchor {PIN} (timestamp {}, base fee {} wei/gas) | quote-frame gas limit {GAS_LIMIT}",
        scratch_block.timestamp, scratch_block.base_fee_next
    );

    let mut reads = Reads::new(provider, PIN);
    let mut chain = Chain::new(provider, scratch_block, nonce);
    let mut samples: Vec<Sample> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut drift_rows: Vec<(String, usize, usize)> = Vec::new();

    // ── the V3 crossing sweep ───────────────────────────────────────────
    println!();
    println!("=== the V3 crossing sweep (bisected to hit each k exactly) ===");
    for spec in SWEEP_POOLS {
        for zero_for_one in [true, false] {
            sweep_pool_direction(
                &mut reads,
                &mut chain,
                spec,
                zero_for_one,
                &mut samples,
                &mut notes,
            )
            .await;
        }
    }

    v2_legs(&mut chain, &mut samples);
    transfer_cost(&mut reads, &mut chain).await;
    v4_legs(&mut reads, &mut chain, &mut samples, &mut notes).await;

    // ── the V4 bitmap-word sweep (the floor's last unvalidated term) ────
    let mut word_samples: Vec<WordSample> = Vec::new();
    for spec in &v4_word_pools() {
        for zero_for_one in [true, false] {
            v4_word_sweep(
                &mut reads,
                &mut chain,
                spec,
                zero_for_one,
                &mut word_samples,
                &mut notes,
            )
            .await;
        }
    }
    v4_word_report(&word_samples, &mut notes);
    v4_shared_account(&mut reads, &mut chain, &mut notes).await;
    corpus_samples(
        &mut reads,
        &mut chain,
        &mut samples,
        &mut drift_rows,
        &mut notes,
    )
    .await;

    // ── the coefficient table ───────────────────────────────────────────
    println!();
    println!("=== the measured coefficient table ===");
    println!(
        "   fitted on the SWEEP's liquid-region samples (the corpus is the held-out test set)"
    );
    println!(
        "   {:<4} {:>10} {:>14} {:>12} {:>5}",
        "fam", "fixed", "per_crossing", "per_word", "n"
    );
    // `a + b·crossings` from the sweep; the word term from the samples that
    // walk empty words (the `EMPTY-HALT` shape), regressed on the residual.
    // A group is usable as a coefficient source only if its crossing axis
    // actually moves: a pool whose swaps all land in one range (or on one
    // crossing level) contributes an intercept and nothing else.
    let mut usable_groups: Vec<String> = Vec::new();
    {
        let mut seen: Vec<String> = Vec::new();
        for sample in &samples {
            if !seen.contains(&sample.group) {
                seen.push(sample.group.clone());
            }
        }
        for group in &seen {
            let rows: Vec<&Sample> = samples.iter().filter(|s| &s.group == group).collect();
            let mut levels: Vec<usize> = rows.iter().map(|s| s.crossings).collect();
            levels.sort_unstable();
            levels.dedup();
            if rows.len() >= 3 && levels.len() >= 3 {
                usable_groups.push(group.clone());
            }
        }
    }
    let fit_family_sweep = |family: Family| -> Option<Coefficients> {
        let rows: Vec<&Sample> = samples
            .iter()
            .filter(|s| {
                s.family == family
                    && !s.group.starts_with("corpus:")
                    && usable_groups.contains(&s.group)
            })
            .collect();
        if rows.len() < 2 {
            return None;
        }
        let xs: Vec<f64> = rows.iter().map(|s| s.crossings as f64).collect();
        let ys: Vec<f64> = rows.iter().map(|s| s.gas as f64).collect();
        let (fixed, per_crossing) = fit2(&xs, &ys);
        let word_rows: Vec<(f64, f64)> = samples
            .iter()
            .filter(|s| s.family == family && s.words >= 4)
            .map(|s| {
                (
                    s.words as f64,
                    s.gas as f64 - (fixed + per_crossing * s.crossings as f64),
                )
            })
            .collect();
        let per_word = if word_rows.len() >= 3 {
            let wx: Vec<f64> = word_rows.iter().map(|r| r.0).collect();
            let wy: Vec<f64> = word_rows.iter().map(|r| r.1).collect();
            fit2(&wx, &wy).1
        } else {
            0.0
        };
        Some(Coefficients {
            fixed,
            per_crossing,
            per_word,
            samples: rows.len(),
        })
    };
    let mut coeffs: HashMap<&'static str, Coefficients> = HashMap::new();
    for family in [Family::V3, Family::V4] {
        if let Some(c) = fit_family_sweep(family) {
            println!(
                "   {:<4} {:>10.0} {:>14.1} {:>12.1} {:>5}",
                family.label(),
                c.fixed,
                c.per_crossing,
                c.per_word,
                c.samples
            );
            coeffs.insert(family.label(), c);
        }
    }
    println!(
        "   (V4's per_word is unfittable on this ladder — the in-range amounts never move the word \
         regressor; the bitmap-word sweep above measures it separately)"
    );
    // A V2 leg has no crossing axis at all: its coefficient IS the constant.
    {
        let gases: Vec<f64> = samples
            .iter()
            .filter(|s| s.family == Family::V2)
            .map(|s| s.gas as f64)
            .collect();
        if !gases.is_empty() {
            let c = Coefficients {
                fixed: quantile(&gases, 0.5),
                per_crossing: 0.0,
                per_word: 0.0,
                samples: gases.len(),
            };
            println!(
                "   {:<4} {:>10.0} {:>14.1} {:>12.1} {:>5}",
                "V2", c.fixed, c.per_crossing, c.per_word, c.samples
            );
            coeffs.insert("V2", c);
        }
    }

    // The intercept is a POOL property, not a family constant: the per-pool
    // fits show how far a pool's own fixed cost sits from the family's.
    println!();
    println!("   per-pool fits over the sweep (gas = a + b·crossings):");
    let mut groups: Vec<String> = Vec::new();
    for sample in &samples {
        if !groups.contains(&sample.group) {
            groups.push(sample.group.clone());
        }
    }
    let mut intercepts: Vec<f64> = Vec::new();
    for group in &groups {
        if group.starts_with("corpus:") {
            continue;
        }
        let rows: Vec<&Sample> = samples.iter().filter(|s| &s.group == group).collect();
        let mut levels: Vec<usize> = rows.iter().map(|s| s.crossings).collect();
        levels.sort_unstable();
        levels.dedup();
        if rows.len() < 3 || levels.len() < 2 {
            println!(
                "     {group:<36} (degenerate: {} samples, {} crossing level(s))",
                rows.len(),
                levels.len()
            );
            continue;
        }
        let xs: Vec<f64> = rows.iter().map(|s| s.crossings as f64).collect();
        let ys: Vec<f64> = rows.iter().map(|s| s.gas as f64).collect();
        let (a, b) = fit2(&xs, &ys);
        intercepts.push(a);
        println!("     {group:<36} a={a:>9.0}  b={b:>10.1}  n={}", rows.len());
    }
    if !intercepts.is_empty() {
        println!(
            "     intercept spread: min {:.0} | p50 {:.0} | max {:.0} (the family's fixed cost is \
             the LOW end — the floor under-predicts)",
            quantile(&intercepts, 0.0),
            quantile(&intercepts, 0.5),
            quantile(&intercepts, 1.0)
        );
    }

    // ── the chain's own crossing count, against the census ───────────────
    let mut tally = [0usize; 3];
    for sample in &samples {
        let Some(quoter) = sample.quoter_crossings else {
            continue;
        };
        if quoter == sample.crossings {
            tally[0] += 1;
        } else if quoter == sample.crossings + 1 {
            tally[1] += 1;
        } else {
            tally[2] += 1;
        }
    }
    println!();
    println!(
        "   QuoterV2 `initializedTicksCrossed` vs the pool's census: {} exact, {} = census + 1 \
         (the quoter counts the tick the price starts in), {} otherwise",
        tally[0], tally[1], tally[2]
    );

    // ── the estimator's error over the corpus ───────────────────────────
    println!();
    println!("=== the estimator against the ACTUAL corpus gas ===");
    println!("   formula: gas = a_family + b_cross·crossings + b_word·words");
    let v3 = coeffs
        .get("V3")
        .copied()
        .expect("the V3 sweep produced a fit");
    let v2 = coeffs.get("V2").copied();
    let v4 = coeffs.get("V4").copied();

    let corpus: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.group.starts_with("corpus:"))
        .collect();
    println!("   corpus samples: {}", corpus.len());

    let mut errs_b: Vec<f64> = Vec::new();
    let mut errs_c: Vec<f64> = Vec::new();
    for sample in &corpus {
        let coeff = match sample.family {
            Family::V3 => v3,
            Family::V4 => v4.unwrap_or(v3),
            Family::V2 => v2.unwrap_or(v3),
        };
        let pred_b = estimate(coeff, sample.crossings as f64, sample.words as f64);
        let pred_c = estimate(
            coeff,
            sample.drift as f64,
            (sample.drift as f64 / 256.0).max(1.0),
        );
        let actual = sample.gas as f64;
        if actual > 0.0 {
            errs_b.push((pred_b - actual) / actual);
            errs_c.push((pred_c - actual) / actual);
        }
    }
    for (label, errs) in [
        ("(b) census/walk count", &errs_b),
        ("(c) |Δtick|/spacing", &errs_c),
    ] {
        let abs: Vec<f64> = errs.iter().map(|e| e.abs()).collect();
        let under = errs.iter().filter(|e| **e <= 0.0).count();
        println!(
            "   {label:<24} signed p50 {:>7.2}%  p90 {:>9.2}%  min {:>9.2}%  max {:>10.2}%  |err| \
             p50 {:>6.2}%  p90 {:>8.2}%  max {:>10.2}%  under-predicts {}/{}",
            quantile(errs, 0.5) * 100.0,
            quantile(errs, 0.9) * 100.0,
            quantile(errs, 0.0) * 100.0,
            quantile(errs, 1.0) * 100.0,
            quantile(&abs, 0.5) * 100.0,
            quantile(&abs, 0.9) * 100.0,
            quantile(&abs, 1.0) * 100.0,
            under,
            errs.len()
        );
    }
    for family in [Family::V2, Family::V3, Family::V4] {
        let family_errs: Vec<f64> = corpus
            .iter()
            .filter(|s| s.family == family)
            .filter_map(|s| {
                let coeff = match s.family {
                    Family::V3 => v3,
                    Family::V4 => v4.unwrap_or(v3),
                    Family::V2 => v2.unwrap_or(v3),
                };
                let pred = estimate(coeff, s.crossings as f64, s.words as f64);
                let actual = s.gas as f64;
                (actual > 0.0).then(|| (pred - actual) / actual)
            })
            .collect();
        if !family_errs.is_empty() {
            println!(
                "   {:<4} legs: signed p50 {:>7.2}%  |err| p50 {:>6.2}%  p90 {:>7.2}%  max {:>8.2}%",
                family.label(),
                quantile(&family_errs, 0.5) * 100.0,
                quantile(
                    &family_errs.iter().map(|e| e.abs()).collect::<Vec<f64>>(),
                    0.5
                ) * 100.0,
                quantile(
                    &family_errs.iter().map(|e| e.abs()).collect::<Vec<f64>>(),
                    0.9
                ) * 100.0,
                quantile(
                    &family_errs.iter().map(|e| e.abs()).collect::<Vec<f64>>(),
                    1.0
                ) * 100.0
            );
        }
    }

    // The floor variant: keep the marginals, drop the intercept to the LOWEST
    // per-pool fixed cost the corpus measured — the family median over-predicts
    // the cheap pools, and over-prediction is the expensive error (a suppressed
    // candidate is lost alpha; an under-predicted one is a wasted sim).
    let mut corpus_intercepts: Vec<f64> = Vec::new();
    {
        let mut pools: Vec<&String> = corpus
            .iter()
            .filter(|s| s.family == Family::V3)
            .map(|s| &s.group)
            .collect();
        pools.sort();
        pools.dedup();
        for pool in pools {
            let rows: Vec<&Sample> = corpus
                .iter()
                .copied()
                .filter(|s| &s.group == pool)
                .collect();
            // A three-point pool cannot support an intercept: its slope absorbs
            // the walk's word cost and its intercept goes negative.
            if rows.len() < 4 {
                continue;
            }
            let xs: Vec<f64> = rows.iter().map(|s| s.crossings as f64).collect();
            let ys: Vec<f64> = rows.iter().map(|s| s.gas as f64).collect();
            corpus_intercepts.push(fit2(&xs, &ys).0);
        }
    }
    let a_floor = if corpus_intercepts.is_empty() {
        v3.fixed
    } else {
        quantile(&corpus_intercepts, 0.10)
    };
    let floor_coeff = Coefficients {
        fixed: a_floor,
        per_crossing: v3.per_crossing,
        per_word: v3.per_word,
        samples: v3.samples,
    };
    let errs_floor: Vec<f64> = corpus
        .iter()
        .filter(|s| s.family != Family::V2)
        .filter_map(|s| {
            let actual = s.gas as f64;
            (actual > 0.0).then(|| {
                (estimate(floor_coeff, s.crossings as f64, s.words as f64) - actual) / actual
            })
        })
        .collect();
    if !errs_floor.is_empty() {
        let abs: Vec<f64> = errs_floor.iter().map(|e| e.abs()).collect();
        let under = errs_floor.iter().filter(|e| **e <= 0.0).count();
        println!(
            "   floor variant (a = {a_floor:.0}, the corpus's cheapest pool fixed cost): signed              p50 {:>7.2}%  p90 {:>7.2}%  min {:>8.2}%  max {:>7.2}%  under-predicts {}/{}",
            quantile(&errs_floor, 0.5) * 100.0,
            quantile(&errs_floor, 0.9) * 100.0,
            quantile(&errs_floor, 0.0) * 100.0,
            quantile(&errs_floor, 1.0) * 100.0,
            under,
            errs_floor.len()
        );
        println!("   (|err| p50 {:.2}%)", quantile(&abs, 0.5) * 100.0);
    }

    // The floor must UNDER-predict: the admissible estimate is a LOW quantile of
    // the residual, so the candidates it lets through are the cheap mistake.
    for q in [0.05f64, 0.10, 0.25] {
        let shift = quantile(&errs_b, q);
        println!(
            "   floor at q{:.2} of the (b) residual: est = pred + {:.1}% of the leg's gas \
             (under-predicts ~{:.0}% of legs)",
            q,
            shift * 100.0,
            (1.0 - q) * 100.0
        );
    }
    // ── the crossing-source verdict ────────────────────────────────────
    println!();
    println!("=== the crossing source: option (c)'s drift ===");
    println!(
        "   {:>26} {:>12} {:>12} {:>10}",
        "group", "true_xing", "drift_xing", "ratio"
    );
    let mut ratios: Vec<f64> = Vec::new();
    for (group, drift, crossings) in &drift_rows {
        if *crossings == 0 && *drift == 0 {
            continue;
        }
        let ratio = if *crossings == 0 {
            f64::INFINITY
        } else {
            *drift as f64 / *crossings as f64
        };
        if ratio.is_finite() {
            ratios.push(ratio);
        }
        println!(
            "   {:>26} {:>12} {:>12} {:>10.1}",
            group, crossings, drift, ratio
        );
    }
    println!();
    println!("   the widest drifts (corpus V3 samples; the ratio is what inflates the estimate):");
    println!(
        "   {:>18} {:>26} {:>20} {:>10} {:>10}",
        "pool", "amount_in", "tick span", "true", "drift"
    );
    let mut worst: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.family == Family::V3 && s.group.starts_with("corpus:"))
        .filter(|s| s.drift > 0)
        .collect();
    worst.sort_by_key(|s| std::cmp::Reverse(s.drift / s.crossings.max(1)));
    for s in worst.iter().take(5) {
        println!(
            "   {:>18} {:>26} {:>20} {:>10} {:>10}",
            alloy::hex::encode(&s.group.as_bytes()[s.group.len().saturating_sub(6)..]),
            s.amount_in.to_string(),
            format!("{} -> {}", s.tick_before, s.tick_after),
            s.crossings,
            s.drift
        );
    }
    if !ratios.is_empty() {
        println!(
            "   drift/true ratio: p50 {:.2}x  p90 {:.2}x  max {:.2}x  (1.0 = exact)",
            quantile(&ratios, 0.5),
            quantile(&ratios, 0.9),
            quantile(&ratios, 1.0)
        );
    }

    // ── cross-check notes ──────────────────────────────────────────────
    println!();
    println!("=== cross-checks and notes ({}) ===", notes.len());
    for note in &notes {
        println!("   x {note}");
    }
    // The walk's warm/cold split: the FIRST crossing in a bitmap word also pays
    // the word's cold `SLOAD`; each further crossing in that word does not. A
    // regression on (warm, cold) crossings cannot identify it (on a sparse pool
    // `cold` is constant, and on a dense one both regressors move together), so
    // the rig compares the measured 0 → 1 increment against the same pool's
    // fitted marginal.
    let mut first_increments: Vec<f64> = Vec::new();
    let mut marginals: Vec<f64> = Vec::new();
    for group in &usable_groups {
        if group.starts_with("corpus:") {
            continue;
        }
        let rows: Vec<&Sample> = samples.iter().filter(|s| &s.group == group).collect();
        let zero = rows.iter().find(|s| s.crossings == 0);
        let one = rows.iter().find(|s| s.crossings == 1);
        if let (Some(zero), Some(one)) = (zero, one) {
            first_increments.push(one.gas as f64 - zero.gas as f64);
            let xs: Vec<f64> = rows.iter().map(|s| s.crossings as f64).collect();
            let ys: Vec<f64> = rows.iter().map(|s| s.gas as f64).collect();
            marginals.push(fit2(&xs, &ys).1);
        }
    }
    if !first_increments.is_empty() {
        let first = quantile(&first_increments, 0.5);
        let marginal = quantile(&marginals, 0.5);
        println!();
        println!(
            "   the first crossing in a bitmap word costs {first:.0} gas (p50 over {} pools) vs a \
             marginal crossing in an already-loaded word {marginal:.0} — the cold-word `SLOAD` + \
             bit walk is ~{:.0} gas, i.e. {:.0}% of the marginal",
            first_increments.len(),
            first - marginal,
            (first - marginal) / marginal * 100.0
        );
    }
    println!();
    println!(
        "unique bitmap words read: {} | truncated censuses {} | distinct calls {} | storage reads \
         {} | wall {:.1} s",
        reads.bitmap_words,
        reads.truncated_censuses,
        reads.calls.len(),
        reads.storages.len(),
        started.elapsed().as_secs_f64()
    );
}

/// The word sweep on its own — the focused measurement of the V4 per-word
/// coefficient and the shared-account term, without the rest of the census.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC V4 bitmap-word sweep over archive mainnet state; run with --ignored"]
async fn v4_bitmap_word_coefficient() {
    let started = Instant::now();
    let (provider, scratch_block, nonce) = anchor_setup().await;
    let provider: &AlloyProvider = &provider;
    let mut reads = Reads::new(provider, PIN);
    let mut chain = Chain::new(provider, scratch_block, nonce);
    let mut notes: Vec<String> = Vec::new();
    let mut samples: Vec<WordSample> = Vec::new();
    println!("=== the V4 bitmap-word sweep (evm2, anchor {PIN}) ===");
    for spec in &v4_word_pools() {
        for zero_for_one in [true, false] {
            v4_word_sweep(
                &mut reads,
                &mut chain,
                spec,
                zero_for_one,
                &mut samples,
                &mut notes,
            )
            .await;
        }
    }
    v4_word_report(&samples, &mut notes);
    v4_shared_account(&mut reads, &mut chain, &mut notes).await;
    println!();
    println!("=== notes ({}) ===", notes.len());
    for note in &notes {
        println!("   x {note}");
    }
    println!(
        "unique bitmap words read: {} | truncated censuses {} | distinct calls {} | storage reads \
         {} | wall {:.1} s",
        reads.bitmap_words,
        reads.truncated_censuses,
        reads.calls.len(),
        reads.storages.len(),
        started.elapsed().as_secs_f64()
    );
}

// ─────────────────────────────────────────────────────────────────────────
// The pure part (no network): the fit + quantile arithmetic
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn fits_and_quantiles_recover_known_coefficients() {
    // An exact plane: gas = 30_000 + 13_500·k + 2_300·w.
    let rows: Vec<(f64, f64, f64)> = [
        (0.0, 1.0, 30_000.0 + 2_300.0),
        (2.0, 1.0, 30_000.0 + 27_000.0 + 2_300.0),
        (5.0, 3.0, 30_000.0 + 67_500.0 + 6_900.0),
        (9.0, 4.0, 30_000.0 + 121_500.0 + 9_200.0),
    ]
    .to_vec();
    let (a, b, c) = fit3(&rows);
    assert!((a - 30_000.0).abs() < 0.5, "fixed: {a}");
    assert!((b - 13_500.0).abs() < 0.5, "per crossing: {b}");
    assert!((c - 2_300.0).abs() < 0.5, "per word: {c}");

    // The same plane with its confidence: an exact fit has no residual, so the
    // standard errors vanish and R² is 1.
    let (a, b, c, errors, r2) = fit3_se(&rows);
    assert!((a - 30_000.0).abs() < 0.5, "fixed: {a}");
    assert!((b - 13_500.0).abs() < 0.5, "per crossing: {b}");
    assert!((c - 2_300.0).abs() < 0.5, "per word: {c}");
    assert!(
        errors.0 < 1e-6 && errors.1 < 1e-6 && errors.2 < 1e-6,
        "{errors:?}"
    );
    assert!((r2 - 1.0).abs() < 1e-12, "r2: {r2}");

    // A noisy plane still recovers the coefficients, and the standard error
    // stays a real (positive) confidence rather than a zeroed placeholder.
    let noisy: Vec<(f64, f64, f64)> = (0..8)
        .map(|index| {
            let x1 = f64::from(index % 4);
            let x2 = f64::from(index / 2);
            let noise = if index % 2 == 0 { 40.0 } else { -40.0 };
            (x1, x2, 30_000.0 + 13_500.0 * x1 + 2_300.0 * x2 + noise)
        })
        .collect();
    let (a, b, c, errors, r2) = fit3_se(&noisy);
    assert!((a - 30_000.0).abs() < 100.0, "fixed: {a}");
    assert!((b - 13_500.0).abs() < 100.0, "per crossing: {b}");
    assert!((c - 2_300.0).abs() < 100.0, "per word: {c}");
    assert!(errors.2 > 0.0, "the per-word standard error is positive");
    assert!(r2 > 0.99, "r2: {r2}");

    let xs = [0.0, 1.0, 2.0, 3.0];
    let ys = [100.0, 150.0, 200.0, 250.0];
    let (a, b) = fit2(&xs, &ys);
    assert!((a - 100.0).abs() < 1e-9 && (b - 50.0).abs() < 1e-9);
    assert!((r2_of2(&xs, &ys, a, b) - 1.0).abs() < 1e-12);

    let values = [4.0, 1.0, 3.0, 2.0, 5.0];
    assert!((quantile(&values, 0.0) - 1.0).abs() < 1e-12);
    assert!((quantile(&values, 0.5) - 3.0).abs() < 1e-12);
    assert!((quantile(&values, 1.0) - 5.0).abs() < 1e-12);
    assert!((quantile(&values, 0.25) - 2.0).abs() < 1e-12);

    // The estimator is the fit, evaluated.
    let coeff = Coefficients {
        fixed: a,
        per_crossing: b,
        per_word: 0.0,
        samples: 4,
    };
    assert!((estimate(coeff, 2.0, 1.0) - 200.0).abs() < 1e-9);
}
