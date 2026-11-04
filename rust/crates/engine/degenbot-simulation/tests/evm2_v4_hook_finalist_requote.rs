//! ADR-090 H3's live evidence: a real hooked V4 leg through the finalist
//! re-quote seam.
//!
//! One question: does the clamp's installed tier stage the CHAIN's figure —
//! the wei-exact black-box quote — where the white-box standard-math walk (the
//! search approximation `Caveats::HOOKED_POOL` flags) gets it wrong?
//!
//! Every census pool is a REAL mainnet hooked V4 pool
//! (`fixtures/v4_hook_census.tsv`, the H1 census). For each row the harness:
//!
//! 1. reads the pool's `slot0` + `liquidity` words from the `PoolManager` at
//!    the anchor (the two computable slots, H1's `leg_state_slots`);
//! 2. prices the leg WHITE-BOX: the standard concentrated-liquidity walk over
//!    that state (`degenbot_pools::v4_simulate_swap`) — the search tier's
//!    approximation, hook deltas ignored;
//! 3. prices it BLACK-BOX: the deployed `Quoter` over an anchor-pinned scratch
//!    (the H1 primitive);
//! 4. builds the clamp's own facts (`hooked_leg_facts`) and re-quotes through
//!    [`HookedLegPlan`] with a tier shaped exactly like the production one
//!    (the same quoter over the same scratch), then asserts the staged figure
//!    IS the chain's to the wei.
//!
//! The selected row is one where (2) and (3) DIFFER — the test exists to show
//! the approximation being corrected, so a row where the white-box walk
//! already lands on the hook's answer proves nothing.
//!
//! # Running it
//!
//! ```sh
//! cargo test -p degenbot-simulation --test evm2_v4_hook_finalist_requote \
//!   finalist_requote_stages_the_chain_figure -- --ignored --nocapture
//! ```
//!
//! `DEGENBOT_RPC_HTTP_CHAINID_1` (or `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`)
//! selects the node; the anchor needs archive state.

#![expect(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]

use hashbrown::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, B256, I256, U256};

use ::evm2::evm::{CacheDB, Database, Db};

use degenbot_bot::arb_engine::{
    hooked_leg_facts, HookedLeg, HookedLegAnchor, HookedLegPlan, HookedLegQuote, HookedLegReQuote,
};
use degenbot_pools::solve::{HopType, MixedPoolRef};
use degenbot_pools::{v4_pool_id, v4_simulate_swap, V3PoolState, V4PoolKey};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_simulation::sim::evm2::frame_replay::{Evm2CountingFrameDb, Evm2ScratchEvm};
use degenbot_simulation::sim::evm2::v4_black_box::{
    leg_state_slots, V4BlackBoxQuoter, V4Leg, V4_POOL_MANAGER,
};
use degenbot_simulation::sim::frame_replay::{FrameRpcCounter, ScratchBlock};
use degenbot_substrate::{BotState, PoolTickCoverage, RegisterV4PoolParams, TickInfo};

/// The archived block every read pins (the H1 census anchor).
const PIN: u64 = 26_320_000;

/// `hook  currency0  currency1  fee  tick_spacing  pool_id` (H1's census).
const CENSUS: &str = include_str!("fixtures/v4_hook_census.tsv");

/// The census stack's ext: forwarded reads are the anchor's own storage.
type TierExt<'a> = CacheDB<Db<Evm2CountingFrameDb<BlockingAsyncDb<'a>>>>;

/// The production tier's shape, in the test: the black-box quoter over an
/// anchor-pinned scratch, one quote at a time.
struct ScratchTier<'a> {
    inner: Mutex<(V4BlackBoxQuoter, Evm2ScratchEvm<TierExt<'a>>)>,
}

