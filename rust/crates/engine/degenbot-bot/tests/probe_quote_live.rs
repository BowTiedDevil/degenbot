//! LIVE, opt-in proof at a pinned mainnet block: the core's exact-input quote
//! prices a REAL hooked V4 hop through the chain's OWN quoter — never through
//! unhooked white-box math.
//!
//! # Why this leg lives in `tests/`
//!
//! Opt-in network evidence needs an operator endpoint, and the endpoint arrives
//! through the ENVIRONMENT. ADR-062 D7 confines `std::env::var` to the config
//! loader in `src/` (the `no_stray_env_reads` gate fails closed on any read
//! there), so a `cfg(test)` module under `src/` is the wrong home for a live
//! leg. The offline unit tests for the seam stay in
//! `src/arb_engine/probe_quote.rs`; this harness is its own integration-test
//! binary and drives only the crate's PUBLIC surface — [`EngineStages`]
//! (constructed over a `BotState` the test seeds itself from chain reads) and
//! the re-exported quote values. Nothing new is opened up in production for
//! this test's convenience.
//!
//! # What the five gates prove
//!
//! The independent side is a hand-encoded raw `eth_call` (never
//! `V4Quote::quote_exact_in`), so the comparison cannot be the seam agreeing
//! with itself. What the gates prove, in order:
//!
//! 1. **The pin is a read fact.** `PoolManager` slot0's `sqrtPriceX96` and the
//!    pool's `liquidity` are read AT the pin and asserted non-zero — the pool is
//!    initialized there because the chain says so — and the block's own
//!    timestamp and base fee must still be the pinned ones.
//! 2. **The hook reaches the chain.** For every hop, the calldata the core
//!    emitted is byte-identical to the hand-encoded call, and the hook address
//!    is its key's fifth word.
//! 3. **The vector is the chain's answers.** Every `hop_outputs` entry equals
//!    the independent call for that hop's chained amount, `consumed_inputs` is
//!    the fixed input followed by the previous hop's output, the first hop's
//!    output is the PINNED answer, and the block context is the pin's own
//!    timestamp and base fee.
//! 4. **Nothing unhooked could have produced it.** The same currencies, fee, and
//!    spacing with `hooks: Address::ZERO` is a DIFFERENT (never initialized)
//!    pool: the chain refuses it, so the answer above is hook-specific — an
//!    unhooked substitution has no matching answer.
//! 5. **The unhooked WALK differs.** The pool's own registered state, priced by
//!    the production math the solver would use, does NOT reproduce the chain's
//!    answer for the first hop (the hook's charge is the gap) — the substitution
//!    this seam must never make is measurably a different number.
//!
//! The path is registered through the PRODUCTION seam
//! (`EngineStages::register_path`), which structurally refuses a path shorter
//! than two hops — so each direction is a two-hop ROUND TRIP on the pinned pool:
//! the fixed input enters in the direction under test, and the second hop
//! re-quotes the opposite direction with the chained amount at the same pin. The
//! pinned answers below are the FIRST hop's (the fixed input's), exactly as
//! recorded; the reverse hop is checked against its own independent `eth_call`
//! rather than a second hand-copied constant.
//!
//! ```sh
//! DEGENBOT_RPC_HTTP_CHAINID_1=<archive endpoint> cargo nextest run \
//!   -p degenbot-bot -E 'test(live_hooked_v4_quote)' --run-ignored only --nocapture
//! ```

#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    reason = "a live proof fails loudly: every read must answer, and the report is printed for the operator"
)]

use std::sync::{Arc, Mutex};

use alloy::eips::BlockId;
use alloy::primitives::{address, Address, Bytes, B256, U256};
use degenbot_bot::arb_engine::{
    EngineStages, ExactInputQuote, ProbeBlockContext, ProbeQuoteTransport,
};
use degenbot_core::errors::ProviderResult;
use degenbot_pools::solve::PoolHop;
use degenbot_pools::{simulate_swap, PoolEntry, PoolTickCoverage, RegisterV4PoolParams, V4PoolKey};
use degenbot_rpc::abi::{selector, ContractRead, V4_QUOTER};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::{BotState, EpochDelta};
use hashbrown::HashMap;

