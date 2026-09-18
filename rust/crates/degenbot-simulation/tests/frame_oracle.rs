//! Fork-fixture oracle: replay fidelity of the frame-replay seam +
//! journal-pool extraction against chain truth, per live router archetype.
//!
//! Engine-level claims (code-only, no RPC):
//! - The oracle comparator is divergence-probe shaped: the engine's typed
//!   field is packed into the on-chain word layout (untracked bits zeroed)
//!   and the chain truth is masked to the tracked-bit range, so untracked
//!   bits (`blockTimestampLast`, `feeProtocol`/`observationIndex`/`unlocked`)
//!   can never flag while any tracked-field disagreement always does.
//! - The archived capture loader classifies frames to router archetypes and
//!   dedupes (the capture is best-effort by contract; a `0x0x`-prefixed hex
//!   hash parses).
//!
//! Live claims (ignored by default, network-gated, the same tier as the
//! sibling suites): for one frame per live router archetype — a captured
//! fixture resolved on-chain when its landing tx is retrievable, else a clean
//! frame from a live-scan window — the replayed frame's EXTRACTED post-states
//! match fork truth exactly on every tracked field: V2 `reserves` (slot 8),
//! V3 `slot0` (sqrtPriceX96 + tick) + `liquidity`, and every recovered
//! per-tick word. Truth is the node's per-tx state diff (`prestateTracer`
//! diffMode) at the block the tx actually ran in, with the parent-block
//! storage word as the fallback for journal-surfaced unchanged slots — the
//! discovery + discipline shape of the sibling suites, not a new one.
//!
//! Archetypes (one fixture each): direct V2 router (Router02), direct V3
//! router (an exactInput/exactOutput call on the v1 SwapRouter deployment or
//! its SwapRouter02 successor — measured on the live node: the v1 deployment
//! itself draws ZERO direct calls for hours on end, so the direct-V3 router
//! form is only observable through the shared selector surface), SwapRouter02
//! (selector-agnostic), UniversalRouter multi-hop, 1inch v5 aggregation
//! router, CoW batch settle (multi-pool touch — the FULL descriptor set is
//! asserted recovered, not just the first pool). The archived capture's
//! aggregation frame rode the 1inch v6 router; both deployments journal the
//! same pool shapes, so the archetype's window accepts either and the
//! capture frame rides it.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use alloy::consensus::Transaction as ConsensusTx;
use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::aliases::U112;
use alloy::primitives::{address, keccak256, Address, Bytes, B256, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use degenbot_executor::WarmupSlots;
use degenbot_pools::{slot_layout, ClSlotLayout};
use degenbot_simulation::sim::evm::frame_replay::{ReplayStatus, ReplayableTx};
use degenbot_simulation::sim::evm::journal_pools::{
    extract_pool_post_states, PoolFamily, PoolPostKind, PoolPostState, TouchedTickWord,
    TypedPoolPost,
};
use degenbot_simulation::{BlockSimHandle, SimulationOverrideParams, WarmCodeCacheInner};
use hashbrown::HashMap;
use serde_json::Value;

// ─────────────────────────────────────────────────────────────────────────
// Offline oracle: archetypes, capture loader, masked-word comparator
// ─────────────────────────────────────────────────────────────────────────

/// The Uniswap V2 Router 02.
const V2_ROUTER_02: Address = address!("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
/// The Uniswap SwapRouter (V3-first router, v1 of the 02 line).
const SWAP_ROUTER_V1: Address = address!("0xE592427A0AEce92De3Edee1F18E0157C05861564");
/// The Uniswap `SwapRouter02`.
const SWAP_ROUTER_02: Address = address!("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");
/// The Uniswap UniversalRouter (both mainnet deployments).
const UNIVERSAL_ROUTER: Address = address!("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD");
const UNIVERSAL_ROUTER_2: Address = address!("0x66a9893cc07d91d95644aedd05d03f95e1dba8af");
/// 1inch aggregation routers (v5, plus the v6 the archived capture rode).
const ONE_INCH_V5: Address = address!("0x1111111254EEB25477B68fb85Ed929f73A960582");
const ONE_INCH_V6: Address = address!("0x111111125421cA6DC452D289314280a0f8842A65");
/// CoW Protocol's GPv2 Settlement (batch settles fan out over many pools).
const GPV2_SETTLEMENT: Address = address!("0x9008D19f58AAbD9eD0D60971565AA8510560ab41");

/// The live router archetypes a fixture frame is classified into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Archetype {
    DirectV2Router,
    DirectV3Router,
    SwapRouter02,
    UniversalRouterMultiHop,
    OneInchV5,
    CoWBatchSettle,
}

impl Archetype {
    /// The `to` addresses that classify a frame into this archetype.
    fn router_targets(self) -> &'static [Address] {
        match self {
            Self::DirectV2Router => &[V2_ROUTER_02],
            // Both V3-router deployments carry the direct-call surface; the
            // selector gate in `archetype_of` discriminates the DIRECT form.
            Self::DirectV3Router => &[SWAP_ROUTER_V1, SWAP_ROUTER_02],
            Self::SwapRouter02 => &[SWAP_ROUTER_02],
            Self::UniversalRouterMultiHop => &[UNIVERSAL_ROUTER, UNIVERSAL_ROUTER_2],
            Self::OneInchV5 => &[ONE_INCH_V5, ONE_INCH_V6],
            Self::CoWBatchSettle => &[GPV2_SETTLEMENT],
        }
    }

    /// How many tracked pools a fixture frame must write-touch (CoW batch
    /// settles are multi-pool by construction; the others need one).
    fn min_tracked_pools(self) -> usize {
        match self {
            Self::CoWBatchSettle => 2,
            _ => 1,
        }
    }

    /// The live-scan window depth the archetype's clean-fixture rate
    /// justifies: measured on the node, a direct V1-router V3 call and a
    /// multi-pool CoW settle are rare frames (V1 direct calls trade at a
    /// couple per hundred blocks; multi-pool settles are ~2% of settles) —
    /// those two scan deeper than the dense router stream.
    fn scan_depth(self) -> u64 {
        match self {
            Self::DirectV3Router | Self::CoWBatchSettle => 2_000,
            _ => 300,
        }
    }

    /// The trace-attempt cap per window (bounded fixture search when a
    /// window won't yield a clean frame; the sparse archetypes are allowed
    /// more attempts than the dense ones need).
    fn max_tried(self) -> usize {
        match self {
            Self::DirectV3Router => 128,
            Self::CoWBatchSettle => 192,
            _ => 64,
        }
    }

    /// The env-pinnable single-block window overrides the live scan
    /// (`DEGENBOT_ORACLE_<suffix>_BLOCK`).
    fn pin_env_suffix(self) -> &'static str {
        match self {
            Self::DirectV2Router => "DIRECT_V2_ROUTER",
            Self::DirectV3Router => "DIRECT_V3_ROUTER",
            Self::SwapRouter02 => "SWAP_ROUTER_02",
            Self::UniversalRouterMultiHop => "UNIVERSAL_ROUTER",
            Self::OneInchV5 => "ONE_INCH_V5",
            Self::CoWBatchSettle => "COW_BATCH_SETTLE",
        }
    }
}

