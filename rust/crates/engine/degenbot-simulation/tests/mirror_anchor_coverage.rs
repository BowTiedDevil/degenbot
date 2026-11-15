#![expect(
    clippy::expect_used,
    reason = "the pinned read-set constants fail loudly on a typo, not through a silent wrong value"
)]

//! Offline mirror-coverage regression: the sim anchor's enumerated scalar
//! projection (ADR-039) and the serveable-versus-comparable word
//! classification, measured over REAL mainnet candidate read sets.
//!
//! # Provenance
//!
//! The read sets below are pinned constants taken from the node's
//! `prestateTracer` over the PARENT state of two frames of the checked-in
//! capture `degenbot-strategy/tests/fixtures/frame_replay_capture.jsonl`
//! (the same pre/post sets `frame_oracle.rs` compares fork truth against):
//!
//! - V3 frame `0xa2fe840bff879fee1393cfe553df2ba2bbd8ac5528cb2e00f53216f572c976ab`
//!   @ 26303798 — `SwapRouter02` `exactInput`, USDC → WETH → TKN (two canonical
//!   Uniswap V3 pools), 9 accounts / 25 storage slots.
//! - V2 frame `0xf2ca47766dfa8d9aeb280f19b6f8abc41e4479113229ad89311ef94c43e41bcf`
//!   @ 26305173 — `UniswapV2Router` `swapExactETHForTokens` (one canonical V2
//!   pair), 6 accounts / 10 storage slots.
//!
//! The pools are registered at their measured pre-state, snapshotted with the
//! production `SimAnchorState::snapshot`, and every measured key is classified
//! with the production projection (`probe_tracked_storage_slot`) plus the word
//! comparison a serve would have to pass (`TrackedSlotKind::tracked_bit_mask`).
//!
//! The measured overlap/classification table and its interpretation — in
//! particular why a comparable carried word is NOT serveable — live in
//! `docs/architecture/sim-anchor-mirror-coverage.md`. This test keeps that
//! table honest offline: no RPC, and no fixture bytes duplicated here (the
//! frame hashes are verified against the committed capture instead).

use std::path::PathBuf;
use std::str::FromStr;

use alloy::primitives::aliases::U112;
use alloy::primitives::{Address, B256, U256};
use degenbot_core::fee::fee_ratio;
use degenbot_pools::{ConcentratedLiquiditySlotLayout, PoolTickCoverage, RegisterV3PoolParams};
use degenbot_substrate::divergence_probe::TrackedSlotKind;
use degenbot_substrate::{BotState, RegisterV2PoolParams, SimAnchorState};
use degenbot_uniswap::dex_identity::DexVariant;

const V3_FRAME_TX: &str = "0xa2fe840bff879fee1393cfe553df2ba2bbd8ac5528cb2e00f53216f572c976ab";
const V2_FRAME_TX: &str = "0xf2ca47766dfa8d9aeb280f19b6f8abc41e4479113229ad89311ef94c43e41bcf";

fn capture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../degenbot-strategy/tests/fixtures/frame_replay_capture.jsonl")
}

fn addr(s: &str) -> Address {
    Address::from_str(s).expect("fixture address")
}

fn word(s: &str) -> B256 {
    let digits = s.trim_start_matches("0x");
    let padded = format!("{digits:0>64}");
    B256::from_str(&format!("0x{padded}")).expect("fixture word")
}

fn u256(s: &str) -> U256 {
    U256::from_str_radix(s.trim_start_matches("0x"), 16).expect("fixture word")
}

/// One measured storage read: the account, the slot, the chain's pre-word,
/// and the role (for the assertion messages).
struct Read {
    account: Address,
    slot: U256,
    chain_word: B256,
    role: &'static str,
}

fn read(account: &str, slot: u64, chain_word: &str, role: &'static str) -> Read {
    Read {
        account: addr(account),
        slot: U256::from(slot),
        chain_word: word(chain_word),
        role,
    }
}

