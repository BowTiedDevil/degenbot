//! Phantom-candidate fidelity: does the white-box solver fabricate margin that
//! the chain cannot pay?
//!
//! The 7-hour bot run produced ~322k inline candidates, ~97% of which failed
//! EVM verification as `no-profit`, with ~44 paths failing EVERY block. The
//! divergence probe proved the engine's pool state equals chain state at its
//! anchors (255k probes, 0 divergent slots), so the gap is white-box MATH vs
//! EVM EXECUTION on identical state.
//!
//! This harness is the gate for that claim, over a committed corpus of the
//! trap-armed run's `[sim-fixture]` dumps
//! (`tests/fixtures/phantom_paths.tsv`):
//!
//! 1. Rebuild each pool's white-box state FROM CHAIN at the pinned anchor —
//!    V2 reserves (the pair's packed reserves word), V3 `slot0`/`liquidity` +
//!    the FULL swap-direction tick map (`tickBitmap(int16)`/`ticks(int24)`),
//!    V4 `PoolManager.extsload` for the same slots. Full direction maps
//!    matter: the solver's tick-range sequence is built over every initialized
//!    tick in the walk direction, so a truncated map would be a rig artifact,
//!    not a finding.
//! 2. Build the solver's own hop projections (`build_int_v3_sequence` /
//!    `build_int_v4_sequence` → `PreparedConcentratedLiquidityHop`, V2 →
//!    `IntHopState`), run `solve_path` → the claimed optimal input, per-hop
//!    outputs, profit.
//! 3. Execute the SAME hops with the SAME inputs on evm2 (the real pool
//!    bytecode: `QuoterV2`, the V2 router, the V4 `Quoter`) at the anchor.
//! 4. Report per-hop, per-input divergence: the solver leaf figure vs the
//!    byte-exact family sim (`simulate_swap`) vs the chain.
//!
//! The corpus asserts parity: a hop whose solver figure exceeds the chain's is
//! a fabrication. The exit gate is `solver == sim == evm` per hop per path,
//! plus "any solved profit survives the chain composition".
//!
//! # What the corpus says
//!
//! Read the printed ledger, not this comment: the rig prints, per path, the
//! solver's claim, the chain composition at that claim, and the claim priced
//! against the block's gas. On every phantom path in the committed corpus the
//! claim and the chain agree TO THE WEI (so the white-box walk is not inventing
//! margin on this state), and the margin is one to two orders of magnitude below
//! the gas the same path costs — which is an economic verdict, not a math one.
//!
//! Two properties make this rig trustworthy rather than self-confirming:
//!
//! - the chain figure comes from each POOL'S OWN `swap()` bytecode (see
//!   [`v3_caller_code`]), never from the same white-box code under test, and it
//!   runs for fork pools no canonical quoter would route to;
//! - on every leg the canonical factory also serves, the rig cross-checks the
//!   canonical quoter against the pool's own bytecode and refuses to trust the
//!   leg's chain figure when they disagree (a `RIG SELF-CHECK` finding).
//!
//! # Running it
//!
//! ```sh
//! just test-rust-nextest -p degenbot-simulation --test evm2_phantom_parity -- --ignored
//! # or
//! cargo test -p degenbot-simulation --test evm2_phantom_parity \
//!   phantom_corpus_parity -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `DEGENBOT_RPC_HTTP_CHAINID_1` (or `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`)
//! selects the node; the pinned blocks need archive state.
//! `DEGENBOT_PHANTOM_PATH=<substr>` narrows the corpus to matching path ids
//! while drilling one leg.

#![expect(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::single_match_else,
    clippy::print_stderr,
    clippy::print_literal,
    clippy::uninlined_format_args,
    clippy::too_many_arguments,
    clippy::extend_with_drain
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use alloy::primitives::{address, keccak256, Address, Bytes, U256};
use degenbot_math::concentrated_liquidity::tick_math::{
    MAX_SQRT_RATIO, MAX_TICK, MIN_SQRT_RATIO, MIN_TICK,
};
use degenbot_math::v2::IntHopState;
use degenbot_pools::solve::{solve_path, ResolvedHop, ResolvedMixedPath};
use degenbot_pools::{
    decode_tick_word, decode_v2_reserves_word, decode_v4_slot0, simulate_swap, v4_liquidity_slot,
    v4_pool_id, v4_pool_state_base_slot, v4_slot0_slot, v4_tick_bitmap_word_slot,
    v4_tick_mapping_slot, word_of, ConcentratedLiquidityPathSim, GateDeps, PoolEntry,
    PoolTickCoverage, PreparedConcentratedLiquidityHop, RegisterV2PoolParams, RegisterV3PoolParams,
    RegisterV4PoolParams, TickInfo, V2PoolState, V3PoolState, V4PoolKey, V4PoolState,
    V2_RESERVES_SLOT,
};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_simulation::sim::evm2::frame_replay::Evm2ScratchEvm;
use degenbot_simulation::sim::frame_replay::{ReplayableTx, ScratchBlock};
use degenbot_simulation::sim::RunVerdict;
use degenbot_uniswap::dex_identity::DexVariant;

// ─────────────────────────────────────────────────────────────────────────
// Mainnet constants
// ─────────────────────────────────────────────────────────────────────────

