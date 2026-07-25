//! A `revm::Inspector` capturing swap-related LOG events emitted inside a
//! simulated `execute()` — the V2 `Swap`, V3 `Swap`, and V4 `Swap` events the
//! pools' own `swap()` functions emit.
//!
//! # The onchain-recompute replacement (spike KCKGP4, findings Q1 + epic 63I7WJ)
//!
//! `execute()` drives real swaps into V2/V3/V4 pools; those pools emit
//! `Swap` LOG events revm surfaces via `Inspector::log_full`. The existing
//! `degenbot_decoders::{decode_v2_swap_log, decode_v3_swap_log,
//! decode_v4_swap_log}` decode them. So "did the solver's `hop_outputs[i]`
//! match reality" is `decode_swap_log(event).amount == solver.hop_outputs[i]`
//! — no Multicall3 re-fetch, no off-chain recompute, no tick-map fetch, no
//! storage-layout coupling. This is the swap-event capture that retires
//! `diagnostic.rs::fetch_onchain` / `recompute_v2/v3/v4_amount_out`.
//!
//! # V2 capture: `Swap` (amounts), not `Sync` (reserves)
//!
//! V2 pairs emit BOTH `Sync(uint112,uint112)` (reserves) and
//! `Swap(address,uint256,uint256,uint256,uint256,address)` (in/out amounts).
//! The capture keys on `Swap` — the in/out amounts map directly to the signed
//! `amount0`/`amount1` fields (`out - in`), so the captured amount IS the hop
//! output. Capturing `Sync` (as the prototype did) left the v2 amounts zeroed
//! and required a `getAmountOut` recompute from separately-fetched reserves,
//! which is the half `diagnostic.rs` the onchain-recompute retirement deletes.
//!
//! # Why `log_full` and not `log` (spike KCKGP4, finding Q1)
//!
//! For LOG opcodes, revm calls `Inspector::log_full` (with the `&mut
//! Interpreter`), NOT `Inspector::log`. `log` fires only for the frame-init
//! value-transfer path (the `interpreter = None` arm of `inspect_logs` in
//! `revm-inspector/src/handler.rs`). The inspector's emitter address is on
//! the `Log` itself (`.address`), so the interpreter is not strictly needed
//! for the address — but the hook that fires is `log_full`.
//!
//! # The Log-type conversion (spike KCKGP4, finding Q1)
//!
//! `Inspector::log_full` hands out `alloy_primitives::Log`, but the decoders
//! consume `alloy::rpc::types::Log` (an RPC wrapper around `primitives::Log`
//! plus optional block/tx metadata). The conversion happens here, at the
//! inspector boundary, by wrapping the captured `primitives::Log` into an RPC
//! `Log` (all metadata `None`) before decoding. The captured struct
//! [`CapturedSwap`] stores the DECODED fields — no `Log` type in the public
//! surface. Whether the decoders should be widened to accept `primitives::Log`
//! directly is a decision for the JHPW5W follow-on task.

use std::cell::RefCell;
use std::rc::Rc;

use alloy::primitives::{Address, Log, I256, U256};
use alloy::rpc::types::Log as RpcLog;
use degenbot_decoders::{
    v2_swap_decoder::{decode_v2_swap_log, V2_SWAP_TOPIC},
    v3_swap_decoder::{decode_v3_swap_log, V3_SWAP_TOPIC},
    v4_swap_decoder::{decode_v4_swap_log, V4_SWAP_TOPIC},
};
use revm::inspector::Inspector;
use revm::interpreter::Interpreter;
use serde::Serialize;

/// The Uniswap pool family a captured swap event belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SwapFamily {
    /// Uniswap-V2 (or V2-compatible) — `Sync(uint112,uint112)`.
    V2,
    /// Uniswap-V3 — `Swap(address,address,int256,int256,uint160,uint128,int24)`.
    V3,
    /// Uniswap-V4 PoolManager —
    /// `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)`.
    ///
    /// **V4 amount correctness is blocked on task `5RI47E`** (the V4
    /// PoolManager transient-storage slot-key mapping): without the
    /// transient seeder, the PoolManager reads stale on-chain state and the
    /// V4 `Swap` amounts would not match the solver's `hop_outputs` for
    /// reasons unrelated to a solver bug. V4 capture is wired (the hook
    /// fires + the decoder runs); the *correctness* of the captured amounts
    /// is the 5RI47E-gated question.
    V4,
}

