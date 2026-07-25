//! The LIFO frame-pairing helper used by [`CallTraceInspector::call_end`] and
//! [`CallTraceInspector::create_end`].
//!
//! `call_end`/`create_end` fire LIFO (the innermost call's `call_end` fires
//! before its parent's). Frames are pushed in call order at the `call` hook,
//! so the most-recently-pushed frame whose `outcome` is still `None` is the
//! innermost unmatched — pair it.
//!
//! This was confirmed by spike KCKGP4 (Q3): a parent STATICCALLs a child that
//! reverts; `call_end` for the child (depth 2) fires first, then `call_end` for
//! the parent (depth 1). Naive `last_mut()` pairing mis-attributes the child's
//! outcome to itself but then leaves the parent's outcome unset (the `is_none`
//! guard stops it). The innermost-unmatched walk pairs both correctly.
//!
//! [`CallTraceInspector::call_end`]: super::call_trace::CallTraceInspector::call_end
//! [`CallTraceInspector::create_end`]: super::call_trace::CallTraceInspector::create_end
use super::call_trace::{CallFrame, FrameOutcome};

/// Pair `outcome` with the innermost unmatched frame (the most-recently-pushed
/// frame whose `outcome` is still `None`).
pub(super) fn pair_lifo(frames: &mut [CallFrame], outcome: FrameOutcome) {
    if let Some(frame) = frames.iter_mut().rev().find(|f| f.outcome.is_none()) {
        frame.outcome = Some(outcome);
    }
}