/// The direct V3 router calls: identical selector surface on the v1 and 02
/// deployments (`exactInputSingle`/`exactInput`/`exactOutputSingle`/
/// `exactOutput`).
const DIRECT_V3_SELECTORS: [[u8; 4]; 4] = [
    [0x41, 0x4b, 0xf3, 0x89], // exactInputSingle
    [0xc0, 0x4b, 0x8d, 0x59], // exactInput
    [0xdb, 0x3e, 0x21, 0x98], // exactOutputSingle
    [0xf2, 0x8c, 0x04, 0x91], // exactOutput
];

/// A frame (`to` + calldata head) belongs to `arch`: the router target plus
/// the archetype's selector discrimination (only the direct V3 router form
/// is selector-gated).
fn classifies_as(arch: Archetype, to: Address, input: &[u8]) -> bool {
    arch.router_targets().contains(&to)
        && match arch {
            Archetype::DirectV3Router => input
                .get(0..4)
                .is_some_and(|sel| DIRECT_V3_SELECTORS.iter().any(|s| sel == *s)),
            _ => true,
        }
}

/// The archetype a frame's `to` + calldata classify into (`None` = untracked;
/// the direct V3 router form additionally discriminates its selector).
fn archetype_of(to: Address, input: &[u8]) -> Option<Archetype> {
    const ARCHETYPES: [Archetype; 6] = [
        Archetype::DirectV2Router,
        Archetype::DirectV3Router,
        Archetype::SwapRouter02,
        Archetype::UniversalRouterMultiHop,
        Archetype::OneInchV5,
        Archetype::CoWBatchSettle,
    ];
    ARCHETYPES
        .into_iter()
        .find(|arch| classifies_as(*arch, to, input))
}

/// The archived capture loader's tolerance surface, narrowed to the fields
/// the oracle consumes (the loader contract is best-effort: skip malformed
/// lines, keep the rest).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CapturedFrame {
    hash: B256,
    from: Address,
    to: Address,
    nonce: u64,
    gas: u64,
}

/// The checked-in capture (the live soak's archived frames). The env
/// override exists for a re-capture; the default resolves the sibling
/// crate fixture the capture pipeline owns.
fn capture_path() -> PathBuf {
    if let Ok(path) = std::env::var("DEGENBOT_FRAME_ORACLE_CAPTURE") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../degenbot-submission/tests/fixtures/frame_replay_capture.jsonl")
}

/// One captured frame with its archetype (archetype, frame), deduped by hash.
#[derive(Debug)]
struct ClassifiedFrame {
    archetype: Archetype,
    frame: CapturedFrame,
}

/// Load + classify the capture, deduping by hash. Malformed lines are
/// skipped and unknown `to` targets contribute nothing.
fn load_capture_frames(path: &PathBuf) -> Vec<ClassifiedFrame> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<ClassifiedFrame> = Vec::new();
    for line in text.lines() {
        let Some(frame) = parse_captured_frame(line) else {
            continue;
        };
        // The capture loader stores no calldata, so the archived frames can
        // only ride selector-agnostic archetypes — their `to` targets
        // (CoW/1inch) are all outside the selector-gated one anyway.
        let Some(archetype) = archetype_of(frame.to, &[]) else {
            continue;
        };
        if out.iter().any(|c| c.frame.hash == frame.hash) {
            continue;
        }
        out.push(ClassifiedFrame { archetype, frame });
    }
    out
}

/// A capture line → frame. The capture writes `hash` as `0x0x…` (a double
/// prefix) — stripping every leading `0x` then re-parsing is the exact
/// tolerance the frame pipeline's fixture loader applies.
fn parse_captured_frame(line: &str) -> Option<CapturedFrame> {
    let obj = serde_json::from_str::<Value>(line)
        .ok()?
        .as_object()?
        .to_owned();
    let sget = |k: &str| obj.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let uget = |k: &str| obj.get(k).and_then(Value::as_u64);
    let addr = |k: &str| sget(k).and_then(|s| s.parse().ok());
    let hash = sget("hash")?.trim_start_matches("0x");
    Some(CapturedFrame {
        hash: format!("0x{hash}").parse().ok()?,
        from: addr("from")?,
        to: addr("to")?,
        nonce: uget("nonce")?,
        gas: uget("gas")?,
    })
}

/// One tracked-slot disagreement with fork truth — the oracle's flag unit,
/// the same field set the sim's divergence probe logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SlotDivergence {
    pool: Address,
    slot: U256,
    kind: &'static str,
    /// The engine's packed word (tracked fields, untracked bits zeroed).
    engine_word: B256,
    /// The fork truth masked to the tracked-bit range.
    truth_masked: B256,
}

impl SlotDivergence {
    fn missing_typed_post(pool: Address, slot: U256) -> Self {
        Self {
            pool,
            slot,
            kind: "missing_typed_post",
            engine_word: B256::ZERO,
            truth_masked: B256::ZERO,
        }
    }
}

impl fmt::Display for SlotDivergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[sim-divergence] pool={:?} slot=0x{:x} kind={} engine=0x{} truth=0x{}",
            self.pool,
            self.slot,
            self.kind,
            alloy::hex::encode(self.engine_word),
            alloy::hex::encode(self.truth_masked)
        )
    }
}

