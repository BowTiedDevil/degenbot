//! The black-box V4 leg quoter — exact hooked-pool pricing by executing the
//! chain's own answer (ADR-090).
//!
//! A hop through a V4 pool whose hooks modify amounts cannot be priced by the
//! white-box concentrated-liquidity math: a hook's `beforeSwap`/`afterSwap`
//! deltas are arbitrary bytecode. The remedy is to stop modelling and execute:
//! this module drives the DEPLOYED V4 `Quoter` ([`V4_QUOTER`],
//! `quoteExactInputSingle`) on the evm2 engine, and the quoter's
//! unlock → swap → capture dance runs the real `PoolManager`, the real pool,
//! and the real hook contracts. The answer is exact by construction because it
//! is the chain's own answer — no hook semantics are modeled here.
//!
//! # State
//!
//! PoolManager pool storage is computable ([`leg_state_slots`]: `slot0` and
//! `liquidity`, at `keccak256(pool_id, 6)`), so a caller can warm those words
//! in the per-anchor prefetch batch (`crate::sim::prefetch`). Hook storage is
//! arbitrary and has no derivation; it resolves through the DB's
//! anchor-pinned RPC fallback, one slot read at a time. The layered
//! `CacheDB` beneath a [`Evm2ScratchEvm`] is therefore the per-anchor storage
//! memo: a second quote on the same scratch reads no slot twice. Nothing is
//! cached across anchors — the scratch's DB IS the anchor, and every value it
//! holds was read from the node at that anchor.
//!
//! # Learning the hook's slot SET
//!
//! The one thing a quote CAN carry across anchors is which slots the hook
//! touched: [`V4BlackBoxQuote::hook_slots`] is the frame's settled touched set
//! at the pool key's hook address, and a quoter holding a
//! [`HookSlotStore`](super::hook_slots::HookSlotStore) records it (union-merged,
//! capped) so the anchor's prefetch can join those slots into its ONE batch.
//! The VALUES are never carried: every prefetch member is read from the node at
//! the anchor the quote runs at, so a drifted hook word costs one wasted batch
//! member, never a wrong price.
//!
//! The production seam is `Evm2BlockSimHandle::scratch_evm`: its scratch is
//! pinned to the handle's block, shares the process-wide warm code cache, and
//! already carries the interpreter runner, so a quoter built over it prices legs
//! on the same chain view the verification frame executes.
//!
//! # The quoter's limits
//!
//! Two are structural, and both fail loudly rather than silently misprice:
//!
//! - **One `PoolManager` per quoter.** The deployed quoter binds the canonical
//!   singleton immutably (its `poolManager()`), so a leg on another manager
//!   cannot be priced by it. [`V4BlackBoxQuoter`] resolves the binding once
//!   per instance and refuses a mismatched leg
//!   ([`V4QuoteError::ManagerMismatch`]) instead of quoting the wrong pool.
//! - **A call-shaped frame, not a transaction.** The quoter is invoked from an
//!   EOA-shaped frame with no funds (the frame replay runs without the balance
//!   check), so a hook that demands actual token transfers inside the swap
//!   reverts here exactly as it reverts under `eth_call` — a hook whose pricing
//!   depends on settlement is outside what any quoter can answer, and the
//!   revert is the honest report.
//!
//! # Example
//!
//! ```ignore
//! let mut quoter = V4BlackBoxQuoter::default();     // canonical quoter, zero sender
//! let leg = V4Leg::canonical(pool_key, /* zero_for_one */ true);
//! let quote = quoter.quote(scratch, &leg, amount_in)?;   // scratch: anchor-pinned
//! let amount_out = quote.amount_out;
//! ```

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{address, Address, Bytes, U256};

use ::evm2::evm::Database;

use degenbot_pools::{
    v4_liquidity_slot, v4_pool_id, v4_pool_state_base_slot, v4_slot0_slot, V4PoolKey,
};

use super::frame_replay::Evm2ScratchEvm;
use super::hook_slots::HookSlotStore;
use crate::sim::frame_replay::{BaseFeeSource, ReplayFrameError, ReplayableTx};
use crate::sim::RunVerdict;

/// The canonical mainnet `PoolManager` singleton — every mainnet V4 pool's
/// storage lives here.
pub const V4_POOL_MANAGER: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");

/// The canonical mainnet V4 `Quoter` (v4-periphery `Quoter`, bound to
/// [`V4_POOL_MANAGER`]). Its `quoteExactInputSingle` is the execute-then-revert
/// leg primitive.
pub const V4_QUOTER: Address = address!("52F0E24D1c21C8A0cB1e5a5dD6198556BD9E1203");

