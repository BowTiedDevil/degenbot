//! The exact-input quote interface: one registered path's fixed-input amount
//! vector at ONE block.
//!
//! # The design problem this seam answers
//!
//! An execution probe is handed a FIXED input and must state the per-hop amount
//! vector that input produces — one output per hop, each hop priced at the pool
//! state the block carries. Deriving that vector in a driver would re-implement
//! each family's pricing: the V2 constant-product read, the V3 fee-tier walk, and
//! the V4 pool's hook-laden swap. Every one of those already has an owner, and a
//! driver-side copy would drift from it.
//!
//! So this module owns the derivation ONCE, as a family-dispatched quote:
//!
//! - **V2 hops** price through [`simulate_swap`] — the production pool math the
//!   solver, the inline sim, and the Python companion read all dispatch to. The
//!   [`PoolEntry`] is SNAPSHOTTED when the plan is built, so no read lock is
//!   held across an `await`; a snapshot whose own block stamp is not the plan's
//!   block is re-anchored from the pair's `getReserves()` at that block, so an
//!   in-process hop never prices a state the vector does not claim.
//! - **V3 and V4 hops** price through the chain's OWN quoter at the same block
//!   ([`V3Quote`] / [`V4Quote`]): `quoteExactInputSingle` on the canonical
//!   v3-periphery and v4-periphery deployments. A V4 hop carries its WHOLE
//!   [`V4PoolKey`] — the hook address included — so a hooked pool is priced with
//!   its hook attached and is never replaced by unhooked solver math (ADR-090
//!   §1/§2).
//!
//! # One block, one vector
//!
//! [`ExactInputQuotePlan::block`] is the quote's ONE anchor: every concentrated
//! hop is called at it, every in-process hop prices the reserve-pair state read
//! AT it — the snapshot it holds when that snapshot's own stamp is the anchor,
//! the pair's `getReserves()` at the anchor otherwise — and [`ProbeBlockContext`]
//! reports that same block's timestamp and base fee. A
//! caller that hands the resulting vector to an execution probe is therefore
//! describing one block, not a smear of them.
//!
//! # Parity coverage (what the tests pin, and what they do not)
//!
//! The committed `py_oracle` quoter corpora replay OFFLINE as pinned-mainnet
//! parity fixtures: the recorded answers at block 24407242 for the WBTC/WETH
//! 0.3% V3 pool and the ETH/USDC 0.05% V4 pool, BOTH directions, are the
//! vectors this seam must reproduce (see the module tests). Those corpora record
//! an UNHOOKED V4 pool, so they can never be cited as hooked evidence: the
//! offline tests pin the hooked behavior STRUCTURALLY (the hook address travels
//! in the key, and an unrecorded hooked key is REFUSED rather than modelled).
//!
//! A hooked pool's pinned-block parity is proven LIVE, opt-in, by the
//! `#[ignore]`d integration test `tests/probe_quote_live.rs`
//! (`live_hooked_v4_quote_matches_the_canonical_quoter_both_directions` — it
//! lives outside `src/` because an opt-in leg reads its endpoint from the
//! environment, which ADR-062 D7 confines to the config loader). It drives the
//! public stage surface and prices a REAL mainnet hooked V4 pool — census row
//! `hooks 0x322dcEc4958C14e021A9F1cD49DF11b9457968cC`,
//! `currency0 0x4E67db19044549ff420860834c91b45bad298722`,
//! `currency1 0xAe5b1b51ECB247218157ddc6f1E457db25F971d7`, `fee 0`,
//! `tickSpacing 200`, pool id
//! `0x004de370c1ef268265d1f3f8a91d00bd4bc2d97699f9877dc3d81286fb657bf3` — at
//! pinned mainnet block `26_320_000`, and compares the core's whole request
//! vector against an INDEPENDENTLY hand-encoded canonical-quoter `eth_call` at
//! that same block, in BOTH directions. (The registered path is a two-hop round
//! trip — the production registration seam refuses a one-hop path — so every
//! hop's output is checked against the chain.) At that pin (one token unit in,
//! both
//! currencies 18 decimals) the chain answers `4_854_067_101_419_274_970_894`
//! (zero for one) and `201_912_091_397_373` (one for zero), the core reproduces
//! each exactly, the unhooked twin key is REFUSED by the chain, and the pool's
//! own white-box walk returns `4_903_097_821_562_843_550_261` /
//! `203_951_607_472_093` — a visibly different number, which is the substitution
//! this seam must never make:
//!
//! ```sh
//! DEGENBOT_RPC_HTTP_CHAINID_1=<archive endpoint> cargo nextest run \
//!   -p degenbot-bot -E 'test(live_hooked_v4_quote)' --run-ignored only --nocapture
//! ```
//!
//! # What this seam deliberately does NOT do
//!
//! It prices nothing white-box for a concentrated hop, decides nothing about a
//! path's profitability, and carries no capital posture. It answers exactly one
//! question — "what does this fixed input produce along this path at this
//! block?" — and answers it with each family's own owner.

use std::future::Future;

use alloy::primitives::{aliases::U112, Address, U256};
use degenbot_core::errors::{ProviderError, ProviderResult};
use degenbot_pools::solve::HopType;
use degenbot_pools::{simulate_swap, PoolEntry, SimulateSwapError, V4PoolKey};
use degenbot_rpc::abi::{
    ContractRead, V2Reserves, V3Quote, V3QuoteRequest, V4Quote, V4QuoteRequest, V3_QUOTER,
    V4_QUOTER,
};
use degenbot_rpc::provider::AlloyProvider;
use thiserror::Error;

/// The block context one quote reports: the anchor's timestamp and the base fee
/// the next block inherits (`block N`'s `base_fee_per_gas`).
///
/// These are the facts the probe's request carries, read at the SAME block the
/// quote priced at — never a second, later `latest` read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeBlockContext {
    /// The block the quote priced at.
    pub block: u64,
    /// That block's base fee (wei) — the next block's `base_fee`.
    pub base_fee_next: u128,
    /// That block's timestamp (Unix seconds).
    pub timestamp: u64,
}

/// One V2-family hop: the registered pool, the direction, and the pool state the
/// hop prices against.
///
/// The [`PoolEntry`] is a CLONE taken when the plan was built, under the one core
/// read the plan owns — so a quote's in-process hops cannot straddle a state
/// change, and the shared read lock is never held across an `await`.
#[derive(Debug, Clone)]
pub struct InProcessHop {
    /// The `BotState` pool id this hop prices (the error report's subject).
    pub pool_id: u64,
    /// `true` when the path trades `token0` → `token1` through this pool.
    pub zero_for_one: bool,
    /// The pool state snapshot the hop starts from: it prices as it stands when
    /// its own block stamp IS the plan's block, and is otherwise replaced by the
    /// pair's reserves at that block.
    pub entry: PoolEntry,
}

/// One V3 hop: everything the canonical v3-periphery `Quoter` needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V3ExactInputHop {
    /// The token the hop spends (the identity's `token0` or `token1`).
    pub token_in: Address,
    /// The token the hop receives.
    pub token_out: Address,
    /// The pool's fee tier (hundredths of a bip).
    pub fee: u32,
    /// `true` when `token_in` is the pool's `token0`.
    pub zero_for_one: bool,
}

/// One V4 hop: the pool's immutable key (HOOK INCLUDED) and the direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4ExactInputHop {
    /// The pool's key — currencies, fee, tick spacing, and its hook address.
    pub pool_key: V4PoolKey,
    /// `true` when currency0 is the input.
    pub zero_for_one: bool,
}

/// One hop of an exact-input quote, dispatched by family.
#[derive(Debug, Clone)]
pub enum ExactInputHop {
    /// A V2-family hop, priced in process by the family's production math.
    InProcess(InProcessHop),
    /// A V3 hop, priced by the canonical on-chain quoter.
    V3(V3ExactInputHop),
    /// A V4 hop, priced by the canonical on-chain quoter with its hook attached.
    V4(V4ExactInputHop),
}