/// The divergence-probe comparison: `true` iff the engine's packed word
/// (tracked fields only, untracked bits zeroed) equals the fork truth
/// masked to the tracked-bit range.
fn tracked_word_matches(mask: B256, engine: B256, truth: U256) -> bool {
    let mask_u = U256::from_be_bytes(mask.0);
    engine == B256::from((truth & mask_u).to_be_bytes::<32>())
}

/// Record a divergence when the engine word does not match the masked truth.
fn require_masked_word(
    pool: Address,
    slot: U256,
    kind: &'static str,
    mask: B256,
    engine: B256,
    truth: U256,
    divergences: &mut Vec<SlotDivergence>,
) {
    if !tracked_word_matches(mask, engine, truth) {
        divergences.push(SlotDivergence {
            pool,
            slot,
            kind,
            engine_word: engine,
            truth_masked: B256::from((truth & U256::from_be_bytes(mask.0)).to_be_bytes::<32>()),
        });
    }
}

/// V2 pair parity: the extracted `reserves` word vs the fork truth at the
/// reserves slot (masked to the tracked reserves bits). A tracked pool with
/// NO typed post-state is itself a divergence — the extraction must recover
/// every tracked pool.
fn v2_parity(
    pool: Address,
    state: Option<&PoolPostState>,
    truth_reserves: U256,
    divergences: &mut Vec<SlotDivergence>,
) {
    let slot8 = U256::from(slot_layout::V2_RESERVES_SLOT);
    let Some(PoolPostState {
        kind: PoolPostKind::Typed(TypedPoolPost::V2 { reserves }),
        ..
    }) = state
    else {
        divergences.push(SlotDivergence::missing_typed_post(pool, slot8));
        return;
    };
    let engine = slot_layout::pack_v2_reserves_word(reserves.reserve0, reserves.reserve1);
    require_masked_word(
        pool,
        slot8,
        "v2_reserves",
        slot_layout::V2_RESERVES_TRACKED_MASK,
        engine,
        truth_reserves,
        divergences,
    );
}

/// V3 scalar parity (slot0 + liquidity). `slot0_changed`/`liquidity_changed`
/// (from the node's diff) turn a frame-only read into the "extraction must
/// surface it" obligation: a dropped surface is a divergence, never an
/// excuse.
fn v3_scalar_parity(
    pool: Address,
    state: Option<&PoolPostState>,
    slot0_changed: bool,
    liquidity_changed: bool,
    truth_slot0: U256,
    truth_liquidity: U256,
    divergences: &mut Vec<SlotDivergence>,
) {
    let liquidity_slot = U256::from(ClSlotLayout::UniswapV3.liquidity_slot());
    let Some(PoolPostState {
        kind:
            PoolPostKind::Typed(TypedPoolPost::V3 {
                sqrt_price_x96,
                tick,
                liquidity,
                ..
            }),
        ..
    }) = state
    else {
        divergences.push(SlotDivergence::missing_typed_post(pool, U256::ZERO));
        divergences.push(SlotDivergence::missing_typed_post(pool, liquidity_slot));
        return;
    };
    if slot0_changed && sqrt_price_x96.is_none() && tick.is_none() {
        divergences.push(SlotDivergence {
            pool,
            slot: U256::ZERO,
            kind: "missing_v3_slot0",
            engine_word: B256::ZERO,
            truth_masked: B256::ZERO,
        });
    } else if let (Some(sqrt), Some(tk)) = (*sqrt_price_x96, *tick) {
        let engine = slot_layout::cl_slot0_tracked_word(sqrt, tk);
        require_masked_word(
            pool,
            U256::ZERO,
            "v3_slot0",
            slot_layout::CL_SLOT0_TRACKED_MASK,
            engine,
            truth_slot0,
            divergences,
        );
    }
    if liquidity_changed && liquidity.is_none() {
        divergences.push(SlotDivergence {
            pool,
            slot: liquidity_slot,
            kind: "missing_v3_liquidity",
            engine_word: B256::ZERO,
            truth_masked: B256::ZERO,
        });
    } else if let Some(liq) = *liquidity {
        let engine = slot_layout::cl_liquidity_tracked_word(liq);
        require_masked_word(
            pool,
            liquidity_slot,
            "v3_liquidity",
            slot_layout::CL_LIQUIDITY_TRACKED_MASK,
            engine,
            truth_liquidity,
            divergences,
        );
    }
}

/// V3 per-tick parity: a recovered tick word must equal the fork truth at
/// its `ticks(tick)` slot — the full 256-bit word is tracked
/// (`liquidityGross | liquidityNet`). `truth_words` runs parallel to the
/// recovered ticks.
fn v3_tick_parity(
    pool: Address,
    layout: ClSlotLayout,
    touched: &[TouchedTickWord],
    truth_words: &[U256],
    divergences: &mut Vec<SlotDivergence>,
) {
    assert_eq!(touched.len(), truth_words.len(), "parallel truth words");
    for (tt, truth) in touched.iter().zip(truth_words) {
        let slot = slot_layout::tick_mapping_slot_at_base(
            tt.tick,
            U256::from(layout.ticks_mapping_slot()),
        );
        let engine = slot_layout::tick_word_of(tt.liquidity_gross, tt.liquidity_net);
        require_masked_word(
            pool,
            slot,
            "v3_tick_word",
            slot_layout::CL_TICK_WORD_MASK,
            engine,
            *truth,
            divergences,
        );
    }
}

// ── code-only claims ──