/// `quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))`
/// (`cast sig`); the module tests re-derive it from the signature.
const QUOTE_EXACT_INPUT_SINGLE_SELECTOR: [u8; 4] = [0xaa, 0x9d, 0x21, 0xcb];

/// `poolManager()` (`cast sig`); the module tests re-derive it from the
/// signature.
const POOL_MANAGER_SELECTOR: [u8; 4] = [0xdc, 0x4c, 0x90, 0xd3];

/// The frame gas limit a quote runs under: generous enough for a full tick walk
/// plus a stateful hook's `beforeSwap`/`afterSwap`, and just under the engine's
/// per-transaction gas cap (2^24), which a frame's gas limit may not exceed. A
/// leg that exhausts it is reported as [`V4QuoteError::Halted`], never priced.
pub const V4_QUOTE_GAS_LIMIT: u64 = 16_000_000;

/// The default `max_fee_per_gas`/`max_priority_fee_per_gas` of a quote frame —
/// the production executor's gas policy. The frame's base-fee check only needs
/// the cap to clear the anchor's base fee.
pub const V4_QUOTE_MAX_FEE_PER_GAS: u128 = 200_000_000_000;
/// See [`V4_QUOTE_MAX_FEE_PER_GAS`].
pub const V4_QUOTE_MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000;

/// One V4 leg to price black-box: the pool's manager, key, and direction.
///
/// The key carries everything the swap needs — the pool id is
/// `keccak256(abi.encode(key))`, so nothing else identifies the pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4Leg {
    /// The `PoolManager` holding the pool (must be the quoter's manager).
    pub pool_manager: Address,
    /// The pool's immutable key (currencies, fee, tick spacing, hooks).
    pub pool_key: V4PoolKey,
    /// `true` = `currency0` in, `currency1` out.
    pub zero_for_one: bool,
}

impl V4Leg {
    /// A leg on `pool_manager`.
    #[must_use]
    pub fn new(pool_manager: Address, pool_key: V4PoolKey, zero_for_one: bool) -> Self {
        Self {
            pool_manager,
            pool_key,
            zero_for_one,
        }
    }

    /// A leg on the canonical mainnet [`V4_POOL_MANAGER`] — the only manager
    /// [`V4_QUOTER`] can price.
    #[must_use]
    pub fn canonical(pool_key: V4PoolKey, zero_for_one: bool) -> Self {
        Self::new(V4_POOL_MANAGER, pool_key, zero_for_one)
    }

    /// The leg's pool id (`keccak256(abi.encode(pool_key))`).
    #[must_use]
    pub fn pool_id(&self) -> alloy::primitives::B256 {
        v4_pool_id(&self.pool_key)
    }

    /// The currency the leg spends — the amount the caller denominates.
    #[must_use]
    pub fn amount_in_currency(&self) -> Address {
        if self.zero_for_one {
            self.pool_key.currency0
        } else {
            self.pool_key.currency1
        }
    }
}

/// The two `PoolManager` storage words every quote of a leg reads, in read
/// order: `slot0` (price/tick/fees) and `liquidity`. Computable from the key
/// alone, so a per-anchor prefetch can warm them in one batch; the tick bitmap
/// words and tick infos beyond it depend on where the walk crosses and stay on
/// the RPC fallback.
#[must_use]
pub fn leg_state_slots(pool_key: &V4PoolKey) -> [U256; 2] {
    let base = v4_pool_state_base_slot(v4_pool_id(pool_key));
    [v4_slot0_slot(base), v4_liquidity_slot(base)]
}

/// The slots a frame touched at `hook`, ascending — the hook's own storage,
/// and nothing else: the PoolManager's words are computable
/// ([`leg_state_slots`]) and belong to the path derivation, while storage at
/// any other address is some other contract's business.
fn hook_slots_of(touched: &[(Address, Vec<U256>)], hook: Address) -> Vec<U256> {
    if hook.is_zero() {
        return Vec::new();
    }
    touched
        .iter()
        .filter(|(address, _)| *address == hook)
        .flat_map(|(_, slots)| slots.iter().copied())
        .collect()
}

/// The caller and gas policy of a quote frame. A quote is `eth_call`-shaped:
/// the sender is a plain EOA (no funds needed — the frame replay does not
/// check balances), and its nonce must be the anchor's nonce for that account
/// (the frame replay does check nonces).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V4QuoterConfig {
    /// The quoter contract to execute.
    pub quoter: Address,
    /// The frame's caller — the value hooks observe as `msg.sender`'s
    /// counterpart/tx origin.
    pub from: Address,
    /// The caller's nonce at the anchor.
    pub nonce: u64,
    /// The frame's gas limit.
    pub gas_limit: u64,
    /// The frame's fee cap.
    pub max_fee_per_gas: u128,
    /// The frame's priority fee.
    pub max_priority_fee_per_gas: u128,
}