fn read_key(account: &str, slot: &str, chain_word: &str, role: &'static str) -> Read {
    Read {
        account: addr(account),
        slot: u256(slot),
        chain_word: word(chain_word),
        role,
    }
}

const POOL_A: &str = "0x7261bb346ccc02911e4b07f933ccd69dc51ee3e1";
const POOL_B: &str = "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640";
const USDC: &str = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";
const TKN_V3: &str = "0xb6ee9668771a79be7967ee29a63d4184f8097143";
const WETH: &str = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";
const V2_PAIR: &str = "0xecfbe9b182f6477a93065c1c11271232147838e5";
const V3_FACTORY: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";

/// The V3 frame's measured reads (`prestateTracer` over the parent state).
fn v3_frame_reads() -> Vec<Read> {
    let mut reads = v3_pool_reads();
    reads.extend(v3_erc20_reads());
    reads
}

/// The V3 frame's pool-contract reads (the two registered pools).
fn v3_pool_reads() -> Vec<Read> {
    vec![
        read(
            POOL_A,
            0,
            "0x000166000200020000fe84b7000000000000000001feb124f14f5115032aa85d",
            "V3 slot0 (pool A)",
        ),
        read(
            POOL_A,
            2,
            "0x00000000000000000000000000000000033319d3dcf2ff5fa262e7f51a4408b9",
            "V3 feeGrowthGlobal1X128 (pool A)",
        ),
        read(
            POOL_A,
            3,
            "0x0000000000000000000000000000000100000000000000000000000000000001",
            "V3 protocolFees (pool A)",
        ),
        read(
            POOL_A,
            4,
            "0x000000000000000000000000000000000000000000000242c2117b790999cda2",
            "V3 liquidity (pool A)",
        ),
        read(
            POOL_A,
            8,
            "0x0100185e020000000001bd54c7b8d75cf70826d2f4fff12141f76a196ae43283",
            "V3 observations[0] (pool A)",
        ),
        read(
            POOL_A,
            9,
            "0x0100185e020000000001bd54c5d5d21f61c3fca2fdfff12148499a456ae42e3f",
            "V3 observations[1] (pool A)",
        ),
        read_key(
            POOL_A,
            "0x9c04773acff4c5c42718bd0120c72761f458e43068a3961eb935577d1ed4effb",
            "0x0080000000000000000000000000000000020000000000014000050000800040",
            "V3 keccak-keyed mapping word, pool A (tick/bitmap class)",
        ),
        read(
            POOL_B,
            0,
            "0x00014402d302d3016c0309df000000000000525c2a1fa8492714c6c29a1a5d23",
            "V3 slot0 (pool B)",
        ),
        read(
            POOL_B,
            1,
            "0x000000000000000000000000000000000000f1d89d4e59ad060257745b589099",
            "V3 feeGrowthGlobal0X128 (pool B)",
        ),
        read(
            POOL_B,
            3,
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            "V3 protocolFees (pool B, untouched word)",
        ),
        read(
            POOL_B,
            4,
            "0x0000000000000000000000000000000000000000000000001397852f0df535e0",
            "V3 liquidity (pool B)",
        ),
        read_key(
            POOL_B,
            "0x1988775e6c6f4b25d912ae5577f9b19952e8e16a59481301121feafa0d673092",
            "0xfffd7ffffffffeffffffffffffffffffffffffffffffffffffffffffffffffff",
            "V3 tickBitmap word, pool B (wordPos 77)",
        ),
    ]
}