/// The canonical v4-periphery `PoolManager` on chain 1 — the storage the pinned
/// pool's own facts are read from.
const POOL_MANAGER: Address = address!("0x000000000004444c5dc75cB358380D2e3dE08A90");

/// The chain-1 endpoint family every Rust live leg in this tree dials:
/// `just test-fork` exports `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`'s value under
/// this name, so this proof and the pytest fork legs hit ONE node.
const LIVE_RPC_ENV: &str = "DEGENBOT_RPC_HTTP_CHAINID_1";

/// The archived block the live proof prices at — the anchor the committed
/// hooked-V4 parity census (`degenbot-simulation`'s
/// `tests/fixtures/v4_hook_census.tsv`) was drawn at, deep enough in the
/// archive window that the pool below is live there.
const LIVE_PIN: u64 = 26_320_000;

/// `block LIVE_PIN`'s own facts. The test re-reads them from the node and
/// refuses to proceed if they moved: a pin that drifted is not a pin.
const LIVE_PIN_TIMESTAMP: u64 = 1_793_537_051;
const LIVE_PIN_BASE_FEE: u128 = 40_859_637;

/// The REAL mainnet hook this proof prices through: 10,185 bytes of code at the
/// pin, low 16 bits `0x68cc` (`& 0xCC != 0` — an amount-modifying hook). The
/// pool's charge lives in that hook, which is exactly the pool an unhooked
/// white-box walk cannot price correctly.
const LIVE_HOOK: Address = address!("322dcEc4958C14e021A9F1cD49DF11b9457968cC");

/// The pool id [`live_pool_key`] hashes to — the census row's own `pool_id`
/// column, asserted at run time so a mistyped key fails loudly instead of
/// quoting a different pool.
const LIVE_POOL_ID: &str = "004de370c1ef268265d1f3f8a91d00bd4bc2d97699f9877dc3d81286fb657bf3";

/// The fixed input both sides price: one token unit (both currencies report
/// 18 decimals at the pin).
const LIVE_AMOUNT_IN: u64 = 1_000_000_000_000_000_000;

/// The two hops of the registered round-trip path (see the module docs: the
/// production registration seam refuses a one-hop path).
const LIVE_HOPS: usize = 2;

/// The canonical v4-periphery quoter's answers at [`LIVE_PIN`] for the FIRST
/// hop (the fixed input's), recorded from the INDEPENDENT raw `eth_call` for
/// each direction. Pinned so a move on either side is a loud failure rather
/// than a silent re-baseline.
const LIVE_OUT_ZERO_FOR_ONE: u128 = 4_854_067_101_419_274_970_894;
const LIVE_OUT_ONE_FOR_ZERO: u128 = 201_912_091_397_373;

/// One census row of the hooked-V4 parity census, as a pool key:
/// `hook currency0 currency1 fee tick_spacing pool_id`. `fee: 0` means the
/// charge is HOOK-carried, so the pool cannot be priced by its key's static fee
/// alone.
fn live_pool_key() -> V4PoolKey {
    V4PoolKey {
        currency0: address!("4E67db19044549ff420860834c91b45bad298722"),
        currency1: address!("Ae5b1b51ECB247218157ddc6f1E457db25F971d7"),
        fee: 0,
        tick_spacing: 200,
        hooks: LIVE_HOOK,
    }
}

/// A pass-through transport that RECORDS the calldata the core emits while
/// forwarding every call to the live provider — the bridge between "what this
/// seam encoded" and "what the chain answered".
struct RecordingTransport<'a> {
    inner: &'a AlloyProvider,
    calls: Mutex<Vec<(Address, Bytes, u64)>>,
}