impl HookedLegReQuote for ScratchTier<'_> {
    fn requote_leg(&self, _anchor: HookedLegAnchor, leg: &HookedLeg) -> Option<HookedLegQuote> {
        let mut guard = self.inner.lock().unwrap();
        let (quoter, scratch) = &mut *guard;
        let v4_leg = V4Leg::canonical(leg.pool_key.clone(), leg.zero_for_one);
        quoter
            .quote(scratch, &v4_leg, leg.amount_in)
            .ok()
            .map(|quote| HookedLegQuote {
                amount_out: quote.amount_out,
                gas: quote.gas_used,
            })
    }
}

fn census() -> Vec<(Address, V4PoolKey)> {
    let mut rows = Vec::new();
    for line in CENSUS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 6, "census row shape: {line:?}");
        let hook = Address::from_str(fields[0]).expect("hook address");
        let key = V4PoolKey {
            currency0: Address::from_str(fields[1]).expect("currency0"),
            currency1: Address::from_str(fields[2]).expect("currency1"),
            fee: fields[3].parse().expect("fee"),
            tick_spacing: fields[4].parse().expect("tick spacing"),
            hooks: hook,
        };
        let pinned = B256::from_str(fields[5]).expect("pool id");
        assert_eq!(v4_pool_id(&key), pinned, "census row integrity: {line:?}");
        rows.push((hook, key));
    }
    rows
}

fn rpc_url() -> String {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .expect("set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI)")
}

/// V4 `slot0` packing (v4-core `StateLibrary`): `sqrtPriceX96` in the low 160
/// bits, `tick` (int24) at 160, `protocolFee` at 184, `lpFee` at 208.
fn decode_slot0(slot0: U256) -> (U256, i32, u32) {
    let sqrt_price = slot0 & ((U256::ONE << 160) - U256::ONE);
    let raw: u32 = ((slot0 >> 160usize) & U256::from(0x00FF_FFFFu32)).to();
    let tick = if raw & 0x0080_0000 != 0 {
        (raw | 0xFF00_0000).cast_signed()
    } else {
        raw.cast_signed()
    };
    let protocol_fee: u32 = ((slot0 >> 184usize) & U256::from(0x00FF_FFFFu32)).to();
    (sqrt_price, tick, protocol_fee)
}

/// A single-range tick map straddling `tick` — the standard-math state the
/// white-box walk prices (each bound `spread` spacings away, so a tiny input
/// never crosses).
fn single_range(tick: i32, spacing: i32, liquidity: u128, spread: i32) -> HashMap<i32, TickInfo> {
    let lower = tick - spread * spacing;
    let upper = tick + spread * spacing;
    let net = i128::try_from(liquidity).unwrap_or(i128::MAX);
    let mut ticks = HashMap::new();
    ticks.insert(
        lower,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liquidity),
            liquidity_net: net,
            block: 0,
        },
    );
    ticks.insert(
        upper,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(liquidity),
            liquidity_net: -net,
            block: 0,
        },
    );
    ticks
}

/// One selected census pool with both figures.
struct Selection {
    hook: Address,
    key: V4PoolKey,
    leg: V4Leg,
    amount_in: U256,
    white_box_out: U256,
    exact: U256,
    slot0: U256,
    liquidity: U256,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC over archive mainnet state; run with --ignored --nocapture"]
async fn finalist_requote_stages_the_chain_figure() {
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    assert_eq!(
        provider.get_chain_id().await.unwrap(),
        1,
        "the census pools are mainnet fixtures"
    );
    let block = provider
        .get_block(PIN)
        .await
        .unwrap()
        .expect("the anchor block is archived");
    let anchor = HookedLegAnchor {
        block: PIN,
        timestamp: block.header.timestamp,
        base_fee: u128::from(block.header.base_fee_per_gas.unwrap_or(0)),
    };
    println!(
        "=== ADR-090 H3 finalist re-quote evidence ===\nanchor {PIN} (timestamp {}, base fee {} wei/gas)",
        anchor.timestamp, anchor.base_fee
    );

    // The anchor-pinned scratch the tier quotes over (the production shape).
    let counter = Arc::new(FrameRpcCounter::default());
    let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(&provider, PIN))
        .expect("a multi-threaded runtime is ambient");
    let cold_path = Evm2CountingFrameDb::new(blocking, Arc::clone(&counter));
    let ext: TierExt<'_> = CacheDB::new(Db::new(cold_path));
    let mut scratch = Evm2ScratchEvm::with_counter(
        ext,
        ScratchBlock {
            number: PIN,
            timestamp: anchor.timestamp,
            base_fee_next: anchor.base_fee,
        },
        counter,
    );

