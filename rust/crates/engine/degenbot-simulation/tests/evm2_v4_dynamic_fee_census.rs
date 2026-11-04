//! The dynamic-fee fee census — what the hooked zero-fee pools actually charge.
//!
//! The population is the 32,358 mainnet V4 pools keyed at `fee = 0` with a
//! non-zero hook — the pools registration refuses today. A pool keyed at fee 0
//! has no LP fee for a hook to override: the hook IS the fee, carried as a
//! `beforeSwap`/`afterSwap` swap delta (every address in this population carries
//! the amount-modifying flag bits, `hooks & 0xCC != 0`). The search tier cannot
//! model that, so phase 2's one open decision — the search-time fee policy (F2)
//! — is a measurement question, and this is the measurement.
//!
//! # What this rig answers
//!
//! 1. **Parity** ([`parity_census_dynamic_fee_hook_legs`]): the black-box tier
//!    prices a dynamic-fee pool exactly as the node does. The H1 primitive
//!    (deployed `Quoter` over an anchor-pinned scratch) vs the node's `eth_call`
//!    of the same calldata at the same anchor, both directions, a spread of
//!    amounts, to the wei.
//! 2. **What the hooks charge** ([`implied_fee_distribution_per_hook`]): the
//!    implied fee per pool, and its per-hook distribution across the census.
//! 3. **The derivation's own accuracy**
//!    ([`implied_fee_derivation_validated_on_static_fee_pools`]): the same
//!    estimator over hookless pools whose static fee is readable off the key must
//!    reproduce that fee. The residual is the walk-curvature bias, and it bounds
//!    the numbers in (2) at the same amount scale.
//! 4. **Amount dependence** ([`implied_fee_amount_dependence`]): three amount
//!    scales at one anchor.
//! 5. **Time dependence** ([`implied_fee_time_dependence`]): five anchors over
//!    ~one hour of blocks.
//! 6. **Where a static pool's LP fee comes from**
//!    ([`slot0_lp_fee_word_is_the_static_pool_swap_fee`]): the deployed manager
//!    charges `calculateSwapFee(protocol_fee_dir, slot0.lpFee)`, so rewriting
//!    that word alone — a reth `stateDiff` `eth_call` and the evm2 twin, the
//!    override read back through the manager's own `extsload` — moves the quote
//!    to the wei on both arms. A key-fee white-box is exact on a static pool
//!    because `initialize` seeds the word from `key.fee` and only a dynamic-fee
//!    pool's hook may move it.
//!
//! # The implied-fee derivation
//!
//! A fee `f` on an exact-input swap enters as `in·(1 − f)` handed to the curve,
//! so the output-side equivalent fee is
//!
//! ```text
//! implied = 1 − out(as traded) / out(same walk, nothing charged)
//! ```
//!
//! The reference walk is the SAME pool at the SAME anchor with the hook's code
//! replaced by a no-op ([`NOOP_HOOK`]) that answers every callback with
//! `(BeforeSwapDelta(0), lpFeeOverride 0)`. Because the pool key's own fee is
//! zero, that twin is exactly the pool's own curve — same tick data, same
//! sqrtPrice, same liquidity — with nothing charged. (`measure` refuses a twin
//! whose frame touches storage at the hook address, so a botched override cannot
//! masquerade as a fee.)
//!
//! Three independent checks keep the number honest:
//!
//! - the static-fee controls (3) reproduce a fee read off the pool key itself.
//!   Their fee-free reference is the engine's own chain-verified swap math over
//!   the pool's state at fee 0. The control's closed form is counted only where
//!   it reproduces the chain's answer TO THE WEI at the same fee, so the
//!   reference is the chain's own walk, not an assumption about it. On a static
//!   pool the key's `fee` and the `slot0.lpFee` word are the same figure, and the
//!   word is the one the manager reads — (6) proves it by moving the quote
//!   through an override;
//! - the frame's settled `slot0.lpFee` word is reported beside every reading: a
//!   hook that charges through the pool's LP-fee field lands there, one that
//!   charges through a swap delta leaves it zero — the mechanism is read out of
//!   the data, not assumed;
//! - a charge is a ratio of two walks over identical state, so its sign is
//!   meaningful: a hook that rebates reads negative.
//!
//! # Reading the numbers
//!
//! `measured` means both sides answered; `refused` means the leg cannot be priced
//! at all (this population's hooks refuse whole swap directions: a launch-pad
//! hook's sell side reverts inside `beforeSwap`, and the deployed quoter inherits
//! the refusal); `twin-refused` means the hookless twin refused (an abnormal hook,
//! reported rather than smoothed over); `thin` means no in-range liquidity or a
//! zero-output walk — nothing to price. Every count is reported, never silently
//! dropped.
//!
//! # Running it
//!
//! ```sh
//! cargo test -p degenbot-simulation --test evm2_v4_dynamic_fee_census \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Each test is its own census over the shared fixture (they carry no state
//! between them), so a partial run is a partial report, never a wrong one.
//! `DEGENBOT_RPC_HTTP_CHAINID_1` (or `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`)
//! selects the node; the anchors need archive state.

#![expect(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines
)]

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use alloy::eips::BlockId;
use alloy::primitives::{address, b256, keccak256, Address, Bytes, B256, I256, U256};
use alloy::rpc::types::state::AccountOverride;
use alloy::rpc::types::TransactionRequest;

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, CacheDB, Database, Db};

use degenbot_math::concentrated_liquidity::sqrt_price_math::{
    get_amount0_delta, get_amount1_delta,
};
use degenbot_math::concentrated_liquidity::swap_math::{
    calculate_swap_fee, compute_swap_step_v4, protocol_fee_one_for_zero, protocol_fee_zero_for_one,
};
use degenbot_math::concentrated_liquidity::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_pools::{v4_pool_id, V4PoolKey};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_simulation::sim::evm2::frame_replay::{
    Evm2CountingFrameDb, Evm2ReplayOutcome, Evm2ScratchEvm,
};
use degenbot_simulation::sim::evm2::v4_black_box::{
    decode_quote_return, leg_state_slots, quote_calldata, V4BlackBoxQuote, V4BlackBoxQuoter, V4Leg,
    V4QuoteError, V4QuoterConfig, V4_POOL_MANAGER, V4_QUOTER, V4_QUOTE_GAS_LIMIT,
    V4_QUOTE_MAX_FEE_PER_GAS, V4_QUOTE_MAX_PRIORITY_FEE_PER_GAS,
};
use degenbot_simulation::sim::frame_replay::{FrameRpcCounter, ReplayableTx, ScratchBlock};
use degenbot_simulation::sim::RunVerdict;

// ─────────────────────────────────────────────────────────────────────────
// The anchors, the census, and the fee controls
// ──────────────────────────────────────────────────────────────────────────

/// The archived block the census pins: past the registration DB's last V4
/// liquidity update, so every census pool is live, and deep enough in the
/// archive window to be cheap to re-read (H1's anchor).
const PIN: u64 = 26_320_000;

/// Mainnet's 12 s blocks: 300 blocks is ~one hour, sampled at five anchors.
const HOUR_BLOCKS: u64 = 300;

/// Anchors per hour-long time-dependence sample (five even divisions of
/// [`HOUR_BLOCKS`], [`TIME_STEP`] blocks apart).
const TIME_SAMPLES: usize = 5;

/// The block gap between two time-dependence anchors (`HOUR_BLOCKS / 4`).
const TIME_STEP: u64 = HOUR_BLOCKS / 4;

/// The seed the census fixture's long-tail draw was generated with.
const SAMPLE_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// `hook  currency0  currency1  fee  tick_spacing  pool_id` — one line per
/// candidate pool, grouped by hook in census order.
const CENSUS: &str = include_str!("fixtures/v4_dynamic_fee_census.tsv");

/// `decimal()` — the only token-metadata read the census needs (to scale each
/// amount to the input currency).
const DECIMALS_CALLDATA: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];

/// The bits of `Slot0` holding `lpFee` (`[208..232)`, the v4-core packing
/// `degenbot_pools::v4_slot0_slot` documents).
const LP_FEE_MASK: U256 = U256::from_limbs([0, 0, 0, 0xFF_FFFFu64 << 16]);

