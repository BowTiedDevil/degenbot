#![expect(
    clippy::cast_precision_loss,
    clippy::doc_markdown,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::too_many_lines,
    clippy::unusual_byte_groupings
)]

//! The BotState-mirror overlap probe: of a REAL mainnet candidate's cold SLOAD
//! set, how many keys does the sim anchor already carry — and is the word the
//! anchor carries *serveable* (byte-identical to the chain word) or only
//! *comparable* (the divergence probe's masked shape)?
//!
//! # Method
//!
//! The read sets are measured, not invented: each key below is a storage slot
//! a real chain-1 transaction touched, taken from the node's `prestateTracer`
//! over the transaction's parent state (the same pre/post sets
//! `tests/frame_oracle.rs` compares fork truth against). Two frames from the
//! checked-in capture (`degenbot-strategy/tests/fixtures/frame_replay_capture.jsonl`):
//!
//! - `0xa2fe840b…` @ 26303798 — SwapRouter02 `exactInput`, USDC → WETH → TKN
//!   (two canonical Uniswap V3 pools), 9 accounts / 25 storage slots.
//! - `0xf2ca4776…` @ 26305173 — UniswapV2Router `swapExactETHForTokens`
//!   (one canonical V2 pair), 6 accounts / 10 storage slots.
//!
//! The pools are registered in a real `BotState` at their measured pre-state
//! (values read from the same parent block), snapshotted with the production
//! `SimAnchorState::snapshot`, and every measured key is then classified with
//! the production projection (`probe_tracked_storage_slot`) plus the word
//! comparison the sim would have to make to SERVE it
//! (`TrackedSlotKind::tracked_bit_mask`).
//!
//! # Why the word comparison is the point
//!
//! The anchor's word is a *comparison* shape: tracked fields packed in the
//! on-chain layout, untracked bits ZEROED (`degenbot_substrate::divergence_probe`
//! module docs). A serve has to hand the EVM the FULL chain word — a zeroed
//! `blockTimestampLast` changes `UniswapV2Pair._update`'s elapsed-time math, a
//! zeroed V3 `slot0` `feeProtocol` changes protocol-fee math and a zeroed
//! `unlocked` trips `nonReentrant`'s `require(unlocked == 1)` (`LOK`). This
//! probe measures, per covered key, whether `engine_word == chain_word`.
//!
//! Usage:
//!
//! ```sh
//! cargo run --release -p degenbot-simulation --example mirror_coverage_probe
//! ```

use std::str::FromStr;

use alloy::primitives::aliases::U112;
use alloy::primitives::{Address, B256, U256};
use degenbot_pools::{ConcentratedLiquiditySlotLayout, PoolTickCoverage, RegisterV3PoolParams};
use degenbot_substrate::divergence_probe::TrackedSlotKind;
use degenbot_substrate::{BotState, RegisterV2PoolParams, SimAnchorState};
use degenbot_uniswap::dex_identity::DexVariant;

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

/// One measured storage read: the account, the slot, the chain's pre-word, and
/// the role (for the report line).
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

