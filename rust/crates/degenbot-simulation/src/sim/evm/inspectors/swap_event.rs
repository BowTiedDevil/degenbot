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
use revm::handler::FrameResult;
use revm::inspector::Inspector;
use revm::interpreter::{FrameInput, Interpreter};
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
    /// **V4 capture correctness is proven** by the `swap_capture_correctness`
    /// mainnet probe — captured V4 swaps (emitter = the PoolManager address,
    /// the post-swap `sqrtPriceX96`/`liquidity`/`tick`, AND the `int128`
    /// amounts) byte-match the onchain receipt across V2/V3/V4 families. The
    /// V4 pool state is PERSISTENT (`_pools[poolId]` at slot 6, per
    /// `docs/architecture/v4_poolmanager_storage_layout.md`), cold-loaded via
    /// `WrapDatabaseAsync<AlloyDB>` in production.
    ///
    /// **Reverted-frame over-capture (fixed):** `log_full` fires per LOG
    /// opcode BEFORE the enclosing frame's revert resolves, so a swap emitted
    /// in a reverting sub-call (common in V4's unlock/settle/take callback
    /// flow + router revert-retry) was over-captured vs the committed receipt.
    /// The frame-stack in `SwapEventBuffer` (begin_frame / end_frame_committed
    /// / end_frame_reverted, driven by `Inspector::frame_start`/`frame_end`)
    /// now drops reverted-frame swaps — `take_swaps()` returns only committed
    /// swaps, matching revm's own journal flattening. The `log_full_count`
    /// counter is unchanged (it counts hook firings, including reverted-frame
    /// logs) so the parity tests asserting the hook fires stay green.
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
    /// Committed swaps — only logs from frames that **committed** (didn't
    /// revert). Reverted-frame swaps are dropped at `end_frame_reverted` so
    /// the captured set matches the onchain committed receipt logs (revm's
    /// own journal pops reverted logs; `log_full` fires pre-revert, so without
    /// the frame stack the inspector over-captures reverted sub-call swaps —
    /// a V4-correctness gap surfaced by the `swap_capture_correctness`
    /// mainnet probe at block 25615015 tx[0], where 8 reverted-frame logs
    /// were captured against 22 receipt commits).
    pub swaps: Vec<CapturedSwap>,
    /// Swaps emitted inside a frame that **reverted** — kept SEPARATE from
    /// [`swaps`](Self::swaps) (which holds only committed swaps, matching
    /// revm's committed-receipt notion). The revert-tolerant diagnostic buffer:
    /// a V4 `unlock` that reverts with `CurrencyNotSettled` drops its inner
    /// V4/V3 swap events from `swaps`, but they land here so a driver can
    /// compare the ACTUAL swap output (`out_a'`) to the solver's predicted
    /// `hop_outputs[0]` — the decisive state-divergence-vs-composer-bug test
    /// (ergo `TR6GWT`). Drained via [`SwapEventCaptureHandle::take_reverted_swaps`].
    pub reverted_swaps: Vec<CapturedSwap>,
    /// Per-frame tentative buffers (top = innermost frame). A swap decoded by
    /// `log_full` lands in the top buffer; on `frame_end` it's either merged
    /// into the parent (commit) or moved to `reverted_swaps` (revert) —
    /// mirroring revm's journal flattening, so only committed swaps reach
    /// `swaps`, while reverted-frame swaps are preserved (not discarded) in
    /// `reverted_swaps` for the divergence diagnostic.
    frame_stack: Vec<Vec<CapturedSwap>>,
    /// Every `log_full` invocation count (diagnostic — includes non-swap logs
    /// AND reverted-frame logs; counts hook firings, NOT committed swaps).
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

    // -----------------------------------------------------------------
    // Frame-revert tracking — internal state-machine methods, kept
    // revm-frame-type-free so the frame-stack logic is unit-testable without
    // constructing `FrameInput`/`FrameResult`. The `Inspector::frame_start`
    // /`frame_end` overrides below are the thin revm-bridging layer.
    // -----------------------------------------------------------------

    /// Push a fresh tentative buffer for a new execution frame (`LOG`s decoded
    /// here land in the top buffer until the frame's commit/revert is known).
    fn begin_frame(&self) {
        self.buf.borrow_mut().frame_stack.push(Vec::new());
    }

    /// Decode + stage a `LOG` into the top frame's tentative buffer (or directly
    /// into the committed `swaps` if no frame is active — a defensive fallback
    /// for the case where `log_full` fires outside any `frame_start`/`frame_end`,
    /// which shouldn't occur under revm's frame dispatch).
    fn capture_swap_log(&self, log: &Log) {
        let Some(swap) = CapturedSwap::from_log_with_emitter(log) else {
            return;
        };
        let mut buf = self.buf.borrow_mut();
        match buf.frame_stack.last_mut() {
            Some(frame) => frame.push(swap),
            None => buf.swaps.push(swap),
        }
    }

    /// Pop the top frame + merge its tentative swaps into the parent frame (or
    /// the committed `swaps` sink if this was the root frame). Called when the
    /// frame's `FrameResult` indicates success.
    fn end_frame_committed(&self) {
        let mut buf = self.buf.borrow_mut();
        let Some(frame_swaps) = buf.frame_stack.pop() else {
            return;
        };
        match buf.frame_stack.last_mut() {
            Some(parent) => parent.extend(frame_swaps),
            None => buf.swaps.extend(frame_swaps),
        }
    }

    /// Pop the top frame + move its tentative swaps into `reverted_swaps`
    /// (NOT discarded). Called when the frame's `FrameResult` indicates revert
    /// — the swap logs it emitted are NOT in the committed receipt, so they
    /// must not be in `swaps` (the committed buffer) either. But they ARE
    /// preserved in `reverted_swaps` for the revert-tolerant divergence
    /// diagnostic (the V4 `unlock`-reverts-`CurrencyNotSettled` case where the
    /// actual swap output `out_a'` is needed to compare to `hop_outputs[0]`).
    fn end_frame_reverted(&self) {
        let mut buf = self.buf.borrow_mut();
        if let Some(frame_swaps) = buf.frame_stack.pop() {
            buf.reverted_swaps.extend(frame_swaps);
        }
    }
}