/// The static-fee controls' currencies.
const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
const DAI: Address = address!("6B175474E89094C44Da98b954EedeAC495271d0F");

/// One census row.
struct CensusRow {
    hook: Address,
    key: V4PoolKey,
}

/// Parse the census, re-deriving each pool id from its key (the row's integrity
/// check: a stale or mistyped row fails before any RPC read).
fn census() -> Vec<CensusRow> {
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
        assert_eq!(
            v4_pool_id(&key),
            pinned,
            "census row's key does not hash to its recorded pool id: {line:?}"
        );
        assert!(
            !hook.is_zero(),
            "the census is the HOOKED population: {line:?}"
        );
        rows.push(CensusRow { hook, key });
    }
    assert!(!rows.is_empty(), "the census fixture carries rows");
    rows
}

/// The census's hooks, in census order (the four most-populous hooks first).
fn census_hooks(rows: &[CensusRow]) -> Vec<Address> {
    let mut hooks: Vec<Address> = Vec::new();
    for row in rows {
        if !hooks.contains(&row.hook) {
            hooks.push(row.hook);
        }
    }
    hooks
}

/// The census's four most-populous hooks, most populous first (a tie breaks by
/// address, so the order is deterministic).
fn top_hooks(rows: &[CensusRow]) -> Vec<Address> {
    let mut counted: Vec<(Address, usize)> = Vec::new();
    for row in rows {
        match counted.iter_mut().find(|(hook, _)| *hook == row.hook) {
            Some((_, count)) => *count += 1,
            None => counted.push((row.hook, 1)),
        }
    }
    counted.sort_by(|(hook_a, count_a), (hook_b, count_b)| {
        count_b.cmp(count_a).then(hook_a.cmp(hook_b))
    });
    counted.into_iter().take(4).map(|(hook, _)| hook).collect()
}

/// The first census pool of each of the four most-populous hooks — the amount-
/// and time-dependence subjects (one per hook family, so a per-hook quirk cannot
/// masquerade as a global law).
fn one_pool_per_top_hook(rows: &[CensusRow]) -> Vec<&CensusRow> {
    let hooks = top_hooks(rows);
    hooks
        .iter()
        .filter_map(|hook| rows.iter().find(|row| row.hook == *hook))
        .collect()
}

/// Hookless pools with a static fee readable off the key — the derivation's
/// controls, one per fee tier, each a live mainnet pool at the anchor:
/// `(currency0, currency1, fee, tick_spacing, pool_id)`.
const STATIC_FEE_CONTROLS: [(Address, Address, u32, i32, B256); 4] = [
    (
        DAI,
        USDC,
        100,
        1,
        b256!("d967702f17f83d907b36e66c9a62eb50ac327432c581d5b273a76519692434be"),
    ),
    (
        Address::ZERO,
        USDC,
        500,
        10,
        b256!("21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27"),
    ),
    (
        Address::ZERO,
        USDC,
        3_000,
        60,
        b256!("dce6394339af00981949f5f3baf27e3610c76326a700af57e4b3e3ae4977f78d"),
    ),
    (
        Address::ZERO,
        USDC,
        10_000,
        200,
        b256!("d934712639fede326a3ff8d2a9c2f73749bac510c23fb33443cea7f3c9aca6f3"),
    ),
];

/// The no-op hook a pool's twin runs: every callback answers ZERO.
///
/// The shape is not free. The deployed manager validates each hook callback's
/// return (`InvalidHookResponse` — selector `0x1e048e1d`, observed here when the
/// stub answered a bare zero word): a callback must return its own selector in
/// the first word, `beforeSwap` is exactly 96 bytes (`selector || delta ||
/// lpFeeOverride`) and a delta-returning `afterSwap` is exactly 64 bytes
/// (`selector || delta`). So the stub dispatches on `msg.sig`:
///
/// ```text
/// selector = calldataload(0) >> 224
/// if selector == afterSwap (0xb47b2fb1): mstore(0, afterSwap << 224);  return 64 bytes
/// else:                                  mstore(0, beforeSwap << 224); return 96 bytes
/// ```
///
/// Both bodies are zero beyond the selector word, i.e. `BeforeSwapDelta(0)`,
/// `lpFeeOverride(0)` and `hookDelta(0)` — the pool's walk with nothing charged
/// and no hook storage read at all (which `measure` asserts).
const NOOP_HOOK: [u8; 48] = [
    // calldataload(0) >> 224
    0x60, 0x00, 0x35, 0x60, 0xE0, 0x1C, //
    // selector == afterSwap? jump to the 64-byte body
    0x63, 0xB4, 0x7B, 0x2F, 0xB1, 0x14, 0x60, 0x1F, 0x57, //
    // beforeSwap body: mstore(0, beforeSwap << 224); return (0, 96)
    0x63, 0x57, 0x5E, 0x24, 0xB4, 0x60, 0xE0, 0x1B, 0x60, 0x00, 0x52, 0x60, 0x60, 0x60, 0x00, 0xF3,
    // JUMPDEST: mstore(0, afterSwap << 224); return (0, 64)
    0x5B, 0x63, 0xB4, 0x7B, 0x2F, 0xB1, 0x60, 0xE0, 0x1B, 0x60, 0x00, 0x52, 0x60, 0x40, 0x60, 0x00,
    0xF3,
];

// ─────────────────────────────────────────────────────────────────────────
// The anchor
// ──────────────────────────────────────────────────────────────────────────

/// The census stack's ext: forwarded reads are the anchor's own storage.
type AnchorExt<'p> = CacheDB<Db<Evm2CountingFrameDb<BlockingAsyncDb<'p>>>>;

/// One anchor: the node, the cold-read counter, and an anchor-pinned scratch
/// (whose read cache IS the anchor's storage memo — nothing is carried across
/// anchors).
struct Anchor<'p> {
    provider: &'p AlloyProvider,
    counter: Arc<FrameRpcCounter>,
    scratch: Evm2ScratchEvm<AnchorExt<'p>>,
    number: u64,
    timestamp: u64,
    base_fee: u128,
}