/// The V3 frame's ERC-20/proxy reads (RPC-only by construction).
fn v3_erc20_reads() -> Vec<Read> {
    vec![
        read_key(
            USDC,
            "0x1f21a62c4538bacf2aabeca410f0fe63151869f172e03c0e00357ba26a341eff",
            "0x0",
            "ERC20 balance/allowance (USDC)",
        ),
        read_key(
            USDC,
            "0x2c0b6903b032df8ab8ecc11c2891098abe08d5fa977ce8aa077abc5e993086e6",
            "0x0",
            "ERC20 balance/allowance (USDC)",
        ),
        read_key(
            USDC,
            "0x4d0bd3dc7a4ccdaeb5f918c922b9e4fed6c369a569732c7aaa713da8c0edd687",
            "0x0",
            "ERC20 balance/allowance (USDC)",
        ),
        read_key(
            USDC,
            "0x10d6a54a4754c8869d6886b5f5d7fbfa5b4522237ea5c60d11bc4e7a1ff9390b",
            "0x0",
            "ERC20/proxy admin slot (USDC)",
        ),
        read_key(
            USDC,
            "0x7050c9e0f4ca769c69bd3a8ef740bc37934f8e2c036e5a723fd8ee048ed3f8c3",
            "0x0",
            "ERC20 proxy impl (USDC)",
        ),
        read_key(
            USDC,
            "0xde2897c64f0af56474cba6e3bb2d58b5c85a42ee6dff042546bc919c03512137",
            "0x0",
            "ERC20 proxy slot (USDC)",
        ),
        read(
            USDC,
            1,
            "0x0000000000000000000000004914f61d25e5c567143774b76edbf4d5109a8566",
            "ERC20 proxy slot 1 (USDC)",
        ),
        read(
            TKN_V3,
            4,
            "0x0000000000000000000000010e086439b8081012a186660c10a7b61218870e0e",
            "ERC20 slot 4 (TKN)",
        ),
        read_key(
            TKN_V3,
            "0x5952bc172b339247fe812609688b261193a55726139dd2eca6e1c8a81e1ed82a",
            "0x0",
            "ERC20 balance/allowance (TKN)",
        ),
        read_key(
            TKN_V3,
            "0xdf2e2e67f402b3b32918a4c6c009fcce607251739b9aa42e329bed392256a7b1",
            "0x0",
            "ERC20 balance/allowance (TKN)",
        ),
        read_key(
            WETH,
            "0x13bff509410c1e7605f09296dd4f8deb6c9d20159d45ee4d15b44562dabda838",
            "0x0",
            "ERC20 balance/allowance (WETH)",
        ),
        read_key(
            WETH,
            "0x390f6178407c9b8e95802b8659e6df8e34c1e3d4f8d6a49e6132bbcdd937b63a",
            "0x0",
            "ERC20 balance/allowance (WETH)",
        ),
        read_key(
            WETH,
            "0x5cfa01b9a733d8746bc28fdd3f8992b7dd3bb6d323fe96a760fbd1b87ca3a2bd",
            "0x0",
            "ERC20 allowance, zero (WETH)",
        ),
    ]
}

/// The V2 frame's measured reads.
fn v2_frame_reads() -> Vec<Read> {
    vec![
        read(
            V2_PAIR,
            6,
            "0x000000000000000000000000865377367054516e17014ccded1e7d814edc9ce4",
            "V2 token0",
        ),
        read(
            V2_PAIR,
            7,
            "0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
            "V2 token1",
        ),
        read(
            V2_PAIR,
            8,
            "0x6ae478a3000000000000b35b41683aae1733000000000625cd3cbbefbdc3c4ca",
            "V2 reserves|timestamp",
        ),
        read(
            V2_PAIR,
            9,
            "0x0000000000000000000000000000000137546d1be12361ebd15e937afa1b626a",
            "V2 price0CumulativeLast",
        ),
        read(
            V2_PAIR,
            10,
            "0x0000000000000000000000000069ab2a37bae24c245fb1f3cc9d7bf5fe1386f3",
            "V2 price1CumulativeLast",
        ),
        read(
            V2_PAIR,
            12,
            "0x0000000000000000000000000000000000000000000000000000000000000001",
            "V2 unlocked",
        ),
        read_key(
            "0x865377367054516e17014ccded1e7d814edc9ce4",
            "0x24c6b122c73b01089738c9a799e43c39ad2cb5554eb37007d0d2e96e80eeaf12",
            "0x0",
            "ERC20 balance/allowance (TKN_V2)",
        ),
        read_key(
            "0x865377367054516e17014ccded1e7d814edc9ce4",
            "0x61dd21b3c4e9c04a7a4dba9539e37ba9b4dd1ad53798119775af87406183aff5",
            "0x5",
            "ERC20 balance/allowance (TKN_V2)",
        ),
        read_key(
            WETH,
            "0xb81b493c3ab470e190a428e2f091a4a30fd6c1f08d8452c3a65cf35903d5fb4f",
            "0x0",
            "ERC20 balance/allowance (WETH)",
        ),
        read_key(
            WETH,
            "0xfb19a963956c9cb662dd3ae48988c4b90766df71ea130109840abe0a1b23dba8",
            "0x0",
            "ERC20 allowance, zero (WETH)",
        ),
    ]
}