const V3_FACTORY: Address = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");
const FACTORY_V2: Address = address!("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
const FACTORY_SUSHI: Address = address!("0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac");
const ROUTER_V2: Address = address!("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
const ROUTER_SUSHI: Address = address!("0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F");
const QUOTER_V3: Address = address!("0x61fFE014bA17989E743c5F6cB21bF9697530B21e");
const QUOTER_V4: Address = address!("0x52F0E24D1c21C8A0cB1e5a5dD6198556BD9E1203");

const SENDER: Address = address!("0x9C56a29c7231974c269E24F9FB3c29203039089E");
const GAS_LIMIT: u64 = 8_000_000;
const MAX_FEE_PER_GAS: u128 = 200_000_000_000;
const MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000;

/// The V2 constant-product fee both canonical routers charge (0.3%).
const V2_FEE: degenbot_core::fee::FeeRatio = degenbot_core::fee::fee_ratio(997, 1_000);

// ─────────────────────────────────────────────────────────────────────────
// ABI helpers (hand-encoded)
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

/// A 32-byte ABI word for a signed integer — sign-extended, the form
/// `abi.encode` produces for an `int16`/`int24` mapping key.
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

async fn call_word(
    provider: &AlloyProvider,
    to: Address,
    data: Bytes,
    block: u64,
    label: &str,
) -> U256 {
    let out = provider
        .eth_call(&to, data, Some(block))
        .await
        .unwrap_or_else(|e| panic!("{label}: eth_call to {to} at {block} failed: {e:?}"));
    out_words(&out).first().copied().unwrap_or(U256::ZERO)
}

/// Every returned word of one `eth_call` (the `ticks(int24)` getter returns
/// the whole `Tick.Info` struct, so its `liquidityNet` is the SECOND word —
/// not the high half of the first).
async fn call_words(
    provider: &AlloyProvider,
    to: Address,
    data: Bytes,
    block: u64,
    label: &str,
) -> Vec<U256> {
    let out = provider
        .eth_call(&to, data, Some(block))
        .await
        .unwrap_or_else(|e| panic!("{label}: eth_call to {to} at {block} failed: {e:?}"));
    out_words(&out)
}

async fn read_u256(provider: &AlloyProvider, to: Address, slot: U256, block: u64) -> U256 {
    let value = provider
        .get_storage_at(&to, slot, Some(block))
        .await
        .unwrap();
    U256::from_be_bytes(value.0)
}

async fn getter_u256(provider: &AlloyProvider, to: Address, sig: &str, block: u64) -> U256 {
    call_word(provider, to, calldata(selector(sig), &[]), block, sig).await
}

// ─────────────────────────────────────────────────────────────────────────
// The corpus
// ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    V2,
    V3,
    V4,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dex {
    Uniswap,
    Sushi,
}

impl Dex {
    fn router(self) -> Address {
        match self {
            Self::Uniswap => ROUTER_V2,
            Self::Sushi => ROUTER_SUSHI,
        }
    }

    fn factory(self) -> Address {
        match self {
            Self::Uniswap => FACTORY_V2,
            Self::Sushi => FACTORY_SUSHI,
        }
    }
}

#[derive(Clone, Debug)]
struct HopSpec {
    kind: Kind,
    pool: Address,
    token0: Address,
    token1: Address,
    fee: u32,
    /// V4: part of the pool key. V3: `None` = read `tickSpacing()`.
    tick_spacing: Option<i32>,
    zfo: bool,
    dex: Dex,
}

impl HopSpec {
    fn token_in(&self) -> Address {
        if self.zfo {
            self.token0
        } else {
            self.token1
        }
    }

    fn token_out(&self) -> Address {
        if self.zfo {
            self.token1
        } else {
            self.token0
        }
    }

    fn label(&self) -> String {
        let family = match self.kind {
            Kind::V2 => "V2",
            Kind::V3 => "V3",
            Kind::V4 => "V4",
        };
        format!("{family} {} fee={} zfo={}", self.pool, self.fee, self.zfo)
    }
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
        // NOTE: never trim the row — the trailing column (dex) is legitimately
        // empty on V3/V4 hops, and a tab it carries is the column separator.
        if line.trim().is_empty() || line.starts_with('#') || line.starts_with("path_id") {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        assert!(
            cols.len() == 11,
            "phantom corpus line {} has {} columns, expected 11: {line}",
            index + 1,
            cols.len()
        );
        let id = cols[0].to_string();
        let anchor: u64 = cols[1].parse().expect("anchor block");
        let hop_index: usize = cols[2].parse().expect("hop index");
        let kind = match cols[3] {
            "V2" => Kind::V2,
            "V3" => Kind::V3,
            "V4" => Kind::V4,
            other => panic!("unknown hop kind {other:?} on line {}", index + 1),
        };
        let hop = HopSpec {
            kind,
            pool: cols[4].parse().expect("pool address"),
            token0: cols[5].parse().expect("token0"),
            token1: cols[6].parse().expect("token1"),
            fee: cols[7].parse().expect("fee"),
            tick_spacing: cols[8].parse().ok(),
            zfo: match cols[9] {
                "True" | "true" => true,
                "False" | "false" => false,
                other => panic!("unknown zfo {other:?} on line {}", index + 1),
            },
            dex: match cols[10] {
                "" | "uniswap" => Dex::Uniswap,
                "sushi" => Dex::Sushi,
                other => panic!("unknown dex {other:?} on line {}", index + 1),
            },
        };
        match paths.iter_mut().find(|p| p.id == id) {
            Some(existing) => {
                assert_eq!(existing.anchor, anchor, "path {id} mixes anchor blocks");
                assert_eq!(
                    existing.hops.len(),
                    hop_index,
                    "path {id} hop indices must be contiguous from 0"
                );
                existing.hops.push(hop);
            }
            None => {
                assert_eq!(hop_index, 0, "path {id} must start at hop 0");
                paths.push(PathSpec {
                    id,
                    anchor,
                    hops: vec![hop],
                });
            }
        }
    }
    assert!(!paths.is_empty(), "the phantom corpus is empty");
    // The token chain must compose: hop i's output token is hop i+1's input.
    for path in &paths {
        for pair in path.hops.windows(2) {
            assert_eq!(
                pair[0].token_out(),
                pair[1].token_in(),
                "path {} does not compose at {} -> {}",
                path.id,
                pair[0].label(),
                pair[1].label()
            );
        }
    }
    paths
}

// ─────────────────────────────────────────────────────────────────────────
// Chain state -> white-box state + solver projection
// ─────────────────────────────────────────────────────────────────────────

struct FetchedHop {
    entry: PoolEntry,
    /// `None` when the pool's tick data cannot produce a solver sequence (the
    /// engine would have refused the candidate at projection, so there is no
    /// solver claim to check).
    resolved: Option<ResolvedHop>,
    prepared: Option<PreparedConcentratedLiquidityHop>,
    spacing: i32,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    ticks_in_direction: usize,
    decimals_in: u8,
}

impl FetchedHop {
    fn ranges(&self) -> usize {
        self.prepared.as_ref().map_or(0, |p| p.int_seq.ranges.len())
    }
}

/// The full swap-direction V3 tick map at `block`: every initialized tick the
/// walk can reach, one bitmap read plus one `ticks()` read per initialized
/// tick.
async fn v3_tick_map(
    provider: &AlloyProvider,
    pool: Address,
    spacing: i32,
    current_tick: i32,
    zfo: bool,
    block: u64,
) -> HashMap<i32, TickInfo> {
    let center = word_of(current_tick, spacing);
    let lo = word_of(MIN_TICK, spacing);
    let hi = word_of(MAX_TICK, spacing);
    let words: Vec<i32> = if zfo {
        (lo..=center).rev().collect()
    } else {
        (center..=hi).collect()
    };
    let mut map: HashMap<i32, TickInfo> = HashMap::new();
    let bitmap_sig = selector("tickBitmap(int16)");
    let ticks_sig = selector("ticks(int24)");
    for word in words {
        let bitmap = call_word(
            provider,
            pool,
            calldata(bitmap_sig, &[w_int(word)]),
            block,
            "tickBitmap",
        )
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
            let info = call_words(
                provider,
                pool,
                calldata(ticks_sig, &[w_int(tick)]),
                block,
                "ticks",
            )
            .await;
            assert!(
                info.len() >= 2,
                "ticks(int24) returns the whole Tick.Info struct"
            );
            let gross = (info[0] & U256::from(u128::MAX)).to::<u128>();
            // `liquidityNet` is `int128`, ABI-sign-extended into its own word:
            // reinterpret the low 16 bytes two's-complement.
            let net_bits = (info[1] & U256::from(u128::MAX)).to::<u128>();
            let net = i128::from_le_bytes(net_bits.to_le_bytes());
            map.insert(
                tick,
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(gross),
                    liquidity_net: net,
                    block,
                },
            );
        }
    }
    map
}

/// The full swap-direction V4 tick map, read straight out of `PoolManager`
/// storage (`extsload`) — the layout the V4 journal extractors use.
async fn v4_tick_map(
    provider: &AlloyProvider,
    manager: Address,
    base: U256,
    spacing: i32,
    current_tick: i32,
    zfo: bool,
    block: u64,
) -> HashMap<i32, TickInfo> {
    let center = word_of(current_tick, spacing);
    let lo = word_of(MIN_TICK, spacing);
    let hi = word_of(MAX_TICK, spacing);
    let words: Vec<i32> = if zfo {
        (lo..=center).rev().collect()
    } else {
        (center..=hi).collect()
    };
    let extsload = selector("extsload(bytes32)");
    let mut map: HashMap<i32, TickInfo> = HashMap::new();
    for word in words {
        let Ok(word_pos) = i16::try_from(word) else {
            continue;
        };
        let bitmap = call_word(
            provider,
            manager,
            calldata(
                extsload,
                &[w_uint(v4_tick_bitmap_word_slot(word_pos, base))],
            ),
            block,
            "extsload tickBitmap",
        )
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
            let info = call_word(
                provider,
                manager,
                calldata(extsload, &[w_uint(v4_tick_mapping_slot(tick, base))]),
                block,
                "extsload ticks",
            )
            .await;
            let (gross, net) = decode_tick_word(info);
            map.insert(
                tick,
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(gross),
                    liquidity_net: net,
                    block,
                },
            );
        }
    }
    map
}

