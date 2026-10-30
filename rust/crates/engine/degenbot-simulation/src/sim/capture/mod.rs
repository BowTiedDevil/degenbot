//! The engine's capture vocabulary: the decoded, engine-neutral shapes the
//! composite inspector produces — [`CapturedSwap`] + the tentative-frame
//! [`SwapEventBuffer`], [`CallFrame`]/[`CallTrace`] + the LIFO-paired
//! [`CallTraceBuffer`], and the access-list slot collector.

pub mod access_list_slots;
pub mod call_end;
pub mod call_trace;
pub mod swap_event;

pub use access_list_slots::CollectedSlots;
pub use call_end::pair_lifo;
pub use call_trace::{CallFrame, CallTrace, CallTraceBuffer, FrameOutcome};
pub use swap_event::{CapturedSwap, SwapEventBuffer, SwapFamily};