/// The mirror as it exists at the V3 frame's block `26303798`: the two V3
/// pools registered at their measured pre-state (the slot0/liquidity words
/// are exactly what a hydration read produced).
fn anchor_at_v3_block() -> SimAnchorState {
    let mut core = BotState::new();
    for (pool, token0, token1, fee, spacing, sqrt_price, liquidity, tick) in [
        (
            POOL_A,
            TKN_V3,
            WETH,
            10_000u32,
            200i32,
            "0x1feb124f14f5115032aa85d",
            0x0242_c211_7b79_0999_cda2u128,
            -97_097i32,
        ),
        (
            POOL_B,
            USDC,
            WETH,
            500u32,
            10i32,
            "0x525c2a1fa8492714c6c29a1a5d23",
            0x1397_852f_0df5_35e0u128,
            199_135i32,
        ),
    ] {
        core.register_v3_pool(&RegisterV3PoolParams {
            address: addr(pool),
            token0: addr(token0),
            token1: addr(token1),
            fee,
            tick_spacing: spacing,
            factory: addr(V3_FACTORY),
            sqrt_price_x96: u256(sqrt_price),
            liquidity,
            tick,
            tick_data: hashbrown::HashMap::new(),
            update_block: 26_303_798,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            slot_layout: ConcentratedLiquiditySlotLayout::UniswapV3,
            ..Default::default()
        })
        .expect("V3 registration");
    }
    SimAnchorState::snapshot(&core)
}

/// The mirror as it exists at the V2 frame's block `26305173`: the V2 pair at
/// its measured pre-state.
fn anchor_at_v2_block() -> SimAnchorState {
    let mut core = BotState::new();
    // reserve0 (low 112) | reserve1 (bits 112..224) | blockTimestampLast (high 32)
    core.register_v2_pool(&RegisterV2PoolParams {
        address: addr(V2_PAIR),
        token0: addr("0x865377367054516e17014ccded1e7d814edc9ce4"),
        token1: addr(WETH),
        reserve0: U112::from(0x0625_cd3c_bbef_bdc3_c4cau128),
        reserve1: U112::from(0xb35b_4168_3aae_1733u128),
        fee_token0: fee_ratio(997, 1000),
        fee_token1: fee_ratio(997, 1000),
        factory: addr("0x5c69bee701ef814a2b6a3edd4b1652cb9cc5aa6f"),
        update_block: 26_305_173,
        variant: DexVariant::UniswapV2,
        stable_swap: false,
        deployer: Address::ZERO,
        init_hash: B256::ZERO,
    })
    .expect("V2 registration");
    SimAnchorState::snapshot(&core)
}