async fn token_decimals(provider: &AlloyProvider, token: Address, block: u64) -> u8 {
    let call = provider
        .eth_call(&token, calldata(selector("decimals()"), &[]), Some(block))
        .await;
    match call {
        Ok(out) => {
            let d = out_words(&out).first().copied().unwrap_or(U256::ZERO);
            if d.is_zero() {
                // Not every token exposes `decimals()` (a dead token may not);
                // the probe ladder only needs a magnitude.
                eprintln!("       note: {token} has no decimals() at {block}; assuming 18");
                18
            } else {
                u8::try_from(d.to::<u64>()).unwrap_or(18)
            }
        }
        Err(_) => 18,
    }
}

/// `slot0()` as the pool's own getter returns it: `(uint160 sqrtPriceX96,
/// int24 tick, uint16, uint16, uint16, uint8 feeProtocol, bool)`. Read through
/// the getter, not the raw word, so a fork's storage layout cannot skew the
/// rig's state (Pancake/Sushi V3 keep `liquidity` at their own slot).
async fn v3_slot0(provider: &AlloyProvider, pool: Address, block: u64) -> (U256, i32) {
    let words = call_words(
        provider,
        pool,
        calldata(selector("slot0()"), &[]),
        block,
        "slot0",
    )
    .await;
    assert!(
        words.len() >= 2,
        "slot0() returns the packed struct as words"
    );
    let sqrt_price_x96 = words[0] & ((U256::ONE << 160u32) - U256::ONE);
    // `int24 tick`, ABI sign-extended into its own word.
    let tick_bits = (words[1] & U256::from(0x00FF_FFFFu32)).to::<u32>();
    let tick = if (tick_bits & 0x80_0000) != 0 {
        i32::try_from(tick_bits).expect("24 bits fit i32") - (1 << 24)
    } else {
        i32::try_from(tick_bits).expect("24 bits fit i32")
    };
    (sqrt_price_x96, tick)
}

async fn fetch_v3_hop(provider: &AlloyProvider, spec: &HopSpec, block: u64) -> FetchedHop {
    let (sqrt_price_x96, tick) = v3_slot0(provider, spec.pool, block).await;
    let liquidity = getter_u256(provider, spec.pool, "liquidity()", block)
        .await
        .to::<u128>();
    let spacing = match spec.tick_spacing {
        Some(spacing) => spacing,
        None => i32::try_from(
            getter_u256(provider, spec.pool, "tickSpacing()", block)
                .await
                .to::<i64>(),
        )
        .expect("tick spacing fits i32"),
    };
    let onchain_fee = u32::try_from(
        getter_u256(provider, spec.pool, "fee()", block)
            .await
            .to::<u64>(),
    )
    .expect("fee fits u32");
    assert_eq!(
        onchain_fee,
        spec.fee,
        "{}: corpus fee {} disagrees with chain fee {onchain_fee}",
        spec.label(),
        spec.fee
    );
    let tick_data = v3_tick_map(provider, spec.pool, spacing, tick, spec.zfo, block).await;
    let ticks_in_direction = tick_data
        .keys()
        .filter(|&&t| if spec.zfo { t <= tick } else { t > tick })
        .count();
    let params = RegisterV3PoolParams {
        address: spec.pool,
        token0: spec.token0,
        token1: spec.token1,
        fee: spec.fee,
        tick_spacing: spacing,
        factory: V3_FACTORY,
        sqrt_price_x96,
        liquidity,
        tick,
        tick_data: tick_data.into_iter().collect(),
        update_block: block,
        tick_data_block: Some(block),
        coverage: PoolTickCoverage::Tracked,
        ..Default::default()
    };
    let (identity, state) = V3PoolState::from_params(params, 8);
    let decimals_in = token_decimals(provider, spec.token_in(), block).await;
    let prepared = state
        .build_int_v3_sequence(spacing, spec.fee, spec.zfo)
        .map(|seq| PreparedConcentratedLiquidityHop::derive(Arc::new(seq)));
    let resolved = prepared
        .clone()
        .map(|prepared| ResolvedHop::V3 { prepared });
    FetchedHop {
        entry: PoolEntry::V3(Box::new((identity, state))),
        resolved,
        prepared,
        spacing,
        sqrt_price_x96,
        liquidity,
        tick,
        ticks_in_direction,
        decimals_in,
    }
}

async fn fetch_v4_hop(provider: &AlloyProvider, spec: &HopSpec, block: u64) -> FetchedHop {
    let spacing = spec.tick_spacing.expect("V4 hops carry their tick spacing");
    let pool_key = V4PoolKey {
        currency0: spec.token0,
        currency1: spec.token1,
        fee: spec.fee,
        tick_spacing: spacing,
        hooks: Address::ZERO,
    };
    let pool_id = v4_pool_id(&pool_key);
    let base = v4_pool_state_base_slot(pool_id);
    let extsload = selector("extsload(bytes32)");
    let slot0_word = call_word(
        provider,
        spec.pool,
        calldata(extsload, &[w_uint(v4_slot0_slot(base))]),
        block,
        "extsload slot0",
    )
    .await;
    let slot0 = decode_v4_slot0(slot0_word);
    let liquidity_word = call_word(
        provider,
        spec.pool,
        calldata(extsload, &[w_uint(v4_liquidity_slot(base))]),
        block,
        "extsload liquidity",
    )
    .await;
    let liquidity = (liquidity_word & U256::from(u128::MAX)).to::<u128>();
    let tick_data = v4_tick_map(
        provider, spec.pool, base, spacing, slot0.tick, spec.zfo, block,
    )
    .await;
    let ticks_in_direction = tick_data
        .keys()
        .filter(|&&t| {
            if spec.zfo {
                t <= slot0.tick
            } else {
                t > slot0.tick
            }
        })
        .count();
    let params = RegisterV4PoolParams {
        pool_manager: spec.pool,
        pool_id: pool_id.0,
        pool_key: V4PoolKey {
            fee: slot0.lp_fee,
            ..pool_key
        },
        state_view: Address::ZERO,
        hook_flags: 0,
        protocol_fee: slot0.protocol_fee,
        sqrt_price_x96: slot0.sqrt_price_x96,
        liquidity,
        tick: slot0.tick,
        tick_data: tick_data.into_iter().collect(),
        update_block: block,
        tick_data_block: Some(block),
        coverage: PoolTickCoverage::Tracked,
        known_bitmap_words: Vec::new(),
        fetcher: None,
    };
    let (identity, state) = V4PoolState::from_params(params, 8);
    let decimals_in = token_decimals(provider, spec.token_in(), block).await;
    let prepared = state
        .build_int_v4_sequence(spacing, slot0.lp_fee, spec.zfo)
        .map(|seq| PreparedConcentratedLiquidityHop::derive(Arc::new(seq)));
    let resolved = prepared
        .clone()
        .map(|prepared| ResolvedHop::V4 { prepared });
    FetchedHop {
        entry: PoolEntry::V4(Box::new((identity, state))),
        resolved,
        prepared,
        spacing,
        sqrt_price_x96: slot0.sqrt_price_x96,
        liquidity,
        tick: slot0.tick,
        ticks_in_direction,
        decimals_in,
    }
}

