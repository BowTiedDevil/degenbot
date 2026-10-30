//! The evm2 composite inspector — the port of the revm-side `SimInspector`
//! trio (access-list collector, call trace, swap-event capture) into ONE
//! evm2 `Inspector`. evm2's typed-retrieval surface (`inspector_mut` +
//! `NonStaticAny` downcast) removes the `Rc<RefCell>` handle pattern: the
//! inspector stays in the EVM across runs and the caller drains through a
//! downcast.
//!
//! Every captured type and every piece of frame-stack/LIFO logic is the
//! SHARED engine-neutral shape from `sim::evm` — this module is hook
//! bindings only, so a divergence between engines is an engine bug, not a
//! port skew:
//!
//! - swap capture — `SwapEventBuffer`'s tentative-frame stack, driven by
//!   evm2's `log` (fires at LOG emission, like revm's `log_full`) +
//!   `call`/`create` + `call_end`/`create_end` frame brackets.
//! - call trace — `CallFrame`/`CallTrace` with `pair_lifo` outcome pairing;
//!   the outcome maps from evm2's `InstrStop` (`is_success` is the same
//!   three-variant set as revm's).
//! - access list — `CollectedSlots`, driven by the `step` hook on
//!   `SLOAD`/`SSTORE` (the slot is the stack top, same as revm).

use alloy::primitives::{Address, Bytes, B256};
use alloy::rpc::types::eth::AccessList;

use ::evm2::interpreter::{op, Interpreter, Message, MessageResult};
use ::evm2::{BaseEvmTypes, Inspector};

use crate::sim::evm::access_list::CollectedSlots;
use crate::sim::evm::inspectors::call_end::pair_lifo;
use crate::sim::evm::inspectors::call_trace::{
    CallFrame, CallTrace, CallTraceBuffer, FrameOutcome,
};
use crate::sim::evm::inspectors::swap_event::{CapturedSwap, SwapEventBuffer};

/// The evm2 composite inspector (see the module doc). `Default` — set into
/// the EVM once at build; drained after runs via the `take_*` verbs.
#[derive(Debug, Default)]
pub struct Evm2SimInspector {
    swaps: SwapEventBuffer,
    trace: CallTraceBuffer,
    slots: CollectedSlots,
}

impl Evm2SimInspector {
    /// Drain the committed swaps + reset for the next run (see
    /// `SwapEventCaptureHandle::take_swaps`).
    pub fn take_swaps(&mut self) -> Vec<CapturedSwap> {
        let swaps = std::mem::take(&mut self.swaps.swaps);
        self.swaps.frame_stack.clear();
        self.swaps.log_full_count = 0;
        swaps
    }

    /// Drain the reverted-frame swaps (see
    /// `SwapEventCaptureHandle::take_reverted_swaps`).
    pub fn take_reverted_swaps(&mut self) -> Vec<CapturedSwap> {
        std::mem::take(&mut self.swaps.reverted_swaps)
    }

    /// Every `log` hook invocation count (diagnostic — includes non-swap
    /// logs and reverted-frame logs; the revm-side `log_full_count` analog).
    #[must_use]
    pub fn log_count(&self) -> usize {
        self.swaps.log_full_count
    }

    /// Drain the captured call trace + reset (see `CallTraceHandle::take_trace`).
    pub fn take_trace(&mut self) -> CallTrace {
        let frames = std::mem::take(&mut self.trace.frames);
        self.trace.depth = 0;
        CallTrace { frames }
    }

    /// Drain the collected slot set into an EIP-2930 access list + reset
    /// (see `AccessListHandle::take_access_list`).
    pub fn take_access_list(&mut self) -> AccessList {
        self.slots.take_access_list()
    }
}

/// The first 4 bytes of the calldata, zero-padded if shorter (`0x00000000`
/// for value transfers with no calldata) — mirrors the revm-side selector.
fn selector_of(input: &Bytes) -> [u8; 4] {
    let mut s = [0u8; 4];
    let n = input.len().min(4);
    s[..n].copy_from_slice(&input[..n]);
    s
}

/// Map an evm2 message result to the shared `FrameOutcome` (the revm-side
/// mapping: `is_success` → Success, `Revert` → Revert, anything else → Halt;
/// the gas figure is the after-refund `used`, revm's `total_gas_spent`).
fn outcome_of(result: &MessageResult<BaseEvmTypes>) -> FrameOutcome {
    let gas_used = result.gas.used();
    if result.stop.is_success() {
        FrameOutcome::Success {
            gas_used,
            output: result.output.clone(),
        }
    } else if result.stop == ::evm2::interpreter::InstrStop::Revert {
        FrameOutcome::Revert {
            gas_used,
            data: result.output.clone(),
        }
    } else {
        FrameOutcome::Halt {
            gas_used,
            reason: format!("{:?}", result.stop),
        }
    }
}

impl Inspector<BaseEvmTypes> for Evm2SimInspector {
    /// Record the slot on every `SLOAD`/`SSTORE` (the slot is the stack top,
    /// the address the running frame's destination — the revm-side rule).
    fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
        let opcode = interp.opcode();
        if opcode == op::SLOAD || opcode == op::SSTORE {
            let address = interp.message().destination;
            if let Some(&slot) = interp.stack().last() {
                self.slots.record(address, B256::from(slot));
            }
        }
    }

    /// Fires at LOG emission (evm2's single unified hook — the revm-side
    /// `log_full` analog). The decoded swap lands in the top frame's
    /// tentative buffer.
    fn log(&mut self, log: &alloy::primitives::Log, _host: &mut ::evm2::Evm<'_, BaseEvmTypes>) {
        self.swaps.log_full_count += 1;
        self.swaps.capture_swap_log(log);
    }

    fn call(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message<BaseEvmTypes>,
    ) -> Option<MessageResult<BaseEvmTypes>> {
        self.swaps.begin_frame();
        self.trace.depth += 1;
        self.trace.frames.push(CallFrame {
            depth: self.trace.depth,
            caller: message.caller,
            target: message.destination,
            selector: selector_of(&message.input),
            gas_limit: message.gas_limit,
            data: message.input.clone(),
            outcome: None,
        });
        None
    }

    fn call_end(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        _message: &Message<BaseEvmTypes>,
        result: &mut MessageResult<BaseEvmTypes>,
    ) {
        pair_lifo(&mut self.trace.frames, outcome_of(result));
        self.trace.depth = self.trace.depth.saturating_sub(1);
        if result.stop.is_success() {
            self.swaps.end_frame_committed();
        } else {
            self.swaps.end_frame_reverted();
        }
    }

    fn create(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Message<BaseEvmTypes>,
    ) -> Option<MessageResult<BaseEvmTypes>> {
        self.swaps.begin_frame();
        self.trace.depth += 1;
        self.trace.frames.push(CallFrame {
            depth: self.trace.depth,
            caller: message.caller,
            // Mirrors the revm prototype: `execute()` uses CALLs, not CREATEs,
            // so the derived address stays unresolved here.
            target: Address::ZERO,
            selector: [0u8; 4],
            gas_limit: message.gas_limit,
            data: Bytes::new(),
            outcome: None,
        });
        None
    }

    fn create_end(
        &mut self,
        _interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        _message: &Message<BaseEvmTypes>,
        result: &mut MessageResult<BaseEvmTypes>,
    ) {
        pair_lifo(&mut self.trace.frames, outcome_of(result));
        self.trace.depth = self.trace.depth.saturating_sub(1);
        if result.stop.is_success() {
            self.swaps.end_frame_committed();
        } else {
            self.swaps.end_frame_reverted();
        }
    }
}