impl SwapEventCaptureHandle {
    /// Drain the captured swaps + reset the buffer for reuse on the next path.
    /// Returns only COMMITTED swaps (reverted-frame swaps are in
    /// [`take_reverted_swaps`]).
    ///
    /// [`take_reverted_swaps`]: Self::take_reverted_swaps
    #[must_use]
    pub fn take_swaps(&self) -> Vec<CapturedSwap> {
        let mut buf = self.buf.borrow_mut();
        let swaps = std::mem::take(&mut buf.swaps);
        buf.frame_stack.clear();
        buf.log_full_count = 0;
        swaps
    }

    /// Drain the reverted-frame swaps (swaps emitted inside a frame that
    /// reverted). These are NOT in the committed receipt, but preserving them
    /// lets a driver compare the actual swap output to the solver's predicted
    /// `hop_outputs` for a reverting path — the decisive
    /// state-divergence-vs-composer-bug test for V4 `CurrencyNotSettled`
    /// (ergo `TR6GWT`).
    #[must_use]
    pub fn take_reverted_swaps(&self) -> Vec<CapturedSwap> {
        let mut buf = self.buf.borrow_mut();
        std::mem::take(&mut buf.reverted_swaps)
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
    /// The decoded swap lands in the **top frame's** tentative buffer (not the
    /// committed `swaps`) so a swap emitted in a reverting sub-frame is dropped
    /// at `frame_end` — the bug `log_full` fires pre-revert.
    fn log_full(&mut self, _interp: &mut Interpreter<INTR>, _ctx: &mut CTX, log: Log) {
        self.buf.borrow_mut().log_full_count += 1;
        self.capture_swap_log(&log);
    }

    /// Every frame (the root tx + every CALL/CREATE sub-invocation) brackets
    /// with `frame_start`/`frame_end`; push a tentative buffer here, pop it at
    /// `frame_end` with commit/revert disposition from the `FrameResult`.
    fn frame_start(
        &mut self,
        _context: &mut CTX,
        _frame_input: &mut FrameInput,
    ) -> Option<FrameResult> {
        self.begin_frame();
        None
    }

    /// `FrameResult` carries the frame's success/revert; merge (commit) or drop
    /// (revert) the top frame's tentative swaps. `InstructionResult::is_success`
    /// matches revm's own committed-frame notion (success reasons only —
    /// `Revert`/`Halt` variants are NOT success).
    fn frame_end(
        &mut self,
        _context: &mut CTX,
        _frame_input: &FrameInput,
        frame_result: &mut FrameResult,
    ) {
        let committed = match frame_result {
            FrameResult::Call(outcome) => outcome.result.result.is_ok(),
            FrameResult::Create(outcome) => outcome.result.result.is_ok(),
        };
        if committed {
            self.end_frame_committed();
        } else {
            self.end_frame_reverted();
        }
    }
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
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

    // =====================================================================
    // Frame-revert tracking (ergo: V4 reverted-frame over-capture).
    //
    // `log_full` fires per LOG opcode BEFORE the enclosing frame's revert is
    // resolved, so a swap emitted in a reverting sub-call (common in V4's
    // unlock/settle/take callback flow + router revert-retry) gets captured
    // though it's absent from the committed receipt logs. The fix: a frame
    // stack — `begin_frame` pushes a tentative buffer, `log_full` pushes onto
    // the top buffer, and `end_frame_*` either merges (commit) or drops
    // (revert) the top buffer back into its parent.

    fn capture_log(pool: Address) -> Log {
        // A minimal V2 Swap-shaped log (sender=to=pool, all amounts zero) —
        // the amounts don't matter for the frame-tracking assertions, only
        // that each is a distinct decode event on the buffer.
        v2_swap_log(
            pool,
            Address::repeat_byte(0x11),
            Address::repeat_byte(0x22),
            U256::ZERO,
            U256::ZERO,
            U256::ZERO,
            U256::ZERO,
        )
    }

    #[test]
    fn reverted_subframe_swap_log_is_dropped_not_captured() {
        // root frame → root-frame swap log → a reverting sub-call emits a swap
        // → root commits. Only the root-frame swap survives.
        let (insp, handle) = SwapEventCaptureInspector::new();
        insp.begin_frame();
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xA1)));
        insp.begin_frame();
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xA2)));
        insp.end_frame_reverted();
        insp.end_frame_committed();
        let captured = handle.take_swaps();
        assert_eq!(captured.len(), 1, "reverted sub-frame swap must be dropped");
        assert_eq!(
            captured[0].emitter,
            Address::repeat_byte(0xA1),
            "only the root-frame committed swap remains"
        );
    }

    #[test]
    fn committed_subframe_swap_log_is_merged_into_parent() {
        // root + a succeeding sub-call emitting a swap → both kept.
        let (insp, handle) = SwapEventCaptureInspector::new();
        insp.begin_frame();
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xB1)));
        insp.begin_frame();
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xB2)));
        insp.end_frame_committed();
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xB3)));
        insp.end_frame_committed();
        let captured = handle.take_swaps();
        assert_eq!(captured.len(), 3, "all three committed swaps kept");
    }

    #[test]
    fn nested_reverted_subframe_drops_only_inner_logs() {
        // root log + outer-committed-sub log + inner-reverted-sub log → 2 kept.
        let (insp, handle) = SwapEventCaptureInspector::new();
        insp.begin_frame();
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xC1)));
        insp.begin_frame(); // outer sub-call (commits)
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xC2)));
        insp.begin_frame(); // inner sub-call (reverts)
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xC3)));
        insp.end_frame_reverted();
        insp.end_frame_committed();
        insp.end_frame_committed();
        let captured = handle.take_swaps();
        assert_eq!(captured.len(), 2, "inner reverted logs dropped, outer kept");
    }

    #[test]
    fn reverted_root_frame_drops_all_logs_from_committed_set() {
        // The whole tx reverted → no committed swaps, even though logs fired.
        // BUT the reverted-frame swaps are preserved in `reverted_swaps` (the
        // revert-tolerant diagnostic buffer) — `take_swaps()` returns committed
        // only (`[]` here), `take_reverted_swaps()` returns the dropped swaps.
        let (insp, handle) = SwapEventCaptureInspector::new();
        insp.begin_frame();
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xD1)));
        insp.end_frame_reverted();
        assert!(
            handle.take_swaps().is_empty(),
            "a reverted root frame drops all swaps from the committed set"
        );
        let reverted = handle.take_reverted_swaps();
        assert_eq!(
            reverted.len(),
            1,
            "the reverted swap is preserved separately"
        );
        assert_eq!(reverted[0].emitter, Address::repeat_byte(0xD1));
    }

    #[test]
    fn reverted_unlock_inner_swaps_capturable_separately() {
        // The V4-unlock-reverts-CurrencyNotSettled shape: the unlock's INNER
        // swaps (V4 + V3) committed at their own frame level + merged into
        // the unlock's tentative buffer, but the unlock frame reverts → the
        // merged swaps are dropped from `swaps` BUT preserved in
        // `reverted_swaps` so a driver can compare actual out_a' to
        // hop_outputs[0] (the state-divergence-vs-composer-bug test).
        let (insp, handle) = SwapEventCaptureInspector::new();
        // root (execute) frame.
        insp.begin_frame();
        // unlock frame (will revert at the end — CurrencyNotSettled).
        insp.begin_frame();
        // V4 swap (inner) — commits at its own level, merges into unlock.
        insp.begin_frame();
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xA1)));
        insp.end_frame_committed(); // V4 swap OK
                                    // V3 swap (inner) — commits, merges into unlock.
        insp.begin_frame();
        insp.capture_swap_log(&capture_log(Address::repeat_byte(0xA2)));
        insp.end_frame_committed(); // V3 swap OK
                                    // unlock reverts (CurrencyNotSettled) → its merged swaps drop to
                                    // reverted_swaps, NOT swaps.
        insp.end_frame_reverted();
        insp.end_frame_committed(); // root (execute) commits
        assert!(
            handle.take_swaps().is_empty(),
            "the reverted unlock drops its inner swaps from the committed set"
        );
        let reverted = handle.take_reverted_swaps();
        assert_eq!(
            reverted.len(),
            2,
            "both inner swaps (V4 + V3) preserved in reverted_swaps"
        );
        assert_eq!(reverted[0].emitter, Address::repeat_byte(0xA1));
        assert_eq!(reverted[1].emitter, Address::repeat_byte(0xA2));
    }
}