impl<'p> Anchor<'p> {
    /// Open `number`, reading its header (timestamp + base fee) so the frame env
    /// is the anchor's own.
    async fn open(provider: &'p AlloyProvider, number: u64) -> Self {
        let block = provider
            .get_block(number)
            .await
            .unwrap()
            .expect("the anchor block is archived");
        let timestamp = block.header.timestamp;
        let base_fee = u128::from(block.header.base_fee_per_gas.unwrap_or(0));
        let scratch_block = ScratchBlock {
            number,
            timestamp,
            base_fee_next: base_fee,
        };
        let counter = Arc::new(FrameRpcCounter::default());
        let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(provider, number))
            .expect("a multi-threaded runtime is ambient");
        let ext = CacheDB::new(Db::new(Evm2CountingFrameDb::new(
            blocking,
            Arc::clone(&counter),
        )));
        Self {
            provider,
            scratch: Evm2ScratchEvm::with_counter(ext, scratch_block, Arc::clone(&counter)),
            counter,
            number,
            timestamp,
            base_fee,
        }
    }

    /// The pool's in-range liquidity — the computable `PoolManager` word. A pool
    /// with none cannot price anything (both sides agree on a revert or zero), so
    /// it is reported as thin, never silently dropped.
    fn liquidity(&mut self, key: &V4PoolKey) -> U256 {
        let slot = leg_state_slots(key)[1];
        self.scratch
            .ext_mut()
            .get_storage(&V4_POOL_MANAGER, &slot)
            .unwrap_or(U256::ZERO)
    }

    /// The pool's `slot0` word (price, tick, protocol fee, LP fee).
    fn slot0(&mut self, key: &V4PoolKey) -> U256 {
        let slot = leg_state_slots(key)[0];
        self.scratch
            .ext_mut()
            .get_storage(&V4_POOL_MANAGER, &slot)
            .unwrap_or(U256::ZERO)
    }

    /// The typed primitive's quote (the H1 path, manager binding checked).
    fn quote(
        &mut self,
        quoter: &mut V4BlackBoxQuoter,
        leg: &V4Leg,
        amount_in: U256,
    ) -> Result<V4BlackBoxQuote, V4QuoteError> {
        quoter.quote(&mut self.scratch, leg, amount_in)
    }

    /// The same frame the typed quoter builds, hand-built, because the fee census
    /// also wants the frame's settled slot set (which [`V4BlackBoxQuote`] does not
    /// surface): `slot0`'s post-frame `lpFee` word is how a hook charging through
    /// the pool's LP-fee field is told apart from one charging through a swap
    /// delta.
    fn fee_frame(&mut self, leg: &V4Leg, amount_in: U256) -> Result<FrameQuote, String> {
        let transaction = ReplayableTx {
            from: Address::ZERO,
            to: Some(V4_QUOTER),
            value: U256::ZERO,
            data: quote_calldata(leg, amount_in),
            gas_limit: V4_QUOTE_GAS_LIMIT,
            max_fee_per_gas: V4_QUOTE_MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: V4_QUOTE_MAX_PRIORITY_FEE_PER_GAS,
            nonce: 0,
        };
        let outcome = self
            .scratch
            .replay(&transaction)
            .map_err(|error| format!("frame: {error}"))?;
        let amount_out = match outcome.status {
            RunVerdict::Success => {
                decode_quote_return(&outcome.output)
                    .ok_or_else(|| format!("malformed return ({} bytes)", outcome.output.len()))?
                    .0
            }
            RunVerdict::Reverted => {
                return Err(format!("reverted: {}", reverts::class(&outcome.output)))
            }
            RunVerdict::Halted => {
                return Err(format!("halted: {}", reverts::class(&outcome.output)))
            }
        };
        Ok(FrameQuote {
            amount_out,
            settled_lp_fee: settled_lp_fee(&outcome, &leg.pool_key),
            hook_slots_touched: outcome
                .touched
                .iter()
                .filter(|(address, _)| *address == leg.pool_key.hooks)
                .map(|(_, slots)| slots.len())
                .sum(),
            gas_used: outcome.gas_used,
        })
    }

    /// Replace the hook's code with [`NOOP_HOOK`], returning the account it
    /// displaced (the real nonce/balance ride along, so a hook reading its own
    /// account still sees the chain's answer).
    fn neuter_hook(&mut self, hook: Address) -> Option<AccountInfo> {
        let saved = self.scratch.ext_mut().get_account(&hook).ok().flatten();
        let mut stub =
            AccountInfo::default().with_code(Bytecode::new_raw(Bytes::from_static(&NOOP_HOOK)));
        if let Some(real) = saved.as_ref() {
            stub.nonce = real.nonce;
            stub.balance = real.balance;
        }
        self.scratch.ext_mut().insert_account_info(&hook, stub);
        saved
    }

    /// Put the real hook code back (the scratch outlives one pool's measurement,
    /// and the pools of one hook share the hook address).
    fn restore_hook(&mut self, hook: Address, saved: Option<AccountInfo>) {
        if let Some(real) = saved {
            self.scratch.ext_mut().insert_account_info(&hook, real);
        }
    }

    /// Rewrite one `PoolManager` storage word in the anchor's scratch — the evm2
    /// twin of a `stateDiff` override: the cache IS the state every frame reads,
    /// so the next quote sees the rewritten word and nothing else changed.
    fn put_storage(&mut self, slot: U256, word: U256) {
        self.scratch
            .ext_mut()
            .insert_account_storage(&V4_POOL_MANAGER, &slot, &word);
    }

    /// The node's answer for `data` at the anchor, with `slot` in the
    /// `PoolManager`'s state re-pointed to `word` (a reth `stateDiff` override).
    async fn node_call_state_diff(
        &self,
        to: &Address,
        data: Bytes,
        slot: U256,
        word: U256,
    ) -> Result<Bytes, String> {
        let diff = [(
            B256::from(slot.to_be_bytes::<32>()),
            B256::from(word.to_be_bytes::<32>()),
        )]
        .into_iter()
        .collect();
        self.provider
            .provider_arc()
            .call(TransactionRequest::default().to(*to).input(data.into()))
            .block(BlockId::number(self.number))
            .account_override(
                V4_POOL_MANAGER,
                AccountOverride {
                    state_diff: Some(diff),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| format!("stateDiff eth_call: {error}"))
    }

    /// What the hook charges for one leg at `amount_in`: the real frame against
    /// the no-op-hook twin on the same walk.
    fn measure(
        &mut self,
        row: &CensusRow,
        zero_for_one: bool,
        amount_in: U256,
    ) -> Result<FeePoint, String> {
        let leg = V4Leg::canonical(row.key.clone(), zero_for_one);
        let traded = self.fee_frame(&leg, amount_in)?;
        let saved = self.neuter_hook(row.hook);
        let twin = self.fee_frame(&leg, amount_in);
        self.restore_hook(row.hook, saved);
        let twin = twin.map_err(|error| format!("twin {error}"))?;
        if twin.hook_slots_touched != 0 {
            return Err(format!(
                "the no-op hook left {} slot(s) at the hook address (the override did not land)",
                twin.hook_slots_touched
            ));
        }
        Ok(FeePoint {
            amount_out: traded.amount_out,
            hookless_out: twin.amount_out,
            hundredths_bp: implied_hundredths_bp(traded.amount_out, twin.amount_out),
            lp_fee_word: traded.settled_lp_fee,
            gas_used: traded.gas_used,
        })
    }

    /// The node's answer for the identical calldata at the identical block tag.
    async fn node_quote(&self, leg: &V4Leg, amount_in: U256) -> Result<U256, String> {
        let output = self
            .provider
            .eth_call(
                &V4_QUOTER,
                quote_calldata(leg, amount_in),
                Some(self.number),
            )
            .await
            .map_err(|_| "revert".to_string())?;
        match decode_quote_return(&output) {
            Some((amount_out, _)) => Ok(amount_out),
            None => Err(format!("answered {} bytes", output.len())),
        }
    }
}

/// One frame's answer, with the two fee-mechanism diagnostics.
struct FrameQuote {
    amount_out: U256,
    /// The settled `lpFee` field of the pool's `slot0` after the frame (`0` when
    /// the pool did not touch the word, or the hook charges by delta).
    settled_lp_fee: u32,
    /// How many slots at the hook's address the frame touched (`0` for the no-op
    /// twin — the override's own check).
    hook_slots_touched: usize,
    gas_used: u64,
}

/// One measured walk: the fee the hook charged, as the output-side equivalent.
struct FeePoint {
    amount_out: U256,
    hookless_out: U256,
    /// The charge in hundredths of a bip (1 bp = 1e-4; this unit is 1e-6), signed
    /// so a hook that rebates reads negative instead of wrapping.
    hundredths_bp: Option<i64>,
    lp_fee_word: u32,
    gas_used: u64,
}

/// `1 − traded/hookless` in hundredths of a bip. `None` when the twin output is
/// zero (nothing to take a ratio against — a walk the curve rounds away).
fn implied_hundredths_bp(amount_out: U256, hookless_out: U256) -> Option<i64> {
    if hookless_out.is_zero() {
        return None;
    }
    let scale = U256::from(1_000_000u64);
    let (diff, negative) = if hookless_out >= amount_out {
        (hookless_out - amount_out, false)
    } else {
        (amount_out - hookless_out, true)
    };
    let scaled = (diff * scale / hookless_out).min(U256::from(u64::MAX));
    let magnitude = i64::try_from(scaled.saturating_to::<u64>()).unwrap_or(i64::MAX);
    Some(if negative { -magnitude } else { magnitude })
}

/// A fee in hundredths of a bip, rendered as `123.45` — integer math only (a fee
/// is never worth a float).
fn render_bps(hundredths: i64) -> String {
    let sign = if hundredths < 0 { "-" } else { "" };
    let magnitude = hundredths.unsigned_abs();
    format!("{sign}{}.{:02}", magnitude / 100, magnitude % 100)
}

/// The middle value of a set of fee readings (sorts in place).
fn median(values: &mut [i64]) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