/// The bytes the anchor zeroes vs the chain word, split by the kind's
/// tracked-bit mask: the first component is a tracked-field divergence (a
/// real staleness signal), the second the untracked-bit divergence (why the
/// word is comparable but NOT serveable).
fn diff_words(engine_word: B256, chain_word: B256, kind: TrackedSlotKind) -> (U256, U256) {
    let mask = U256::from_be_bytes(kind.tracked_bit_mask().0);
    let engine = U256::from_be_bytes(engine_word.0);
    let chain = U256::from_be_bytes(chain_word.0);
    let xor = engine ^ chain;
    (xor & mask, xor & !mask)
}

struct FrameTally {
    total: usize,
    covered: usize,
    serveable: usize,
    rpc_only: usize,
}

fn classify_frame(anchor: &SimAnchorState, reads: &[Read]) -> FrameTally {
    let mut tally = FrameTally {
        total: reads.len(),
        covered: 0,
        serveable: 0,
        rpc_only: 0,
    };
    for r in reads {
        match anchor.probe_tracked_storage_slot(r.account, r.slot) {
            Some(probe) => {
                tally.covered += 1;
                if probe.engine_word == r.chain_word {
                    tally.serveable += 1;
                }
            }
            None => tally.rpc_only += 1,
        }
    }
    tally
}

#[test]
fn committed_capture_carries_both_measured_frames() {
    let capture = std::fs::read_to_string(capture_path())
        .expect("the committed frame_replay_capture fixture is present");
    assert!(
        capture.contains(V3_FRAME_TX),
        "the V3 frame {V3_FRAME_TX} is missing from the committed capture"
    );
    assert!(
        capture.contains(V2_FRAME_TX),
        "the V2 frame {V2_FRAME_TX} is missing from the committed capture"
    );
}

#[test]
fn v3_frame_anchor_projection_is_alive_and_matches_the_measured_table() {
    let anchor = anchor_at_v3_block();

    // A REGISTERED pool must project its scalar rows. An empty projection
    // over a registered pool means the registry's entry insert was compiled
    // away (the id binding and the entry insert are separate writes), so
    // every anchor-served read would silently fall through to RPC.
    let liquidity = anchor
        .probe_tracked_storage_slot(addr(POOL_A), U256::from(4u64))
        .expect(
            "the registered V3 pool has no scalar row: the anchor projection \
             is dead in this build",
        );
    assert_eq!(liquidity.kind, TrackedSlotKind::V3Liquidity);
    assert_eq!(liquidity.update_block, 26_303_798);

    let reads = v3_frame_reads();
    let tally = classify_frame(&anchor, &reads);
    assert_eq!(tally.total, 25, "the pinned V3 read set drifted");
    assert_eq!(
        tally.covered, 4,
        "V3 anchor coverage must match the measured table"
    );
    assert_eq!(
        tally.serveable, 2,
        "V3 serveable-word count must match the measured table"
    );
    assert_eq!(tally.rpc_only, 21);

    // Pool A slot0: tracked fields agree, but the untracked slot0 bits
    // (observationIndex / feeProtocol / unlocked) differ — comparable only.
    let a_slot0 = &reads[0];
    let probe = anchor
        .probe_tracked_storage_slot(a_slot0.account, a_slot0.slot)
        .expect("pool A slot0 must be projected");
    assert_eq!(probe.kind, TrackedSlotKind::V3Slot0);
    let (tracked_diff, untracked_diff) =
        diff_words(probe.engine_word, a_slot0.chain_word, probe.kind);
    assert_eq!(
        tracked_diff,
        U256::ZERO,
        "{}: the tracked slot0 fields must agree with the chain word",
        a_slot0.role
    );
    assert_ne!(probe.engine_word, a_slot0.chain_word);
    assert_eq!(
        untracked_diff,
        u256("0x0001660002000200000000000000000000000000000000000000000000000000"),
        "{}: the untracked-bit divergence is the whole gap",
        a_slot0.role
    );

    // Pool A slot4: the liquidity word serves verbatim.
    let a_slot4 = &reads[3];
    let probe = anchor
        .probe_tracked_storage_slot(a_slot4.account, a_slot4.slot)
        .expect("pool A slot4 must be projected");
    assert_eq!(probe.kind, TrackedSlotKind::V3Liquidity);
    assert_eq!(
        probe.engine_word, a_slot4.chain_word,
        "{}: the liquidity word is serveable",
        a_slot4.role
    );

    // Pool B slot0: comparable only, same shape as pool A.
    let b_slot0 = &reads[7];
    let probe = anchor
        .probe_tracked_storage_slot(b_slot0.account, b_slot0.slot)
        .expect("pool B slot0 must be projected");
    assert_eq!(probe.kind, TrackedSlotKind::V3Slot0);
    let (tracked_diff, untracked_diff) =
        diff_words(probe.engine_word, b_slot0.chain_word, probe.kind);
    assert_eq!(tracked_diff, U256::ZERO, "{}", b_slot0.role);
    assert_ne!(probe.engine_word, b_slot0.chain_word);
    assert_eq!(
        untracked_diff,
        u256("0x00014402d302d3016c0000000000000000000000000000000000000000000000"),
        "{}: the untracked-bit divergence is the whole gap",
        b_slot0.role
    );

    // Pool B slot4: serveable, like pool A.
    let b_slot4 = &reads[10];
    let probe = anchor
        .probe_tracked_storage_slot(b_slot4.account, b_slot4.slot)
        .expect("pool B slot4 must be projected");
    assert_eq!(probe.kind, TrackedSlotKind::V3Liquidity);
    assert_eq!(
        probe.engine_word, b_slot4.chain_word,
        "{}: the liquidity word is serveable",
        b_slot4.role
    );
}