impl<'a> RecordingTransport<'a> {
    fn new(inner: &'a AlloyProvider) -> Self {
        Self {
            inner,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(Address, Bytes, u64)> {
        self.calls
            .lock()
            .expect("the call log is unpoisoned")
            .clone()
    }
}

impl ContractRead for RecordingTransport<'_> {
    async fn call(&self, to: Address, data: Bytes, block: Option<u64>) -> ProviderResult<Bytes> {
        let block = block.expect("an exact-input quote always pins its block");
        self.calls
            .lock()
            .expect("the call log is unpoisoned")
            .push((to, data.clone(), block));
        self.inner.call(to, data, Some(block)).await
    }
}

impl ProbeQuoteTransport for RecordingTransport<'_> {
    async fn block_context(&self, block: u64) -> ProviderResult<ProbeBlockContext> {
        self.inner.block_context(block).await
    }
}

/// One 32-byte ABI word holding `value`.
fn word_uint(value: U256) -> [u8; 32] {
    value.to_be_bytes()
}

/// One 32-byte ABI word holding an address, right-aligned.
fn word_address(value: Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(value.as_slice());
    word
}

/// The INDEPENDENT hand-encoding of v4-periphery
/// `quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))`
/// — written here, never produced by the seam under test, so a parity assertion
/// cannot be the seam agreeing with itself.
fn v4_calldata(key: &V4PoolKey, zero_for_one: bool, amount_in: U256) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 10 * 32);
    out.extend_from_slice(&selector(
        b"quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))",
    ));
    for word in [
        word_uint(U256::from(0x20)),
        word_address(key.currency0),
        word_address(key.currency1),
        word_uint(U256::from(key.fee)),
        word_uint(U256::from(key.tick_spacing)),
        word_address(key.hooks),
        word_uint(U256::from(u8::from(zero_for_one))),
        word_uint(amount_in),
        word_uint(U256::from(0x100)),
        word_uint(U256::ZERO),
    ] {
        out.extend_from_slice(&word);
    }
    out
}

/// The LOW 16 bits of a hook address — the `hook_flags` word a V4 key
/// pre-decodes (the amount-modifying mask tests against them).
fn hook_flags_of(hook: Address) -> u16 {
    u16::from_be_bytes([
        *hook.as_slice().get(18).expect("an address is 20 bytes"),
        *hook.as_slice().get(19).expect("an address is 20 bytes"),
    ])
}

/// The pinned pool's own on-chain state — every field read from `PoolManager`
/// storage at [`LIVE_PIN`].
struct PinnedPoolState {
    /// The raw slot0 word (the printed proof).
    slot0: U256,
    /// `slot0.sqrtPriceX96` (low 160 bits).
    sqrt_price_x96: U256,
    /// `slot0.tick` (next 24 bits, sign-extended).
    tick: i32,
    /// `slot0.protocolFee` (next 24 bits).
    protocol_fee: u32,
    /// The pool's in-range liquidity (the `Pool.State` word three slots on).
    liquidity: U256,
}

/// Read `PoolManager`'s storage for `pool_id` at [`LIVE_PIN`] and decode slot0 —
/// the pool's initialization is PROVEN by this read, never assumed from the
/// census row that names it.
async fn read_pinned_pool_state(provider: &AlloyProvider, pool_id: B256) -> PinnedPoolState {
    let state_base = degenbot_pools::v4_pool_state_base_slot(pool_id);
    let slot0 = U256::from_be_bytes(
        provider
            .get_storage_at(
                &POOL_MANAGER,
                degenbot_pools::v4_slot0_slot(state_base),
                Some(BlockId::number(LIVE_PIN)),
            )
            .await
            .expect("PoolManager.slot0 reads at the pin")
            .0,
    );
    let liquidity = U256::from_be_bytes(
        provider
            .get_storage_at(
                &POOL_MANAGER,
                degenbot_pools::v4_liquidity_slot(state_base),
                Some(BlockId::number(LIVE_PIN)),
            )
            .await
            .expect("PoolManager liquidity reads at the pin")
            .0,
    );
    let tick_raw = i32::try_from(((slot0 >> 160u32) & U256::from(0x00ff_ffffu32)).to::<u32>())
        .expect("24 bits fit an i32");
    PinnedPoolState {
        slot0,
        sqrt_price_x96: slot0 & ((U256::from(1u8) << 160) - U256::from(1u8)),
        tick: if tick_raw >= 1 << 23 {
            tick_raw - (1 << 24)
        } else {
            tick_raw
        },
        protocol_fee: ((slot0 >> 184u32) & U256::from(0x00ff_ffffu32)).to::<u32>(),
        liquidity,
    }
}

