//! A `revm::Inspector` capturing the structural call trace — every `call`/
//! `call_end`/`create`/`create_end` frame as a tree of [`CallFrame`] — for
//! post-run failure attribution + diagnostic rendering.
//!
//! Modeled on revm's built-in `TestInspector`
//! (`revm-inspector/src/test_inspector.rs`) minus the per-opcode `Step`
//! records (which `TracerEip3155` shows how to emit if later wanted).
//!
//! # The revert-attribution seam (spike KCKGP4, finding Q3)
//!
//! `call_end` receives the [`CallOutcome`] of the **deepest reverting frame**
//! (a child contract that reverts), not just the top-level bubble. The
//! reverting frame's `target` + the revert data are visible at the reverting
//! frame's `call_end`. [`CallTrace::reverting_frame_label`] walks the trace
//! for the deepest `Revert` frame + runs [`classify_revert`] on its data —
//! the *attribution* the backrun bot's `fail_index` (0–6 top-level call
//! index) replaces with `(reverting_frame_depth, reverting_frame_target,
//! reverting_frame_selector, revert_label)`.
//!
//! `call_end` fires LIFO (innermost call before its parent); frames are paired
//! by matching the most-recently-pushed frame whose outcome is still unset
//! (the innermost unmatched).
//!
//! [`classify_revert`]: degenbot_decoders::revert::classify_revert

use std::cell::RefCell;
use std::rc::Rc;

use alloy::primitives::{Address, Bytes};
use revm::inspector::Inspector;
use revm::interpreter::{CallInputs, CallOutcome, CreateInputs, CreateOutcome, InstructionResult};
use serde::Serialize;

use super::call_end::pair_lifo;

/// The outcome of a single call frame, captured at `call_end`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrameOutcome {
    /// `STOP`/`RETURN`/`SELFDESTRUCT`.
    Success { gas_used: u64, output: Bytes },
    /// `REVERT` — the frame reverted; `data` is the revert reason (the
    /// thing [`classify_revert`] consumes).
    Revert { gas_used: u64, data: Bytes },
    /// Any other halt (OOG, invalid opcode, call-too-deep, …).
    Halt { gas_used: u64 },
}

impl FrameOutcome {
    fn from_result(res: &revm::interpreter::InterpreterResult) -> Self {
        let gas_used = res.gas.total_gas_spent();
        match res.result {
            InstructionResult::Revert => Self::Revert {
                gas_used,
                data: res.output.clone(),
            },
            InstructionResult::Stop
            | InstructionResult::Return
            | InstructionResult::SelfDestruct => Self::Success {
                gas_used,
                output: res.output.clone(),
            },
            _ => Self::Halt { gas_used },
        }
    }
}

/// A captured `call`/`create` frame — caller, target, calldata selector, gas,
/// and the frame's [`FrameOutcome`] once `call_end`/`create_end` pairs it.
///
/// The calldata selector is the first 4 bytes of the call input (zero-padded if
/// shorter); `0x00000000` for value transfers with no calldata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CallFrame {
    /// Call depth (1 = top-level `execute()`, 2 = first sub-call, …).
    pub depth: usize,
    /// The caller (the EOA or contract invoking this frame).
    pub caller: Address,
    /// The frame's target (the account whose storage/code runs).
    pub target: Address,
    /// The first 4 bytes of the calldata (the Solidity selector).
    pub selector: [u8; 4],
    /// The gas limit of the call.
    pub gas_limit: u64,
    /// `None` until `call_end`/`create_end` pairs this frame.
    pub outcome: Option<FrameOutcome>,
}

/// A captured call/create frame — the common shape `call`/`create` hooks
/// produce. Kept as a flat `Vec` (LIFO-paired); the tree is reconstructed by
/// [`CallTrace::frames`] in call order for rendering.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CallTrace {
    /// All frames in call order (LIFO-paired at their `call_end`).
    pub frames: Vec<CallFrame>,
}