/// A path's quote plan: the path identity, the ONE block every hop prices at, and
/// the per-hop quote inputs, in path order.
#[derive(Debug, Clone)]
pub struct ExactInputQuotePlan {
    /// The registered engine path this plan prices.
    pub path_id: u64,
    /// The block every hop prices at (the RPC anchor and the context's subject).
    pub block: u64,
    /// The hops, in path order.
    pub hops: Vec<ExactInputHop>,
}

/// One path's exact-input answer: the per-hop inputs consumed and outputs
/// produced, plus the block context the whole vector belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactInputQuote {
    /// The registered engine path this answer prices.
    pub path_id: u64,
    /// The block every hop priced at.
    pub block: u64,
    /// Per-hop consumed inputs, in path order (hop 1's entry is the fixed input).
    pub consumed_inputs: Vec<U256>,
    /// Per-hop outputs, in path order.
    pub hop_outputs: Vec<U256>,
    /// The anchor's block facts, read at [`Self::block`].
    pub block_context: ProbeBlockContext,
}

/// Why an exact-input quote could not be produced.
///
/// Every variant names the hop it came from, so a refusal reads as "this hop of
/// this path", never as a bare transport error.
#[derive(Debug, Error)]
pub enum ExactInputQuoteError {
    /// The path id is not registered on the engine.
    #[error("path {path_id} is not registered")]
    PathNotRegistered {
        /// The path that was asked for.
        path_id: u64,
    },
    /// A hop's pool id is not registered in the shared state.
    #[error("pool {pool_id} of path {path_id} is not registered")]
    PoolNotRegistered {
        /// The path whose hop could not be resolved.
        path_id: u64,
        /// The missing pool id.
        pool_id: u64,
    },
    /// A hop's family has neither an in-process read nor a canonical quoter.
    #[error("hop {hop} of path {path_id} has no exact-input quote arm ({hop_type:?})")]
    UnsupportedHop {
        /// The path whose hop was refused.
        path_id: u64,
        /// The hop's index in path order.
        hop: usize,
        /// The hop's family tag.
        hop_type: HopType,
    },
    /// The in-process (V2) pool math refused the hop.
    #[error("hop {hop} of path {path_id} refused the in-process quote: {error:?}")]
    PoolMath {
        /// The path whose hop was refused.
        path_id: u64,
        /// The hop's index in path order.
        hop: usize,
        /// The family's typed refusal (a not-computable walk).
        error: SimulateSwapError,
    },
    /// The reserve-pair state at the quote's block could not be read, so the hop
    /// has no anchor-correct reserves to price.
    #[error(
        "hop {hop} of path {path_id}: the reserves of {pool} at block {block} could not be read: {source}"
    )]
    ReservePairAnchor {
        /// The path whose hop was refused.
        path_id: u64,
        /// The hop's index in path order.
        hop: usize,
        /// The pair whose `getReserves()` was read.
        pool: Address,
        /// The block every hop of the quote prices at.
        block: u64,
        /// The provider's failure.
        #[source]
        source: ProviderError,
    },

    /// The on-chain quoter refused (or could not answer) the hop.
    #[error("hop {hop} of path {path_id}: {source}")]
    HopQuote {
        /// The path whose hop was refused.
        path_id: u64,
        /// The hop's index in path order.
        hop: usize,
        /// The quoter's refusal — a revert (`ExecutionReverted`) is the chain
        /// declining to fill the FULL input at this block.
        #[source]
        source: ProviderError,
    },
    /// The anchor block's own context could not be read.
    #[error("path {path_id}: the block context at {block} could not be read: {source}")]
    BlockContext {
        /// The path being quoted.
        path_id: u64,
        /// The anchor block.
        block: u64,
        /// The provider's failure.
        #[source]
        source: ProviderError,
    },
}

/// The one transport an exact-input quote needs: the read primitive every quoter
/// call crosses, plus the anchor block's own context.
///
/// Implemented for [`AlloyProvider`] (the production path) and by test doubles
/// that key canned answers on the encoded calldata — such a double asserts the
/// calldata the chain would receive.
pub trait ProbeQuoteTransport: ContractRead + Send + Sync {
    /// Read the anchor block's context: its timestamp and base fee.
    ///
    /// # Errors
    ///
    /// The provider's own failure (a missing block, a transport refusal).
    fn block_context(
        &self,
        block: u64,
    ) -> impl Future<Output = ProviderResult<ProbeBlockContext>> + Send;
}

impl ProbeQuoteTransport for AlloyProvider {
    async fn block_context(&self, block: u64) -> ProviderResult<ProbeBlockContext> {
        let block_data =
            self.get_block(block)
                .await?
                .ok_or_else(|| ProviderError::InvalidResponse {
                    message: format!("block {block} is not available"),
                })?;
        Ok(ProbeBlockContext {
            block,
            base_fee_next: u128::from(block_data.header.inner.base_fee_per_gas.unwrap_or(0)),
            timestamp: block_data.header.inner.timestamp,
        })
    }
}

/// Price one path's fixed input along its plan, at the plan's block.
///
/// The hops run in path order and each hop's output is the next hop's input, so
/// the returned vectors are one coherent chain of amounts rather than per-hop
/// answers to a shared input.
///
/// # Errors
///
/// [`ExactInputQuoteError::PoolMath`] when an in-process hop's own math refuses,
/// [`ExactInputQuoteError::ReservePairAnchor`] when an in-process hop's reserves
/// at the anchor block cannot be read — including when the pair answers a word
/// wider than its own `uint112` field, since that read validates,
/// [`ExactInputQuoteError::HopQuote`] when the canonical quoter refuses (or
/// cannot answer) a concentrated hop, and
/// [`ExactInputQuoteError::BlockContext`] when the anchor block's context cannot
/// be read.
pub async fn quote_exact_input_hops<T: ProbeQuoteTransport>(
    plan: &ExactInputQuotePlan,
    transport: &T,
    amount_in: U256,
) -> Result<ExactInputQuote, ExactInputQuoteError> {
    let mut consumed_inputs = Vec::with_capacity(plan.hops.len());
    let mut hop_outputs = Vec::with_capacity(plan.hops.len());
    let mut amount = amount_in;
    for (hop, spec) in plan.hops.iter().enumerate() {
        consumed_inputs.push(amount);
        amount = match spec {
            ExactInputHop::InProcess(spec) => {
                let entry = anchored_reserve_pair(plan, hop, spec, transport).await?;
                simulate_swap(&entry, spec.zero_for_one, amount).map_err(|error| {
                    ExactInputQuoteError::PoolMath {
                        path_id: plan.path_id,
                        hop,
                        error,
                    }
                })?
            }
            ExactInputHop::V3(spec) => {
                V3Quote::quote_exact_in(
                    transport,
                    V3_QUOTER,
                    &V3QuoteRequest {
                        token_in: spec.token_in,
                        token_out: spec.token_out,
                        fee: spec.fee,
                        amount_in: amount,
                        zero_for_one: spec.zero_for_one,
                        block: plan.block,
                    },
                )
                .await
                .map_err(|source| ExactInputQuoteError::HopQuote {
                    path_id: plan.path_id,
                    hop,
                    source,
                })?
                .amount_out
            }
            ExactInputHop::V4(spec) => {
                V4Quote::quote_exact_in(
                    transport,
                    V4_QUOTER,
                    &V4QuoteRequest {
                        pool_key: spec.pool_key.clone(),
                        zero_for_one: spec.zero_for_one,
                        amount_in: amount,
                        block: plan.block,
                    },
                )
                .await
                .map_err(|source| ExactInputQuoteError::HopQuote {
                    path_id: plan.path_id,
                    hop,
                    source,
                })?
                .amount_out
            }
        };
        hop_outputs.push(amount);
    }
    let block_context = transport
        .block_context(plan.block)
        .await
        .map_err(|source| ExactInputQuoteError::BlockContext {
            path_id: plan.path_id,
            block: plan.block,
            source,
        })?;
    Ok(ExactInputQuote {
        path_id: plan.path_id,
        block: plan.block,
        consumed_inputs,
        hop_outputs,
        block_context,
    })
}