/// A captured swap event — the decoded, pool-family-tagged shape consumed by
/// the backrun strategy's four-way classifier (Drift/SolverCalc/Encoding).
/// Engine-generic: no `SimulateContext`/`SimResult`/strategy vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CapturedSwap {
    /// The pool/PoolManager contract that emitted the event.
    pub emitter: Address,
    /// Which Uniswap family emitted the event.
    pub family: SwapFamily,
    /// Amount of token0/currency0 (signed — negative for exact-in).
    pub amount0: I256,
    /// Amount of token1/currency1 (signed — negative for exact-in).
    pub amount1: I256,
    /// Post-swap `sqrtPriceX96` (V3/V4) or `U256::ZERO` (V2 `Sync` has none).
    pub sqrt_price_x96: U256,
    /// Post-swap active liquidity (V3/V4) or `U256::ZERO` (V2 `Sync`).
    pub liquidity: U256,
    /// Post-swap tick (V3/V4) or `0` (V2 `Sync`).
    pub tick: i32,
}

impl CapturedSwap {
    /// Decode a `primitives::Log` into a [`CapturedSwap`] if it is a V2/V3/V4
    /// swap-family event; `None` otherwise (a non-swap log, or a malformed
    /// swap log the decoder rejects). The conversion wraps the `primitives::Log`
    /// into an RPC `Log` (all metadata `None`) for the decoder boundary.
    #[must_use]
    pub fn from_log(log: &Log) -> Option<Self> {
        let topic0 = log.topics().first()?;
        let rpc_log = RpcLog {
            inner: log.clone(),
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };
        if *topic0 == V2_SWAP_TOPIC {
            // V2 `Swap(address,uint256,uint256,uint256,uint256,address)` —
            // the in/out amounts. Mapped to the V3 signed convention
            // (amount > 0 = token RECEIVED by the swapper): amount0 =
            // amount0_out - amount0_in, amount1 = amount1_out - amount1_in.
            // A V2 swap pays in exactly one direction (one non-zero *_in,
            // one non-zero *_out), so each difference fits in I256. This
            // retires `diagnostic.rs::recompute_v2_amount_out` — the
            // captured amount IS the hop output, no `getAmountOut` recompute.
            let ev = decode_v2_swap_log(&rpc_log)?;
            let amount0 = u256_to_signed_delta(ev.amount0_out, ev.amount0_in)?;
            let amount1 = u256_to_signed_delta(ev.amount1_out, ev.amount1_in)?;
            Some(Self {
                emitter: ev.pool_address,
                family: SwapFamily::V2,
                amount0,
                amount1,
                sqrt_price_x96: U256::ZERO,
                liquidity: U256::ZERO,
                tick: 0,
            })
        } else if *topic0 == V3_SWAP_TOPIC {
            let ev = decode_v3_swap_log(&rpc_log)?;
            Some(Self {
                emitter: ev.pool_address,
                family: SwapFamily::V3,
                amount0: ev.amount0,
                amount1: ev.amount1,
                sqrt_price_x96: ev.sqrt_price_x96,
                liquidity: U256::from(ev.liquidity),
                tick: ev.tick,
            })
        } else if *topic0 == V4_SWAP_TOPIC {
            let ev = decode_v4_swap_log(&rpc_log)?;
            Some(Self {
                emitter: Address::ZERO, // V4 emits from the PoolManager; the
                // decoder's `V4SwapEvent` carries `pool_id`, not the PoolManager
                // address — the emitter is on the captured `Log.address`,
                // surfaced separately by the inspector.
                family: SwapFamily::V4,
                amount0: ev.amount0,
                amount1: ev.amount1,
                sqrt_price_x96: ev.sqrt_price_x96,
                liquidity: U256::from(ev.liquidity),
                tick: ev.tick,
            })
        } else {
            None
        }
    }

    /// Decode a `primitives::Log` into a [`CapturedSwap`], preserving the
    /// emitter address from the log (the V4 decoder carries `pool_id`, not the
    /// PoolManager address — the emitter comes from the `Log.address` field).
    #[must_use]
    pub fn from_log_with_emitter(log: &Log) -> Option<Self> {
        let mut swap = Self::from_log(log)?;
        // For V4, the emitter (PoolManager) is the Log's address, not the
        // decoded `pool_id`. Override the zeroed emitter set in `from_log`.
        if swap.family == SwapFamily::V4 {
            swap.emitter = log.address;
        }
        Some(swap)
    }
}