async fn fetch_v2_hop(provider: &AlloyProvider, spec: &HopSpec, block: u64) -> FetchedHop {
    // The pair's packed reserves word — the same word its own `getReserves()`
    // reads.
    let word = read_u256(provider, spec.pool, U256::from(V2_RESERVES_SLOT), block).await;
    let reserves = decode_v2_reserves_word(word);
    let params = RegisterV2PoolParams {
        address: spec.pool,
        token0: spec.token0,
        token1: spec.token1,
        reserve0: reserves.reserve0,
        reserve1: reserves.reserve1,
        fee_token0: V2_FEE,
        fee_token1: V2_FEE,
        factory: spec.dex.factory(),
        update_block: block,
        variant: DexVariant::UniswapV2,
        deployer: Address::ZERO,
        init_hash: alloy::primitives::B256::ZERO,
        stable_swap: false,
    };
    let (identity, state) = V2PoolState::from_params(&params, 8, (None, None));
    let (reserve_in, reserve_out) = if spec.zfo {
        (U256::from(reserves.reserve0), U256::from(reserves.reserve1))
    } else {
        (U256::from(reserves.reserve1), U256::from(reserves.reserve0))
    };
    let hop_state = IntHopState::new(
        reserve_in,
        reserve_out,
        V2_FEE.gamma_numer(),
        V2_FEE.denominator(),
    );
    let decimals_in = token_decimals(provider, spec.token_in(), block).await;
    FetchedHop {
        entry: PoolEntry::V2(Box::new((identity, state))),
        resolved: Some(ResolvedHop::V2 { state: hop_state }),
        prepared: None,
        spacing: 0,
        sqrt_price_x96: U256::ZERO,
        liquidity: 0,
        tick: 0,
        ticks_in_direction: 0,
        decimals_in,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The chain side: one `eth_call`-shaped frame per leg on evm2
// ─────────────────────────────────────────────────────────────────────────

fn leg_calldata(spec: &HopSpec, amount_in: U256) -> (Address, Bytes) {
    match spec.kind {
        Kind::V3 => (
            QUOTER_V3,
            calldata(
                selector("quoteExactInputSingle((address,address,uint256,uint24,uint160))"),
                &[
                    w_addr(spec.token_in()),
                    w_addr(spec.token_out()),
                    w_uint(amount_in),
                    w_small(u64::from(spec.fee)),
                    w_uint(U256::ZERO),
                ],
            ),
        ),
        Kind::V2 => (
            spec.dex.router(),
            calldata(
                selector("getAmountsOut(uint256,address[])"),
                &[
                    w_uint(amount_in),
                    w_small(0x40),
                    w_small(2),
                    w_addr(spec.token_in()),
                    w_addr(spec.token_out()),
                ],
            ),
        ),
        Kind::V4 => (
            QUOTER_V4,
            calldata(
                selector(
                    "quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))",
                ),
                &[
                    w_small(0x20),
                    w_addr(spec.token0),
                    w_addr(spec.token1),
                    w_small(u64::from(spec.fee)),
                    w_small(u64::try_from(spec.tick_spacing.expect("V4 spacing")).unwrap()),
                    w_addr(Address::ZERO),
                    w_small(u64::from(spec.zfo)),
                    w_uint(amount_in),
                    w_small(0x100),
                    w_small(0),
                ],
            ),
        ),
    }
}

fn quoted_output(spec: &HopSpec, out: &Bytes) -> U256 {
    let words = out_words(out);
    match spec.kind {
        Kind::V2 => {
            assert!(words.len() >= 4, "getAmountsOut shape: {words:?}");
            let len = words[1].to::<usize>();
            assert_eq!(len, 2, "a 2-token path returns 2 amounts");
            words[1 + len]
        }
        Kind::V3 | Kind::V4 => {
            assert!(!words.is_empty(), "empty quoter return");
            words[0]
        }
    }
}

/// The pool the canonical quoter/router would route through for this hop. A
/// mismatch means the EVM leg answers from a DIFFERENT pool, so its figure is
/// not this hop's chain truth (reported, not asserted).
async fn canonical_pool(provider: &AlloyProvider, spec: &HopSpec, block: u64) -> Option<Address> {
    let out = match spec.kind {
        Kind::V3 => {
            let data = calldata(
                selector("getPool(address,address,uint24)"),
                &[
                    w_addr(spec.token0),
                    w_addr(spec.token1),
                    w_small(u64::from(spec.fee)),
                ],
            );
            provider
                .eth_call(&V3_FACTORY, data, Some(block))
                .await
                .ok()?
        }
        Kind::V2 => {
            let data = calldata(
                selector("getPair(address,address)"),
                &[w_addr(spec.token0), w_addr(spec.token1)],
            );
            provider
                .eth_call(&spec.dex.factory(), data, Some(block))
                .await
                .ok()?
        }
        Kind::V4 => return Some(spec.pool),
    };
    let word = out_words(&out).first().copied()?;
    let bytes = word.to_be_bytes::<32>();
    let candidate = Address::from_slice(&bytes[12..]);
    if candidate.is_zero() {
        None
    } else {
        Some(candidate)
    }
}

type RigExt<'p> = ::evm2::evm::CacheDB<::evm2::evm::Db<BlockingAsyncDb<'p>>>;

/// The quote caller: the contract that invokes `pool.swap` and answers the
/// pool's `uniswapV3SwapCallback`.
const CALLBACK: Address = address!("0x00000000000000000000000000000000c0ffee01");

fn huge_balance() -> U256 {
    ten_pow(60)
}

/// The quote caller's bytecode — the mechanism `QuoterV2` itself uses, minus the
/// factory: a `pool.swap` driven from a contract whose
/// `uniswapV3SwapCallback` REVERTS with the `(amount0Delta, amount1Delta)` the
/// pool just computed. The pool propagates that revert, the frame ends with a
/// 64-byte revert payload, and those two words are the chain's amounts.
///
/// Nothing is settled (no token balances, no transfers), so this works for ANY
/// V3-family pool address — not only the ones a canonical quoter would route
/// to. Calldata layout for the forward arm: the 32-byte pool address followed
/// by the `swap(...)` calldata.
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
    // Forward arm: calldata[0x20..] is the swap call; call the pool with it and
    // capture the two returned deltas at memory[0x100..0x140]. The callback's
    // revert propagates: bubble its payload.
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
    // Bubble arm: re-revert with the callee's payload (the pool's own revert).
    c.extend_from_slice(&[
        0x5b, // JUMPDEST
        0x3d, // RETURNDATASIZE (size)
        0x60, 0x00, // PUSH1 0 (offset)
        0x60, 0x00, // PUSH1 0 (destOffset)
        0x3e, // RETURNDATACOPY
        0x3d, // RETURNDATASIZE
        0x60, 0x00, // PUSH1 0
        0xfd, // REVERT
    ]);
    assert_eq!(c.len(), 78, "the quote arm starts at 78");
    // Quote arm: revert with (amount0Delta, amount1Delta) — the pool's own
    // computed amounts (the QuoterV2 mechanism).
    c.extend_from_slice(&[
        0x5b, // JUMPDEST
        0x60, 0x04, 0x35, // PUSH1 4 CALLDATALOAD (amount0Delta)
        0x60, 0x00, // PUSH1 0
        0x52, // MSTORE
        0x60, 0x24, 0x35, // PUSH1 0x24 CALLDATALOAD (amount1Delta)
        0x60, 0x20, // PUSH1 0x20
        0x52, // MSTORE
        0x60, 0x40, // PUSH1 0x40 (size)
        0x60, 0x00, // PUSH1 0 (offset)
        0xfd, // REVERT — the payload IS the quote
    ]);
    c
}

/// The magnitude of a negative `int256` word (the pool reports its output leg
/// as a negative delta).
fn neg_magnitude(word: U256) -> U256 {
    (!word).overflowing_add(U256::ONE).0
}

/// One frame's raw result: the returndata ALWAYS (a revert payload is a result
/// here), plus whether the frame succeeded and its note.
struct FrameOut {
    output: Bytes,
    success: bool,
    gas: u64,
    note: String,
}

struct Chain<'p> {
    scratch: Evm2ScratchEvm<RigExt<'p>>,
    nonce: u64,
    self_checks: Vec<String>,
    /// Gas the most recent leg frame burned (the economic verdict's input).
    last_gas: u64,
}