/// The settled `slot0` word's `lpFee` field after `outcome`, if the frame touched
/// (read or wrote) the pool's `slot0` at all.
fn settled_lp_fee(outcome: &Evm2ReplayOutcome, key: &V4PoolKey) -> u32 {
    let slot0_slot = leg_state_slots(key)[0];
    for (address, slot, value) in &outcome.slot_values {
        if *address == V4_POOL_MANAGER && *slot == slot0_slot {
            let word: U256 = *value;
            return ((word & LP_FEE_MASK) >> 208usize).saturating_to::<u32>();
        }
    }
    0
}

/// The `slot0` fields the pool's curve needs, decoded from the packed word
/// (v4-core `types/Slot0.sol`: `sqrtPriceX96 | tick | protocolFee | lpFee`).
struct Slot0View {
    sqrt_price: U256,
    tick: i32,
    protocol_fee: u32,
    lp_fee: u32,
}

/// Decode a `slot0` word.
fn slot0_view(word: U256) -> Slot0View {
    let field = |shift: usize| -> u32 {
        ((word >> shift) & U256::from(0xFF_FFFFu32)).saturating_to::<u32>()
    };
    let tick = i32::try_from(field(160)).unwrap_or(i32::MAX);
    Slot0View {
        sqrt_price: word & ((U256::from(1u8) << 160usize) - U256::from(1u8)),
        tick: if tick >= 1 << 23 {
            tick - (1 << 24)
        } else {
            tick
        },
        protocol_fee: field(184),
        lp_fee: field(208),
    }
}

/// `word` with its `lpFee` field replaced — every other packed field untouched.
fn with_lp_fee(word: U256, lp_fee: u32) -> U256 {
    (word & !LP_FEE_MASK) | ((U256::from(lp_fee) & U256::from(0xFF_FFFFu32)) << 208usize)
}

/// The `lpFee` field of a `slot0` word.
fn lp_fee_of(word: U256) -> u32 {
    ((word & LP_FEE_MASK) >> 208usize).saturating_to::<u32>()
}

/// `PoolManager.extsload(bytes32)` calldata for `slot` — the read-back a
/// `stateDiff` override lands in (the selector is derived, not pasted, so a
/// signature typo cannot masquerade as a landed override).
fn extsload_calldata(slot: U256) -> Bytes {
    let mut out = Vec::with_capacity(4 + 32);
    out.extend_from_slice(&keccak256(b"extsload(bytes32)")[..4]);
    out.extend_from_slice(&slot.to_be_bytes::<32>());
    Bytes::from(out)
}

/// One exact-input step of the pool's OWN curve at `fee_pips`: the engine's
/// chain-verified V4 swap math ([`compute_swap_step_v4`]) over the anchor's
/// `sqrtPrice`/`liquidity`, with the current tick range's boundary as the step
/// target. A walk that exhausts its input before that boundary is
/// target-independent, which is exactly why the control asserts the closed form
/// reproduces the chain's answer to the wei before it trusts the reference.
fn curve_step_out(
    view: &Slot0View,
    liquidity: U256,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_in: U256,
    fee_pips: u32,
) -> Option<U256> {
    let spacing = tick_spacing.max(1);
    let lower = view.tick.div_euclid(spacing) * spacing;
    let boundary = if zero_for_one { lower } else { lower + spacing };
    let target = U256::from(get_sqrt_ratio_at_tick_internal(boundary).ok()?);
    let liquidity = i128::try_from(liquidity).ok()?;
    let remaining = -I256::try_from(amount_in).ok()?;
    let step = compute_swap_step_v4(
        view.sqrt_price,
        target,
        liquidity,
        remaining,
        U256::from(fee_pips),
    )
    .ok()?;
    Some(step.amount_out)
}

/// Revert payload classification: the deployed quoter wraps an inner revert in
/// `UnexpectedRevertBytes(bytes)`, and the inner selector is the hook family's own
/// error — the census reports it rather than flattening every refusal into
/// "revert".
mod reverts {
    use alloy::primitives::U256;

    /// `UnexpectedRevertBytes(bytes)` (`cast sig`).
    const UNEXPECTED_REVERT_BYTES: [u8; 4] = [0x61, 0x90, 0xb2, 0xb0];

    /// `outer→inner` selector tags, or the bare outer selector when the payload is
    /// not the wrapped-bytes shape.
    pub(super) fn class(data: &[u8]) -> String {
        let outer = data
            .get(..4)
            .map_or_else(|| "empty".to_string(), alloy::hex::encode);
        if data.get(..4) == Some(&UNEXPECTED_REVERT_BYTES) {
            if let Some(inner) = wrapped_bytes(data).and_then(|payload| payload.get(..4)) {
                return format!("{outer}→{}", alloy::hex::encode(inner));
            }
        }
        outer
    }

    /// A typed quote error's class tag.
    pub(super) fn of_error(error: &super::V4QuoteError) -> String {
        match error {
            super::V4QuoteError::Reverted { data_hex, .. } => {
                class(&alloy::hex::decode(data_hex).unwrap_or_default())
            }
            super::V4QuoteError::Halted { .. } => "halt".to_string(),
            other => format!("frame: {other}"),
        }
    }