/// The oracle flags any tracked-field disagreement and never flags an
/// untracked bit: garbage in the V2 timestamp bits, the V3
/// `observationIndex`/`feeProtocol`/`unlocked` bits, or the liquidity word's
/// high half is masked out; a one-bit change inside a tracked field flags;
/// a tick word is tracked full-width; a tracked pool with no typed post
/// flags as a missing recovery.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "every branch is a distinct mask/flag cell of the comparator's truth table; \
              splitting it would bury the table"
)]
fn oracle_masks_untracked_bits_and_flags_tracked_divergence() {
    const POOL: Address = address!("0x2222222222222222222222222222222222222222");

    // V2: no typed post at all flags as a missing recovery.
    let mut div = Vec::new();
    v2_parity(POOL, None, U256::ZERO, &mut div);
    assert_eq!(div.len(), 1);
    assert_eq!(div[0].kind, "missing_typed_post");

    // V2: untracked blockTimestampLast bits never flag; a tweaked reserve does.
    let engine =
        slot_layout::pack_v2_reserves_word(U112::from(1_500_u64), U112::from(2_500_000_u64));
    let dirty = U256::from_be_bytes(engine.0) | (U256::from(1_780_000_000_u64) << 224_u32);
    let mut div = Vec::new();
    require_masked_word(
        POOL,
        U256::from(slot_layout::V2_RESERVES_SLOT),
        "v2_reserves",
        slot_layout::V2_RESERVES_TRACKED_MASK,
        engine,
        dirty,
        &mut div,
    );
    assert!(div.is_empty(), "untracked timestamp bits masked: {div:?}");
    require_masked_word(
        POOL,
        U256::from(slot_layout::V2_RESERVES_SLOT),
        "v2_reserves",
        slot_layout::V2_RESERVES_TRACKED_MASK,
        engine,
        dirty ^ U256::from(1_u128 << 5),
        &mut div,
    );
    assert_eq!(div.len(), 1, "a tweaked tracked reserve bit flags");
    assert_eq!(div[0].kind, "v2_reserves");
    assert_eq!(div[0].pool, POOL);
    assert_eq!(div[0].slot, U256::from(slot_layout::V2_RESERVES_SLOT));

    // V3 slot0: garbage in bits 184..256 (observationIndex/feeProtocol/
    // unlocked) is masked; a wrong tick (tracked bits 160..184) flags.
    let sqrt = U256::from(1_u128) << 96;
    let engine0 = slot_layout::cl_slot0_tracked_word(sqrt, -5010);
    let dirty0 = U256::from_be_bytes(engine0.0) | (U256::from(0xdead_beef_u64) << 184_u32);
    let mut div = Vec::new();
    require_masked_word(
        POOL,
        U256::ZERO,
        "v3_slot0",
        slot_layout::CL_SLOT0_TRACKED_MASK,
        engine0,
        dirty0,
        &mut div,
    );
    assert!(div.is_empty(), "slot0 untracked bits masked: {div:?}");
    let wrong_tick = slot_layout::cl_slot0_tracked_word(sqrt, 5010);
    require_masked_word(
        POOL,
        U256::ZERO,
        "v3_slot0",
        slot_layout::CL_SLOT0_TRACKED_MASK,
        engine0,
        U256::from_be_bytes(wrong_tick.0),
        &mut div,
    );
    assert_eq!(div.len(), 1, "a wrong tracked tick flags");

    // V3 liquidity: the high half is untracked; the low uint128 is tracked.
    let liq_slot = U256::from(ClSlotLayout::UniswapV3.liquidity_slot());
    let engine_liq = slot_layout::cl_liquidity_tracked_word(0x0000_0000_006b_5d49_e99f_8835);
    let dirty_liq = U256::from_be_bytes(engine_liq.0) | (U256::from(0xfeed_u64) << 200_u32);
    let mut div = Vec::new();
    require_masked_word(
        POOL,
        liq_slot,
        "v3_liquidity",
        slot_layout::CL_LIQUIDITY_TRACKED_MASK,
        engine_liq,
        dirty_liq,
        &mut div,
    );
    assert!(div.is_empty(), "liquidity high bits masked: {div:?}");
    let wrong_liq = slot_layout::cl_liquidity_tracked_word(7_u128);
    require_masked_word(
        POOL,
        liq_slot,
        "v3_liquidity",
        slot_layout::CL_LIQUIDITY_TRACKED_MASK,
        engine_liq,
        U256::from_be_bytes(wrong_liq.0),
        &mut div,
    );
    assert_eq!(div.len(), 1, "a wrong tracked liquidity flags");

    // Tick words are tracked FULL-width (gross|net): any bit change flags.
    let engine_tick = slot_layout::tick_word_of(7_000, -250);
    let mut div = Vec::new();
    require_masked_word(
        POOL,
        U256::from(9_u64),
        "v3_tick_word",
        slot_layout::CL_TICK_WORD_MASK,
        engine_tick,
        U256::from_be_bytes(engine_tick.0),
        &mut div,
    );
    assert!(div.is_empty());
    let flipped = U256::from_be_bytes(engine_tick.0) ^ U256::ONE;
    require_masked_word(
        POOL,
        U256::from(9_u64),
        "v3_tick_word",
        slot_layout::CL_TICK_WORD_MASK,
        engine_tick,
        flipped,
        &mut div,
    );
    assert_eq!(div.len(), 1, "a one-bit tick-word change flags (full mask)");
    assert_eq!(div[0].kind, "v3_tick_word");

    // The divergence line carries the probe's field set.
    let line = div[0].to_string();
    assert!(line.starts_with("[sim-divergence] pool="), "{line}");
    assert!(line.contains("kind=v3_tick_word"), "{line}");
    assert!(line.contains("engine=0x"), "{line}");
    assert!(line.contains("truth=0x"), "{line}");
}