/// Assert the node still serves [`LIVE_PIN`] with the pinned block's own
/// timestamp and base fee — the independent side of the context check.
async fn assert_pinned_block(provider: &AlloyProvider) {
    let block = provider
        .get_block(LIVE_PIN)
        .await
        .expect("the pinned block reads")
        .expect("the pinned block exists");
    assert_eq!(
        block.header.inner.timestamp, LIVE_PIN_TIMESTAMP,
        "the pinned block's timestamp moved — re-pin before trusting the amounts"
    );
    assert_eq!(
        u128::from(block.header.inner.base_fee_per_gas.unwrap_or(0)),
        LIVE_PIN_BASE_FEE,
        "the pinned block's base fee moved — re-pin before trusting the amounts"
    );
}

/// The vector's context belongs to ONE block: the pin's own facts.
fn assert_quote_context(quote: &ExactInputQuote) {
    assert_eq!(quote.block, LIVE_PIN, "the vector belongs to the pin");
    assert_eq!(
        quote.block_context,
        ProbeBlockContext {
            block: LIVE_PIN,
            base_fee_next: LIVE_PIN_BASE_FEE,
            timestamp: LIVE_PIN_TIMESTAMP,
        },
        "the block context is the pinned block's own"
    );
}

/// Gate 4: the same currencies, fee, and spacing with `hooks = ZERO` is a
/// DIFFERENT (never initialized) pool. The chain refuses it, so the hooked
/// answer is hook-specific — an unhooked substitution has no matching answer.
async fn assert_unhooked_twin_is_refused(
    provider: &AlloyProvider,
    key: &V4PoolKey,
    zero_for_one: bool,
) {
    let unhooked_key = V4PoolKey {
        hooks: Address::ZERO,
        ..key.clone()
    };
    let unhooked = ContractRead::call(
        provider,
        V4_QUOTER,
        Bytes::from(v4_calldata(
            &unhooked_key,
            zero_for_one,
            U256::from(LIVE_AMOUNT_IN),
        )),
        Some(LIVE_PIN),
    )
    .await;
    assert!(
        unhooked.is_err(),
        "the unhooked twin (hooks = 0) must be REFUSED at the pin, so the answer \
         above is hook-specific (zfo={zero_for_one}): {unhooked:?}"
    );
}

/// Gate 5: the registered pool's OWN production math — the figure an unhooked
/// substitution would report — must not reproduce the chain's answer (at this
/// pin `4_903_097_821_562_843_550_261` / `203_951_607_472_093`: the hook's
/// charge is the gap). A walk that refuses the hop claims no comparison rather
/// than a false one.
fn assert_white_box_differs(white_box: &PoolEntry, zero_for_one: bool, amount_out: U256) {
    match simulate_swap(white_box, zero_for_one, U256::from(LIVE_AMOUNT_IN)) {
        Ok(modelled) => {
            assert_ne!(
                modelled, amount_out,
                "the unhooked walk must differ from the chain's answer for a \
                 hook-charged pool (zfo={zero_for_one})"
            );
            println!(
                "  zfo={zero_for_one:<5} white-box walk {modelled} — the hook's charge is the gap"
            );
        }
        Err(error) => {
            println!("  zfo={zero_for_one:<5} white-box walk refused this hop: {error:?}");
        }
    }
}

/// Register the pinned hooked pool over the state the chain just reported, and
/// hand back its `BotState` pool id.
fn register_pinned_pool(
    core: &StateLock<BotState>,
    key: &V4PoolKey,
    pool_id: B256,
    pinned: &PinnedPoolState,
) -> u64 {
    let mut state = core.write_at(LockSite::Solver);
    state
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: POOL_MANAGER,
            pool_id: pool_id.0,
            pool_key: key.clone(),
            hook_flags: hook_flags_of(LIVE_HOOK),
            protocol_fee: pinned.protocol_fee,
            sqrt_price_x96: pinned.sqrt_price_x96,
            liquidity: pinned.liquidity.to::<u128>(),
            tick: pinned.tick,
            tick_data: HashMap::new(),
            update_block: LIVE_PIN,
            tick_data_block: Some(LIVE_PIN),
            coverage: PoolTickCoverage::Tracked,
            ..Default::default()
        })
        .expect("the pinned hooked pool registers")
}