    /// The `bytes` payload of a `bytes`-taking custom error (`selector || offset ||
    /// length || payload`).
    fn wrapped_bytes(data: &[u8]) -> Option<&[u8]> {
        let head = data.get(4..36)?;
        let offset = usize::try_from(U256::from_be_slice(head)).ok()?;
        let length_at = 4usize.checked_add(offset)?;
        let length =
            usize::try_from(U256::from_be_slice(data.get(length_at..length_at + 32)?)).ok()?;
        let body_at = length_at + 32;
        data.get(body_at..body_at.checked_add(length)?)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Amount scales + token metadata
// ──────────────────────────────────────────────────────────────────────────

/// `10^(decimals − drop)` for each `drop`, floored at `10^0`.
fn scales(decimals: u32, drop: &[u32]) -> Vec<U256> {
    let d = decimals.min(30);
    drop.iter()
        .map(|below| {
            let exponent = d.saturating_sub(*below);
            U256::from(10u64).pow(U256::from(exponent))
        })
        .collect()
}

/// The parity spread for a currency of `decimals`: two orders of magnitude below
/// a whole unit, one below, and a whole unit (H1's ladder, kept identical so the
/// two censuses compare).
fn parity_ladder(decimals: u32) -> Vec<U256> {
    scales(decimals, &[2, 1, 0])
}

/// The amount that walks the pool's own price by ~1 bip, derived from its
/// `slot0`/`liquidity` — the fee census's unit of scale. An ABSOLUTE token scale
/// cannot serve: 1e-2 of a unit is dust on a deep pool (the output quantizes and
/// the ratio's resolution collapses) and a price shock on a thin one (the walk
/// crosses ticks and the ratio stops being a fee). Every fee reading is
/// therefore taken at a relative depth the pool itself defines.
fn one_bip_amount(view: &Slot0View, liquidity: U256, zero_for_one: bool) -> Option<U256> {
    let liquidity = i128::try_from(liquidity).ok()?;
    // A 1 bip price move is a 5e-5 move in sqrt price (to first order), in the
    // direction the leg walks.
    let shift = U256::from(50_000u64);
    let denominator = U256::from(1_000_000u64);
    if zero_for_one {
        let next = view.sqrt_price * (denominator - shift) / denominator;
        get_amount0_delta(next, view.sqrt_price, liquidity, Some(true)).ok()
    } else {
        let next = view.sqrt_price * (denominator + shift) / denominator;
        get_amount1_delta(view.sqrt_price, next, liquidity, Some(true)).ok()
    }
}

/// The census's fee-derivation scales: 1/100, 1/10 and one whole "bip of price
/// impact" amount (`one_bip_amount`), floored at one base unit — a 100× ladder in
/// trade size whose shallowest rung is well inside the walk's curvature budget
/// (the static controls size the residual).
fn fee_scales(view: &Slot0View, liquidity: U256, zero_for_one: bool) -> Vec<U256> {
    let bip = one_bip_amount(view, liquidity, zero_for_one).unwrap_or(U256::ZERO);
    [bip / U256::from(100u64), bip / U256::from(10u64), bip]
        .into_iter()
        .map(|amount| amount.max(U256::from(1u64)))
        .collect()
}

fn rpc_url() -> String {
    std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI"))
        .expect(
            "set DEGENBOT_RPC_HTTP_CHAINID_1 (or TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI) \
             to an archive mainnet endpoint",
        )
}

/// A token's decimals at the anchor, defaulting to 18 where the metadata is
/// absent (native ETH has no `decimals()`, and a non-standard token may revert).
async fn token_decimals(provider: &AlloyProvider, token: Address, block: u64) -> u32 {
    if token.is_zero() {
        return 18;
    }
    let Ok(output) = provider
        .eth_call(&token, Bytes::from_static(&DECIMALS_CALLDATA), Some(block))
        .await
    else {
        return 18;
    };
    output.get(..32).map_or(18, |word| {
        U256::from_be_slice(word).saturating_to::<u32>().min(30)
    })
}

/// The input currency's decimals for a leg.
async fn leg_decimals(
    provider: &AlloyProvider,
    leg: &V4Leg,
    block: u64,
    cache: &mut HashMap<Address, u32>,
) -> u32 {
    let input = leg.amount_in_currency();
    if let Some(decimals) = cache.get(&input) {
        return *decimals;
    }
    let decimals = token_decimals(provider, input, block).await;
    cache.insert(input, decimals);
    decimals
}

/// A short tag for a pool or hook address (`0x` + six bytes).
fn short(address: &Address) -> String {
    format!("0x{}", alloy::hex::encode(&address.as_slice()[..6]))
}

// ─────────────────────────────────────────────────────────────────────────
// 1. Parity — the black-box tier IS the node's answer
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC fee census over archive mainnet state; run with --ignored"]
async fn parity_census_dynamic_fee_hook_legs() {
    let started = Instant::now();
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    assert_eq!(
        provider.get_chain_id().await.unwrap(),
        1,
        "the census pools are mainnet fixtures"
    );
    let mut anchor = Anchor::open(&provider, PIN).await;
    let rows = census();
    let hooks = census_hooks(&rows);
    let mut quoter = V4BlackBoxQuoter::new(V4QuoterConfig::default());
    let mut decimals: HashMap<Address, u32> = HashMap::new();

    println!("=== dynamic-fee V4 leg parity census ===");
    println!(
        "anchor    : {PIN} (timestamp {}, base fee {} wei/gas)",
        anchor.timestamp, anchor.base_fee
    );
    println!(
        "census    : {} candidate pools across {} hooks (4 top by pool count + 20 seeded long-tail, seed 0x{SAMPLE_SEED:016x})",
        rows.len(),
        hooks.len()
    );
    println!(
        "sides     : evm2 (deployed Quoter 0x{}) vs the node's eth_call of the same calldata",
        alloy::hex::encode(V4_QUOTER)
    );

    let mut points = 0usize;
    let mut answered = 0usize;
    let mut both_refused = 0usize;
    let mut mismatches = 0usize;
    let mut thin = 0usize;
    let mut max_diff = U256::ZERO;
    let mut priced_pools = 0usize;
    let mut priced_hooks: Vec<Address> = Vec::new();
    let mut refusal_classes: BTreeMap<String, usize> = BTreeMap::new();

    println!();
    println!(
        "{:<34} {:>6} {:>7} {:>8} {:>8} {:>6}",
        "pool (id prefix)", "legs", "points", "answered", "refused", "mism"
    );

    for row in &rows {
        let id = v4_pool_id(&row.key);
        if anchor.liquidity(&row.key).is_zero() {
            thin += 1;
            println!(
                "{:<34}  no in-range liquidity at the anchor — thin",
                short(&Address::from_slice(&id[..20]))
            );
            continue;
        }
        priced_pools += 1;

        let mut pool_points = 0usize;
        let mut pool_answered = 0usize;
        let mut pool_refused = 0usize;
        let mut pool_mismatch = 0usize;

        for zero_for_one in [true, false] {
            let leg = V4Leg::canonical(row.key.clone(), zero_for_one);
            let input_decimals = leg_decimals(&provider, &leg, PIN, &mut decimals).await;
            for amount_in in parity_ladder(input_decimals) {
                points += 1;
                pool_points += 1;

                let ours = anchor.quote(&mut quoter, &leg, amount_in);
                let theirs = anchor.node_quote(&leg, amount_in).await;

                match (&ours, &theirs) {
                    (Ok(quote), Ok(node)) => {
                        answered += 1;
                        pool_answered += 1;
                        if !priced_hooks.contains(&row.hook) {
                            priced_hooks.push(row.hook);
                        }
                        if quote.amount_out != *node {
                            pool_mismatch += 1;
                            let diff = if quote.amount_out > *node {
                                quote.amount_out - *node
                            } else {
                                *node - quote.amount_out
                            };
                            max_diff = max_diff.max(diff);
                            println!(
                                "  MISMATCH {} zfo={zero_for_one} in={amount_in}: evm2 {} vs node {node} (diff {diff})",
                                short(&Address::from_slice(&id[..20])),
                                quote.amount_out
                            );
                        }
                    }
                    (Err(error), Err(_)) => {
                        both_refused += 1;
                        pool_refused += 1;
                        let class = reverts::of_error(error);
                        *refusal_classes.entry(class).or_default() += 1;
                    }
                    (Err(error), Ok(node)) => {
                        pool_mismatch += 1;
                        println!(
                            "  MISMATCH {} zfo={zero_for_one} in={amount_in}: evm2 refused ({}) but the node priced it at {node}",
                            short(&Address::from_slice(&id[..20])),
                            reverts::of_error(error)
                        );
                    }
                    (Ok(quote), Err(reason)) => {
                        pool_mismatch += 1;
                        println!(
                            "  MISMATCH {} zfo={zero_for_one} in={amount_in}: evm2 priced {} but the node refused ({reason})",
                            short(&Address::from_slice(&id[..20])),
                            quote.amount_out
                        );
                    }
                }
            }
        }

        mismatches += pool_mismatch;
        println!(
            "{:<34} {:>6} {:>7} {:>8} {:>8} {:>6}",
            format!(
                "{} (hook {})",
                short(&Address::from_slice(&id[..20])),
                short(&row.hook)
            ),
            2,
            pool_points,
            pool_answered,
            pool_refused,
            pool_mismatch
        );
    }

    println!();
    println!(
        "totals: {} candidate pools ({} with in-range liquidity, {} thin) across {} hooks ({} hooks answered)",
        rows.len(),
        priced_pools,
        thin,
        hooks.len(),
        priced_hooks.len()
    );
    println!(
        "totals: {points} quote points — {answered} answered, {both_refused} refused on both sides, {mismatches} mismatches, max |diff| {max_diff} wei"
    );
    if !refusal_classes.is_empty() {
        println!("refusal classes (evm2 side, the node's own revert classes match):");
        for (class, count) in &refusal_classes {
            println!("  {class}: {count}");
        }
    }
    println!(
        "totals: {} cold reads forwarded | wall {:.1} s",
        anchor.counter.reads(),
        started.elapsed().as_secs_f64()
    );

    assert_eq!(
        mismatches, 0,
        "the primitive must agree with the node's own quoter call at the anchor"
    );
    assert!(
        answered >= 20,
        "the census must actually price legs ({answered} answered of {points})"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// 2. The fee distribution — what the population actually charges
// ──────────────────────────────────────────────────────────────────────────

/// One hook's accumulated readings.
#[derive(Default)]
struct HookTally {
    pools: usize,
    measured: usize,
    refused: usize,
    thin: usize,
    hundredths: Vec<i64>,
    lp_fee_words: BTreeMap<u32, usize>,
    refusals: BTreeMap<String, usize>,
}

impl HookTally {
    /// The hook's census line: counts, the fee distribution (bps), and the
    /// mechanism the settled word shows.
    fn line(&self, hook: &Address) -> String {
        let mut readings = self.hundredths.clone();
        let middle = median(&mut readings).map_or_else(|| "-".to_string(), render_bps);
        let low = readings
            .first()
            .map_or_else(|| "-".to_string(), |v| render_bps(*v));
        let high = readings
            .last()
            .map_or_else(|| "-".to_string(), |v| render_bps(*v));
        let mechanisms: Vec<String> = self
            .lp_fee_words
            .iter()
            .map(|(word, count)| format!("{word}×{count}"))
            .collect();
        format!(
            "{:<16} {:>5} {:>7} {:>8} {:>5} | {:>9} {:>9} {:>9} | {}",
            short(hook),
            self.pools,
            self.measured,
            self.refused,
            self.thin,
            low,
            middle,
            high,
            if mechanisms.is_empty() {
                "-".to_string()
            } else {
                mechanisms.join(",")
            }
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC fee census over archive mainnet state; run with --ignored"]
async fn implied_fee_distribution_per_hook() {
    let started = Instant::now();
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    let mut anchor = Anchor::open(&provider, PIN).await;
    let rows = census();
    let mut tallies: BTreeMap<Address, HookTally> = BTreeMap::new();

    println!("=== implied fee per pool — what each zero-fee hook charges ===");
    println!(
        "anchor : {PIN} (timestamp {}) | amount: 1/10 of a 1-bip walk of the pool's own state | implied = 1 - traded/hookless on the same walk",
        anchor.timestamp
    );
    println!(
        "{:<34} {:>5} {:>12} {:>8} {:>9} {:>9} {:>9}",
        "pool / direction", "zfo", "implied bps", "lpFee", "gas", "traded", "hookless"
    );

    for row in &rows {
        let tally = tallies.entry(row.hook).or_default();
        tally.pools += 1;
        let id = v4_pool_id(&row.key);
        let liquidity = anchor.liquidity(&row.key);
        if liquidity.is_zero() {
            tally.thin += 1;
            println!(
                "{:<34} {:>5} {:>12}",
                short(&Address::from_slice(&id[..20])),
                "-",
                "thin"
            );
            continue;
        }
        let view = slot0_view(anchor.slot0(&row.key));
        for zero_for_one in [true, false] {
            let amount_in = fee_scales(&view, liquidity, zero_for_one)[1];
            match anchor.measure(row, zero_for_one, amount_in) {
                Ok(point) => {
                    tally.measured += 1;
                    *tally.lp_fee_words.entry(point.lp_fee_word).or_default() += 1;
                    if let Some(hundredths) = point.hundredths_bp {
                        tally.hundredths.push(hundredths);
                    }
                    println!(
                        "{:<34} {:>5} {:>12} {:>8} {:>9} {:>9} {:>9}",
                        format!(
                            "{} (hook {})",
                            short(&Address::from_slice(&id[..20])),
                            short(&row.hook)
                        ),
                        u8::from(zero_for_one),
                        point
                            .hundredths_bp
                            .map_or_else(|| "thin".to_string(), render_bps),
                        point.lp_fee_word,
                        point.gas_used,
                        point.amount_out,
                        point.hookless_out
                    );
                }
                Err(reason) => {
                    tally.refused += 1;
                    *tally.refusals.entry(reason.clone()).or_default() += 1;
                    println!(
                        "{:<34} {:>5} {:>12}",
                        format!(
                            "{} (hook {})",
                            short(&Address::from_slice(&id[..20])),
                            short(&row.hook)
                        ),
                        u8::from(zero_for_one),
                        reason
                    );
                }
            }
        }
    }

    println!();
    println!("=== per-hook distribution ===");
    println!(
        "{:<16} {:>5} {:>7} {:>8} {:>5} | {:>9} {:>9} {:>9} | lpFee words",
        "hook", "pools", "walks", "refused", "thin", "min bps", "median", "max bps"
    );
    let mut measured = 0usize;
    let mut refused = 0usize;
    let mut all: Vec<i64> = Vec::new();
    let mut all_refusals: BTreeMap<String, usize> = BTreeMap::new();
    for (hook, tally) in &tallies {
        measured += tally.measured;
        refused += tally.refused;
        all.extend(tally.hundredths.iter().copied());
        for (class, count) in &tally.refusals {
            *all_refusals.entry(class.clone()).or_default() += count;
        }
        println!("{}", tally.line(hook));
    }
    let mut all_sorted = all.clone();
    all_sorted.sort_unstable();
    let (low, middle, high) = (
        all_sorted.first().copied(),
        median(&mut all_sorted),
        all_sorted.last().copied(),
    );
    println!();
    println!(
        "totals: {} pools across {} hooks | {} walks measured, {} refused | implied fee min {} bps, median {} bps, max {} bps ({} readings)",
        rows.len(),
        tallies.len(),
        measured,
        refused,
        low.map_or_else(|| "-".to_string(), render_bps),
        middle.map_or_else(|| "-".to_string(), render_bps),
        high.map_or_else(|| "-".to_string(), render_bps),
        all.len()
    );
    if !all_refusals.is_empty() {
        println!("refusal classes:");
        for (class, count) in &all_refusals {
            println!("  {class}: {count}");
        }
    }
    println!(
        "wall: {:.1} s | cold reads: {}",
        started.elapsed().as_secs_f64(),
        anchor.counter.reads()
    );

    assert!(
        measured >= 8,
        "the census must measure the fee on real walks ({measured} measured)"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// 3. The derivation validated against a fee read off the key
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC fee census over archive mainnet state; run with --ignored"]
async fn implied_fee_derivation_validated_on_static_fee_pools() {
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    let mut anchor = Anchor::open(&provider, PIN).await;
    let mut quoter = V4BlackBoxQuoter::new(V4QuoterConfig::default());
    let mut controls = 0usize;
    let mut exact_pairs = 0usize;
    let mut worst_bias = 0i64;

    println!("=== the implied-fee estimator against a KNOWN static fee ===");
    println!(
        "reference: the pool's own curve at fee 0 (`degenbot_math::compute_swap_step_v4`), counted only where the same math at the identified fee reproduces the chain's quote to the wei"
    );
    println!(
        "{:<26} {:>12} {:>5} {:>10} {:>12} {:>12} {:>9}",
        "control pool", "amount", "zfo", "chain fee", "expected bps", "implied bps", "bias"
    );

    for (currency0, currency1, fee, tick_spacing, pool_id) in STATIC_FEE_CONTROLS {
        let key = V4PoolKey {
            currency0,
            currency1,
            fee,
            tick_spacing,
            hooks: Address::ZERO,
        };
        assert_eq!(
            v4_pool_id(&key),
            pool_id,
            "a static control's key does not hash to its pool id (fee {fee})"
        );
        let liquidity = anchor.liquidity(&key);
        if liquidity.is_zero() {
            println!(
                "{:<26}  no in-range liquidity at the anchor — thin",
                format!("fee {fee}")
            );
            continue;
        }
        controls += 1;
        let view = slot0_view(anchor.slot0(&key));
        for zero_for_one in [true, false] {
            let leg = V4Leg::canonical(key.clone(), zero_for_one);
            let direction_protocol_fee = if zero_for_one {
                protocol_fee_zero_for_one(view.protocol_fee)
            } else {
                protocol_fee_one_for_zero(view.protocol_fee)
            };
            // The two fees the chain's own answer can identify: the key's fee
            // alone, or that fee composed with the direction's protocol fee
            // (V4's `ProtocolFeeLibrary.calculateSwapFee`).
            let composed = calculate_swap_fee(direction_protocol_fee, fee).unwrap_or(fee);

            let amounts = fee_scales(&view, liquidity, zero_for_one);
            for amount_in in amounts.into_iter().take(2) {
                let chain = anchor
                    .quote(&mut quoter, &leg, amount_in)
                    .expect("a static-fee control prices its leg")
                    .amount_out;
                let Some(no_fee) =
                    curve_step_out(&view, liquidity, tick_spacing, zero_for_one, amount_in, 0)
                else {
                    println!(
                        "{:<26} {:>12} {:>5}  the curve math refused the walk",
                        format!("fee {fee} (ts {tick_spacing})"),
                        amount_in,
                        u8::from(zero_for_one)
                    );
                    continue;
                };
                let identified = [fee, composed].into_iter().find(|candidate| {
                    curve_step_out(
                        &view,
                        liquidity,
                        tick_spacing,
                        zero_for_one,
                        amount_in,
                        *candidate,
                    ) == Some(chain)
                });
                let Some(effective) = identified else {
                    println!(
                        "{:<26} {:>12} {:>5}  the walk leaves the current tick range",
                        format!("fee {fee} (ts {tick_spacing})"),
                        amount_in,
                        u8::from(zero_for_one)
                    );
                    continue;
                };
                exact_pairs += 1;
                let expected = i64::from(effective);
                let implied = implied_hundredths_bp(chain, no_fee)
                    .expect("a static-fee control's fee-free walk is non-zero");
                let bias = implied - expected;
                worst_bias = worst_bias.max(bias.abs());
                println!(
                    "{:<26} {:>12} {:>5} {:>10} {:>12} {:>12} {:>9}",
                    format!("fee {fee} (ts {tick_spacing})"),
                    amount_in,
                    u8::from(zero_for_one),
                    format!("{effective} (slot0 {})", view.lp_fee),
                    render_bps(expected),
                    render_bps(implied),
                    render_bps(bias)
                );
                assert!(
                    bias.abs() <= (expected.unsigned_abs() / 10).max(10).cast_signed(),
                    "the estimator must reproduce the identified fee: expected {} bps, measured {} bps",
                    render_bps(expected),
                    render_bps(implied)
                );
            }
        }
    }

    println!();
    println!(
        "{controls} control pools priced, {exact_pairs} readings whose closed form matched the chain to the wei | worst bias {} bps — the walk-curvature term, and the bound on the distribution table's readings at these scales",
        render_bps(worst_bias)
    );
    assert_eq!(controls, 4, "every control tier prices at the anchor");
    assert!(
        exact_pairs >= 4,
        "the closed form must reproduce the chain on real control walks ({exact_pairs})"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// 4. Amount dependence
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC fee census over archive mainnet state; run with --ignored"]
async fn implied_fee_amount_dependence() {
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    let mut anchor = Anchor::open(&provider, PIN).await;
    let rows = census();
    let subjects = one_pool_per_top_hook(&rows);

    println!(
        "=== amount dependence: the hook's charge across three scales of the pool's own depth ==="
    );
    println!("anchor: {PIN} (timestamp {})", anchor.timestamp);
    println!(
        "{:<34} {:>5} {:>24} {:>12} {:>12} {:>12} {:>10}",
        "pool / direction", "zfo", "1-bip walk amount", "bip/100", "bip/10", "1 bip", "spread"
    );

    let mut measured = 0usize;
    for row in &subjects {
        let id = v4_pool_id(&row.key);
        let liquidity = anchor.liquidity(&row.key);
        if liquidity.is_zero() {
            println!(
                "{:<34}  no in-range liquidity at the anchor — thin",
                short(&Address::from_slice(&id[..20]))
            );
            continue;
        }
        let view = slot0_view(anchor.slot0(&row.key));
        for zero_for_one in [true, false] {
            let amounts = fee_scales(&view, liquidity, zero_for_one);
            let mut readings: Vec<Option<i64>> = Vec::new();
            for amount_in in &amounts {
                match anchor.measure(row, zero_for_one, *amount_in) {
                    Ok(point) => readings.push(point.hundredths_bp),
                    Err(_) => readings.push(None),
                }
            }
            let present: Vec<i64> = readings.iter().flatten().copied().collect();
            if present.len() == 3 {
                measured += 1;
            }
            let spread = match (present.first(), present.last()) {
                (Some(low), Some(high)) => render_bps(high - low),
                _ => "-".to_string(),
            };
            println!(
                "{:<34} {:>5} {:>24} {:>12} {:>12} {:>12} {:>10}",
                format!(
                    "{} (hook {})",
                    short(&Address::from_slice(&id[..20])),
                    short(&row.hook)
                ),
                u8::from(zero_for_one),
                amounts[2],
                readings[0].map_or_else(|| "refused".to_string(), render_bps),
                readings[1].map_or_else(|| "refused".to_string(), render_bps),
                readings[2].map_or_else(|| "refused".to_string(), render_bps),
                spread
            );
        }
    }

    println!();
    println!(
        "{measured} walks priced at all three scales | a spread near the static controls' curvature bias is FLAT; a spread that grows with size is AMOUNT-SHAPED"
    );
    assert!(measured >= 2, "the amount ladder must price real walks");
}

// ─────────────────────────────────────────────────────────────────────────
// 5. Time dependence
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC fee census over archive mainnet state; run with --ignored"]
async fn implied_fee_time_dependence() {
    let started = Instant::now();
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    let rows = census();
    let subjects = one_pool_per_top_hook(&rows);

    println!("=== time dependence: the hook's charge across one hour of blocks ===");
    println!(
        "anchors: {TIME_SAMPLES} samples over {HOUR_BLOCKS} blocks (~one hour), amount 1/10 of a 1-bip walk at each anchor's own state"
    );
    println!(
        "{:<34} {:>5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "pool / direction", "zfo", "T-0", "T-75", "T-150", "T-225", "T-300", "spread"
    );

    // One reading per (pool, direction, anchor), gathered before the table prints.
    let mut grid: BTreeMap<(String, bool), Vec<Option<i64>>> = BTreeMap::new();
    let mut timestamps = Vec::new();
    for sample in 0..TIME_SAMPLES {
        let number = PIN + TIME_STEP * u64::try_from(sample).unwrap();
        let mut anchor = Anchor::open(&provider, number).await;
        timestamps.push(anchor.timestamp);
        for row in &subjects {
            let id = v4_pool_id(&row.key);
            let label = format!(
                "{} (hook {})",
                short(&Address::from_slice(&id[..20])),
                short(&row.hook)
            );
            for zero_for_one in [true, false] {
                let liquidity = anchor.liquidity(&row.key);
                let reading = if liquidity.is_zero() {
                    None
                } else {
                    let view = slot0_view(anchor.slot0(&row.key));
                    let amount_in = fee_scales(&view, liquidity, zero_for_one)[1];
                    anchor
                        .measure(row, zero_for_one, amount_in)
                        .ok()
                        .and_then(|point| point.hundredths_bp)
                };
                grid.entry((label.clone(), zero_for_one))
                    .or_insert_with(|| vec![None; TIME_SAMPLES])[sample] = reading;
            }
        }
    }

    let mut stable = 0usize;
    let mut moving = 0usize;
    for ((label, zero_for_one), readings) in &grid {
        let present: Vec<i64> = readings.iter().flatten().copied().collect();
        let spread = present
            .iter()
            .max()
            .zip(present.iter().min())
            .map(|(h, l)| h - l);
        if let Some(spread) = spread {
            if spread == 0 {
                stable += 1;
            } else {
                moving += 1;
            }
        }
        println!(
            "{:<34} {:>5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
            label,
            u8::from(*zero_for_one),
            readings[0].map_or_else(|| "-".to_string(), render_bps),
            readings[1].map_or_else(|| "-".to_string(), render_bps),
            readings[2].map_or_else(|| "-".to_string(), render_bps),
            readings[3].map_or_else(|| "-".to_string(), render_bps),
            readings[4].map_or_else(|| "-".to_string(), render_bps),
            spread.map_or_else(|| "-".to_string(), render_bps)
        );
    }

    println!();
    println!(
        "block timestamps: {:?} (span {} s over {} blocks)",
        timestamps,
        timestamps.last().unwrap_or(&0) - timestamps.first().unwrap_or(&0),
        HOUR_BLOCKS
    );
    println!(
        "{stable} series flat across the hour, {moving} series moved | wall {:.1} s",
        started.elapsed().as_secs_f64()
    );
    assert!(
        grid.values().flatten().flatten().count() >= 4,
        "the time sample must price real walks"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// 6. Is the `slot0.lpFee` word the swap's LP fee?
// ──────────────────────────────────────────────────────────────────────────

/// A static pool's LP fee: the `slot0.lpFee` WORD, not a white-box assumption.
///
/// The deployed `PoolManager` charges `calculateSwapFee(protocol_fee_dir, lpFee)`
/// where `lpFee` is the `beforeSwap` hook's override when it returned one, else
/// `slot0.lpFee` — a word `initialize` writes from `key.fee` and that only a
/// DYNAMIC-fee pool's hook can move (`updateDynamicLPFee` is hook-only and
/// reverts unless `key.fee.isDynamicFee()`). So a static pool's word IS its key
/// fee, and a key-fee white-box is exact on one.
///
/// This test settles which word the chain actually reads, by rewriting that word
/// alone and asking both arms — a reth `stateDiff` `eth_call` and the evm2 twin:
///
/// - the override LANDS: the node's own `extsload` at that slot answers it
///   inside the overridden state;
/// - the quote MOVES, on BOTH arms, to the wei the composed fee over the
///   OVERRIDDEN word predicts;
/// - at the pool's own word the same closed form reproduces the chain's quote.
///
/// The amount is the shallowest census rung whose closed form reproduces the
/// chain to the wei — never dust. A swap step charges `ceil(amount·fee/1e6)`, so
/// on a 1-wei walk every fee in `(0, 1e6]` charges the same single unit and an
/// override probe reports a false "inert".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live-RPC fee census over archive mainnet state; run with --ignored"]
async fn slot0_lp_fee_word_is_the_static_pool_swap_fee() {
    let provider = Arc::new(AlloyProvider::new(&rpc_url(), 3).await.unwrap());
    let mut anchor = Anchor::open(&provider, PIN).await;
    let mut quoter = V4BlackBoxQuoter::new(V4QuoterConfig::default());

    println!("=== a static pool's LP fee: the slot0.lpFee WORD, not the key fee by fiat ===");
    println!(
        "anchor: {PIN}; the manager charges calculateSwapFee(protocol_fee_dir, lpFee), lpFee = hook override else slot0.lpFee"
    );
    println!(
        "{:<20} {:>4} {:>22} {:>7} {:>8} {:>24}",
        "control pool", "zfo", "amount", "lpFee", "swapFee", "chain quote"
    );

    let mut priced = 0usize;
    let mut applied = 0usize;
    let mut moved = 0usize;

    for (currency0, currency1, fee, tick_spacing, pool_id) in STATIC_FEE_CONTROLS {
        let key = V4PoolKey {
            currency0,
            currency1,
            fee,
            tick_spacing,
            hooks: Address::ZERO,
        };
        assert_eq!(
            v4_pool_id(&key),
            pool_id,
            "a static control's key does not hash to its pool id (fee {fee})"
        );
        let label = format!("fee {fee} (ts {tick_spacing})");
        let liquidity = anchor.liquidity(&key);
        if liquidity.is_zero() {
            println!("{label:<20}  no in-range liquidity at the anchor — thin");
            continue;
        }
        let word = anchor.slot0(&key);
        let view = slot0_view(word);
        // The invariant a key-fee white-box rests on: `initialize` seeds this word
        // with `key.fee`, and only a dynamic-fee pool's hook may move it.
        assert_eq!(
            view.lp_fee, fee,
            "a static pool's slot0.lpFee is its key fee ({label})"
        );
        let slot0_slot = leg_state_slots(&key)[0];
        for zero_for_one in [true, false] {
            let leg = V4Leg::canonical(key.clone(), zero_for_one);
            let direction_protocol_fee = if zero_for_one {
                protocol_fee_zero_for_one(view.protocol_fee)
            } else {
                protocol_fee_one_for_zero(view.protocol_fee)
            };
            let own_swap_fee =
                calculate_swap_fee(direction_protocol_fee, view.lp_fee).unwrap_or(view.lp_fee);

            let mut rung = None;
            for amount_in in fee_scales(&view, liquidity, zero_for_one) {
                let Ok(chain) = anchor.quote(&mut quoter, &leg, amount_in) else {
                    continue;
                };
                if curve_step_out(
                    &view,
                    liquidity,
                    tick_spacing,
                    zero_for_one,
                    amount_in,
                    own_swap_fee,
                ) == Some(chain.amount_out)
                {
                    rung = Some((amount_in, chain.amount_out));
                    break;
                }
            }
            let Some((amount_in, baseline)) = rung else {
                println!(
                    "{label:<20} {:>4}  the walk leaves the current tick range at every rung",
                    u8::from(zero_for_one)
                );
                continue;
            };
            priced += 1;
            println!(
                "{label:<20} {:>4} {amount_in:>22} {:>7} {own_swap_fee:>8} {baseline:>24}",
                u8::from(zero_for_one),
                view.lp_fee
            );

            for override_lp_fee in [0u32, 30_000] {
                if override_lp_fee == view.lp_fee {
                    continue;
                }
                let overridden = with_lp_fee(word, override_lp_fee);
                let override_swap_fee = calculate_swap_fee(direction_protocol_fee, override_lp_fee)
                    .unwrap_or(override_lp_fee);
                let Some(expected) = curve_step_out(
                    &slot0_view(overridden),
                    liquidity,
                    tick_spacing,
                    zero_for_one,
                    amount_in,
                    override_swap_fee,
                ) else {
                    println!(
                        "{label:<20} {:>4}  the overridden walk leaves the current tick range",
                        u8::from(zero_for_one)
                    );
                    continue;
                };

                // (1) the override lands: the node's own `extsload` at the slot
                // answers it inside the overridden state.
                let landed = anchor
                    .node_call_state_diff(
                        &V4_POOL_MANAGER,
                        extsload_calldata(slot0_slot),
                        slot0_slot,
                        overridden,
                    )
                    .await
                    .expect("the node answers an extsload under a stateDiff override");
                assert_eq!(
                    lp_fee_of(U256::from_be_slice(&landed)),
                    override_lp_fee,
                    "the stateDiff lpFee override must land in the PoolManager's slot0 ({label})"
                );

                // (2) the node's quote under the same override.
                let node_output = anchor
                    .node_call_state_diff(
                        &V4_QUOTER,
                        quote_calldata(&leg, amount_in),
                        slot0_slot,
                        overridden,
                    )
                    .await
                    .expect("the node quotes a static pool under an lpFee override");
                let node_out = decode_quote_return(&node_output)
                    .expect("(amountOut, gasEstimate)")
                    .0;

                // (3) the evm2 twin: rewrite the word in the scratch, re-quote,
                // put it back (the scratch outlives one pool's readings).
                anchor.put_storage(slot0_slot, overridden);
                let evm2_out = anchor
                    .quote(&mut quoter, &leg, amount_in)
                    .map(|quote| quote.amount_out);
                anchor.put_storage(slot0_slot, word);
                let evm2_out =
                    evm2_out.expect("the evm2 twin quotes a static pool under an lpFee override");

                applied += 1;
                let leg_moved = evm2_out != baseline;
                if leg_moved {
                    moved += 1;
                }
                println!(
                    "{label:<20} {:>4} lpFee {override_lp_fee:>7} swapFee {override_swap_fee:>7}: evm2 {evm2_out:>24} node {node_out:>24} curve {expected:>24} lands=true moved={leg_moved}",
                    u8::from(zero_for_one)
                );
                assert_eq!(
                    evm2_out, expected,
                    "the evm2 quote must follow the overridden lpFee word ({label})"
                );
                assert_eq!(
                    node_out, expected,
                    "the node's quote must follow the overridden lpFee word ({label})"
                );
                assert_ne!(
                    evm2_out, baseline,
                    "rewriting slot0.lpFee alone must move the quote — the word is the swap's LP fee ({label})"
                );
            }
        }
    }

    println!();
    println!(
        "{priced} legs priced to the wei at their own slot0.lpFee word, {applied} lpFee overrides applied, {moved} moved the quote"
    );
    assert!(priced >= 2, "the controls must price real walks ({priced})");
    assert!(
        applied >= 4,
        "the override must run on real walks ({applied})"
    );
    assert_eq!(moved, applied, "every lpFee override must move the quote");
}