impl CallTrace {
    /// The deepest non-`Success` frame in the trace (Revert OR Halt) — the
    /// frame that caused the top-level failure. Covers both the `Revert`
    /// case (`call_end` with revert data) and the `Halt` case (e.g. `0xfe`
    /// INVALID, OOG) — a `Halt` has no revert data, so `reverting_frame_label`
    /// (`Revert`-only) returns `None` for it; this method returns the halting
    /// frame so the strategy can attribute it (the bucket label for a Halt
    /// is `classify_revert` on empty bytes → the `"empty"` bucket, parity
    /// with the pre-inspector behavior).
    #[must_use]
    pub fn failing_frame(&self) -> Option<&CallFrame> {
        self.frames
            .iter()
            .rev()
            .find(|f| !matches!(f.outcome, Some(FrameOutcome::Success { .. }) | None))
    }

    /// The deepest `Revert` frame in the trace — the frame that caused the
    /// top-level revert (or `None` if no frame reverted). This is the
    /// *attribution* seam: the reverting target + selector + revert data.
    #[must_use]
    pub fn deepest_revert(&self) -> Option<&CallFrame> {
        self.frames
            .iter()
            .rev()
            .find(|f| matches!(f.outcome, Some(FrameOutcome::Revert { .. })))
    }

    /// Render the whole trace as an indented, chronological listing — one
    /// line per frame, indented by call depth, showing target + selector +
    /// outcome + gas. Frames are kept in call order (pushed at `call`;
    /// `call_end` LIFO-pairs the outcome in place), so iterating in order and
    /// indenting by `depth` reconstructs the full nested call sequence — e.g.
    /// `execute() → v3c.swap → callback → v3a.swap → callback → V4_UNLOCK →
    /// unlockCallback → swap → … Halt`. `#[must_use]` debug aid for the
    /// `V4 dynamic swap Halt` attribution (2LTKVO / W2UWZO).
    #[must_use]
    pub fn render_debug(&self) -> String {
        use FrameOutcome as FO;
        let mut out = String::new();
        for f in &self.frames {
            let kind = match &f.outcome {
                Some(FO::Revert { .. }) => "revert",
                Some(FO::Success { .. }) => "ok",
                _ => "halt",
            };
            let gas = f
                .outcome
                .as_ref()
                .map(|o| match o {
                    FO::Revert { gas_used, .. }
                    | FO::Halt { gas_used }
                    | FO::Success { gas_used, .. } => *gas_used,
                })
                .unwrap_or_default();
            for _ in 0..f.depth.saturating_sub(1) {
                out.push_str("  ");
            }
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!(
                    "d{} {}:0x{}:{} g{}\n",
                    f.depth,
                    f.target,
                    alloy::primitives::hex::encode(f.selector),
                    kind,
                    gas
                ),
            );
        }
        out
    }

    /// The deepest `Revert` frame + its `classify_revert` label (the
    /// bytes→label taxonomy from `degenbot_decoders::revert`, fed by the
    /// reverting *frame*'s data — NOT the top-level bubble). Returns `None`
    /// if no frame reverted.
    ///
    /// This is the prototype's `classify_revert`-fed-at-depth wiring (not yet
    /// surfaced into `SimFailure` — that is the JHPW5W follow-on task).
    #[must_use]
    pub fn reverting_frame_label(&self) -> Option<(&CallFrame, String)> {
        self.deepest_revert().map(|f| {
            let label = match &f.outcome {
                Some(FrameOutcome::Revert { data, .. }) => {
                    degenbot_decoders::revert::classify_revert(data)
                }
                _ => String::new(),
            };
            (f, label)
        })
    }
}

/// The internal buffer shared between the inspector + its handle.
#[derive(Debug, Default)]
pub(super) struct CallTraceBuffer {
    /// Pushed in call order; outcomes LIFO-paired at `call_end`.
    pub frames: Vec<CallFrame>,
    /// The current call depth (incremented at `call`/`create`, decremented at
    /// `call_end`/`create_end`).
    pub depth: usize,
}