/// One registered path per direction, through the PRODUCTION registration seam:
/// the hooked hop in the direction under test, then the reverse hop (a two-hop
/// path is the shortest `register_path` admits).
fn register_round_trip(stages: &EngineStages, registered: u64) -> (u64, u64) {
    let path = |zero_for_one: bool| {
        stages
            .register_path(vec![
                PoolHop {
                    pool_id: registered,
                    zero_for_one,
                },
                PoolHop {
                    pool_id: registered,
                    zero_for_one: !zero_for_one,
                },
            ])
            .expect("the round-trip path registers")
            .0
    };
    (path(true), path(false))
}

/// Quote ONE direction through the CORE and hold its whole request vector
/// against the INDEPENDENTLY hand-encoded canonical-quoter `eth_call`s at the
/// same block (see
/// [`live_hooked_v4_quote_matches_the_canonical_quoter_both_directions`]).
async fn assert_direction_parity(
    provider: &AlloyProvider,
    stages: &EngineStages,
    path_id: u64,
    key: &V4PoolKey,
    white_box: &PoolEntry,
    zero_for_one: bool,
    pinned_answer: u128,
) {
    let transport = RecordingTransport::new(provider);
    let quote = stages
        .quote_exact_input_path(&transport, path_id, U256::from(LIVE_AMOUNT_IN), LIVE_PIN)
        .await
        .expect("the core quotes the hooked hops at the pin");

    // ── the core's whole request vector ──
    assert_eq!(quote.path_id, path_id);
    assert_eq!(quote.hop_outputs.len(), LIVE_HOPS, "one hop, one output");
    assert_eq!(
        quote.consumed_inputs,
        vec![U256::from(LIVE_AMOUNT_IN), quote.hop_outputs[0]],
        "hop 1 consumes the fixed input, hop 2 consumes hop 1's output"
    );
    assert_eq!(
        quote.hop_outputs[0],
        U256::from(pinned_answer),
        "the fixed input's hop must be the pinned answer (zfo={zero_for_one})"
    );
    assert_quote_context(&quote);

    // ── gates 2 and 3: per hop, the calldata the core emitted, and the
    //    INDEPENDENT raw eth_call at the same block ──
    let calls = transport.calls();
    assert_eq!(
        calls.len(),
        LIVE_HOPS,
        "one concentrated hop makes one quoter call"
    );
    let hops = [
        (zero_for_one, U256::from(LIVE_AMOUNT_IN)),
        (!zero_for_one, quote.consumed_inputs[1]),
    ];
    let mut independent: Vec<U256> = Vec::with_capacity(LIVE_HOPS);
    for (hop, (direction, amount_in)) in hops.into_iter().enumerate() {
        assert_eq!(calls[hop].0, V4_QUOTER, "the canonical v4-periphery quoter");
        assert_eq!(
            calls[hop].2, LIVE_PIN,
            "the call is pinned to the quote's block"
        );
        let hand_encoded = v4_calldata(key, direction, amount_in);
        assert_eq!(
            calls[hop].1.as_ref(),
            hand_encoded.as_slice(),
            "the core's calldata must be the hand-encoded call, byte for byte (hop {hop})"
        );
        assert_eq!(
            &hand_encoded[4 + 5 * 32 + 12..4 + 6 * 32],
            LIVE_HOOK.as_slice(),
            "the hooked pool's key carries its hook at the quoter"
        );

        let returned = ContractRead::call(
            provider,
            V4_QUOTER,
            Bytes::from(hand_encoded),
            Some(LIVE_PIN),
        )
        .await
        .expect("the canonical quoter answers the hooked key at the pin");
        let amount_out = U256::from_be_slice(&returned[0..32]);
        let gas_estimate = U256::from_be_slice(&returned[32..64]);
        println!(
            "  zfo={direction:<5} hop {hop} core hop_outputs={} independent amountOut={amount_out} gasEstimate={gas_estimate} → PARITY",
            quote.hop_outputs[hop],
        );
        independent.push(amount_out);
    }
    assert_eq!(
        quote.hop_outputs, independent,
        "the core's whole vector must BE the chain's answers (zfo={zero_for_one})"
    );

    // ── gates 4 and 5: hook-specific, and never the unhooked walk ──
    assert_unhooked_twin_is_refused(provider, key, zero_for_one).await;
    assert_white_box_differs(white_box, zero_for_one, quote.hop_outputs[0]);
}