impl Default for V4QuoterConfig {
    /// The canonical quoter, called from the null account (nonce 0, no code,
    /// no balance — no RPC account read, and a hook inspecting the origin sees
    /// the same value the node's `eth_call` from that address would see).
    fn default() -> Self {
        Self {
            quoter: V4_QUOTER,
            from: Address::ZERO,
            nonce: 0,
            gas_limit: V4_QUOTE_GAS_LIMIT,
            max_fee_per_gas: V4_QUOTE_MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: V4_QUOTE_MAX_PRIORITY_FEE_PER_GAS,
        }
    }
}

impl V4QuoterConfig {
    /// Point the frame at `from` (with its anchor nonce) — the fidelity knob
    /// for hooks that read the origin.
    #[must_use]
    pub fn with_caller(mut self, from: Address, nonce: u64) -> Self {
        self.from = from;
        self.nonce = nonce;
        self
    }

    /// The frame transaction for one call.
    fn transaction(&self, to: Address, data: Bytes) -> ReplayableTx {
        ReplayableTx {
            from: self.from,
            to: Some(to),
            value: U256::ZERO,
            data,
            gas_limit: self.gas_limit,
            max_fee_per_gas: self.max_fee_per_gas,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas,
            nonce: self.nonce,
        }
    }
}

/// One exact leg quote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4BlackBoxQuote {
    /// The leg's output amount, in the output currency's base units — the
    /// answer, exact to the wei.
    pub amount_out: U256,
    /// The quoter's own second return word (its `gasEstimate`): the swap's gas
    /// inside the quote frame. Diagnostic — it scales with the frame's gas
    /// limit, so it is never part of a parity comparison.
    pub gas_estimate: U256,
    /// Gas the whole quote frame burned (the outer call, not the reported
    /// estimate).
    pub gas_used: u64,
    /// Cold reads the quote forwarded to the DB leaf (zero on a warm scratch —
    /// the per-anchor memo already holds the leg's words).
    pub rpc_reads: u64,
    /// Wall time of the quote frame.
    pub wall: Duration,
    /// Whether the projected base fee validated the frame.
    pub base_fee_source: BaseFeeSource,

    /// The slots the frame touched at the pool key's hook address, ascending —
    /// the hook's own storage, which has no derivation. Empty for a pool with
    /// no hook.
    pub hook_slots: Vec<U256>,

    /// How many of [`Self::hook_slots`] the quoter's store did not already
    /// hold (`0` when no store is attached, and on a re-quote of an
    /// already-learned hook).
    pub hook_slots_learned: usize,
}

/// A quote that could not be answered. A reverted/halted quoter is a
/// legitimate answer for a leg the chain cannot price — the variants carry the
/// frame's own output so the caller sees what the chain said.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum V4QuoteError {
    /// The frame never ran (DB read failure, nonce mismatch, malformed tx).
    #[error("frame refused: {0}")]
    Frame(#[from] ReplayFrameError),
    /// The quoter reverted; `data_hex` is the frame's revert payload
    /// (`alloy::hex::encode`, no `0x`) — the quoter's custom errors carry the
    /// inner `PoolManager`/hook revert.
    #[error("quoter reverted (gas {gas_used}): 0x{data_hex}")]
    Reverted {
        /// Gas the frame burned before reverting.
        gas_used: u64,
        /// The revert payload, hex-encoded without a prefix.
        data_hex: String,
    },
    /// The quoter halted (out of gas or an invalid instruction) — no answer,
    /// and no revert reason.
    #[error("quoter halted (gas {gas_used}): 0x{data_hex}")]
    Halted {
        /// Gas the frame burned before halting.
        gas_used: u64,
        /// The halt's output bytes, hex-encoded without a prefix.
        data_hex: String,
    },
    /// The successful return was not the `(amountOut, gasEstimate)` pair.
    #[error("quoter returned {len} bytes, not the (amountOut, gasEstimate) pair")]
    MalformedReturn {
        /// The returned byte count.
        len: usize,
    },
    /// The leg names a `PoolManager` the quoter is not bound to.
    #[error("leg pool manager {requested} is not the quoter's bound manager {bound}")]
    ManagerMismatch {
        /// The manager the quoter resolved.
        bound: Address,
        /// The manager the leg named.
        requested: Address,
    },
    /// The quoter's `poolManager()` answer was not one address word.
    #[error("quoter's poolManager() answered {len} bytes, not one address word")]
    UnboundQuoter {
        /// The returned byte count.
        len: usize,
    },
}