/// The pool state the hop prices at `plan.block`.
///
/// A snapshot whose own stamp IS the plan's block is that block's state, so it
/// prices with no I/O. Any other stamp — a registration seed priced at the node
/// head, a sync from an adjacent block — describes a DIFFERENT state than the
/// block the vector claims and the EVM executes at, and one block of reserve
/// drift moves a whole unit of the next token in a vector whose every amount is a
/// floor. So the pair's own `getReserves()` at the plan's block replaces the
/// clone's reserves and stamp. A refused read is a typed refusal, never a fall
/// back to the mismatched snapshot.
async fn anchored_reserve_pair<T: ProbeQuoteTransport>(
    plan: &ExactInputQuotePlan,
    hop: usize,
    spec: &InProcessHop,
    transport: &T,
) -> Result<PoolEntry, ExactInputQuoteError> {
    let Some((pool, update_block)) = reserve_pair_anchor(&spec.entry) else {
        // `path_info::exact_input_hop` builds this arm only from a reserve-pair
        // entry: a family with no `getReserves` state has no anchor to read, and
        // its own quoter arm prices it.
        return Ok(spec.entry.clone());
    };
    if update_block == plan.block {
        return Ok(spec.entry.clone());
    }
    let answer = V2Reserves::read(transport, pool, Some(plan.block))
        .await
        .map_err(|source| ExactInputQuoteError::ReservePairAnchor {
            path_id: plan.path_id,
            hop,
            pool,
            block: plan.block,
            source,
        })?;
    // The read's validating decode already bounded both words to the pair's
    // `uint112` fields, so this is the typed projection into the snapshot's
    // fields — a wider word never leaves `V2Reserves::read`.
    let (reserve0, reserve1) = (answer.reserve0.to::<U112>(), answer.reserve1.to::<U112>());
    Ok(re_anchored(&spec.entry, reserve0, reserve1, plan.block))
}

/// The `(pair address, snapshot block)` a reserve-pair entry binds — `None` for a
/// family whose state carries no `getReserves`.
fn reserve_pair_anchor(pool_entry: &PoolEntry) -> Option<(Address, u64)> {
    match pool_entry {
        PoolEntry::V2(pair) => Some((pair.0.address, pair.1.update_block)),
        PoolEntry::AerodromeV2(pair) => Some((pair.0.address, pair.1.update_block)),
        _ => None,
    }
}