/// LIVE, opt-in (never in an offline gate): one REAL mainnet hooked V4 pool,
/// initialized at a pinned block, quoted through the CORE
/// (`EngineStages::quote_exact_input_path`) and compared against an
/// INDEPENDENT canonical-quoter `eth_call` at the same block, in BOTH
/// directions.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 archive RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_hooked_v4_quote_matches_the_canonical_quoter_both_directions() {
    let rpc_url = std::env::var(LIVE_RPC_ENV).unwrap_or_else(|_| {
        panic!("{LIVE_RPC_ENV} must name a chain-1 archive endpoint (just test-fork exports it)")
    });
    let provider = AlloyProvider::from_provider(Arc::new(
        alloy::providers::ProviderBuilder::default()
            .connect_http(rpc_url.parse().expect("the RPC URL parses")),
    ));

    let key = live_pool_key();
    let pool_id = degenbot_pools::v4_pool_id(&key);
    assert_eq!(
        alloy::hex::encode(pool_id),
        LIVE_POOL_ID,
        "the key must hash to the census row's own pool id"
    );

    // ── gate 1: the pool is initialized AT the pin, proven by the read ──
    let pinned = read_pinned_pool_state(&provider, pool_id).await;
    assert!(
        pinned.sqrt_price_x96 > U256::ZERO,
        "the pool must be INITIALIZED at {LIVE_PIN}: slot0.sqrtPriceX96 was zero"
    );
    assert!(
        pinned.liquidity > U256::ZERO,
        "the pool must hold in-range liquidity at {LIVE_PIN}"
    );
    println!(
        "\n▸ live hooked-V4 quote proof\n  node            {rpc_url}\n  block           {LIVE_PIN}\n  pool id         0x{}\n  pool key        currency0={} currency1={} fee={} tickSpacing={} hooks={}\n  pinned slot0    0x{:064x}\n  decoded         sqrtPriceX96={} tick={} protocolFee={} liquidity={}\n  amount in       {LIVE_AMOUNT_IN}",
        alloy::hex::encode(pool_id),
        key.currency0,
        key.currency1,
        key.fee,
        key.tick_spacing,
        key.hooks,
        pinned.slot0,
        pinned.sqrt_price_x96,
        pinned.tick,
        pinned.protocol_fee,
        pinned.liquidity,
    );
    assert_pinned_block(&provider).await;

    // The engine over the state the chain just reported, then the two
    // direction paths through the production registration seam.
    let core = Arc::new(StateLock::new(BotState::new()));
    let registered = register_pinned_pool(&core, &key, pool_id, &pinned);
    let white_box = {
        let state = core.read_at(LockSite::Solver);
        state
            .pool_entry(registered)
            .cloned()
            .expect("the registered hooked pool has an entry")
    };
    let stages = EngineStages::with_core(Arc::clone(&core), Arc::new(EpochDelta::new(0u64)));
    let (zero_for_one_path, one_for_zero_path) = register_round_trip(&stages, registered);

    for (path_id, zero_for_one, pinned_answer) in [
        (zero_for_one_path, true, LIVE_OUT_ZERO_FOR_ONE),
        (one_for_zero_path, false, LIVE_OUT_ONE_FOR_ZERO),
    ] {
        assert_direction_parity(
            &provider,
            &stages,
            path_id,
            &key,
            &white_box,
            zero_for_one,
            pinned_answer,
        )
        .await;
    }
    println!("▸ live hooked-V4 quote proof: PASS (both directions)\n");
}