    let mut selection: Option<Selection> = None;
    for (hook, key) in census() {
        let pool_id = v4_pool_id(&key);
        let slots = leg_state_slots(&key);
        let slot0_word = scratch
            .ext_mut()
            .get_storage(&V4_POOL_MANAGER, &slots[0])
            .unwrap_or(U256::ZERO);
        let liquidity = scratch
            .ext_mut()
            .get_storage(&V4_POOL_MANAGER, &slots[1])
            .unwrap_or(U256::ZERO);
        let (sqrt_price, tick, protocol_fee) = decode_slot0(slot0_word);
        if liquidity.is_zero() || sqrt_price.is_zero() {
            println!(
                "  skip 0x{} (hook 0x{}): no in-range liquidity at the anchor",
                alloy::hex::encode(&pool_id[..6]),
                alloy::hex::encode(&hook.as_slice()[..3])
            );
            continue;
        }
        let liq = u128::try_from(liquidity).unwrap_or(u128::MAX);
        // A tiny input (1e-6 of the in-range liquidity): the standard-math walk
        // stays inside one tick range, so the white-box figure is the pool's own
        // single-range price.
        let amount_in = (liquidity / U256::from(1_000_000u64)).max(U256::ONE);

        for zero_for_one in [true, false] {
            let leg = V4Leg::canonical(key.clone(), zero_for_one);
            let quoted = V4BlackBoxQuoter::default().quote(&mut scratch, &leg, amount_in);
            let Some(exact) = quoted.ok().map(|quote| quote.amount_out) else {
                continue;
            };

            // ── the white-box approximation over the SAME state ─────────
            let mut core = BotState::new();
            let spread = 10;
            let ticks = single_range(tick, key.tick_spacing, liq, spread);
            let lower = tick - spread * key.tick_spacing;
            let upper = tick + spread * key.tick_spacing;
            let Ok(v4_id) = core.register_v4_pool(&RegisterV4PoolParams {
                pool_manager: V4_POOL_MANAGER,
                pool_id: pool_id.0,
                pool_key: key.clone(),
                state_view: Address::ZERO,
                hook_flags: 0,
                protocol_fee,
                sqrt_price_x96: sqrt_price,
                liquidity: liq,
                tick,
                tick_data: ticks,
                update_block: PIN,
                tick_data_block: None,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
                known_bitmap_words: Vec::new(),
            }) else {
                continue;
            };
            let white_box = {
                let state = core
                    .pool_slice::<degenbot_pools::V4PoolState>(v4_id)
                    .unwrap();
                let Some(neg) = I256::try_from(amount_in).ok().and_then(I256::checked_neg) else {
                    continue;
                };
                let limit = V3PoolState::default_sqrt_price_limit(zero_for_one);
                v4_simulate_swap(state, key.fee, key.tick_spacing, zero_for_one, neg, limit)
                    .ok()
                    .map(|outcome| {
                        (
                            if zero_for_one {
                                outcome.amount1
                            } else {
                                outcome.amount0
                            },
                            outcome.tick,
                        )
                    })
            };
            let Some((white_box_out, end_tick)) = white_box else {
                continue;
            };
            if end_tick <= lower || end_tick >= upper {
                println!(
                    "  skip 0x{}: white-box walk crossed its single range (end tick {end_tick})",
                    alloy::hex::encode(&pool_id[..6])
                );
                continue;
            }
            if white_box_out == exact {
                println!(
                    "  candidate 0x{} zfo={zero_for_one}: white-box {white_box_out} == exact — nothing to correct",
                    alloy::hex::encode(&pool_id[..6])
                );
                continue;
            }
            println!(
                "  SELECTED 0x{} (hook 0x{}) zfo={zero_for_one} in={amount_in}: white-box {white_box_out} vs exact {exact}",
                alloy::hex::encode(&pool_id[..6]),
                alloy::hex::encode(&hook.as_slice()[..3])
            );
            selection = Some(Selection {
                hook,
                key,
                leg,
                amount_in,
                white_box_out,
                exact,
                slot0: slot0_word,
                liquidity,
            });
            break;
        }
        if selection.is_some() {
            break;
        }
    }