/// The V3 frame's measured reads (`prestateTracer` over the parent state).
fn v3_frame_reads() -> Vec<Read> {
    let a = "0x7261bb346ccc02911e4b07f933ccd69dc51ee3e1";
    let b = "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640";
    let usdc = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";
    let tkn = "0xb6ee9668771a79be7967ee29a63d4184f8097143";
    let weth = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";
    vec![
        read(
            a,
            0,
            "0x000166000200020000fe84b7000000000000000001feb124f14f5115032aa85d",
            "V3 slot0",
        ),
        read(
            a,
            2,
            "0x00000000000000000000000000000000033319d3dcf2ff5fa262e7f51a4408b9",
            "V3 feeGrowthGlobal1X128",
        ),
        read(
            a,
            3,
            "0x0000000000000000000000000000000100000000000000000000000000000001",
            "V3 protocolFees",
        ),
        read(
            a,
            4,
            "0x000000000000000000000000000000000000000000000242c2117b790999cda2",
            "V3 liquidity",
        ),
        read(
            a,
            8,
            "0x0100185e020000000001bd54c7b8d75cf70826d2f4fff12141f76a196ae43283",
            "V3 observations[0]",
        ),
        read(
            a,
            9,
            "0x0100185e020000000001bd54c5d5d21f61c3fca2fdfff12148499a456ae42e3f",
            "V3 observations[1]",
        ),
        read_key(
            a,
            "0x9c04773acff4c5c42718bd0120c72761f458e43068a3961eb935577d1ed4effb",
            "0x0080000000000000000000000000000000020000000000014000050000800040",
            "V3 keccak-keyed mapping word (tick/bitmap class)",
        ),
        read(
            b,
            0,
            "0x00014402d302d3016c0309df000000000000525c2a1fa8492714c6c29a1a5d23",
            "V3 slot0",
        ),
        read(
            b,
            1,
            "0x000000000000000000000000000000000000f1d89d4e59ad060257745b589099",
            "V3 feeGrowthGlobal0X128",
        ),
        read(
            b,
            3,
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            "V3 protocolFees (untouched word)",
        ),
        read(
            b,
            4,
            "0x0000000000000000000000000000000000000000000000001397852f0df535e0",
            "V3 liquidity",
        ),
        read_key(
            b,
            "0x1988775e6c6f4b25d912ae5577f9b19952e8e16a59481301121feafa0d673092",
            "0xfffd7ffffffffeffffffffffffffffffffffffffffffffffffffffffffffffff",
            "V3 tickBitmap word (wordPos 77)",
        ),
        read_key(
            usdc,
            "0x1f21a62c4538bacf2aabeca410f0fe63151869f172e03c0e00357ba26a341eff",
            "0x0",
            "ERC20 balance/allowance",
        ),
        read_key(
            usdc,
            "0x2c0b6903b032df8ab8ecc11c2891098abe08d5fa977ce8aa077abc5e993086e6",
            "0x0",
            "ERC20 balance/allowance",
        ),
        read_key(
            usdc,
            "0x4d0bd3dc7a4ccdaeb5f918c922b9e4fed6c369a569732c7aaa713da8c0edd687",
            "0x0",
            "ERC20 balance/allowance",
        ),
        read_key(
            usdc,
            "0x10d6a54a4754c8869d6886b5f5d7fbfa5b4522237ea5c60d11bc4e7a1ff9390b",
            "0x0",
            "ERC20/proxy admin slot",
        ),
        read_key(
            usdc,
            "0x7050c9e0f4ca769c69bd3a8ef740bc37934f8e2c036e5a723fd8ee048ed3f8c3",
            "0x0",
            "ERC20 (proxy impl)",
        ),
        read_key(
            usdc,
            "0xde2897c64f0af56474cba6e3bb2d58b5c85a42ee6dff042546bc919c03512137",
            "0x0",
            "ERC20 (proxy slot)",
        ),
        read(
            usdc,
            1,
            "0x0000000000000000000000004914f61d25e5c567143774b76edbf4d5109a8566",
            "ERC20 (proxy slot 1)",
        ),
        read(
            tkn,
            4,
            "0x0000000000000000000000010e086439b8081012a186660c10a7b61218870e0e",
            "ERC20 slot 4",
        ),
        read_key(
            tkn,
            "0x5952bc172b339247fe812609688b261193a55726139dd2eca6e1c8a81e1ed82a",
            "0x0",
            "ERC20 balance/allowance",
        ),
        read_key(
            tkn,
            "0xdf2e2e67f402b3b32918a4c6c009fcce607251739b9aa42e329bed392256a7b1",
            "0x0",
            "ERC20 balance/allowance",
        ),
        read_key(
            weth,
            "0x13bff509410c1e7605f09296dd4f8deb6c9d20159d45ee4d15b44562dabda838",
            "0x0",
            "ERC20 balance/allowance",
        ),
        read_key(
            weth,
            "0x390f6178407c9b8e95802b8659e6df8e34c1e3d4f8d6a49e6132bbcdd937b63a",
            "0x0",
            "ERC20 balance/allowance",
        ),
        read_key(
            weth,
            "0x5cfa01b9a733d8746bc28fdd3f8992b7dd3bb6d323fe96a760fbd1b87ca3a2bd",
            "0x0",
            "ERC20 allowance (zero)",
        ),
    ]
}