/// A `revm::Inspector` capturing the call trace. Shares an
/// `Rc<RefCell<CallTraceBuffer>>` (the same handle shape
/// [`AccessListCollector`](super::super::AccessListCollector) uses) so the
/// caller drains after `inspect_one` moves the inspector into the EVM.
#[derive(Debug, Clone)]
pub struct CallTraceInspector {
    buf: Rc<RefCell<CallTraceBuffer>>,
}

/// A read/drain handle to a [`CallTraceInspector`] that was moved into an EVM
/// via `inspect_one`. Drains the captured [`CallTrace`] after the run.
#[derive(Debug, Clone)]
pub struct CallTraceHandle {
    buf: Rc<RefCell<CallTraceBuffer>>,
}

impl Default for CallTraceInspector {
    /// An inspector with no handle — the placeholder baked into a per-block
    /// EVM via `build_mainnet_with_inspector` (its type fixes
    /// `InspectEvm::Inspector`; `inspect_one` swaps in a fresh
    /// inspector-with-handle per run, so the baked-in one is never read).
    fn default() -> Self {
        Self {
            buf: Rc::new(RefCell::new(CallTraceBuffer::default())),
        }
    }
}

impl CallTraceInspector {
    /// Create an inspector + its drain handle (shared `Rc<RefCell<...>>`).
    #[must_use]
    pub fn new() -> (Self, CallTraceHandle) {
        let buf = Rc::new(RefCell::new(CallTraceBuffer::default()));
        (
            Self {
                buf: Rc::clone(&buf),
            },
            CallTraceHandle { buf },
        )
    }
}

impl CallTraceHandle {
    #[allow(dead_code)]
    fn push_frame(&self, depth: usize, inputs: &CallInputs) {
        let selector = selector_of(&inputs.input);
        self.buf.borrow_mut().frames.push(CallFrame {
            depth,
            caller: inputs.caller,
            target: inputs.target_address,
            selector,
            gas_limit: inputs.gas_limit,
            outcome: None,
        });
    }

    /// Drain the captured [`CallTrace`] (frames in call order) and reset the
    /// buffer for reuse on the next path.
    #[must_use]
    pub fn take_trace(&self) -> CallTrace {
        let mut buf = self.buf.borrow_mut();
        let frames = std::mem::take(&mut buf.frames);
        buf.depth = 0;
        CallTrace { frames }
    }
}

fn selector_of(input: &revm::interpreter::CallInput) -> [u8; 4] {
    match input {
        revm::interpreter::CallInput::Bytes(b) => {
            let mut s = [0u8; 4];
            if b.len() >= 4 {
                s.copy_from_slice(&b[..4]);
            } else if !b.is_empty() {
                s[..b.len()].copy_from_slice(b);
            }
            s
        }
        revm::interpreter::CallInput::SharedBuffer(_) => [0u8; 4],
    }
}

impl<CTX, INTR: revm::interpreter::InterpreterTypes> Inspector<CTX, INTR> for CallTraceInspector {
    fn call(&mut self, _ctx: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        let mut buf = self.buf.borrow_mut();
        buf.depth += 1;
        let depth = buf.depth;
        let frame = CallFrame {
            depth,
            caller: inputs.caller,
            target: inputs.target_address,
            selector: selector_of(&inputs.input),
            gas_limit: inputs.gas_limit,
            outcome: None,
        };
        buf.frames.push(frame);
        None
    }

    fn call_end(&mut self, _ctx: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        let mut buf = self.buf.borrow_mut();
        pair_lifo(&mut buf.frames, FrameOutcome::from_result(&outcome.result));
        buf.depth = buf.depth.saturating_sub(1);
    }