impl<'p> Chain<'p> {
    fn new(provider: &'p AlloyProvider, block: ScratchBlock, nonce: u64) -> Self {
        let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(provider, block.number - 1))
            .expect("multi-threaded runtime is ambient");
        let mut ext = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(blocking));
        // The frame sender pays gas; in a scratch that accounting is noise, so
        // fund it rather than depend on a live balance at the anchor.
        ext.insert_account_info(
            &SENDER,
            ::evm2::evm::AccountInfo::default().with_balance(huge_balance()),
        );
        ext.insert_account_info(
            &CALLBACK,
            ::evm2::evm::AccountInfo::default()
                .with_balance(huge_balance())
                .with_code(::evm2::bytecode::Bytecode::new_raw(Bytes::from(
                    v3_caller_code(),
                ))),
        );
        Self {
            scratch: Evm2ScratchEvm::new(ext, block),
            nonce,
            self_checks: Vec::new(),
            last_gas: 0,
        }
    }

    fn frame(&mut self, from: Address, to: Address, data: Bytes, nonce: u64) -> FrameOut {
        let tx = ReplayableTx {
            from,
            to: Some(to),
            value: U256::ZERO,
            data,
            gas_limit: GAS_LIMIT,
            max_fee_per_gas: MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: MAX_PRIORITY_FEE_PER_GAS,
            nonce,
        };
        let out = match self.scratch.replay(&tx) {
            Ok(outcome) => FrameOut {
                success: outcome.status == RunVerdict::Success,
                note: format!(
                    "{:?} gas={} out=0x{}",
                    outcome.status,
                    outcome.gas_used,
                    alloy::hex::encode(&outcome.output)
                ),
                output: outcome.output.clone(),
                gas: outcome.gas_used,
            },
            Err(err) => FrameOut {
                output: Bytes::new(),
                success: false,
                gas: 0,
                note: format!("frame-refused: {err:?}"),
            },
        };
        self.last_gas = out.gas;
        out
    }

    /// The canonical-routed quote (`QuoterV2` / the V2 router / the V4
    /// `Quoter`). Only meaningful when the canonical factory resolves THIS pool
    /// (see [`canonical_pool`]).
    fn quote_canonical(&mut self, spec: &HopSpec, amount_in: U256) -> (Option<U256>, String) {
        let (target, data) = leg_calldata(spec, amount_in);
        let nonce = self.nonce;
        let out = self.frame(SENDER, target, data, nonce);
        if out.success {
            (Some(quoted_output(spec, &out.output)), out.note)
        } else {
            (None, out.note)
        }
    }

    /// A V3 leg's chain truth from the POOL'S OWN `swap()` bytecode: the
    /// address-agnostic path (Pancake/Sushi/Uni forks all answer `swap`, but
    /// only a canonical pool's quoter routes to it). The frame ends in the quote
    /// callback's 64-byte revert payload — `(amount0Delta, amount1Delta)` —
    /// because settlement is deliberately skipped.
    fn quote_v3_pool(&mut self, spec: &HopSpec, amount_in: U256) -> (Option<U256>, String) {
        let limit = if spec.zfo {
            U256::from(MIN_SQRT_RATIO) + U256::ONE
        } else {
            U256::from(MAX_SQRT_RATIO) - U256::ONE
        };
        let swap = calldata(
            selector("swap(address,bool,int256,uint160,bytes)"),
            &[
                w_addr(CALLBACK),
                w_small(u64::from(spec.zfo)),
                w_uint(amount_in),
                w_uint(limit),
                w_small(0xa0),
                w_small(0),
            ],
        );
        let mut forwarded = Vec::with_capacity(32 + swap.len());
        forwarded.extend_from_slice(&w_addr(spec.pool));
        forwarded.extend_from_slice(swap.as_ref());
        let nonce = self.nonce;
        let out = self.frame(SENDER, CALLBACK, Bytes::from(forwarded), nonce);
        // (int256 amount0Delta, int256 amount1Delta) is the quote; anything
        // else is the pool refusing this swap (a dead leg).
        let words = out_words(&out.output);
        if words.len() != 2 {
            return (None, format!("pool.swap {}", out.note));
        }
        let magnitude = if spec.zfo {
            neg_magnitude(words[1])
        } else {
            neg_magnitude(words[0])
        };
        (
            Some(magnitude),
            format!(
                "pool.swap deltas=[{}, {}] gas={}",
                words[0], words[1], out.gas
            ),
        )
    }

    /// One leg's chain truth, with an independent cross-check on every leg the
    /// canonical factory ALSO serves (the quoter must agree with the pool's own
    /// bytecode — if it does not, the rig's quote mechanism is wrong, not the
    /// chain).
    fn quote_leg(
        &mut self,
        spec: &HopSpec,
        amount_in: U256,
        canonical: bool,
    ) -> (Option<U256>, String) {
        if amount_in.is_zero() {
            return (None, "zero-input".to_string());
        }
        match spec.kind {
            Kind::V3 => {
                let (own, own_note) = self.quote_v3_pool(spec, amount_in);
                if canonical {
                    // A dead leg refuses BOTH ways (the quoter reverts with no
                    // quote, the pool's swap returns zero deltas): refuse ==
                    // zero-output, so normalize before comparing.
                    let (quoted, quoted_note) = self.quote_canonical(spec, amount_in);
                    if quoted.unwrap_or(U256::ZERO) != own.unwrap_or(U256::ZERO) {
                        self.self_checks.push(format!(
                            "RIG SELF-CHECK {} input {amount_in}: quoter {} != pool.swap {} \
                             ({quoted_note} / {own_note})",
                            spec.label(),
                            fmt_amt(quoted),
                            fmt_amt(own)
                        ));
                    }
                }
                (own, own_note)
            }
            Kind::V2 | Kind::V4 => {
                if !canonical {
                    return (
                        None,
                        "non-canonical route (no address-agnostic quoter)".to_string(),
                    );
                }
                self.quote_canonical(spec, amount_in)
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// White-box figures
// ─────────────────────────────────────────────────────────────────────────

fn ten_pow(exp: u32) -> U256 {
    let mut v = U256::ONE;
    for _ in 0..exp {
        v *= U256::from(10u64);
    }
    v
}

/// The probe ladder for a hop whose input token has `decimals` whole-token
/// magnitudes: `10^(d-3)`, `10^(d-1)`, `10^d`, `10^(d+1)`.
fn probe_ladder(decimals: u8) -> Vec<U256> {
    let d = u32::from(decimals).max(3);
    vec![ten_pow(d - 3), ten_pow(d - 1), ten_pow(d), ten_pow(d + 1)]
}

fn describe_hop(hop: &FetchedHop) -> String {
    let head = format!(
        "spacing={} tsq={} liq={} tick={} own_ticks={} ranges={}",
        hop.spacing,
        hop.sqrt_price_x96,
        hop.liquidity,
        hop.tick,
        hop.ticks_in_direction,
        hop.ranges(),
    );
    let Some(prepared) = hop.prepared.as_ref() else {
        return format!("{head} (no solver sequence: build_int_sequence returned None)");
    };
    if prepared.int_seq.ranges.is_empty() {
        return format!("{head} (empty solver sequence)");
    }
    let first = &prepared.int_seq.ranges[0];
    let last = prepared.int_seq.ranges.last().unwrap();
    let exit = if last.zero_for_one {
        last.sqrt_price_lower_x96
    } else {
        last.sqrt_price_upper_x96
    };
    let sample: Vec<String> = prepared
        .int_seq
        .ranges
        .iter()
        .enumerate()
        .filter(|(i, r)| *i < 3 || *i + 3 > prepared.int_seq.ranges.len() || r.liquidity != 0)
        .map(|(i, r)| {
            format!(
                "#{i}:L={} in={} out={}",
                r.liquidity,
                r.sqrt_price_x96,
                if r.zero_for_one {
                    r.sqrt_price_lower_x96
                } else {
                    r.sqrt_price_upper_x96
                }
            )
        })
        .collect();
    format!(
        "{head} r0=[L={} entry={}] rN=[L={} exit={}] ranges: {}",
        first.liquidity,
        first.sqrt_price_x96,
        last.liquidity,
        exit,
        sample.join(" ")
    )
}

/// The solver's own hop figure: the crossing/word-profile walk the active set
/// evaluates candidates with (a one-hop `ConcentratedLiquidityPathSim`), or the
/// V2 constant-product hop read.
fn solver_leaf(hop: &FetchedHop, spec: &HopSpec, amount: U256) -> Option<U256> {
    match hop.resolved.as_ref()? {
        ResolvedHop::V2 { state } => Some(state.swap(amount).unwrap_or(U256::ZERO)),
        ResolvedHop::V3 { .. } | ResolvedHop::V4 { .. } => {
            let prepared = hop.prepared.as_ref()?;
            if prepared.int_seq.ranges.is_empty() {
                return None;
            }
            let seq = hop.resolved.as_ref()?.as_int_sequence()?;
            let sim =
                ConcentratedLiquidityPathSim::new(&[seq], Some(std::slice::from_ref(prepared)));
            let _ = spec;
            Some(sim.output(amount).final_output)
        }
        _ => None,
    }
}

/// The byte-exact family sim (`simulate_swap` — the pool read door), with its
/// refusal reason when it declines.
fn family_sim(hop: &FetchedHop, spec: &HopSpec, amount: U256) -> (Option<U256>, String) {
    match simulate_swap(&hop.entry, spec.zfo, amount) {
        Ok(out) => (Some(out), String::new()),
        Err(err) => (None, format!("sim-refused: {err:?}")),
    }
}

fn fmt_amt(v: Option<U256>) -> String {
    match v {
        Some(v) => v.to_string(),
        None => "-".to_string(),
    }
}

fn rpc_url() -> String {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .expect(
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) \
             to an archive mainnet endpoint",
        )
}

// ─────────────────────────────────────────────────────────────────────────
// The run
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC parity gate over archive mainnet state; run with --ignored"]
async fn phantom_corpus_parity() {
    let started = Instant::now();
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    let chain_id = provider.get_chain_id().await.unwrap();
    assert_eq!(chain_id, 1, "the phantom corpus is a mainnet fixture");
    let provider: &AlloyProvider = &provider;

    let mut corpus = load_corpus();
    if let Ok(filter) = std::env::var("DEGENBOT_PHANTOM_PATH") {
        corpus.retain(|p| p.id.contains(&filter));
        assert!(!corpus.is_empty(), "no corpus path matches {filter:?}");
    }

    println!("=== phantom-candidate fidelity rig ===");
    println!(
        "corpus: {} path(s) from tests/fixtures/phantom_paths.tsv",
        corpus.len()
    );

    // Fetched state is shared across paths (the phantom set reuses pools).
    let mut state_cache: HashMap<String, Arc<FetchedHop>> = HashMap::new();
    let mut divergences: Vec<String> = Vec::new();
    let mut admission = AdmissionCensus::default();

    for path in &corpus {
        println!();
        println!(
            "-- path {} @ {} ({} hop(s)) --",
            path.id,
            path.anchor,
            path.hops.len()
        );
        let block = provider
            .get_block(path.anchor)
            .await
            .unwrap()
            .expect("the anchor block is archived");
        let nonce = provider
            .get_transaction_count(&SENDER, Some(path.anchor))
            .await
            .unwrap();
        let scratch_block = ScratchBlock {
            number: path.anchor + 1,
            timestamp: block.header.timestamp,
            base_fee_next: u128::from(block.header.base_fee_per_gas.unwrap_or(0)),
        };

        let mut hops: Vec<Arc<FetchedHop>> = Vec::with_capacity(path.hops.len());
        for spec in &path.hops {
            let key = format!(
                "{:?}|{}|{}|{}|{}",
                spec.kind,
                spec.pool,
                spec.fee,
                spec.zfo,
                spec.tick_spacing.map_or(-1, i32::from)
            );
            let hop = match state_cache.get(&key) {
                Some(hit) => Arc::clone(hit),
                None => {
                    let fetched = match spec.kind {
                        Kind::V2 => fetch_v2_hop(provider, spec, path.anchor).await,
                        Kind::V3 => fetch_v3_hop(provider, spec, path.anchor).await,
                        Kind::V4 => fetch_v4_hop(provider, spec, path.anchor).await,
                    };
                    let fetched = Arc::new(fetched);
                    state_cache.insert(key, Arc::clone(&fetched));
                    fetched
                }
            };
            println!("  hop: {}", spec.label());
            println!("       {}", describe_hop(&hop));
            hops.push(hop);
        }

        // ── the solver's claim ──────────────────────────────────────────
        let resolved_hops: Option<Vec<ResolvedHop>> =
            hops.iter().map(|h| h.resolved.clone()).collect();
        match resolved_hops {
            Some(resolved_hops) => {
                let mut resolved = ResolvedMixedPath {
                    hops: resolved_hops,
                    valid: true,
                    ..Default::default()
                };
                resolved.max_update_block = path.anchor;
                resolved.state_nonces = vec![0; resolved.hops.len()];
                let gate = GateDeps::offline();
                let outcome = solve_path(&resolved, &gate);
                match &outcome.result {
                    Some(result) => println!(
                        "  solver: optimal_input={} profit={} hop_outputs={:?} \
                         consumed_inputs={:?}",
                        result.optimal_input,
                        result.profit,
                        result.hop_outputs,
                        result.consumed_inputs
                    ),
                    None => println!(
                        "  solver: NO PROFITABLE RESULT (the engine would not have \
                         published this path at this block)"
                    ),
                }
                check_path(
                    path,
                    &hops,
                    provider,
                    scratch_block,
                    nonce,
                    outcome.result.as_ref(),
                    &mut divergences,
                    &mut admission,
                )
                .await;
            }
            None => {
                // A refused projection is the engine's own "no candidate here":
                // there is no solver claim to compare, so only the leg parity
                // (family sim vs chain) runs. A hop that the CHAIN pays while
                // the projection refuses is an under-prediction, reported by
                // the parity rows, never a fabrication.
                for (i, hop) in hops.iter().enumerate() {
                    if hop.resolved.is_none() {
                        println!(
                            "  solver: hop[{i}] projection refused (build_int_sequence -> None)"
                        );
                    }
                }
                check_path(
                    path,
                    &hops,
                    provider,
                    scratch_block,
                    nonce,
                    None,
                    &mut divergences,
                    &mut admission,
                )
                .await;
            }
        }
    }

    println!();
    println!(
        "fetched state for {} (pool, direction) projection(s)",
        state_cache.len()
    );
    println!("wall: {:.1} s", started.elapsed().as_secs_f64());
    println!();
    println!("-- the admission floor over the corpus --");
    println!(
        "   floor = settlement frame + per-hop family row (census-priced) at the anchor's base fee;\n   \
         truth = the legs' measured quote-frame gas at the same base fee"
    );
    println!(
        "   closed-loop candidates {} | sub-gas {} | floored {} of {} | profitable suppressed {} \
         | admitted profitable {} | unpriced {}",
        admission.candidates,
        admission.sub_gas,
        admission.floored,
        admission.sub_gas,
        admission.profitable_suppressed,
        admission.admitted_profitable,
        admission.unpriced
    );
    if let Some(headroom) = admission.min_headroom {
        println!("   least floored margin below the floor: {headroom} wei");
    }
    let signed_pct: i128 =
        (i128::from(admission.floor_legs_gas) - i128::from(admission.measured_legs_gas)) * 100
            / i128::from(admission.measured_legs_gas);
    println!(
        "   the floor's leg model vs the measured legs: {} vs {} gas ({signed_pct:+}%; the floor \
         must under-predict here — the settlement terms it adds are measured separately and are \
         not in the quote-frame truth)",
        admission.floor_legs_gas, admission.measured_legs_gas
    );
    assert!(
        admission.candidates > 0,
        "the corpus must contain closed-loop candidates for the floor to be tested"
    );
    assert_eq!(
        admission.unpriced, 0,
        "every corpus leg is V2/V3/V4, so every candidate must be priced"
    );
    assert_eq!(
        admission.floored,
        admission.sub_gas,
        "every sub-gas candidate must be floored: {} were not (see the findings above)",
        admission.sub_gas - admission.floored
    );
    assert_eq!(
        admission.profitable_suppressed, 0,
        "the floor suppressed a candidate the chain pays for — lost alpha"
    );
    // The under-prediction property on the truth set: the floor's leg model must
    // sit below the legs' measured gas, so a candidate the chain pays for clears
    // it. (The settlement terms are charged on top and are measured separately;
    // the quote-frame truth carries none of them.)
    assert!(
        admission.floor_legs_gas <= admission.measured_legs_gas,
        "the floor's leg model OVER-predicts the measured corpus legs ({} vs {} gas) — the \
         suppression direction is the expensive one",
        admission.floor_legs_gas,
        admission.measured_legs_gas
    );

    println!();
    println!(
        "-- divergence summary ({} finding(s)) --",
        divergences.len()
    );
    for finding in &divergences {
        println!("  x {finding}");
    }
    assert!(
        divergences.is_empty(),
        "the phantom corpus diverges: {} finding(s) — every one is a white-box figure the \
         chain does not pay",
        divergences.len()
    );
}

/// The admission floor's verdict over the corpus — the truth-set ledger the
/// corpus assertions read.
///
/// The truth set is the CHAIN: each closed-loop candidate's profit is replayed
/// against the pool's own bytecode, and the candidate's measured execution gas
/// (the same frames the legs are quoted with) prices it at the anchor block's
/// base fee. A candidate whose chain margin cannot cover its measured gas is
/// the sub-gas class the floor exists for.
#[derive(Default)]
struct AdmissionCensus {
    /// Closed-loop candidates with a solver claim (the floor's population).
    candidates: usize,
    /// Candidates the truth set calls sub-gas (chain profit ≤ measured gas
    /// cost at the anchor's base fee).
    sub_gas: usize,
    /// Sub-gas candidates the gas floor suppressed.
    floored: usize,
    /// Candidates the truth set calls PROFITABLE (chain profit > measured gas
    /// cost) that the floor suppressed anyway — the lost-alpha direction, which
    /// must be zero.
    profitable_suppressed: usize,
    /// Profitable candidates the floor admitted.
    admitted_profitable: usize,
    /// Candidates the floor could not price (a hop outside the measured
    /// families). Must be zero on this corpus: every leg is V2/V3/V4.
    unpriced: usize,
    /// Smallest `floor − candidate margin` over the floored candidates (wei) —
    /// how far below the bar the phantom class sits.
    min_headroom: Option<U256>,
    /// The floor's LEG model summed over the closed-loop candidates (gas) — the
    /// quantity the measured quote-frame gas is the truth set for, with the
    /// settlement terms excluded on both sides.
    floor_legs_gas: u64,
    /// The same candidates' legs, MEASURED on the chain (gas).
    measured_legs_gas: u64,
}

/// Per-hop parity plus the path-level claim replay.
async fn check_path(
    path: &PathSpec,
    hops: &[Arc<FetchedHop>],
    provider: &AlloyProvider,
    scratch_block: ScratchBlock,
    nonce: u64,
    claimed: Option<&degenbot_pools::solve::SolvePathResult>,
    divergences: &mut Vec<String>,
    admission: &mut AdmissionCensus,
) {
    let mut canonical: Vec<Option<Address>> = Vec::new();
    for spec in &path.hops {
        let routed = canonical_pool(provider, spec, path.anchor).await;
        if routed != Some(spec.pool) {
            println!(
                "       note: {}: the canonical quoter resolves {:?}, not the corpus pool — \
                 the EVM leg answers from another pool (reported, not asserted)",
                spec.label(),
                routed
            );
        }
        canonical.push(routed);
    }

    let mut chain = Chain::new(provider, scratch_block, nonce);
    // A V3 leg is asserted against its OWN `swap()` bytecode whatever factory
    // it belongs to; a V2/V4 leg needs the canonical route to be quotable.
    let assertable: Vec<bool> = path
        .hops
        .iter()
        .enumerate()
        .map(|(i, spec)| spec.kind == Kind::V3 || canonical[i] == Some(spec.pool))
        .collect();

    for (i, spec) in path.hops.iter().enumerate() {
        let hop = &hops[i];
        let mut probes = probe_ladder(hop.decimals_in);
        if let Some(result) = claimed {
            if let Some(input) = result.consumed_inputs.get(i) {
                if !input.is_zero() {
                    probes.push(*input);
                }
            }
        }
        probes.sort_unstable();
        probes.dedup();
        println!(
            "  hop[{i}] parity ({} probe(s), asserted={}):",
            probes.len(),
            assertable[i]
        );
        println!(
            "       {:>26}  {:>26}  {:>26}  {:>26}  {}",
            "input", "solver leaf", "family sim", "chain (evm2)", "chain note"
        );
        let mut hop_dirty = false;
        for amount in &probes {
            let leaf = solver_leaf(hop, spec, *amount);
            let (sim, sim_note) = family_sim(hop, spec, *amount);
            let (evm, note) = if assertable[i] {
                chain.quote_leg(spec, *amount, canonical[i] == Some(spec.pool))
            } else {
                (None, "non-canonical route (not asserted)".to_string())
            };
            println!(
                "       {:>26}  {:>26}  {:>26}  {:>26}  {}{}",
                amount,
                fmt_amt(leaf),
                fmt_amt(sim),
                fmt_amt(evm),
                note,
                if sim_note.is_empty() {
                    String::new()
                } else {
                    format!(" {sim_note}")
                }
            );
            // The solver's figure must equal the family sim's — both read the
            // same chain state, so a gap here is solver framing. A refused
            // projection (`None` leaf) makes no claim to check; the sim-vs-chain
            // row below still holds the leg to the chain.
            if hop.resolved.is_some() && leaf != sim {
                hop_dirty = true;
                divergences.push(format!(
                    "path {} hop[{i}] input {amount}: solver leaf {} != family sim {}",
                    path.id,
                    fmt_amt(leaf),
                    fmt_amt(sim)
                ));
            }
            // …and both must equal the chain (a reverting route pays 0).
            if assertable[i] {
                let chain_abs = evm.unwrap_or(U256::ZERO);
                if sim.unwrap_or(U256::ZERO) != chain_abs {
                    hop_dirty = true;
                    divergences.push(format!(
                        "path {} hop[{i}] input {amount}: white-box {} != chain {chain_abs} ({note})",
                        path.id,
                        fmt_amt(sim)
                    ));
                }
            }
        }
        if !hop_dirty {
            println!("       parity ok");
        }
    }

    // Path-level: replay the solver's claimed optimum on the chain, hop by
    // hop, comparing each hop's output against what the solver promised.
    //
    // The profit verdict needs a CLOSED LOOP (the path returns to its start
    // token). A single-hop corpus row is a leg test: its "profit" would compare
    // token A's input against token B's output, which is not a claim at all.
    let closed_loop = path.hops.len() >= 2
        && path.hops.first().map(HopSpec::token_in) == path.hops.last().map(HopSpec::token_out);
    if let Some(result) = claimed {
        if !result.optimal_input.is_zero() {
            println!(
                "  path claim parity at optimal_input={}:",
                result.optimal_input
            );
            println!(
                "       {:>4}  {:>26}  {:>26}  {:>26}  {}",
                "hop", "input", "solver hop_output", "chain out", "note"
            );
            let mut amount = result.optimal_input;
            let mut gas_total = 0u64;
            for (i, spec) in path.hops.iter().enumerate() {
                let (evm, note) = if spec.kind == Kind::V3 || canonical[i] == Some(spec.pool) {
                    chain.quote_leg(spec, amount, canonical[i] == Some(spec.pool))
                } else {
                    (None, "non-canonical route (not asserted)".to_string())
                };
                gas_total = gas_total.saturating_add(chain.last_gas);
                let chain_out = evm.unwrap_or(U256::ZERO);
                let promised = result.hop_outputs.get(i).copied().unwrap_or(U256::ZERO);
                println!(
                    "       {:>4}  {:>26}  {:>26}  {:>26}  {}",
                    i, amount, promised, chain_out, note
                );
                if (spec.kind == Kind::V3 || canonical[i] == Some(spec.pool))
                    && promised != chain_out
                {
                    divergences.push(format!(
                        "path {} hop[{i}] at the claimed optimum: solver {} != chain {chain_out}",
                        path.id, promised
                    ));
                }
                amount = chain_out;
            }
            let chain_profit = if amount > result.optimal_input {
                amount - result.optimal_input
            } else {
                U256::ZERO
            };
            // The ECONOMIC verdict: a claim that the chain pays but gas dwarfs
            // is not a fabrication — it is a dispatch-threshold question.
            let gas_price = scratch_block.base_fee_next + MAX_PRIORITY_FEE_PER_GAS;
            let gas_cost = U256::from(gas_total) * U256::from(gas_price);
            println!(
                "       solver profit={} -> chain final={} (chain profit {chain_profit}); \
                 gas {gas_total} @ {gas_price} wei = {gas_cost} wei cost -> net {}",
                result.profit,
                amount,
                if chain_profit > gas_cost {
                    format!("+{}", chain_profit - gas_cost)
                } else {
                    format!("-{}", gas_cost - chain_profit)
                }
            );
            if closed_loop && amount <= result.optimal_input {
                divergences.push(format!(
                    "path {}: solver claims profit {} but the chain composition returns \
                     {amount} <= input {} (FABRICATION)",
                    path.id, result.profit, result.optimal_input
                ));
            }
            if !closed_loop {
                println!(
                    "       (single-hop leg row: not a closed loop, so the profit figure is \
                     cross-token and is NOT asserted)"
                );
            }

            // ── the admission floor, priced off the census the solve carried ──
            //
            // The floor is the policy's own figure: the path's per-hop family
            // rows + the walk's census + the settlement terms, times the
            // anchor block's base fee. The truth set prices the same candidate
            // with the gas its legs MEASURED on the chain, at the same base
            // fee. Because gas_total carries each quote frame's own intrinsic,
            // that truth figure is the dearer side — the floor must sit under
            // it, or the candidate it suppressed was not sub-gas after all.
            let base_fee = scratch_block.base_fee_next;
            let floor = degenbot_pools::solve::path_gas_floor_wei(result, base_fee);
            let measured_cost = U256::from(gas_total) * U256::from(base_fee);
            let sub_gas = chain_profit <= measured_cost;
            let floored = floor.is_some_and(|floor| result.profit <= floor);

            match floor {
                Some(floor) => {
                    println!(
                        "       admission floor={floor} wei (gas {} @ {base_fee}) vs margin {} -> {}",
                        degenbot_pools::solve::MEASURED_GAS_FLOOR
                            .path_gas(&result.gas_hops)
                            .unwrap_or(0),
                        result.profit,
                        if floored { "FLOORED" } else { "admitted" }
                    );
                }
                None => println!(
                    "       admission floor=UNPRICED (a hop outside the measured families)"
                ),
            }
            if closed_loop {
                admission.candidates += 1;
                if let Some(legs) =
                    degenbot_pools::solve::MEASURED_GAS_FLOOR.legs_gas(&result.gas_hops)
                {
                    admission.floor_legs_gas = admission.floor_legs_gas.saturating_add(legs);
                    admission.measured_legs_gas =
                        admission.measured_legs_gas.saturating_add(gas_total);
                }
                if floor.is_none() {
                    admission.unpriced += 1;
                }
                if sub_gas {
                    admission.sub_gas += 1;
                    if floored {
                        admission.floored += 1;
                        if let Some(floor) = floor {
                            let headroom = floor - result.profit;
                            admission.min_headroom = Some(
                                admission
                                    .min_headroom
                                    .map_or(headroom, |least| least.min(headroom)),
                            );
                        }
                    } else {
                        divergences.push(format!(
                            "path {}: sub-gas candidate NOT floored — chain margin {chain_profit} \
                             ≤ measured cost {measured_cost} but the floor {floor:?} admits the \
                             claim {}",
                            path.id, result.profit
                        ));
                    }
                } else if floored {
                    admission.profitable_suppressed += 1;
                    divergences.push(format!(
                        "path {}: PROFITABLE candidate suppressed — chain margin {chain_profit} > \
                         measured cost {measured_cost} but the floor {floor:?} suppresses the \
                         claim {}",
                        path.id, result.profit
                    ));
                } else {
                    admission.admitted_profitable += 1;
                }
            }
        }
    }

    // The rig's own cross-check (quoter vs the pool's own bytecode on the legs
    // both can answer) is a FACT about the rig, not about the corpus: surface it
    // loudly, because a mismatch would invalidate every chain figure above.
    divergences.extend(chain.self_checks.drain(..));
}