/// The V2 frame's measured reads.
fn v2_frame_reads() -> Vec<Read> {
    let pair = "0xecfbe9b182f6477a93065c1c11271232147838e5";
    let tkn = "0x865377367054516e17014ccded1e7d814edc9ce4";
    let weth = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";
    vec![
        read(
            pair,
            6,
            "0x000000000000000000000000865377367054516e17014ccded1e7d814edc9ce4",
            "V2 token0",
        ),
        read(
            pair,
            7,
            "0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
            "V2 token1",
        ),
        read(
            pair,
            8,
            "0x6ae478a3000000000000b35b41683aae1733000000000625cd3cbbefbdc3c4ca",
            "V2 reserves|timestamp",
        ),
        read(
            pair,
            9,
            "0x0000000000000000000000000000000137546d1be12361ebd15e937afa1b626a",
            "V2 price0CumulativeLast",
        ),
        read(
            pair,
            10,
            "0x0000000000000000000000000069ab2a37bae24c245fb1f3cc9d7bf5fe1386f3",
            "V2 price1CumulativeLast",
        ),
        read(
            pair,
            12,
            "0x0000000000000000000000000000000000000000000000000000000000000001",
            "V2 unlocked",
        ),
        read_key(
            tkn,
            "0x24c6b122c73b01089738c9a799e43c39ad2cb5554eb37007d0d2e96e80eeaf12",
            "0x0",
            "ERC20 balance/allowance",
        ),
        read_key(
            tkn,
            "0x61dd21b3c4e9c04a7a4dba9539e37ba9b4dd1ad53798119775af87406183aff5",
            "0x5",
            "ERC20 balance/allowance",
        ),
        read_key(
            weth,
            "0xb81b493c3ab470e190a428e2f091a4a30fd6c1f08d8452c3a65cf35903d5fb4f",
            "0x0",
            "ERC20 balance/allowance",
        ),
        read_key(
            weth,
            "0xfb19a963956c9cb662dd3ae48988c4b90766df71ea130109840abe0a1b23dba8",
            "0x0",
            "ERC20 allowance (zero)",
        ),
    ]
}

const WETH: &str = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";
const USDC: &str = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";
const V3_FACTORY: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";