/// The archived capture classifies to router archetypes and dedupes: the
/// committed fixture carries TWO distinct frames (a CoW batch settle + a
/// 1inch aggregation swap), each captured twice.
#[test]
fn capture_frames_classify_to_archetypes_and_dedupe() {
    let frames = load_capture_frames(&capture_path());
    assert!(
        frames.len() >= 2,
        "the capture must classify its distinct frames: {:?}",
        frames.iter().map(|c| c.frame.hash).collect::<Vec<_>>()
    );
    for a in &frames {
        for b in &frames {
            if std::ptr::eq(a, b) {
                continue;
            }
            assert_ne!(a.frame.hash, b.frame.hash, "frames deduped by hash");
        }
    }
    assert!(
        frames
            .iter()
            .any(|c| matches!(c.archetype, Archetype::CoWBatchSettle)),
        "the capture must carry a CoW settle frame: {frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|c| matches!(c.archetype, Archetype::OneInchV5)),
        "the capture must carry a 1inch aggregation frame: {frames:?}"
    );
    for f in &frames {
        assert_ne!(f.frame.hash, B256::ZERO);
        assert_ne!(f.frame.to, Address::ZERO);
        assert_ne!(f.frame.from, Address::ZERO);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Live tier: pin, replay, extract, compare (network-gated)
// ─────────────────────────────────────────────────────────────────────────

/// When set, every candidate's gate verdict prints (fixture-window
/// diagnostics for an operator tuning the scan).
fn scan_trace() -> bool {
    std::env::var("DEGENBOT_ORACLE_TRACE").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// The oracle's diagnostic surface: which route resolved a fixture and why
/// candidates were rejected. The workspace's no-test-print rule is excepted
/// here because this IS the fixture-window story an operator consumes; the
/// `--nocapture` run is the consumer, not a debug leftover.
#[expect(
    clippy::print_stdout,
    reason = "the acquisition route + candidate verdicts are the harness's operator readout; \
              assertions carry the pass/fail, this carries the why"
)]
fn oracle_note(msg: &str) {
    println!("{msg}");
}

/// The reference V2 pair (USDC/WETH) whose runtime bytecode ids the V2 pair
/// family.
const V2_PAIR_USDC_WETH: Address = address!("0xB4e16d0168e52d35CACD2c6185b44281Ec28C9dc");
/// The reference V3 pool (USDC/WETH 0.05%) whose runtime bytecode ids the V3
/// pool family.
const V3_POOL_USDC_WETH_005: Address = address!("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");

/// The scratch stack never applies the strategy's state overrides, so the
/// live tests only need a well-formed (all-zero) warmup set to pass through
/// `BlockSimHandle::build`.
fn zero_warmup() -> WarmupSlots {
    WarmupSlots {
        weth_balance: U256::ZERO,
        erc6909_weth: U256::ZERO,
        erc6909_native: U256::ZERO,
    }
}

fn live_provider() -> RootProvider {
    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    ProviderBuilder::default().connect_http(rpc_url.parse().unwrap())
}

/// keccak256 of a runtime code blob — the per-family identity the oracle
/// discriminates touched pool accounts with (one runtime = one layout).
fn code_id(code: &Bytes) -> B256 {
    keccak256(code.as_ref())
}

/// The reference code ids of the two tracked pool families: V2 pairs (one
/// deployed-pair runtime) and V3 pools (one deployed-pool runtime).
async fn pool_family_ids(provider: &RootProvider) -> (B256, B256) {
    let v2: Bytes = provider
        .client()
        .request("eth_getCode", (V2_PAIR_USDC_WETH, "latest"))
        .await
        .unwrap();
    let v3: Bytes = provider
        .client()
        .request("eth_getCode", (V3_POOL_USDC_WETH_005, "latest"))
        .await
        .unwrap();
    (code_id(&v2), code_id(&v3))
}

/// The node's per-tx storage diff (prestateTracer diffMode): post-tx values
/// per account (the truth — immune to later-in-block interference) together
/// with the pre-tx values.
async fn node_post_diff(
    provider: &RootProvider,
    tx_hash: B256,
) -> (
    HashMap<Address, HashMap<U256, U256>>,
    HashMap<Address, HashMap<U256, U256>>,
) {
    let diff: Value = provider
        .client()
        .request(
            "debug_traceTransaction",
            (
                tx_hash,
                serde_json::json!({"tracer": "prestateTracer", "tracerConfig": {"diffMode": true}}),
            ),
        )
        .await
        .unwrap();
    assert!(
        diff.get("post").is_some_and(Value::is_object),
        "diffMode trace failed: {diff}"
    );

    let hex_u256 = |s: &str| {
        let t = s.trim_start_matches("0x");
        if t.is_empty() {
            U256::ZERO
        } else {
            U256::from_str_radix(t, 16).unwrap()
        }
    };
    // reth trims leading hex zeros in diff keys/values — left-pad to width.
    let pad = |s: &str, width: usize| format!("{s:0>width$}");

    let grab = |side: &str| -> HashMap<Address, HashMap<U256, U256>> {
        let mut out = HashMap::new();
        let Some(accounts) = diff
            .pointer(&format!("/{side}"))
            .and_then(|p| p.as_object())
        else {
            return out;
        };
        for (addr_key, body) in accounts {
            let addr: Address = pad(addr_key, 40).parse().unwrap();
            let Some(storage) = body.get("storage").and_then(|s| s.as_object()) else {
                continue;
            };
            for (slot_key, value) in storage {
                let slot = hex_u256(&pad(slot_key, 64));
                let val = hex_u256(value.as_str().expect("storage value is hex"));
                out.entry(addr)
                    .or_insert_with(HashMap::new)
                    .insert(slot, val);
            }
        }
        out
    };
    (grab("post"), grab("pre"))
}

/// True when `pool` was already touched by a transaction EARLIER in its
/// block — such a frame would replay against stale parent state and diverge
/// mid-block.
async fn pool_touched_earlier_in_block(
    provider: &RootProvider,
    pool: Address,
    pin: u64,
    fixture_tx_hash: B256,
    fixture_tx_index: u64,
) -> bool {
    let logs = provider
        .get_logs(
            &alloy::rpc::types::Filter::new()
                .address(pool)
                .from_block(BlockNumberOrTag::Number(pin))
                .to_block(BlockNumberOrTag::Number(pin)),
        )
        .await
        .unwrap();
    logs.iter().any(|log| {
        log.transaction_hash.is_some_and(|h| h != fixture_tx_hash)
            && log.transaction_index.is_some_and(|i| i < fixture_tx_index)
    })
}

/// The packed tick (bits 160..184 of a slot0 word), two's-complement.
fn slot0_tick_of(word: U256) -> i32 {
    let tick_u = ((word >> 160_u32) & U256::from(0x00ff_ffff_u32)).to::<u128>();
    let tick_u = u32::try_from(tick_u).unwrap();
    if tick_u & 0x0080_0000 != 0 {
        (tick_u.cast_signed()) - (1 << 24)
    } else {
        tick_u.cast_signed()
    }
}

/// Parent block base fee (wei/gas) — the handle's projection input.
async fn parent_base_fee(provider: &RootProvider, pin: u64) -> u128 {
    let parent = provider
        .get_block_by_number(BlockNumberOrTag::Number(pin.saturating_sub(1)))
        .await
        .unwrap()
        .expect("parent block exists");
    u128::from(parent.header.base_fee_per_gas.unwrap_or(0))
}

/// A parent-block storage word straight off the chain — the pre-tx truth
/// for journal-surfaced words the node's diff does not report.
async fn chain_word(provider: &RootProvider, pool: Address, slot: U256, pin: u64) -> U256 {
    let raw: Bytes = provider
        .client()
        .request(
            "eth_getStorageAt",
            (pool, slot, format!("0x{:x}", pin.saturating_sub(1))),
        )
        .await
        .unwrap();
    U256::from_be_slice(&raw)
}

/// The pool's pre-tx current tick: the node diff's pre map slot0, falling
/// back to the post map (slot0 untouched by this frame), falling back to the
/// parent-block storage word — the preimage anchor the engine would hold.
async fn pool_pre_tx_tick(
    provider: &RootProvider,
    node_pre: &HashMap<Address, HashMap<U256, U256>>,
    node_post: &HashMap<Address, HashMap<U256, U256>>,
    pool: Address,
    pin: u64,
) -> i32 {
    if let Some(w) = node_pre
        .get(&pool)
        .and_then(|s| s.get(&U256::ZERO))
        .or_else(|| node_post.get(&pool).and_then(|s| s.get(&U256::ZERO)))
    {
        return slot0_tick_of(*w);
    }
    let word = chain_word(provider, pool, U256::ZERO, pin).await;
    slot0_tick_of(word)
}

/// The V3 pool's `tickSpacing()` via `eth_call` (int24 → sign-reconstructed).
async fn tick_spacing_of(provider: &RootProvider, pool: Address) -> i32 {
    let raw: Bytes = provider
        .client()
        .request(
            "eth_call",
            (
                serde_json::json!({"to": pool, "input": "0xd0c93a7c"}),
                "latest",
            ),
        )
        .await
        .unwrap();
    let bits = U256::from_be_slice(&raw).to::<u128>();
    let bits = u32::try_from(bits).unwrap_or(60);
    if bits & 0x0080_0000 != 0 {
        (bits.cast_signed()) - (1 << 24)
    } else {
        bits.cast_signed()
    }
}

/// Code id per account written in the node diff (memoized per test). A
/// transient RPC failure is retried and never cached as "untracked" — a
/// dropped lookup must not silently shrink the tracked set.
async fn code_id_of(
    provider: &RootProvider,
    addr: Address,
    cache: &mut HashMap<Address, Option<B256>>,
) -> Option<B256> {
    if let Some(cached) = cache.get(&addr) {
        return *cached;
    }
    for _ in 0..3 {
        match provider
            .client()
            .request::<(Address, &str), Bytes>("eth_getCode", (addr, "latest"))
            .await
        {
            Ok(code) => {
                let id = code_id(&code);
                cache.insert(addr, Some(id));
                return Some(id);
            }
            Err(err) => {
                if scan_trace() {
                    oracle_note(&format!(
                        "oracle: eth_getCode transient failure for {addr:?}: {err}"
                    ));
                }
            }
        }
    }
    None
}

/// The tracked-pool descriptor set a fixture frame recovers: every account
/// the node diff reports as WRITTEN whose runtime bytecode matches a tracked
/// pool family — V2 pair or V3 pool, keyed by the reference code ids. A
/// family-unknown write account (tokens, relayers, the router itself, the V4
/// PoolManager) contributes nothing.
async fn tracked_pool_descriptors(
    provider: &RootProvider,
    family_ids: &(B256, B256),
    post: &HashMap<Address, HashMap<U256, U256>>,
    pre: &HashMap<Address, HashMap<U256, U256>>,
    pin: u64,
    code_cache: &mut HashMap<Address, Option<B256>>,
) -> HashMap<Address, PoolFamily> {
    let mut out = HashMap::new();
    for (addr, storage) in post {
        if storage.is_empty() {
            continue;
        }
        let id = code_id_of(provider, *addr, code_cache).await;
        let family = if id == Some(family_ids.0) {
            PoolFamily::V2Pair
        } else if id == Some(family_ids.1) {
            let spacing = tick_spacing_of(provider, *addr).await;
            let hint = pool_pre_tx_tick(provider, pre, post, *addr, pin).await;
            PoolFamily::V3 {
                layout: ClSlotLayout::UniswapV3,
                tick_spacing: spacing,
                current_tick_hint: Some(hint),
            }
        } else {
            continue;
        };
        out.insert(*addr, family);
    }
    out
}

/// A candidate transaction: first-sender-in-block, EIP-1559, no access
/// list — the seam's replayable input domain.
fn is_replayable_domain(t: &alloy::rpc::types::Transaction, parent_nonce: u64) -> bool {
    matches!(t.inner.tx_type(), alloy::consensus::TxType::Eip1559)
        && t.inner.access_list().is_none_or(|al| al.is_empty())
        && parent_nonce == t.nonce()
}

/// The seam's replayable input for an RPC transaction.
fn replayable_of(t: &alloy::rpc::types::Transaction) -> ReplayableTx {
    ReplayableTx {
        from: t.inner.signer(),
        to: t.to(),
        value: t.value(),
        data: t.input().clone(),
        gas_limit: t.gas_limit(),
        max_fee_per_gas: t.max_fee_per_gas(),
        max_priority_fee_per_gas: t.max_priority_fee_per_gas().unwrap_or(0),
        nonce: t.nonce(),
    }
}

/// Build a live scratch over the fixture frame's parent block — the
/// `BlockSimHandle::build` shape the sibling live tests use. The handle
/// borrows the override/anchor/cache locals, so the pairing expands in the
/// CALLER's scope.
macro_rules! live_scratch {
    ($scratch:ident, $handle:ident, $provider:expr, $pin:expr, $timestamp:expr, $base_fee:expr) => {
        let alloy_provider =
            degenbot_rpc::provider::AlloyProvider::from_provider(Arc::new($provider.clone()));
        let override_params = SimulationOverrideParams {
            owner: Address::ZERO,
            inject_code: false,
            injected_address: None,
            runtime_bytecode: Bytes::new(),
            warmup: zero_warmup(),
            weth_address: Address::ZERO,
            pool_manager_address: Address::ZERO,
        };
        let anchor = degenbot_bot::bot_core::SimAnchorState::default();
        let warm_cache = WarmCodeCacheInner::shared_default();
        let mut $handle = BlockSimHandle::build(
            &alloy_provider,
            $base_fee.max(1),
            $pin.saturating_sub(1),
            $timestamp,
            &override_params,
            &anchor,
            &warm_cache,
            None,
            false,
        )
        .expect("live handle builds");
        #[expect(unused_mut)]
        let mut $scratch = $handle.scratch_evm().expect("scratch evm stacks");
    };
}

/// One resolved fixture: the tx the fork actually ran + its pin + its
/// tracked-pool descriptor set.
#[derive(Debug)]
struct OracleFixture {
    tx: ReplayableTx,
    tx_hash: B256,
    tx_index: u64,
    pin: u64,
    timestamp: u64,
    base_fee_next: u128,
    descriptors: HashMap<Address, PoolFamily>,
    via: &'static str,
}

/// True on dirty-pool interference: a descriptor pool touched by an earlier
/// same-block tx (a parent-state replay would diverge mid-block).
async fn clean_frame(provider: &RootProvider, fixture: &OracleFixture) -> bool {
    for pool in fixture.descriptors.keys() {
        if pool_touched_earlier_in_block(
            provider,
            *pool,
            fixture.pin,
            fixture.tx_hash,
            fixture.tx_index,
        )
        .await
        {
            return false;
        }
    }
    true
}

/// Promote a classifier-passing candidate tx into a fixture when EVERY gate
/// holds: replayable domain (EIP-1559, no access list, first-sender-in-block),
/// enough tracked pools write-touched, and no earlier same-block touch.
async fn finalize_candidate(
    provider: &RootProvider,
    arch: Archetype,
    t: &alloy::rpc::types::Transaction,
    family_ids: &(B256, B256),
    code_cache: &mut HashMap<Address, Option<B256>>,
    tried: &mut usize,
) -> Option<OracleFixture> {
    let pin = t.block_number?;
    let parent_nonce = provider
        .get_transaction_count(t.inner.signer())
        .block_id(BlockId::number(pin.saturating_sub(1)))
        .await
        .unwrap();
    if !is_replayable_domain(t, parent_nonce) {
        if scan_trace() {
            oracle_note(&format!(
                "oracle[{arch:?}] candidate {} domain-dirty",
                t.inner.hash()
            ));
        }
        return None;
    }
    let tx_hash = *t.inner.hash();
    let (post, pre) = node_post_diff(provider, tx_hash).await;
    *tried += 1;
    let tx_index = t
        .transaction_index
        .expect("rpc transaction carries its index");
    let descriptors =
        tracked_pool_descriptors(provider, family_ids, &post, &pre, pin, code_cache).await;
    if descriptors.len() < arch.min_tracked_pools() {
        if scan_trace() {
            oracle_note(&format!(
                "oracle[{arch:?}] candidate {tx_hash} pool set too small: {} tracked pools",
                descriptors.len()
            ));
        }
        return None;
    }
    let block = provider
        .get_block_by_number(BlockNumberOrTag::Number(pin))
        .full()
        .await
        .unwrap()
        .expect("pin block exists");
    let fixture = OracleFixture {
        tx: replayable_of(t),
        tx_hash,
        tx_index,
        pin,
        timestamp: block.header.timestamp,
        base_fee_next: parent_base_fee(provider, pin).await,
        descriptors,
        via: "",
    };
    if !clean_frame(provider, &fixture).await {
        if scan_trace() {
            oracle_note(&format!(
                "oracle[{arch:?}] candidate {tx_hash} pool touched earlier in block"
            ));
        }
        return None;
    }
    Some(fixture)
}

/// A captured frame's on-chain identity matches its capture line (guarding
/// against hash-tolerance or pruned-index mismatches).
fn matches_capture(t: &alloy::rpc::types::Transaction, f: &CapturedFrame) -> bool {
    t.inner.signer() == f.from
        && t.nonce() == f.nonce
        && t.gas_limit() == f.gas
        && t.to() == Some(f.to)
}

/// Resolve one fixture per archetype: the archived capture first (its
/// landing tx looked up on-chain), then a clean frame from the live-scan
/// window (env-pinnable per archetype).
async fn resolve_fixture(provider: &RootProvider, arch: Archetype) -> OracleFixture {
    let family_ids = pool_family_ids(provider).await;
    let mut code_cache = HashMap::new();

    // 1. The archived capture.
    for classified in load_capture_frames(&capture_path())
        .into_iter()
        .filter(|c| c.archetype == arch)
    {
        let frame = classified.frame;
        let Ok(Some(t)) = provider.get_transaction_by_hash(frame.hash).await else {
            oracle_note(&format!(
                "oracle[{arch:?}]: archived frame {} is not retrievable on-chain; falling \
                 back to the live-scan window",
                frame.hash
            ));
            continue;
        };
        if !matches_capture(&t, &frame) {
            continue;
        }
        let mut tried = 0usize;
        if let Some(mut fixture) =
            finalize_candidate(provider, arch, &t, &family_ids, &mut code_cache, &mut tried).await
        {
            fixture.via = "archived capture";
            return fixture;
        }
        oracle_note(&format!(
            "oracle[{arch:?}]: archived frame {} is not a clean fixture; falling back to the \
             live-scan window",
            frame.hash
        ));
    }

    // 2. The live-scan window, newest-first.
    let head = provider.get_block_number().await.unwrap();
    let scan_depth: u64 = std::env::var("DEGENBOT_ORACLE_SCAN_BLOCKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(arch.scan_depth());
    let blocks: Vec<u64> =
        match std::env::var(format!("DEGENBOT_ORACLE_{}_BLOCK", arch.pin_env_suffix())) {
            Ok(b) => vec![b.parse::<u64>().unwrap()],
            Err(_) => (1..=scan_depth)
                .rev()
                .map(|i| head.saturating_sub(i))
                .collect(),
        };
    let mut tried = 0usize;
    let max_tried = arch.max_tried();
    for pin in blocks {
        if tried >= max_tried {
            break;
        }
        let Ok(Some(block)) = provider
            .get_block_by_number(BlockNumberOrTag::Number(pin))
            .full()
            .await
        else {
            continue;
        };
        for t in block.transactions.txns() {
            if tried >= max_tried {
                break;
            }
            let Some(to) = t.to() else { continue };
            if !classifies_as(arch, to, t.input()) {
                continue;
            }
            if let Some(mut fixture) =
                finalize_candidate(provider, arch, t, &family_ids, &mut code_cache, &mut tried)
                    .await
            {
                fixture.via = "live scan";
                return fixture;
            }
        }
    }
    panic!(
        "no clean {arch:?} fixture found (archived capture + {max_tried} trace attempts over \
         the window; pin one via DEGENBOT_ORACLE_{}_BLOCK or shrink via \
         DEGENBOT_ORACLE_SCAN_BLOCKS)",
        arch.pin_env_suffix()
    );
}

/// The fork-truth word for `slot`: the node's post-diff word the tx actually
/// wrote, else the parent-block storage word (a journal-surfaced unchanged
/// value). Both are chain truth at the pinned block.
async fn truth_word(
    provider: &RootProvider,
    post: &HashMap<Address, HashMap<U256, U256>>,
    pool: Address,
    slot: U256,
    pin: u64,
) -> U256 {
    match post.get(&pool).and_then(|m| m.get(&slot)) {
        Some(w) => *w,
        None => chain_word(provider, pool, slot, pin).await,
    }
}

/// Full oracle pass for one archetype: replay the fixture frame through the
/// scratch seam, extract, and compare every tracked field of EVERY tracked
/// pool the diff reported against fork truth. Panics listing all
/// divergences (an empty list is parity).
#[expect(
    clippy::too_many_lines,
    reason = "the per-family parity lanes read top-to-bottom per tracked slot kind, \
              mirroring the layout table"
)]
async fn oracle_parity_for(arch: Archetype) {
    let provider = live_provider();
    let fixture = resolve_fixture(&provider, arch).await;
    if arch.min_tracked_pools() >= 2 {
        assert!(
            fixture.descriptors.len() >= 2,
            "a multi-pool fixture is required: {} tracked pools",
            fixture.descriptors.len()
        );
    }
    let (post, _pre) = node_post_diff(&provider, fixture.tx_hash).await;
    oracle_note(&format!(
        "oracle[{arch:?}] via {}: pin={} tracked_pools={} tx={}",
        fixture.via,
        fixture.pin,
        fixture.descriptors.len(),
        fixture.tx_hash
    ));

    live_scratch!(
        scratch,
        handle,
        &provider,
        fixture.pin,
        fixture.timestamp,
        fixture.base_fee_next
    );
    let out = scratch.replay(&fixture.tx).expect("fixture frame executes");
    assert!(
        matches!(out.status, ReplayStatus::Success),
        "status {:?}",
        out.status
    );

    let states = extract_pool_post_states(&out, &fixture.descriptors);
    let mut divergences = Vec::new();
    for (pool, family) in &fixture.descriptors {
        match family {
            PoolFamily::V2Pair => {
                let truth = truth_word(
                    &provider,
                    &post,
                    *pool,
                    U256::from(slot_layout::V2_RESERVES_SLOT),
                    fixture.pin,
                )
                .await;
                v2_parity(
                    *pool,
                    states.iter().find(|s| s.address == *pool),
                    truth,
                    &mut divergences,
                );
            }
            PoolFamily::V3 { layout, .. } => {
                let liquidity_slot = U256::from(layout.liquidity_slot());
                let truth_slot0 =
                    truth_word(&provider, &post, *pool, U256::ZERO, fixture.pin).await;
                let truth_liquidity =
                    truth_word(&provider, &post, *pool, liquidity_slot, fixture.pin).await;
                let slot0_changed = post.get(pool).is_some_and(|m| m.contains_key(&U256::ZERO));
                let liquidity_changed = post
                    .get(pool)
                    .is_some_and(|m| m.contains_key(&liquidity_slot));
                let state = states.iter().find(|s| s.address == *pool);
                v3_scalar_parity(
                    *pool,
                    state,
                    slot0_changed,
                    liquidity_changed,
                    truth_slot0,
                    truth_liquidity,
                    &mut divergences,
                );
                if let Some(PoolPostState {
                    kind: PoolPostKind::Typed(TypedPoolPost::V3 { touched_ticks, .. }),
                    ..
                }) = state
                {
                    let mut truth_words = Vec::with_capacity(touched_ticks.len());
                    for tt in touched_ticks {
                        let slot = slot_layout::tick_mapping_slot_at_base(
                            tt.tick,
                            U256::from(layout.ticks_mapping_slot()),
                        );
                        truth_words
                            .push(truth_word(&provider, &post, *pool, slot, fixture.pin).await);
                    }
                    v3_tick_parity(
                        *pool,
                        *layout,
                        touched_ticks,
                        &truth_words,
                        &mut divergences,
                    );
                }
            }
            // Code-id classification never admits a V4 descriptor.
            PoolFamily::V4PoolManager { .. } => unreachable!("no V4 descriptor is admitted"),
        }
    }

    assert!(
        divergences.is_empty(),
        "extracted post-states diverged from fork truth ({} tracked pools):\n{}",
        fixture.descriptors.len(),
        divergences
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ── the live archetype seats (one fixture each; opt-in tier) ──

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_direct_v2_router_frame_matches_fork_truth() {
    oracle_parity_for(Archetype::DirectV2Router).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_direct_v3_router_frame_matches_fork_truth() {
    oracle_parity_for(Archetype::DirectV3Router).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_swap_router_02_frame_matches_fork_truth() {
    oracle_parity_for(Archetype::SwapRouter02).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_universal_router_multihop_frame_matches_fork_truth() {
    oracle_parity_for(Archetype::UniversalRouterMultiHop).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_1inch_v5_frame_matches_fork_truth() {
    oracle_parity_for(Archetype::OneInchV5).await;
}

/// The multi-pool CoW case: the FULL touched set (every tracked pool the
/// batch settle wrote) is recovered at fork-truth parity — not just the
/// first pool.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_cow_batch_settle_recovers_the_full_pool_set() {
    oracle_parity_for(Archetype::CoWBatchSettle).await;
}