/// Map a V2 swap in/out amount pair to the V3 signed-delta convention:
/// `out - in` (positive = token received by the swapper, negative = paid in).
///
/// Returns `None` if either operand exceeds `i256::MAX` (cannot fit a signed
/// delta) — in practice a V2 swap pays in one direction so one operand is zero
/// and the non-zero one is a `uint256` amount; only an adversarially-malformed
/// log with both directions set triggers the `None`.
#[must_use]
fn u256_to_signed_delta(amount_out: U256, amount_in: U256) -> Option<I256> {
    let out = I256::try_from(amount_out).ok()?;
    let inm = I256::try_from(amount_in).ok()?;
    Some(out - inm)
}

/// The internal buffer shared between the inspector + its handle.
#[derive(Debug, Default)]
pub(super) struct SwapEventBuffer {
    /// Every swap-family event captured during the run (decode-at-capture).
    pub swaps: Vec<CapturedSwap>,
    /// Every `log_full` invocation count (diagnostic — includes non-swap logs).
    pub log_full_count: usize,
}

/// A `revm::Inspector` capturing swap-family LOG events. Shares an
/// `Rc<RefCell<SwapEventBuffer>>` (the same handle shape
/// [`AccessListCollector`](super::super::AccessListCollector) uses) so the
/// caller drains after `inspect_one` moves the inspector into the EVM.
#[derive(Debug, Clone)]
pub struct SwapEventCaptureInspector {
    buf: Rc<RefCell<SwapEventBuffer>>,
}

/// A read/drain handle to a [`SwapEventCaptureInspector`] that was moved into
/// an EVM via `inspect_one`. Drains the captured swaps after the run.
#[derive(Debug, Clone)]
pub struct SwapEventCaptureHandle {
    buf: Rc<RefCell<SwapEventBuffer>>,
}

impl Default for SwapEventCaptureInspector {
    /// An inspector with no handle — the placeholder baked into a per-block
    /// EVM via `build_mainnet_with_inspector`.
    fn default() -> Self {
        Self {
            buf: Rc::new(RefCell::new(SwapEventBuffer::default())),
        }
    }
}

impl SwapEventCaptureInspector {
    /// Create an inspector + its drain handle (shared `Rc<RefCell<...>>`).
    #[must_use]
    pub fn new() -> (Self, SwapEventCaptureHandle) {
        let buf = Rc::new(RefCell::new(SwapEventBuffer::default()));
        (
            Self {
                buf: Rc::clone(&buf),
            },
            SwapEventCaptureHandle { buf },
        )
    }
}

impl SwapEventCaptureHandle {
    /// Drain the captured swaps + reset the buffer for reuse on the next path.
    #[must_use]
    pub fn take_swaps(&self) -> Vec<CapturedSwap> {
        let mut buf = self.buf.borrow_mut();
        let swaps = std::mem::take(&mut buf.swaps);
        buf.log_full_count = 0;
        swaps
    }

    /// The number of `log_full` invocations (including non-swap logs) —
    /// diagnostic, for parity tests confirming the hook fires.
    #[must_use]
    pub fn log_full_count(&self) -> usize {
        self.buf.borrow().log_full_count
    }
}