/// The ABI calldata for one quote — `quoteExactInputSingle` with empty
/// `hookData` (a quoter call cannot supply the hook data a real swap would).
#[must_use]
pub fn quote_calldata(leg: &V4Leg, amount_in: U256) -> Bytes {
    // Head: the sole tuple argument's offset. Params: the 5-word PoolKey, the
    // direction flag, the amount, the hookData offset (relative to the tuple,
    // so 8 words in), and the empty hookData's length word.
    const WORDS: usize = 9;
    let mut out = Vec::with_capacity(4 + WORDS * 32);
    out.extend_from_slice(&QUOTE_EXACT_INPUT_SINGLE_SELECTOR);
    out.extend_from_slice(&word_u256(U256::from(0x20)));
    out.extend_from_slice(&word_address(leg.pool_key.currency0));
    out.extend_from_slice(&word_address(leg.pool_key.currency1));
    out.extend_from_slice(&word_u256(U256::from(leg.pool_key.fee)));
    out.extend_from_slice(&word_i32(leg.pool_key.tick_spacing));
    out.extend_from_slice(&word_address(leg.pool_key.hooks));
    out.extend_from_slice(&word_u256(U256::from(u8::from(leg.zero_for_one))));
    out.extend_from_slice(&word_u256(amount_in));
    out.extend_from_slice(&word_u256(U256::from(0x100)));
    out.extend_from_slice(&word_u256(U256::ZERO));
    Bytes::from(out)
}

/// The `(amountOut, gasEstimate)` pair a successful quote returns, or `None`
/// when the return is too short to be one.
#[must_use]
pub fn decode_quote_return(output: &[u8]) -> Option<(U256, U256)> {
    let amount_out = output.get(0..32)?;
    let gas_estimate = output.get(32..64)?;
    Some((
        U256::from_be_slice(amount_out),
        U256::from_be_slice(gas_estimate),
    ))
}

/// Price one leg black-box by executing the quoter over `scratch`.
///
/// This is the raw frame verb: it never consults the quoter's bound manager
/// ([`V4BlackBoxQuoter::quote`] is the caching, binding-checking wrapper). The
/// scratch IS the anchor — its DB pins the block, and its block env supplies
/// the frame's `number`/`timestamp`/`basefee`.
///
/// # Errors
///
/// [`V4QuoteError`]: the frame's refusal, the quoter's revert/halt, or a
/// return that is not the `(amountOut, gasEstimate)` pair.
pub fn quote_exact_in<Ext: Database>(
    scratch: &mut Evm2ScratchEvm<Ext>,
    config: &V4QuoterConfig,
    leg: &V4Leg,
    amount_in: U256,
) -> Result<V4BlackBoxQuote, V4QuoteError> {
    let transaction = config.transaction(config.quoter, quote_calldata(leg, amount_in));
    let outcome = scratch.replay(&transaction)?;
    match outcome.status {
        RunVerdict::Success => {
            let (amount_out, gas_estimate) =
                decode_quote_return(&outcome.output).ok_or(V4QuoteError::MalformedReturn {
                    len: outcome.output.len(),
                })?;
            Ok(V4BlackBoxQuote {
                amount_out,
                gas_estimate,
                gas_used: outcome.gas_used,
                rpc_reads: outcome.rpc_reads,
                wall: outcome.wall,
                base_fee_source: outcome.base_fee_source,
                hook_slots: hook_slots_of(&outcome.touched, leg.pool_key.hooks),
                hook_slots_learned: 0,
            })
        }
        RunVerdict::Reverted => Err(V4QuoteError::Reverted {
            gas_used: outcome.gas_used,
            data_hex: alloy::hex::encode(&outcome.output),
        }),
        RunVerdict::Halted => Err(V4QuoteError::Halted {
            gas_used: outcome.gas_used,
            data_hex: alloy::hex::encode(&outcome.output),
        }),
    }
}

/// Ask `config.quoter` for its bound `PoolManager` — one view frame over
/// `scratch`. The binding is immutable, so a caller resolves it once and
/// passes the answer around; [`V4BlackBoxQuoter`] caches it.
///
/// # Errors
///
/// [`V4QuoteError`]: the frame's refusal, a revert, or an answer that is not
/// one address word.
pub fn resolve_pool_manager<Ext: Database>(
    scratch: &mut Evm2ScratchEvm<Ext>,
    config: &V4QuoterConfig,
) -> Result<Address, V4QuoteError> {
    let transaction = config.transaction(config.quoter, Bytes::from_static(&POOL_MANAGER_SELECTOR));
    let outcome = scratch.replay(&transaction)?;
    match outcome.status {
        RunVerdict::Success => {
            let word = outcome
                .output
                .get(12..32)
                .ok_or(V4QuoteError::UnboundQuoter {
                    len: outcome.output.len(),
                })?;
            Ok(Address::from_slice(word))
        }
        RunVerdict::Reverted => Err(V4QuoteError::Reverted {
            gas_used: outcome.gas_used,
            data_hex: alloy::hex::encode(&outcome.output),
        }),
        RunVerdict::Halted => Err(V4QuoteError::Halted {
            gas_used: outcome.gas_used,
            data_hex: alloy::hex::encode(&outcome.output),
        }),
    }
}