/// A clone of `pool_entry` carrying the anchor's reserves and block stamp.
///
/// Only the two fields the anchor read owns are written: the identity (family,
/// fees, decimals), the state nonce, and the reorg journal stay the snapshot's,
/// because a quote is a read-only projection and never a state transition.
fn re_anchored(pool_entry: &PoolEntry, reserve0: U112, reserve1: U112, block: u64) -> PoolEntry {
    let mut anchored_entry = pool_entry.clone();
    match &mut anchored_entry {
        PoolEntry::V2(pair) => {
            pair.1.reserve0 = reserve0;
            pair.1.reserve1 = reserve1;
            pair.1.update_block = block;
        }
        PoolEntry::AerodromeV2(pair) => {
            pair.1.reserve0 = reserve0;
            pair.1.reserve1 = reserve1;
            pair.1.update_block = block;
        }
        _ => {}
    }
    anchored_entry
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::{Arc, Mutex};

    use hashbrown::HashMap;

    use alloy::primitives::{address, aliases::U112, Address, Bytes, U256};
    use degenbot_core::errors::{ProviderError, ProviderResult};
    use degenbot_core::fee::fee_fraction;
    use degenbot_pools::solve::{HopType, MixedPoolRef, PoolHop};
    use degenbot_pools::{
        PoolTickCoverage, RegisterAerodromeV2PoolParams, RegisterV3PoolParams,
        RegisterV4PoolParams, V3PoolIdentity, V4PoolIdentity, V4PoolKey,
    };
    use degenbot_rpc::abi::{
        selector, ContractRead, MAX_SQRT_RATIO_MINUS_ONE, MIN_SQRT_RATIO_PLUS_ONE, V3_QUOTER,
        V4_QUOTER,
    };
    use degenbot_rpc::offline::OfflineProvider;
    use degenbot_substrate::state_lock::LockSite;
    use degenbot_substrate::EpochDelta;
    use degenbot_uniswap::dex_identity::DexVariant;

    use crate::arb_engine::path_info::exact_input_quote_plan;
    use crate::arb_engine::path_registry::PathRegistration;
    use crate::arb_engine::{ArbitrageEngine, EngineStages};

    use super::{
        quote_exact_input_hops, reserve_pair_anchor, ExactInputHop, ExactInputQuoteError,
        ProbeBlockContext, ProbeQuoteTransport,
    };

    // ── the pinned mainnet corpora ──

    /// The committed `py_oracle` V3 quoter corpus: the canonical v3-periphery
    /// quoter's recorded `eth_call` answers at block 24407242, covering BOTH
    /// directions of the WBTC/WETH 0.3% pool.
    const V3_QUOTER_CORPUS: &str = include_str!(
        "../../../../../../tests/fixtures/chain_data/1/py_oracle_uniswap_v3_quoter_block24407242.json"
    );

    /// The committed `py_oracle` V4 quoter corpus: the canonical v4-periphery
    /// quoter's exact-input answers at block 24407242 for the ETH/USDC 0.05%
    /// pool, both directions.
    const V4_QUOTER_CORPUS: &str = include_str!(
        "../../../../../../tests/fixtures/chain_data/1/py_oracle_uniswap_v4_quoter_block24407242.json"
    );

    const PIN: u64 = 24_407_242;

    const WBTC: Address = address!("0x2260fac5e5542a773aa44fbcfedf7c193bc2c599");
    const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const USDC: Address = address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    /// The hooked V4 pool's hook — an arbitrary hook-flagged address. The quote's
    /// point is that the address TRAVELS in the key, not that this hook prices.
    const HOOK: Address = address!("0x00000000000000000000000000000000000000c4");
    const POOL_MANAGER: Address = address!("0x000000000004444c5dc75cB358380D2e3dE08A90");

    // ── the over-reported V2-first row ──

    /// A WETH/USDC pair and a WBTC/USDC pair holding the MEASURED mainnet
    /// reserves of the row whose in-process hops were priced one block ahead
    /// (the addresses are fixtures; the reserves are the real ones).
    const WETH_USDC_PAIR: Address = address!("0x00000000000000000000000000000000000000b4");
    const WBTC_USDC_PAIR: Address = address!("0x0000000000000000000000000000000000000043");

    /// The block the row's concentrated hop was quoted at and the EVM executed
    /// at — the anchor every one of its hops must price at.
    const ANCHOR: u64 = 26_410_810;

    /// The block the engine's reserve-pair snapshots were seeded at: the node
    /// head, one block ahead of the anchor.
    const HEAD: u64 = 26_410_811;

    /// Hop 0's WETH/USDC reserves at the anchor block and at the head block. The
    /// head state is the anchor state after one USDC→WETH swap (1567.242473 USDC
    /// in, 0.826849466573099054 WETH out); for a `1e15` WETH input the constant
    /// product floors the anchor state to 1883749 and the head state to 1884417.
    const HOP0_ANCHOR_RESERVES: (u128, u128) = (4_668_167_384_860_983_639_910, 8_820_117_875_000);
    const HOP0_HEAD_RESERVES: (u128, u128) = (4_667_340_535_394_410_540_856, 8_821_685_117_473);

    /// Hop 1's `(USDC, WBTC)` reserves, untouched by the swap above: the same
    /// two reserves floor 1883749 → 2812 and 1884417 → 2813, so one whole WBTC
    /// unit separates the anchor vector from the head-ahead one.
    const HOP1_RESERVES: (u128, u128) = (45_446_079_957, 68_069_013);

    const GAMMA_03: u64 = 997;
    const FEE_DENOM_03: u64 = 1000;
    const SQRT_PRICE_1_1: u128 = 79_228_162_514_264_337_593_543_950_336;

    fn usdc(amount: u64) -> U112 {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(6))).to::<U112>()
    }

    fn weth(amount: u64) -> U112 {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(18))).to::<U112>()
    }

    /// The constant-product exact-input answer with the pool's 0.3% retained
    /// ratio — stated independently of the code under test.
    fn constant_product(amount_in: U256, reserve0: U256, reserve1: U256) -> U256 {
        amount_in * U256::from(GAMMA_03) * reserve1
            / (reserve0 * U256::from(FEE_DENOM_03) + amount_in * U256::from(GAMMA_03))
    }

    // ── the scripted transport ──

    /// An `(address, calldata)`-keyed transport double: one canned answer per
    /// encoded call, plus the calls and block-context reads it received. An
    /// answer is only found under the exact bytes the seam produced, addressed to
    /// the contract the seam named — which is what lets one `getReserves()`
    /// calldata answer differently per pair.
    struct ScriptedTransport {
        answers: std::collections::HashMap<(Address, Vec<u8>), Result<Bytes, ProviderError>>,
        calls: Mutex<Vec<(Address, Bytes, u64)>>,
        contexts: Mutex<Vec<u64>>,
        context: ProbeBlockContext,
    }

    impl Default for ScriptedTransport {
        fn default() -> Self {
            Self {
                answers: std::collections::HashMap::new(),
                calls: Mutex::new(Vec::new()),
                contexts: Mutex::new(Vec::new()),
                context: ProbeBlockContext {
                    block: 0,
                    base_fee_next: 0,
                    timestamp: 0,
                },
            }
        }
    }

    impl ScriptedTransport {
        fn new(block: u64) -> Self {
            Self {
                context: ProbeBlockContext {
                    block,
                    base_fee_next: 7,
                    timestamp: 1_700_000_001,
                },
                ..Self::default()
            }
        }

        fn answer(mut self, to: Address, calldata: Vec<u8>, response: Vec<u8>) -> Self {
            self.answers
                .insert((to, calldata), Ok(Bytes::from(response)));
            self
        }

        fn fail(mut self, to: Address, calldata: Vec<u8>, error: ProviderError) -> Self {
            self.answers.insert((to, calldata), Err(error));
            self
        }

        fn calls(&self) -> Vec<(Address, Bytes, u64)> {
            self.calls.lock().unwrap().clone()
        }

        fn contexts(&self) -> Vec<u64> {
            self.contexts.lock().unwrap().clone()
        }
    }

    impl ContractRead for ScriptedTransport {
        fn call(
            &self,
            to: Address,
            data: Bytes,
            block: Option<u64>,
        ) -> impl std::future::Future<Output = ProviderResult<Bytes>> + Send {
            let block = block.expect("an exact-input quote always pins its block");
            self.calls.lock().unwrap().push((to, data.clone(), block));
            let answer = self
                .answers
                .get(&(to, data.to_vec()))
                .cloned()
                .unwrap_or_else(|| {
                    Err(ProviderError::ExecutionReverted {
                        code: -32000,
                        message: "no canned answer for this calldata".into(),
                    })
                });
            std::future::ready(answer)
        }
    }

    impl ProbeQuoteTransport for ScriptedTransport {
        fn block_context(
            &self,
            block: u64,
        ) -> impl std::future::Future<Output = ProviderResult<ProbeBlockContext>> + Send {
            self.contexts.lock().unwrap().push(block);
            std::future::ready(Ok(self.context))
        }
    }

    // ── calldata builders (hand-rolled: the assertions' independent side) ──

    fn word_uint(value: U256) -> [u8; 32] {
        value.to_be_bytes()
    }

    fn word_address(value: Address) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[12..].copy_from_slice(value.as_slice());
        word
    }

    fn v3_calldata(
        token_in: Address,
        token_out: Address,
        fee: u64,
        amount_in: U256,
        limit: U256,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 5 * 32);
        out.extend_from_slice(&selector(
            b"quoteExactInputSingle(address,address,uint24,uint256,uint160)",
        ));
        for word in [
            word_address(token_in),
            word_address(token_out),
            word_uint(U256::from(fee)),
            word_uint(amount_in),
            word_uint(limit),
        ] {
            out.extend_from_slice(&word);
        }
        out
    }

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

    /// `getReserves()`'s calldata (the reserve-pair anchor read) and the
    /// `(uint112, uint112, uint32)` tuple a pair answers it with.
    fn get_reserves_calldata() -> Vec<u8> {
        selector(b"getReserves()").to_vec()
    }

    fn reserves_answer(reserve0: U256, reserve1: U256) -> Vec<u8> {
        words(&[reserve0, reserve1, U256::ZERO])
    }

    fn words(values: &[U256]) -> Vec<u8> {
        let mut out = Vec::with_capacity(values.len() * 32);
        for value in values {
            out.extend_from_slice(&value.to_be_bytes::<32>());
        }
        out
    }

    // ── fixture pools + paths ──

    /// Register a reserve-pair fixture holding `(reserve0, reserve1)` with its
    /// snapshot stamped at `block` — the state a quote plan carries for it.
    fn register_v2_at(
        engine: &mut ArbitrageEngine,
        address: Address,
        reserve0: U112,
        reserve1: U112,
        block: u64,
    ) -> u64 {
        let pool_id = engine.register_v2_pool(address, reserve0, reserve1, GAMMA_03, FEE_DENOM_03);
        assert_eq!(
            engine
                .core()
                .write_at(LockSite::Solver)
                .apply_sync_by_pool_id(pool_id, reserve0, reserve1, block),
            Some(pool_id),
            "the fixture snapshot lands on a reserve-pair pool"
        );
        pool_id
    }

    fn u112(value: u128) -> U112 {
        U256::from(value).to::<U112>()
    }

    fn register_v2(engine: &mut ArbitrageEngine) -> u64 {
        register_v2_at(
            engine,
            address!("0x0000000000000000000000000000000000000002"),
            usdc(1_500_000),
            weth(800),
            PIN,
        )
    }

    fn register_v2_twin(engine: &mut ArbitrageEngine) -> u64 {
        register_v2_at(
            engine,
            address!("0x0000000000000000000000000000000000000009"),
            usdc(1_600_000),
            weth(820),
            PIN,
        )
    }

    fn register_v3(engine: &mut ArbitrageEngine, token0: Address, token1: Address) -> u64 {
        engine.register_v3_pool(&RegisterV3PoolParams {
            address: address!("0x0000000000000000000000000000000000000003"),
            token0,
            token1,
            fee: 3_000,
            tick_spacing: 60,
            sqrt_price_x96: U256::from(SQRT_PRICE_1_1),
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            deployer: Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            ..Default::default()
        })
    }

    fn v4_key(currency0: Address, currency1: Address, hooks: Address) -> V4PoolKey {
        V4PoolKey {
            currency0,
            currency1,
            fee: 500,
            tick_spacing: 10,
            hooks,
        }
    }

    fn register_v4(engine: &mut ArbitrageEngine, pool_key: V4PoolKey) -> u64 {
        engine
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: POOL_MANAGER,
                pool_id: [0xab; 32],
                pool_key,
                hook_flags: 0,
                sqrt_price_x96: U256::from(SQRT_PRICE_1_1),
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                coverage: PoolTickCoverage::Tracked,
                ..Default::default()
            })
            .expect("register the V4 pool")
    }

    /// Store a path of the given hops directly in the engine registry — the
    /// exact shape `register_path` commits, without the solve-cycle's
    /// routability gate (this seam quotes the refs it is given; path SHAPE is
    /// the solver's subject, not this one's).
    fn store_path(engine: &mut ArbitrageEngine, hops: &[(HopType, u64, bool)]) -> u64 {
        engine.registry.commit(PathRegistration {
            signature: hops.iter().map(|(_, key, zfo)| (*key, *zfo)).collect(),
            pool_refs: hops
                .iter()
                .map(|(hop_type, pool_key, zero_for_one)| MixedPoolRef {
                    hop_type: *hop_type,
                    pool_key: *pool_key,
                    zero_for_one: *zero_for_one,
                })
                .collect(),
        })
    }

    fn plan_at(engine: &ArbitrageEngine, path_id: u64, block: u64) -> super::ExactInputQuotePlan {
        exact_input_quote_plan(engine, path_id, block).expect("the plan projects")
    }

    fn plan(engine: &ArbitrageEngine, path_id: u64) -> super::ExactInputQuotePlan {
        plan_at(engine, path_id, PIN)
    }

    // ── one mixed path, each family through its own owner ──

    /// A V2 → V3 → V4 path: the reserve-pair hop prices through the production
    /// pool math, the two concentrated hops through the canonical quoters at the
    /// plan's ONE block, and the vectors are the CHAIN of amounts rather than
    /// three answers to one shared input.
    #[tokio::test]
    async fn a_mixed_path_prices_each_family_with_its_own_owner() {
        let mut engine = ArbitrageEngine::new();
        let v2 = register_v2(&mut engine);
        let v3 = register_v3(&mut engine, WBTC, WETH);
        let key = v4_key(USDC, WETH, HOOK);
        let v4 = register_v4(&mut engine, key.clone());
        let path_id = store_path(
            &mut engine,
            &[
                (HopType::V2, v2, true),
                (HopType::V3, v3, true),
                (HopType::V4, v4, true),
            ],
        );

        let amount_in = U256::from(1_000_000u64);
        let v2_out = constant_product(
            amount_in,
            U256::from(usdc(1_500_000)),
            U256::from(weth(800)),
        );
        let v3_out = U256::from(777_000u64);
        let v4_out = U256::from(555u64);
        let transport = ScriptedTransport::new(PIN)
            .answer(
                V3_QUOTER,
                v3_calldata(
                    WBTC,
                    WETH,
                    3_000,
                    v2_out,
                    U256::from(MIN_SQRT_RATIO_PLUS_ONE),
                ),
                words(&[v3_out]),
            )
            .answer(
                V4_QUOTER,
                v4_calldata(&key, true, v3_out),
                words(&[v4_out, U256::from(31_000u64)]),
            );

        let quote = quote_exact_input_hops(&plan(&engine, path_id), &transport, amount_in)
            .await
            .expect("the mixed path quotes");

        assert_eq!(quote.path_id, path_id);
        assert_eq!(quote.block, PIN);
        assert_eq!(
            quote.consumed_inputs,
            vec![amount_in, v2_out, v3_out],
            "each hop consumes the previous hop's output"
        );
        assert_eq!(
            quote.hop_outputs,
            vec![v2_out, v3_out, v4_out],
            "one output per hop, in path order"
        );
        assert_eq!(
            quote.block_context,
            ProbeBlockContext {
                block: PIN,
                base_fee_next: 7,
                timestamp: 1_700_000_001,
            },
            "one block context for the whole vector"
        );

        let calls = transport.calls();
        assert_eq!(
            calls.len(),
            2,
            "the V2 hop prices the snapshot stamped with the plan's block: no RPC call"
        );
        assert_eq!(calls[0].0, V3_QUOTER);
        assert_eq!(calls[1].0, V4_QUOTER);
        assert!(calls.iter().all(|(_, _, block)| *block == PIN));
        assert_eq!(
            transport.contexts(),
            vec![PIN],
            "the block context is read once, at the quote's block"
        );

        // The V4 calldata carries the WHOLE key — hook included — so the hooked
        // pool is priced with its hook attached, never as its unhooked twin.
        assert_eq!(
            calls[1].1.as_ref(),
            v4_calldata(&key, true, v3_out).as_slice()
        );
        assert_eq!(
            &calls[1].1.as_ref()[4 + 5 * 32 + 12..4 + 6 * 32],
            HOOK.as_slice()
        );
    }

    /// Reverse directions keep their meaning: the V3 hop swaps its token pair and
    /// flips the quoter's price limit, and the V4 hop flips `zeroForOne`.
    #[tokio::test]
    async fn a_reverse_path_swaps_the_pair_and_the_direction() {
        let mut engine = ArbitrageEngine::new();
        let v3 = register_v3(&mut engine, WBTC, WETH);
        let key = v4_key(USDC, WETH, HOOK);
        let v4 = register_v4(&mut engine, key.clone());
        let path_id = store_path(
            &mut engine,
            &[(HopType::V3, v3, false), (HopType::V4, v4, false)],
        );

        let amount_in = U256::from(1_000_000_000_000_000_000u64);
        let v3_out = U256::from(3_011_444u64);
        let v4_out = U256::from(533_549_005_056u64);
        let transport = ScriptedTransport::new(PIN)
            .answer(
                V3_QUOTER,
                v3_calldata(
                    WETH,
                    WBTC,
                    3_000,
                    amount_in,
                    U256::from(MAX_SQRT_RATIO_MINUS_ONE),
                ),
                words(&[v3_out]),
            )
            .answer(
                V4_QUOTER,
                v4_calldata(&key, false, v3_out),
                words(&[v4_out, U256::from(58_710u64)]),
            );

        let quote = quote_exact_input_hops(&plan(&engine, path_id), &transport, amount_in)
            .await
            .expect("the reverse path quotes");
        assert_eq!(quote.consumed_inputs, vec![amount_in, v3_out]);
        assert_eq!(quote.hop_outputs, vec![v3_out, v4_out]);

        let calls = transport.calls();
        assert_eq!(
            calls[0].1.as_ref(),
            v3_calldata(
                WETH,
                WBTC,
                3_000,
                amount_in,
                U256::from(MAX_SQRT_RATIO_MINUS_ONE)
            )
            .as_slice(),
            "one-for-zero quotes the swapped pair at the high price limit"
        );
        assert_eq!(
            calls[1].1.as_ref(),
            v4_calldata(&key, false, v3_out).as_slice(),
            "the V4 hop flips zeroForOne and keeps its hook"
        );
    }

    /// The public stage surface composes the plan and the hops: one V2 → V2 path
    /// quotes end to end with no transport call at all, and the block context
    /// comes back with the vector.
    #[tokio::test]
    async fn the_engine_stages_quote_wraps_the_plan_and_the_hops() {
        let mut engine = ArbitrageEngine::new();
        let first = register_v2(&mut engine);
        let second = register_v2_twin(&mut engine);
        let stages =
            EngineStages::with_core(Arc::clone(engine.core()), Arc::new(EpochDelta::new(0u64)));
        let (path_id, created) = stages
            .register_path(vec![
                PoolHop {
                    pool_id: first,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: second,
                    zero_for_one: true,
                },
            ])
            .expect("register the two-hop V2 path");
        assert!(created);

        let amount_in = U256::from(1_000_000u64);
        let first_out = constant_product(
            amount_in,
            U256::from(usdc(1_500_000)),
            U256::from(weth(800)),
        );
        let second_out = constant_product(
            first_out,
            U256::from(usdc(1_600_000)),
            U256::from(weth(820)),
        );
        let transport = ScriptedTransport::new(PIN);
        let quote = stages
            .quote_exact_input_path(&transport, path_id, amount_in, PIN)
            .await
            .expect("the V2 path quotes");

        assert_eq!(quote.consumed_inputs, vec![amount_in, first_out]);
        assert_eq!(quote.hop_outputs, vec![first_out, second_out]);
        assert_eq!(quote.block_context.block, PIN);
        assert!(
            transport.calls().is_empty(),
            "an all-V2 path prices in process"
        );
    }

    /// An unregistered path is refused before any transport work.
    #[tokio::test]
    async fn an_unregistered_path_is_refused() {
        let engine = ArbitrageEngine::new();
        let stages =
            EngineStages::with_core(Arc::clone(engine.core()), Arc::new(EpochDelta::new(0u64)));
        let transport = ScriptedTransport::new(PIN);
        let error = stages
            .quote_exact_input_path(&transport, 7, U256::from(1u8), PIN)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ExactInputQuoteError::PathNotRegistered { path_id: 7 }
        ));
        assert!(transport.calls().is_empty());
    }

    /// A hop whose family has no exact-input arm is refused, never priced by a
    /// substitutable approximation.
    #[tokio::test]
    async fn a_hop_without_an_exact_input_arm_is_refused() {
        let mut engine = ArbitrageEngine::new();
        let v2 = register_v2(&mut engine);
        let path_id = store_path(&mut engine, &[(HopType::SolidlyStable, v2, true)]);
        let error = exact_input_quote_plan(&engine, path_id, PIN).unwrap_err();
        assert!(
            matches!(
                error,
                ExactInputQuoteError::UnsupportedHop {
                    hop: 0,
                    hop_type: HopType::SolidlyStable,
                    ..
                }
            ),
            "expected the unsupported-arm refusal, got {error:?}"
        );
    }

    /// A hop whose pool id is not in the shared state is refused by id.
    #[tokio::test]
    async fn a_hop_without_its_pool_is_refused() {
        let mut engine = ArbitrageEngine::new();
        let path_id = store_path(&mut engine, &[(HopType::V3, 4_242, true)]);
        let error = exact_input_quote_plan(&engine, path_id, PIN).unwrap_err();
        assert!(
            matches!(
                error,
                ExactInputQuoteError::PoolNotRegistered { pool_id: 4_242, .. }
            ),
            "expected the missing-pool refusal, got {error:?}"
        );
    }

    /// A quoter refusal (the chain declining to fill the FULL input — a partial
    /// fill) surfaces as the hop it came from, and no vector is returned from a
    /// partially priced path.
    #[tokio::test]
    async fn a_quoter_refusal_is_reported_as_its_hop() {
        let mut engine = ArbitrageEngine::new();
        let v3 = register_v3(&mut engine, WBTC, WETH);
        let path_id = store_path(&mut engine, &[(HopType::V3, v3, true)]);
        let amount_in = U256::from(1_000_000u64);
        let transport = ScriptedTransport::new(PIN).fail(
            V3_QUOTER,
            v3_calldata(
                WBTC,
                WETH,
                3_000,
                amount_in,
                U256::from(MIN_SQRT_RATIO_PLUS_ONE),
            ),
            ProviderError::ExecutionReverted {
                code: -32000,
                message: "execution reverted".into(),
            },
        );
        let error = quote_exact_input_hops(&plan(&engine, path_id), &transport, amount_in)
            .await
            .unwrap_err();
        match error {
            ExactInputQuoteError::HopQuote {
                path_id: refused,
                hop,
                source,
            } => {
                assert_eq!(refused, path_id);
                assert_eq!(hop, 0, "the refusal names the hop that raised it");
                assert!(matches!(source, ProviderError::ExecutionReverted { .. }));
            }
            other => panic!("expected a hop refusal, got {other:?}"),
        }
        assert!(
            transport.contexts().is_empty(),
            "a refused quote never reaches its block context"
        );
    }

    /// A zero amount is the family's own `Ok(0)` sentinel (not a refusal): the
    /// in-process arm passes the production math's answer through unchanged.
    #[tokio::test]
    async fn an_in_process_hop_passes_the_family_answer_through() {
        let mut engine = ArbitrageEngine::new();
        let v2 = register_v2(&mut engine);
        let path_id = store_path(&mut engine, &[(HopType::V2, v2, true)]);
        let quote = quote_exact_input_hops(
            &plan(&engine, path_id),
            &ScriptedTransport::new(PIN),
            U256::ZERO,
        )
        .await
        .expect("the family answers a zero amount with zero");
        assert_eq!(quote.hop_outputs, vec![U256::ZERO]);
    }

    // ── pinned-mainnet parity against the recorded quoter corpora ──

    /// At pinned mainnet block 24407242 the core's V3 vector equals the recorded
    /// canonical-quoter answers — WBTC→WETH and the reverse WETH→WBTC — with the
    /// calldata the corpora were recorded under (so the replay finds them).
    #[tokio::test]
    async fn a_v3_vector_matches_the_pinned_mainnet_quoter_both_directions() {
        let provider = OfflineProvider::from_json_str(V3_QUOTER_CORPUS)
            .expect("the committed V3 quoter corpus parses")
            .as_alloy_provider();

        for (zero_for_one, amount_in, expected) in [
            (true, 1_000_000u64, 330_067_345_350_494_671u128),
            (false, 1_000_000_000_000_000_000u64, 3_011_444u128),
        ] {
            let mut engine = ArbitrageEngine::new();
            let v3 = register_v3(&mut engine, WBTC, WETH);
            let path_id = store_path(&mut engine, &[(HopType::V3, v3, zero_for_one)]);
            let quote =
                quote_exact_input_hops(&plan(&engine, path_id), &provider, U256::from(amount_in))
                    .await
                    .expect("the recorded corpus answers the quote");
            assert_eq!(
                quote.hop_outputs,
                vec![U256::from(expected)],
                "the core's V3 vector must equal the recorded quoter answer (zfo={zero_for_one})"
            );
            assert_eq!(quote.consumed_inputs, vec![U256::from(amount_in)]);
            assert_eq!(
                quote.block_context,
                ProbeBlockContext {
                    block: PIN,
                    base_fee_next: 0,
                    timestamp: 1_770_494_987,
                },
                "the block context is the pinned block's own"
            );
        }
    }

    /// At the same pinned block the core's V4 vector equals the recorded
    /// canonical-quoter answers for the ETH/USDC pool, in BOTH directions.
    #[tokio::test]
    async fn a_v4_vector_matches_the_pinned_mainnet_quoter_both_directions() {
        let provider = OfflineProvider::from_json_str(V4_QUOTER_CORPUS)
            .expect("the committed V4 quoter corpus parses")
            .as_alloy_provider();
        let key = v4_key(Address::ZERO, USDC, Address::ZERO);

        for (zero_for_one, amount_in, expected) in [
            (false, 1_000_000u64, 479_137_520_767_023u128),
            (true, 1_000_000_000_000_000_000u64, 2_084_175_801u128),
        ] {
            let mut engine = ArbitrageEngine::new();
            let v4 = register_v4(&mut engine, key.clone());
            let path_id = store_path(&mut engine, &[(HopType::V4, v4, zero_for_one)]);
            let quote =
                quote_exact_input_hops(&plan(&engine, path_id), &provider, U256::from(amount_in))
                    .await
                    .expect("the recorded corpus answers the quote");
            assert_eq!(
                quote.hop_outputs,
                vec![U256::from(expected)],
                "the core's V4 vector must equal the recorded quoter answer (zfo={zero_for_one})"
            );
            assert_eq!(quote.block, PIN);
        }
    }

    /// A hooked V4 pool is priced on chain with its hook in the key — never
    /// white-box. The offline corpus cannot answer a hooked key (its pool is
    /// unhooked), so the quote must REFUSE rather than return a modelled figure.
    #[tokio::test]
    async fn a_hooked_v4_pool_is_priced_on_chain_and_never_white_box() {
        let provider = OfflineProvider::from_json_str(V4_QUOTER_CORPUS)
            .expect("the committed V4 quoter corpus parses")
            .as_alloy_provider();
        let mut engine = ArbitrageEngine::new();
        let hooked = register_v4(&mut engine, v4_key(Address::ZERO, USDC, HOOK));
        let path_id = store_path(&mut engine, &[(HopType::V4, hooked, true)]);

        let error = quote_exact_input_hops(
            &plan(&engine, path_id),
            &provider,
            U256::from(1_000_000_000_000_000_000u64),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, ExactInputQuoteError::HopQuote { hop: 0, .. }),
            "an unrecorded hooked key must refuse, not fall back: {error:?}"
        );

        // The same pool through a transport that answers the hooked key quotes
        // the hooked calldata: the hook address is in the key the chain sees.
        let transport = ScriptedTransport::new(PIN).answer(
            V4_QUOTER,
            v4_calldata(
                &v4_key(Address::ZERO, USDC, HOOK),
                true,
                U256::from(1_000_000u64),
            ),
            words(&[U256::from(9u64), U256::from(1u64)]),
        );
        let quote = quote_exact_input_hops(
            &plan(&engine, path_id),
            &transport,
            U256::from(1_000_000u64),
        )
        .await
        .expect("the hooked key quotes when the transport answers it");
        assert_eq!(quote.hop_outputs, vec![U256::from(9u64)]);
        let calls = transport.calls();
        assert_eq!(
            &calls[0].1.as_ref()[4 + 5 * 32 + 12..4 + 6 * 32],
            HOOK.as_slice(),
            "the hooked pool's key carries its hook to the quoter"
        );
    }

    // ── the plan ──

    /// The plan projects each family from the SHARED state: a reserve-pair hop
    /// carries the snapshot it prices, a V3 hop its direction's pair and fee, a
    /// V4 hop its whole key.
    #[tokio::test]
    async fn the_plan_projects_each_hop_from_the_registered_state() {
        let mut engine = ArbitrageEngine::new();
        let v2 = register_v2(&mut engine);
        let v3 = register_v3(&mut engine, WBTC, WETH);
        let key = v4_key(USDC, WETH, HOOK);
        let v4 = register_v4(&mut engine, key.clone());
        let path_id = store_path(
            &mut engine,
            &[
                (HopType::V2, v2, true),
                (HopType::V3, v3, false),
                (HopType::V4, v4, true),
            ],
        );

        let plan = plan(&engine, path_id);
        assert_eq!(plan.path_id, path_id);
        assert_eq!(plan.block, PIN);
        assert_eq!(plan.hops.len(), 3);
        match &plan.hops[0] {
            ExactInputHop::InProcess(hop) => {
                assert_eq!(hop.pool_id, v2);
                assert!(hop.zero_for_one);
                assert!(matches!(hop.entry, degenbot_pools::PoolEntry::V2(_)));
            }
            other => panic!("expected an in-process hop, got {other:?}"),
        }
        match &plan.hops[1] {
            ExactInputHop::V3(hop) => {
                assert_eq!(hop.token_in, WETH, "the reverse direction swaps the pair");
                assert_eq!(hop.token_out, WBTC);
                assert_eq!(hop.fee, 3_000);
                assert!(!hop.zero_for_one);
            }
            other => panic!("expected a V3 hop, got {other:?}"),
        }
        match &plan.hops[2] {
            ExactInputHop::V4(hop) => {
                assert_eq!(hop.pool_key, key);
                assert!(hop.zero_for_one);
            }
            other => panic!("expected a V4 hop, got {other:?}"),
        }
        // The identities the hops name are the registered ones.
        {
            let core = engine
                .core()
                .read_at(degenbot_substrate::state_lock::LockSite::Solver);
            assert_eq!(
                core.pool_slice::<V3PoolIdentity>(v3).map(|id| id.fee),
                Some(3_000)
            );
            assert_eq!(
                core.pool_slice::<V4PoolIdentity>(v4)
                    .map(|id| id.pool_key.clone()),
                Some(key)
            );
        }
    }

    /// The snapshot fast path: a hop whose own block stamp IS the plan's block
    /// prices the snapshot it holds, with no `getReserves()` call at all.
    #[tokio::test]
    async fn a_v2_hop_stamped_with_the_plan_block_prices_with_no_io() {
        let mut engine = ArbitrageEngine::new();
        let v2 = register_v2(&mut engine);
        let path_id = store_path(&mut engine, &[(HopType::V2, v2, true)]);
        let plan = plan(&engine, path_id);
        match &plan.hops[0] {
            ExactInputHop::InProcess(hop) => assert_eq!(
                reserve_pair_anchor(&hop.entry).map(|(_, stamp)| stamp),
                Some(PIN),
                "the fixture snapshot is stamped with the plan's block"
            ),
            other => panic!("expected an in-process hop, got {other:?}"),
        }

        let amount_in = U256::from(1_000_000u64);
        let transport = ScriptedTransport::new(PIN);
        let quote = quote_exact_input_hops(&plan, &transport, amount_in)
            .await
            .expect("the V2 hop prices in process");
        assert_eq!(
            quote.hop_outputs,
            vec![constant_product(
                amount_in,
                U256::from(usdc(1_500_000)),
                U256::from(weth(800))
            )]
        );
        assert!(
            transport.calls().is_empty(),
            "a snapshot already stamped with the plan's block needs no read"
        );
    }

    // ── the reserve-pair anchor ──

    /// The over-report that reverted the V2-first rows, at the seam that produced
    /// it: a plan at block N whose reserve-pair snapshots were seeded at N+1 must
    /// price N.
    ///
    /// The reserves are the failing row's measured states (see the fixture
    /// constants): the head-ahead snapshot vector is `[1884417, 2813]` and the
    /// anchor vector is `[1883749, 2812]`. The 2813 is what the V3 flash demanded
    /// back and the 2812 is what the callback delivered — one WBTC unit, one
    /// state.
    #[tokio::test]
    async fn a_plan_block_quote_prices_the_anchor_reserves_not_a_head_ahead_snapshot() {
        let mut engine = ArbitrageEngine::new();
        let first = register_v2_at(
            &mut engine,
            WETH_USDC_PAIR,
            u112(HOP0_HEAD_RESERVES.0),
            u112(HOP0_HEAD_RESERVES.1),
            HEAD,
        );
        let second = register_v2_at(
            &mut engine,
            WBTC_USDC_PAIR,
            u112(HOP1_RESERVES.0),
            u112(HOP1_RESERVES.1),
            HEAD,
        );
        let path_id = store_path(
            &mut engine,
            &[(HopType::V2, first, true), (HopType::V2, second, true)],
        );
        let amount_in = U256::from(1_000_000_000_000_000u64);

        // What the snapshots alone produce: the vector the failing request
        // carried. Both hops are stamped with that block, so the fast path
        // answers with no read at all.
        let head_transport = ScriptedTransport::new(HEAD);
        let head_quote =
            quote_exact_input_hops(&plan_at(&engine, path_id, HEAD), &head_transport, amount_in)
                .await
                .expect("the snapshot-stamped plan prices in process");
        assert_eq!(
            head_quote.consumed_inputs,
            vec![amount_in, U256::from(1_884_417u64)]
        );
        assert_eq!(
            head_quote.hop_outputs,
            vec![U256::from(1_884_417u64), U256::from(2_813u64)],
            "the head state floors hop 1 to the 2813 the failing row demanded"
        );
        assert!(
            head_transport.calls().is_empty(),
            "the mismatched-state vector is the snapshot's own: no read was made"
        );

        // At the plan's block the same two pairs answer the anchor state.
        let transport = ScriptedTransport::new(ANCHOR)
            .answer(
                WETH_USDC_PAIR,
                get_reserves_calldata(),
                reserves_answer(
                    U256::from(HOP0_ANCHOR_RESERVES.0),
                    U256::from(HOP0_ANCHOR_RESERVES.1),
                ),
            )
            .answer(
                WBTC_USDC_PAIR,
                get_reserves_calldata(),
                reserves_answer(U256::from(HOP1_RESERVES.0), U256::from(HOP1_RESERVES.1)),
            );
        let quote =
            quote_exact_input_hops(&plan_at(&engine, path_id, ANCHOR), &transport, amount_in)
                .await
                .expect("the anchor plan prices from the chain's own answer");
        assert_eq!(quote.block, ANCHOR);
        assert_eq!(
            quote.consumed_inputs,
            vec![amount_in, U256::from(1_883_749u64)],
            "hop 1 consumes hop 0's ANCHOR output, not the snapshot's"
        );
        assert_eq!(
            quote.hop_outputs,
            vec![U256::from(1_883_749u64), U256::from(2_812u64)],
            "the anchor state floors hop 1 to the 2812 the callback delivered"
        );
        let calls = transport.calls();
        assert_eq!(calls.len(), 2, "one anchor read per in-process hop");
        assert_eq!(calls[0].0, WETH_USDC_PAIR);
        assert_eq!(calls[1].0, WBTC_USDC_PAIR);
        assert_eq!(
            calls[0].1.as_ref(),
            get_reserves_calldata().as_slice(),
            "the anchor read is the pair's own getReserves()"
        );
        assert!(
            calls.iter().all(|(_, _, block)| *block == ANCHOR),
            "every hop reads AT the plan's block — never the snapshot's {HEAD}"
        );
    }

    /// The Aerodrome reserve-pair family is anchored by the same row: its
    /// `getReserves()` is read at the plan's block, and the production math
    /// prices the anchor state.
    #[tokio::test]
    async fn an_aerodrome_v2_hop_is_anchored_at_the_plan_block() {
        let pair = address!("0x00000000000000000000000000000000000000ad");
        let snapshot_reserves = (1_500_000u128, 800u128);
        let anchor_reserves = (1_400_000u128, 900u128);
        let mut engine = ArbitrageEngine::new();
        let pool_id = engine
            .core()
            .write_at(LockSite::Solver)
            .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
                address: pair,
                token0: Address::ZERO,
                token1: Address::ZERO,
                factory: Address::ZERO,
                variant: DexVariant::AerodromeV2Volatile,
                stable: false,
                fee: fee_fraction(3, FEE_DENOM_03),
                token0_decimals: 6,
                token1_decimals: 18,
                reserve0: u112(snapshot_reserves.0),
                reserve1: u112(snapshot_reserves.1),
                update_block: HEAD,
            })
            .expect("register the Aerodrome fixture");
        let path_id = store_path(&mut engine, &[(HopType::V2, pool_id, true)]);
        let amount_in = U256::from(1_000_000u64);

        let transport = ScriptedTransport::new(ANCHOR).answer(
            pair,
            get_reserves_calldata(),
            reserves_answer(U256::from(anchor_reserves.0), U256::from(anchor_reserves.1)),
        );
        let quote =
            quote_exact_input_hops(&plan_at(&engine, path_id, ANCHOR), &transport, amount_in)
                .await
                .expect("the Aerodrome hop quotes");

        let anchor_out = constant_product(
            amount_in,
            U256::from(anchor_reserves.0),
            U256::from(anchor_reserves.1),
        );
        let snapshot_out = constant_product(
            amount_in,
            U256::from(snapshot_reserves.0),
            U256::from(snapshot_reserves.1),
        );
        assert_ne!(anchor_out, snapshot_out, "the two states price differently");
        assert_eq!(
            quote.hop_outputs,
            vec![anchor_out],
            "the hop prices the anchor state, not the snapshot"
        );
        let calls = transport.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, pair);
        assert_eq!(calls[0].2, ANCHOR);
    }

    /// An unreadable anchor is a typed refusal that names the hop, the pair and
    /// the block — never a quiet fall back to the snapshot, whose vector would
    /// have over-reported.
    #[tokio::test]
    async fn an_unreadable_anchor_is_refused_rather_than_priced_from_the_snapshot() {
        let mut engine = ArbitrageEngine::new();
        let pair = register_v2_at(
            &mut engine,
            WETH_USDC_PAIR,
            u112(HOP0_HEAD_RESERVES.0),
            u112(HOP0_HEAD_RESERVES.1),
            HEAD,
        );
        let path_id = store_path(&mut engine, &[(HopType::V2, pair, true)]);
        let amount_in = U256::from(1_000_000_000_000_000u64);
        let transport = ScriptedTransport::new(ANCHOR).fail(
            WETH_USDC_PAIR,
            get_reserves_calldata(),
            ProviderError::ExecutionReverted {
                code: -32000,
                message: "state pruned".into(),
            },
        );

        let error =
            quote_exact_input_hops(&plan_at(&engine, path_id, ANCHOR), &transport, amount_in)
                .await
                .unwrap_err();
        match error {
            ExactInputQuoteError::ReservePairAnchor {
                path_id: refused,
                hop,
                pool,
                block,
                source,
            } => {
                assert_eq!(refused, path_id);
                assert_eq!(hop, 0, "the refusal names the hop that raised it");
                assert_eq!(pool, WETH_USDC_PAIR);
                assert_eq!(block, ANCHOR);
                assert!(matches!(source, ProviderError::ExecutionReverted { .. }));
            }
            other => panic!("expected the anchor refusal, got {other:?}"),
        }
        assert!(
            transport.contexts().is_empty(),
            "a refused quote never reaches its block context"
        );
        // The snapshot alone would have answered the over-reported 1884417.
        assert_eq!(
            constant_product(
                amount_in,
                U256::from(HOP0_HEAD_RESERVES.0),
                U256::from(HOP0_HEAD_RESERVES.1)
            ),
            U256::from(1_884_417u64)
        );
    }

    /// A reserve wider than the pair's `uint112` field is refused at the read,
    /// never truncated into a price: an anchor answer the pair cannot hold is an
    /// answer about another pool.
    #[tokio::test]
    async fn an_anchor_answer_wider_than_uint112_is_refused_at_the_decode() {
        let mut engine = ArbitrageEngine::new();
        let pair = register_v2_at(
            &mut engine,
            WETH_USDC_PAIR,
            u112(HOP0_HEAD_RESERVES.0),
            u112(HOP0_HEAD_RESERVES.1),
            HEAD,
        );
        let path_id = store_path(&mut engine, &[(HopType::V2, pair, true)]);
        let transport = ScriptedTransport::new(ANCHOR).answer(
            WETH_USDC_PAIR,
            get_reserves_calldata(),
            reserves_answer(
                U112::MAX.to::<U256>() + U256::from(1u8),
                U256::from(HOP0_HEAD_RESERVES.1),
            ),
        );

        let error = quote_exact_input_hops(
            &plan_at(&engine, path_id, ANCHOR),
            &transport,
            U256::from(1_000_000_000_000_000u64),
        )
        .await
        .unwrap_err();

        let ExactInputQuoteError::ReservePairAnchor {
            hop,
            pool,
            block,
            source,
            ..
        } = error
        else {
            panic!("an over-wide reserve word is an unreadable anchor, got {error:?}");
        };
        assert_eq!(
            (hop, pool, block),
            (0, WETH_USDC_PAIR, ANCHOR),
            "the refusal names the hop, the pair and the block"
        );
        assert!(
            matches!(source, ProviderError::DecodingError { .. }),
            "the read's validating decode refused the word: {source:?}"
        );
    }
}