    fn create(&mut self, _ctx: &mut CTX, inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        let mut buf = self.buf.borrow_mut();
        buf.depth += 1;
        let depth = buf.depth;
        let frame = CallFrame {
            depth,
            caller: inputs.caller(),
            // The precise created address needs the caller's nonce from the
            // journal; `execute()` uses CALLs, not CREATE, so this is `ZERO`
            // in the prototype. The JHPW5W follow-on can resolve it via the
            // context if CREATE-frame attribution ever matters.
            target: Address::ZERO,
            selector: [0u8; 4],
            gas_limit: inputs.gas_limit(),
            outcome: None,
        };
        buf.frames.push(frame);
        None
    }

    fn create_end(&mut self, _ctx: &mut CTX, _inputs: &CreateInputs, outcome: &mut CreateOutcome) {
        let mut buf = self.buf.borrow_mut();
        pair_lifo(&mut buf.frames, FrameOutcome::from_result(&outcome.result));
        buf.depth = buf.depth.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn success_frame() -> CallFrame {
        CallFrame {
            depth: 1,
            caller: Address::ZERO,
            target: Address::ZERO,
            selector: [0u8; 4],
            gas_limit: 0,
            outcome: Some(FrameOutcome::Success {
                gas_used: 0,
                output: Bytes::new(),
            }),
        }
    }

    fn revert_frame(data: &[u8]) -> CallFrame {
        CallFrame {
            depth: 2,
            caller: Address::ZERO,
            target: Address::repeat_byte(0x20),
            selector: [0u8; 4],
            gas_limit: 0,
            outcome: Some(FrameOutcome::Revert {
                gas_used: 0,
                data: Bytes::copy_from_slice(data),
            }),
        }
    }

    #[test]
    fn empty_trace_has_no_revert() {
        let trace = CallTrace::default();
        assert!(trace.deepest_revert().is_none());
        assert!(trace.reverting_frame_label().is_none());
    }

    #[test]
    fn render_debug_indents_by_depth_in_call_order() {
        // Frames in call order: execute → v3c.swap → callback → halt frame.
        let trace = CallTrace {
            frames: vec![
                CallFrame {
                    depth: 1,
                    caller: Address::ZERO,
                    target: Address::repeat_byte(0xaa),
                    selector: [0x12, 0x34, 0x56, 0x78],
                    gas_limit: 0,
                    outcome: Some(FrameOutcome::Success {
                        gas_used: 1000,
                        output: Bytes::new(),
                    }),
                },
                CallFrame {
                    depth: 2,
                    caller: Address::repeat_byte(0xaa),
                    target: Address::repeat_byte(0xbb),
                    selector: [0xde, 0xad, 0xbe, 0xef],
                    gas_limit: 0,
                    outcome: Some(FrameOutcome::Revert {
                        gas_used: 500,
                        data: Bytes::new(),
                    }),
                },
            ],
        };
        let rendered = trace.render_debug();
        // Depth-1 line un-indented; depth-2 line indented; selector/kind/gas shown.
        assert!(rendered.contains(":0x12345678:ok g1000"), "{rendered}");
        assert!(
            rendered.contains("  d2 ") && rendered.contains(":0xdeadbeef:revert g500"),
            "{rendered}"
        );
        assert!(rendered.ends_with('\n'), "{rendered}");
    }

    #[test]
    fn deepest_revert_picks_deepest_among_ordered_frames() {
        let trace = CallTrace {
            frames: vec![
                revert_frame(&[0xa1]),
                success_frame(),
                revert_frame(&[0xb2]),
            ],
        };
        // `deepest_revert` walks reversed, so it returns the LAST revert frame
        // in call order — the innermost if the trace appended in depth order.
        let label = trace.reverting_frame_label();
        assert!(label.is_some());
        let (frame, _label) = label.unwrap();
        assert_eq!(
            frame.outcome,
            Some(FrameOutcome::Revert {
                gas_used: 0,
                data: Bytes::from(vec![0xb2])
            })
        );
    }
}