/// The finalist re-quote handle: a quoter configuration plus the quoter's
/// resolved `PoolManager`, so every leg of a candidate batch is quoted knowing
/// the quoter can actually price it.
///
/// Construction is free; the first [`Self::quote`] resolves the bound manager
/// with one view frame (a warm scratch answers it without an RPC read).
pub struct V4BlackBoxQuoter {
    config: V4QuoterConfig,
    manager: Option<Address>,
    /// The learned hook slot sets this quoter teaches (see the module's
    /// learning section). `None` prices without learning.
    hook_slots: Option<Arc<HookSlotStore>>,
}

impl Default for V4BlackBoxQuoter {
    fn default() -> Self {
        Self::new(V4QuoterConfig::default())
    }
}

impl V4BlackBoxQuoter {
    /// A quoter over `config`.
    #[must_use]
    pub fn new(config: V4QuoterConfig) -> Self {
        Self {
            config,
            manager: None,
            hook_slots: None,
        }
    }

    /// Teach `store` every quote's hook-touched slot set — the learning arm of
    /// the finalist re-quote (ADR-090): one cold quote per hook, then the
    /// anchor's prefetch carries the set warm.
    #[must_use]
    pub fn with_hook_slots(mut self, store: Arc<HookSlotStore>) -> Self {
        self.hook_slots = Some(store);
        self
    }

    /// The store this quoter teaches, if any.
    #[must_use]
    pub fn hook_slots(&self) -> Option<&Arc<HookSlotStore>> {
        self.hook_slots.as_ref()
    }

    /// The quoter's configuration.
    #[must_use]
    pub fn config(&self) -> &V4QuoterConfig {
        &self.config
    }

    /// The resolved bound manager, if one has been read yet.
    #[must_use]
    pub fn bound_manager(&self) -> Option<Address> {
        self.manager
    }

    /// Price one leg, refusing a leg on a manager this quoter cannot reach.
    ///
    /// # Errors
    ///
    /// [`V4QuoteError::ManagerMismatch`] for a foreign manager; otherwise
    /// whatever [`quote_exact_in`] reports.
    pub fn quote<Ext: Database>(
        &mut self,
        scratch: &mut Evm2ScratchEvm<Ext>,
        leg: &V4Leg,
        amount_in: U256,
    ) -> Result<V4BlackBoxQuote, V4QuoteError> {
        let bound = if let Some(bound) = self.manager {
            bound
        } else {
            let bound = resolve_pool_manager(scratch, &self.config)?;
            self.manager = Some(bound);
            bound
        };
        if bound != leg.pool_manager {
            return Err(V4QuoteError::ManagerMismatch {
                bound,
                requested: leg.pool_manager,
            });
        }
        let mut quote = quote_exact_in(scratch, &self.config, leg, amount_in)?;
        if let Some(store) = self.hook_slots.as_ref() {
            let merge = store.record(leg.pool_key.hooks, quote.hook_slots.iter().copied());
            quote.hook_slots_learned = merge.added;
        }
        Ok(quote)
    }
}

/// A 32-byte word holding `value` left-padded (an ABI `address`).
fn word_address(value: Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(value.as_slice());
    word
}

/// The word for a plain unsigned ABI value.
fn word_u256(value: U256) -> [u8; 32] {
    value.to_be_bytes()
}

/// The word for an ABI `int24` — two's-complement sign extension into the full
/// word (`tickspacing` is signed on chain, and a negative spacing encoded
/// without extension is a different pool key).
fn word_i32(value: i32) -> [u8; 32] {
    let wide = i64::from(value);
    let mut word = if wide < 0 { [0xFFu8; 32] } else { [0u8; 32] };
    word[24..].copy_from_slice(&wide.to_be_bytes());
    word
}