    let Some(Selection {
        hook,
        key,
        leg,
        amount_in,
        white_box_out,
        exact,
        slot0,
        liquidity,
    }) = selection
    else {
        panic!("no census pool where the white-box walk mis-prices the hook's answer");
    };

    println!();
    println!(
        "pool        : 0x{} (hook 0x{}, fee {}, spacing {})",
        alloy::hex::encode(&v4_pool_id(&key)[..]),
        alloy::hex::encode(hook.as_slice()),
        key.fee,
        key.tick_spacing
    );
    println!(
        "leg         : zero_for_one={} amount_in={amount_in}",
        leg.zero_for_one
    );
    println!("white-box   : {white_box_out}  (standard concentrated-liquidity walk)");
    println!("exact       : {exact}  (deployed Quoter over the anchor)");
    println!(
        "delta       : {}",
        if exact > white_box_out {
            exact - white_box_out
        } else {
            white_box_out - exact
        }
    );

    // ── the clamp's own seam: facts → plan → staged figure ───────────────
    let tier = ScratchTier {
        inner: Mutex::new((V4BlackBoxQuoter::default(), scratch)),
    };
    // The facts need the pool's IDENTITY only (the leg carries the key +
    // committed input), so a Sparse registration of the real key is enough.
    let mut state = BotState::new();
    let (sqrt_price, tick, protocol_fee) = decode_slot0(slot0);
    let liq = u128::try_from(liquidity).unwrap_or(u128::MAX);
    let state_id = state
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: V4_POOL_MANAGER,
            pool_id: v4_pool_id(&key).0,
            pool_key: key.clone(),
            state_view: Address::ZERO,
            hook_flags: 0,
            protocol_fee,
            sqrt_price_x96: sqrt_price,
            liquidity: liq,
            tick,
            tick_data: HashMap::new(),
            update_block: PIN,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            known_bitmap_words: Vec::new(),
        })
        .expect("the census pool's identity registers");
    let pools = [MixedPoolRef {
        hop_type: HopType::V4,
        pool_key: state_id,
        zero_for_one: leg.zero_for_one,
    }];
    let consumed = [amount_in];
    let facts = hooked_leg_facts(&state, &pools, &consumed);
    assert_eq!(
        facts.len(),
        1,
        "the admitted pool is an amount-modifying leg"
    );
    let plan = HookedLegPlan::requote(facts, anchor, Some(&tier));
    assert_eq!(
        plan.exact_out(0),
        Some(exact),
        "the staged figure is the chain's own answer, to the wei"
    );
    // The same frame's gas rides the staged answer — the admission floor's
    // replacement for the hookless V4 row. The second quote shares the warm
    // scratch, so its gas is a little under the first frame's; the assertion is
    // the quantity's SHAPE (a whole frame, intrinsic included), not a parity.
    let measured_gas = plan
        .measured_gas(0)
        .expect("an exact re-quote carries the quote frame's gas");
    assert!(
        measured_gas > 21_000,
        "a quote frame burns at least the transaction intrinsic; got {measured_gas}"
    );
    assert!(plan.lifts_caveat(), "an exact re-quote lifts the caveat");
    assert_ne!(
        plan.exact_out(0).unwrap(),
        white_box_out,
        "the staging path CORRECTS the white-box approximation"
    );
    println!();
    println!(
        "staged      : {:?}\n              == the Quoter truth to the wei, != the white-box figure; \
         measured frame gas {measured_gas} (the floor's replacement for the hookless V4 row)",
        plan.stage(0)
    );
}