/// The mirror as it exists at `26303798`: the two V3 pools registered at their
/// measured pre-state (slot0/liquidity are the words the prestate tracer
/// reported; that is exactly what a hydration read would have produced).
#[expect(clippy::expect_used)]
fn anchor_at_v3_block() -> SimAnchorState {
    let mut core = BotState::new();
    for (pool, token0, token1, fee, spacing, sqrt_price, liquidity, tick) in [
        (
            "0x7261bb346ccc02911e4b07f933ccd69dc51ee3e1",
            "0xb6ee9668771a79be7967ee29a63d4184f8097143",
            WETH,
            10_000u32,
            200i32,
            "0x1feb124f14f5115032aa85d",
            0x242c_2117b790_999cda2u128,
            -97_097i32,
        ),
        (
            "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640",
            USDC,
            WETH,
            500u32,
            10i32,
            "0x525c2a1fa8492714c6c29a1a5d23",
            0x1397_852f0df5_35e0u128,
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
    let probe = core.probe_tracked_storage_slot(
        addr("0x7261bb346ccc02911e4b07f933ccd69dc51ee3e1"),
        U256::from(4u64),
    );
    let anchor = SimAnchorState::snapshot(&core);
    // Live invariant: a REGISTERED pool must project its scalar rows. An empty
    // projection over a registered pool means the registry's entry table is not
    // holding the entry (the id binding and the entry insert are separate
    // writes; a build where the entry insert is compiled away keeps the binding
    // only), so every anchor-served read would silently fall through.
    assert!(
        probe.is_some() && anchor
            .probe_tracked_storage_slot(
                addr("0x7261bb346ccc02911e4b07f933ccd69dc51ee3e1"),
                U256::from(4u64),
            )
            .is_some(),
        "the registered V3 pool {}{} has no scalar row: the anchor projection is dead in this build \
         (debug_assertions={})",
        "0x7261bb346ccc02911e4b07f933ccd69dc51ee3e1",
        "",
        cfg!(debug_assertions),
    );
    anchor
}

/// The mirror as it exists at `26305173`: the V2 pair at its measured pre-state.
fn anchor_at_v2_block() -> SimAnchorState {
    let mut core = BotState::new();
    // reserve0 (low 112) | reserve1 (bits 112..224) | blockTimestampLast (high 32)
    core.register_v2_pool(&RegisterV2PoolParams {
        address: addr("0xecfbe9b182f6477a93065c1c11271232147838e5"),
        token0: addr("0x865377367054516e17014ccded1e7d814edc9ce4"),
        token1: addr(WETH),
        reserve0: U112::from(0x625c_d3cbbefb_dc3c4cau128),
        reserve1: U112::from(0xb35b_41683aae_1733u128),
        fee_token0: (997, 1000),
        fee_token1: (997, 1000),
        factory: addr("0x5c69bee701ef814a2b6a3edd4b1652cb9cc5aa6f"),
        update_block: 26_305_173,
        variant: DexVariant::UniswapV2,
        stable_swap: false,
        fee_denominator: None,
        ..Default::default()
    })
    .expect("V2 registration");
    SimAnchorState::snapshot(&core)
}

/// The bytes of the word outside `mask` (the bits the anchor zeroes).
fn untracked_bits(engine_word: B256, chain_word: B256, kind: TrackedSlotKind) -> (U256, U256) {
    let mask = U256::from_be_bytes(kind.tracked_bit_mask().0);
    let engine = U256::from_be_bytes(engine_word.0);
    let chain = U256::from_be_bytes(chain_word.0);
    let mut outside = U256::ZERO;
    for bit in 0..256usize {
        if mask.bit(bit) {
            continue;
        }
        if (engine ^ chain).bit(bit) {
            outside |= U256::from(1u8) << bit;
        }
    }
    ((engine ^ chain) & mask, outside)
}

struct Tally {
    covered: usize,
    total: usize,
    serveable: usize,
}

fn classify(label: &str, anchor: &SimAnchorState, reads: &[Read], out: &mut Tally) {
    println!("\n[{label}] {} storage reads", reads.len());
    for r in reads {
        match anchor.probe_tracked_storage_slot(r.account, r.slot) {
            Some(probe) => {
                let (tracked_diff, untracked_diff) =
                    untracked_bits(probe.engine_word, r.chain_word, probe.kind);
                let serveable = probe.engine_word == r.chain_word;
                out.covered += 1;
                if serveable {
                    out.serveable += 1;
                }
                println!(
                    "  {:<44} slot {:<4} ANCHOR {:<14} engine==chain={:<5} \
                     tracked-bit diff=0x{tracked_diff:064x} untracked-bit diff=0x{untracked_diff:064x} \
                     update_block={}",
                    r.role,
                    format!("{:#x}", r.slot),
                    format!("{:?}", probe.kind),
                    serveable,
                    probe.update_block,
                );
            }
            None => println!(
                "  {:<44} slot {:<4} RPC-ONLY",
                r.role,
                format!("{:#x}", r.slot)
            ),
        }
        out.total += 1;
    }
}

fn main() {
    println!(
        "mirror_coverage_probe: the sim anchor vs REAL mainnet candidate read sets \
         (debug_assertions={})",
        cfg!(debug_assertions)
    );
    println!(
        "  (read sets: reth `prestateTracer` over the parent state of two checked-in \
         capture frames)"
    );

    let mut v3 = Tally {
        covered: 0,
        total: 0,
        serveable: 0,
    };
    let v3_anchor = anchor_at_v3_block();
    println!(
        "\nV3 frame 0xa2fe840b… @ 26303798 — SwapRouter02 exactInput USDC -> WETH -> TKN \
         (9 accounts: 6 contracts + 3 EOAs)"
    );
    classify("V3 frame storage", &v3_anchor, &v3_frame_reads(), &mut v3);

    let mut v2 = Tally {
        covered: 0,
        total: 0,
        serveable: 0,
    };
    let v2_anchor = anchor_at_v2_block();
    println!(
        "\nV2 frame 0xf2ca4776… @ 26305173 — UniswapV2Router swapExactETHForTokens \
         (6 accounts: 3 contracts + 3 EOAs)"
    );
    classify("V2 frame storage", &v2_anchor, &v2_frame_reads(), &mut v2);

    println!("\n== tally (storage slots only; every ERC20/EOA row is RPC-only by construction)");
    for (label, t) in [("V3 frame", &v3), ("V2 frame", &v2)] {
        println!(
            "  {label}: {}/{} anchor-covered ({:.1}%); {} of those serve the exact chain word ({:.1}% of all reads)",
            t.covered,
            t.total,
            100.0 * t.covered as f64 / t.total as f64,
            t.serveable,
            100.0 * t.serveable as f64 / t.total as f64,
        );
    }
    let covered = v3.covered + v2.covered;
    let total = v3.total + v2.total;
    let serveable = v3.serveable + v2.serveable;
    println!(
        "  BOTH: {covered}/{total} covered ({:.1}%), {serveable}/{total} served verbatim ({:.1}%)",
        100.0 * covered as f64 / total as f64,
        100.0 * serveable as f64 / total as f64,
    );
    println!(
        "  account rows: 15 (9 + 6) — the mirror carries no balances/nonces, WarmCodeCache carries \
         the contract rows cross-block"
    );
}