#[test]
fn v2_frame_anchor_projection_is_alive_and_matches_the_measured_table() {
    let anchor = anchor_at_v2_block();

    // The V2 equivalent of the projection assertion: a registered pair must
    // project its reserves row, or every anchor-served read falls through.
    let reserves = anchor
        .probe_tracked_storage_slot(addr(V2_PAIR), U256::from(8u64))
        .expect(
            "the registered V2 pair has no scalar row: the anchor projection \
             is dead in this build",
        );
    assert_eq!(reserves.kind, TrackedSlotKind::V2Reserves);
    assert_eq!(reserves.update_block, 26_305_173);

    let reads = v2_frame_reads();
    let tally = classify_frame(&anchor, &reads);
    assert_eq!(tally.total, 10, "the pinned V2 read set drifted");
    assert_eq!(
        tally.covered, 1,
        "V2 anchor coverage must match the measured table"
    );
    assert_eq!(
        tally.serveable, 0,
        "V2 serves nothing verbatim: the reserves word carries a zeroed \
         blockTimestampLast the chain word does not"
    );
    assert_eq!(tally.rpc_only, 9);

    // Slot 8: the tracked reserves agree (tracked-bit diff zero) but the
    // untracked high-32 blockTimestampLast differs — comparable, never
    // serveable. A zeroed timestamp changes UniswapV2Pair._update's
    // elapsed-time math.
    let slot8 = &reads[2];
    let probe = anchor
        .probe_tracked_storage_slot(slot8.account, slot8.slot)
        .expect("the pair's slot 8 must be projected");
    assert_eq!(probe.kind, TrackedSlotKind::V2Reserves);
    let (tracked_diff, untracked_diff) =
        diff_words(probe.engine_word, slot8.chain_word, probe.kind);
    assert_eq!(
        tracked_diff,
        U256::ZERO,
        "{}: the tracked reserves must agree with the chain word",
        slot8.role
    );
    assert_ne!(probe.engine_word, slot8.chain_word);
    assert_eq!(
        untracked_diff,
        u256("0x6ae478a300000000000000000000000000000000000000000000000000000000"),
        "{}: the untracked-bit divergence is exactly the zeroed timestamp",
        slot8.role
    );
}