impl<CTX, INTR: revm::interpreter::InterpreterTypes> Inspector<CTX, INTR>
    for SwapEventCaptureInspector
{
    /// Fires for every LOG opcode during instruction execution (spike KCKGP4
    /// Q1: `log` does NOT fire for instruction logs — only `log_full` does).
    fn log_full(&mut self, _interp: &mut Interpreter<INTR>, _ctx: &mut CTX, log: Log) {
        self.buf.borrow_mut().log_full_count += 1;
        if let Some(swap) = CapturedSwap::from_log_with_emitter(&log) {
            self.buf.borrow_mut().swaps.push(swap);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, B256};
    use degenbot_decoders::v2_swap_decoder::V2_SWAP_TOPIC;

    fn v2_swap_log(
        pool: Address,
        sender: Address,
        to: Address,
        amount0_in: U256,
        amount1_in: U256,
        amount0_out: U256,
        amount1_out: U256,
    ) -> Log {
        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(&amount0_in.to_be_bytes::<32>());
        data.extend_from_slice(&amount1_in.to_be_bytes::<32>());
        data.extend_from_slice(&amount0_out.to_be_bytes::<32>());
        data.extend_from_slice(&amount1_out.to_be_bytes::<32>());
        Log::new_unchecked(
            pool,
            vec![V2_SWAP_TOPIC, sender.into_word(), to.into_word()],
            Bytes::from(data),
        )
    }

    #[test]
    fn non_swap_log_returns_none() {
        let log = Log::new_unchecked(
            Address::repeat_byte(0x42),
            vec![B256::repeat_byte(0xab)],
            Bytes::new(),
        );
        assert!(CapturedSwap::from_log(&log).is_none());
    }

    #[test]
    fn v2_swap_log_decodes_to_captured_swap_with_amounts() {
        // A V2 swap paying 1e18 token0 IN, receiving 3000e6 token1 OUT.
        // amount0 = out - in = 0 - 1e18 = -1e18 (paid in, negative).
        // amount1 = out - in = 3000e6 - 0 = +3000e6 (received, positive).
        let pool = Address::repeat_byte(0x42);
        let sender = Address::repeat_byte(0x11);
        let to = Address::repeat_byte(0x22);
        let amount0_in = U256::from(1_000_000_000_000_000_000_u64);
        let amount1_out = U256::from(3_000_000_000_u64);
        let log = v2_swap_log(
            pool,
            sender,
            to,
            amount0_in,
            U256::ZERO,
            U256::ZERO,
            amount1_out,
        );
        let swap = CapturedSwap::from_log_with_emitter(&log).expect("V2 Swap decodes");
        assert_eq!(swap.emitter, pool);
        assert_eq!(swap.family, SwapFamily::V2);
        assert_eq!(
            swap.amount0,
            I256::try_from(-1_000_000_000_000_000_000_i128).unwrap(),
            "amount0 = out - in = -1e18 (token0 paid in)"
        );
        assert_eq!(
            swap.amount1,
            I256::try_from(3_000_000_000_i128).unwrap(),
            "amount1 = out - in = +3000e6 (token1 received)"
        );
        assert_eq!(swap.sqrt_price_x96, U256::ZERO);
        assert_eq!(swap.tick, 0);
    }

    #[test]
    fn v2_swap_log_reverse_direction_negative_amount1() {
        // Reverse: paying token1 in, receiving token0 out.
        let pool = Address::repeat_byte(0x42);
        let sender = Address::repeat_byte(0x11);
        let to = Address::repeat_byte(0x22);
        let log = v2_swap_log(
            pool,
            sender,
            to,
            U256::ZERO,
            U256::from(500_u64),
            U256::from(2_u64),
            U256::ZERO,
        );
        let swap = CapturedSwap::from_log_with_emitter(&log).expect("V2 Swap decodes");
        assert_eq!(swap.amount0, I256::try_from(2_i128).unwrap(), "+2 received");
        assert_eq!(
            swap.amount1,
            I256::try_from(-500_i128).unwrap(),
            "-500 paid in"
        );
    }

    #[test]
    fn empty_log_with_no_topics() {
        let log = Log::new_unchecked(Address::repeat_byte(0x42), vec![], Bytes::new());
        assert_eq!(CapturedSwap::from_log(&log), None);
    }

    #[test]
    fn v2_swap_topic_is_known_constant() {
        // Sanity: the topic0 the decoder keys on matches what an actual V2
        // Swap emits (keccak256("Swap(address,uint256,uint256,uint256,uint256,address)"))
        // — verified against `cast keccak`.
        assert_eq!(
            V2_SWAP_TOPIC,
            B256::new([
                0xd7, 0x8a, 0xd9, 0x5f, 0xa4, 0x6c, 0x99, 0x4b, 0x65, 0x51, 0xd0, 0xda, 0x85, 0xfc,
                0x27, 0x5f, 0xe6, 0x13, 0xce, 0x37, 0x65, 0x7f, 0xb8, 0xd5, 0xe3, 0xd1, 0x30, 0x84,
                0x01, 0x59, 0xd8, 0x22,
            ])
        );
    }
}