#[cfg(test)]
#[expect(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use ::evm2::bytecode::Bytecode;
    use ::evm2::evm::{AccountInfo, InMemoryDB};
    use alloy::primitives::keccak256;

    use super::super::handle::EVM2_PRODUCTION_SPEC;
    use super::super::hook_slots::HookSlotStore;
    use crate::sim::frame_replay::ScratchBlock;

    fn usdc_weth_key(hooks: Address) -> V4PoolKey {
        V4PoolKey {
            currency0: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            currency1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            fee: 3_000,
            tick_spacing: 60,
            hooks,
        }
    }

    /// The baked selectors are the real ones — a drift here would quote
    /// nothing at all.
    #[test]
    fn selectors_match_their_signatures() {
        let quote = keccak256(
            b"quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))",
        );
        assert_eq!(quote[0..4], QUOTE_EXACT_INPUT_SINGLE_SELECTOR);
        let manager = keccak256(b"poolManager()");
        assert_eq!(manager[0..4], POOL_MANAGER_SELECTOR);
    }

    /// The hand-encoded calldata is byte-identical to foundry's encoder for the
    /// same arguments — the layout the deployed quoter decodes.
    #[test]
    fn calldata_matches_foundry_encoding() {
        let leg = V4Leg::canonical(usdc_weth_key(Address::ZERO), true);
        let ours = quote_calldata(&leg, U256::from(1_000_000));
        let golden = "aa9d21cb\
            0000000000000000000000000000000000000000000000000000000000000020\
            000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48\
            000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2\
            0000000000000000000000000000000000000000000000000000000000000bb8\
            000000000000000000000000000000000000000000000000000000000000003c\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000001\
            00000000000000000000000000000000000000000000000000000000000f4240\
            0000000000000000000000000000000000000000000000000000000000000100\
            0000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(alloy::hex::encode(&ours), golden);
    }

    /// A negative `tick_spacing` encodes sign-extended, the way a V4 key with a
    /// negative spacing hashes.
    #[test]
    fn negative_tick_spacing_sign_extends() {
        let key = V4PoolKey {
            tick_spacing: -200,
            ..usdc_weth_key(Address::ZERO)
        };
        let leg = V4Leg::canonical(key, false);
        let ours = quote_calldata(&leg, U256::from(7));
        let golden = "aa9d21cb\
            0000000000000000000000000000000000000000000000000000000000000020\
            000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48\
            000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2\
            0000000000000000000000000000000000000000000000000000000000000bb8\
            ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff38\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000007\
            0000000000000000000000000000000000000000000000000000000000000100\
            0000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(alloy::hex::encode(&ours), golden);
    }

    /// The two computable `PoolManager` words are `slot0` at the pool's state
    /// base and `liquidity` three words later — pinned against the mainnet
    /// layout the V4 journal extractors use.
    #[test]
    fn leg_state_slots_pin_the_v4_layout() {
        let key = V4PoolKey {
            currency0: address!("910B1C87bc16301FB540FAb7a3c25C617E08Efc7"),
            currency1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            fee: 0,
            tick_spacing: 200,
            hooks: address!("322dcEc4958C14e021A9F1cD49DF11b9457968cC"),
        };
        // keccak256(abi.encode(poolId, 6)) for this pinned key, cross-checked
        // against mainnet `PoolManager.extsload`.
        let base = U256::from_be_slice(
            &alloy::hex::decode("ca7f7e8342546431fb480ec734d2337b9aac82181e5735f58320e213a2945c49")
                .expect("hex"),
        );
        assert_eq!(leg_state_slots(&key), [base, base + U256::from(3)]);
    }

    // ── the offline behavioral stub ──────────────────────────────────────

    /// A quoter stub at [`STUB_QUOTER`]: `poolManager()` answers
    /// [`STUB_MANAGER`], and `quoteExactInputSingle` answers
    /// `(amountIn * 2, 7)` — reading the amount from the calldata word at byte
    /// 228 (selector + the tuple's head offset + the six key words before the
    /// amount), so a layout drift in [`quote_calldata`] prices the wrong
    /// number.
    const STUB_QUOTER: Address = address!("0000000000000000000000000000000000001234");
    const STUB_MANAGER: Address = V4_POOL_MANAGER;

    fn stub_bytecode() -> Bytes {
        let mut code = Vec::new();
        // selector = calldataload(0) >> 224
        code.extend_from_slice(&[0x60, 0x00, 0x35, 0x60, 0xE0, 0x1C]);
        // PUSH4 quoteExactInputSingle EQ PUSH1 <quote path> JUMPI
        code.push(0x63);
        code.extend_from_slice(&QUOTE_EXACT_INPUT_SINGLE_SELECTOR);
        code.push(0x14);
        code.push(0x60);
        code.push(0x2C);
        code.push(0x57);
        // poolManager(): PUSH20 manager; MSTORE(0); RETURN(0, 32)
        code.push(0x73);
        code.extend_from_slice(STUB_MANAGER.as_slice());
        code.extend_from_slice(&[0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xF3]);
        // JUMPDEST: amountIn = calldataload(228) * 2; gasEstimate = 7
        code.push(0x5B);
        code.extend_from_slice(&[0x60, 0xE4, 0x35]);
        code.extend_from_slice(&[0x60, 0x01, 0x1B]);
        code.extend_from_slice(&[0x60, 0x00, 0x52]);
        code.extend_from_slice(&[0x60, 0x07, 0x60, 0x20, 0x52]);
        code.extend_from_slice(&[0x60, 0x40, 0x60, 0x00, 0xF3]);
        Bytes::from(code)
    }

    fn stub_db() -> InMemoryDB {
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &STUB_QUOTER,
            AccountInfo::default().with_code(Bytecode::new_raw(stub_bytecode())),
        );
        db
    }

    fn scratch(db: InMemoryDB) -> Evm2ScratchEvm<InMemoryDB> {
        Evm2ScratchEvm::new(
            db,
            ScratchBlock {
                number: 1,
                timestamp: 1,
                base_fee_next: 0,
            },
        )
    }

    fn stub_config() -> V4QuoterConfig {
        V4QuoterConfig {
            quoter: STUB_QUOTER,
            ..V4QuoterConfig::default()
        }
    }

    /// The frame reaches the quoter, the calldata's amount word lands where the
    /// quoter reads it, and the return decodes into the quote — with the
    /// manager resolved once and cached.
    #[test]
    fn quote_executes_the_quoter_and_decodes_the_answer() {
        let mut scratch = scratch(stub_db());
        let mut quoter = V4BlackBoxQuoter::new(stub_config());
        let leg = V4Leg::canonical(usdc_weth_key(Address::ZERO), true);

        let quote = quoter.quote(&mut scratch, &leg, U256::from(1_500)).unwrap();
        assert_eq!(quote.amount_out, U256::from(3_000), "amountIn * 2");
        assert_eq!(quote.gas_estimate, U256::from(7));
        assert_eq!(quoter.bound_manager(), Some(STUB_MANAGER));

        // The binding came from the quoter's own frame, not from the leg.
        assert_eq!(
            super::resolve_pool_manager(&mut scratch, &stub_config()).unwrap(),
            STUB_MANAGER
        );
    }

    /// A leg on a manager the quoter is not bound to is refused before any
    /// swap frame runs.
    #[test]
    fn a_foreign_manager_is_refused() {
        let mut scratch = scratch(stub_db());
        let mut quoter = V4BlackBoxQuoter::new(stub_config());
        let foreign = address!("00000000000000000000000000000000000000FF");
        let leg = V4Leg::new(foreign, usdc_weth_key(Address::ZERO), true);
        let error = quoter
            .quote(&mut scratch, &leg, U256::from(1_000))
            .expect_err("a foreign manager cannot be priced");
        assert_eq!(
            error,
            V4QuoteError::ManagerMismatch {
                bound: STUB_MANAGER,
                requested: foreign,
            }
        );
    }

    /// A quoter address with no code returns nothing — reported as a malformed
    /// answer, never as a zero amount.
    #[test]
    fn a_codeless_quoter_is_a_malformed_answer() {
        let mut scratch = scratch(InMemoryDB::default());
        let leg = V4Leg::canonical(usdc_weth_key(Address::ZERO), true);
        let error = quote_exact_in(&mut scratch, &stub_config(), &leg, U256::from(1))
            .expect_err("an EOA cannot answer a quote");

        assert_eq!(error, V4QuoteError::MalformedReturn { len: 0 });
    }

    // ── the hook slot capture ────────────────────────────────────────────

    /// A hook contract holding its own storage.
    const STUB_HOOK: Address = address!("0000000000000000000000000000000000005678");

    /// The quoter that runs the hook before answering.
    const HOOK_CALLING_QUOTER: Address = address!("0000000000000000000000000000000000001235");

    /// `PUSH1 0x03 SLOAD POP PUSH1 0x07 SLOAD POP STOP` — two reads of the
    /// hook's own slots, and no writes at all.
    fn hook_bytecode() -> Bytes {
        Bytes::from(vec![0x60, 0x03, 0x54, 0x50, 0x60, 0x07, 0x54, 0x50, 0x00])
    }

    /// [`stub_bytecode`] with a `STATICCALL` into `hook` ahead of its
    /// `(amountIn, 7)` answer — the capture's offline subject.
    fn hook_calling_quoter_bytecode(hook: Address) -> Bytes {
        let mut code = Vec::new();
        code.extend_from_slice(&[0x60, 0x00, 0x35, 0x60, 0xE0, 0x1C]);
        code.push(0x63);
        code.extend_from_slice(&QUOTE_EXACT_INPUT_SINGLE_SELECTOR);
        code.push(0x14);
        code.push(0x60);
        code.push(0x2C);
        code.push(0x57);
        code.push(0x73);
        code.extend_from_slice(STUB_MANAGER.as_slice());
        code.extend_from_slice(&[0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xF3]);
        // JUMPDEST: retSize, retOffset, argsSize, argsOffset, address, gas.
        code.extend_from_slice(&[0x5B, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00]);
        code.push(0x73);
        code.extend_from_slice(hook.as_slice());
        code.extend_from_slice(&[0x5A, 0xFA, 0x50]);
        code.extend_from_slice(&[0x60, 0xE4, 0x35]);
        code.extend_from_slice(&[0x60, 0x00, 0x52]);
        code.extend_from_slice(&[0x60, 0x07, 0x60, 0x20, 0x52]);
        code.extend_from_slice(&[0x60, 0x40, 0x60, 0x00, 0xF3]);
        Bytes::from(code)
    }

    fn hook_calling_db() -> InMemoryDB {
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &HOOK_CALLING_QUOTER,
            AccountInfo::default()
                .with_code(Bytecode::new_raw(hook_calling_quoter_bytecode(STUB_HOOK))),
        );
        db.insert_account_info(
            &STUB_HOOK,
            AccountInfo::default().with_code(Bytecode::new_raw(hook_bytecode())),
        );
        db
    }

    /// The capture: the quote's frame reads the hook's own storage, and a
    /// quoter holding a store learns exactly those slots — the frame's settled
    /// touched set, attributed by the pool key's hook address. The answer is
    /// unchanged by the capture, and a re-quote adds nothing.
    #[test]
    fn the_quoter_learns_the_hooks_touched_slots() {
        let mut scratch = scratch(hook_calling_db());
        let config = V4QuoterConfig {
            quoter: HOOK_CALLING_QUOTER,
            ..V4QuoterConfig::default()
        };
        let store = Arc::new(HookSlotStore::in_memory(EVM2_PRODUCTION_SPEC));
        let mut quoter = V4BlackBoxQuoter::new(config).with_hook_slots(Arc::clone(&store));
        let leg = V4Leg::canonical(usdc_weth_key(STUB_HOOK), true);

        let quote = quoter.quote(&mut scratch, &leg, U256::from(1_500)).unwrap();
        assert_eq!(quote.amount_out, U256::from(1_500));
        assert_eq!(
            quote.hook_slots,
            vec![U256::from(3), U256::from(7)],
            "the frame's reads at the hook's address, ascending"
        );
        assert_eq!(quote.hook_slots_learned, 2);
        assert_eq!(
            store.slots_for(STUB_HOOK),
            vec![U256::from(3), U256::from(7)]
        );
        assert!(quoter.hook_slots().is_some());

        let again = quoter.quote(&mut scratch, &leg, U256::from(2_000)).unwrap();
        assert_eq!(again.hook_slots_learned, 0, "the set already held both");
        assert_eq!(store.slots_for(STUB_HOOK).len(), 2);
    }

    /// A pool key with no hook names no storage set: nothing is touched at the
    /// (zero) hook address, so nothing is learned.
    #[test]
    fn a_hookless_pool_learns_nothing() {
        let mut scratch = scratch(stub_db());
        let store = Arc::new(HookSlotStore::in_memory(EVM2_PRODUCTION_SPEC));
        let mut quoter = V4BlackBoxQuoter::new(stub_config()).with_hook_slots(Arc::clone(&store));
        let leg = V4Leg::canonical(usdc_weth_key(Address::ZERO), true);

        let quote = quoter.quote(&mut scratch, &leg, U256::from(1_000)).unwrap();
        assert!(quote.hook_slots.is_empty());
        assert_eq!(quote.hook_slots_learned, 0);
        assert_eq!(store.slot_count(), 0);
    }

    /// A quoter without a store still reports the touched set (the diagnostic
    /// is the frame's own answer) and learns nothing.
    #[test]
    fn a_storeless_quoter_reports_but_does_not_learn() {
        let mut scratch = scratch(hook_calling_db());
        let config = V4QuoterConfig {
            quoter: HOOK_CALLING_QUOTER,
            ..V4QuoterConfig::default()
        };
        let mut quoter = V4BlackBoxQuoter::new(config);
        let leg = V4Leg::canonical(usdc_weth_key(STUB_HOOK), true);

        let quote = quoter.quote(&mut scratch, &leg, U256::from(1_500)).unwrap();
        assert_eq!(quote.hook_slots.len(), 2);
        assert_eq!(quote.hook_slots_learned, 0);
        assert!(quoter.hook_slots().is_none());
    }
}
