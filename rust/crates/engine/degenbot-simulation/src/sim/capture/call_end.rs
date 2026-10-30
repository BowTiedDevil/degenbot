//! The LIFO frame-pairing helper used by the capture inspectors'
//! `call_end`/`create_end` hooks.
//!
//! `call_end`/`create_end` fire LIFO (the innermost call's `call_end` fires
//! before its parent's). Frames are pushed in call order at the `call` hook,
//! so the most-recently-pushed frame whose `outcome` is still `None` is the
//! innermost unmatched — pair it.
use super::call_trace::{CallFrame, FrameOutcome};

/// Pair `outcome` with the innermost unmatched frame (the most-recently-pushed
/// frame whose `outcome` is still `None`).
pub fn pair_lifo(frames: &mut [CallFrame], outcome: FrameOutcome) {
    if let Some(frame) = frames.iter_mut().rev().find(|f| f.outcome.is_none()) {
        frame.outcome = Some(outcome);
    }
}
