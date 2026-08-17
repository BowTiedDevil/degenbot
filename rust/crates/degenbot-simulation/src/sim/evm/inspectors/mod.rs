//! Composable `revm::Inspector` pair for simulation diagnostics — the
//! engine-generic primitives (ADR-019 D4/D7) that retire the settlement-arbitrage bot's
//! ad-hoc, post-hoc failure analysis.
//!
//! Two inspectors, composable with the existing
//! [`AccessListCollector`](super::access_list::AccessListCollector) via
//! revm's blanket `Inspector` impl for `(L, R)` tuples:
//!
//! - [`CallTraceInspector`] — structural call/create frame tree + the
//!   revert-attribution seam (`call_end` on the deepest reverting frame).
//! - [`SwapEventCaptureInspector`] — V2 `Sync` / V3 `Swap` / V4 `Swap` LOG
//!   capture + decode, replacing the onchain-recompute pipeline.
//!
//! # Composition (spike KCKGP4, finding Q2)
//!
//! revm's `Inspector` impl for `(L, R)` delegates every hook to both members.
//! The spike proved `(AccessListCollector, ProbeInspector)` composes on one
//! `inspect_one` run with no borrow-ordering issues and AL parity preserved.
//! The production `BlockEvm` inspector type widens from bare
//! `AccessListCollector` to a composed tuple `(AccessListCollector,
//! CallTraceInspector, SwapEventCaptureInspector)`.
//!
//! # Standalone-Rust claim (ADR-005 Tier 0)
//!
//! A `cargo add degenbot` consumer reaches `CallTrace` + captured swaps +
//! reverting-frame attribution from the engine directly — no Python, no
//! Multicall3 re-fetch. The inspectors are engine-generic (no
//! `SimulateContext`/`SimResult`/strategy vocabulary); the four-way
//! Drift/SolverCalc/Encoding classifier POLICY stays in the strategy
//! (`logs/permutation_analyzer.py`).
//!
//! # Status
//!
//! Prototype (ergo task `2LMT7A`): the inspectors + captured structs land here
//! as additive, test-only modules. Production wiring into `BlockEvm` +
//! `SimFailure` deepening + `diagnostic.rs` retirement is gated on the
//! JHPW5W follow-on implementation-definition task.

pub mod call_end;
pub mod call_trace;
pub mod swap_event;

pub use call_trace::{CallFrame, CallTrace, CallTraceHandle, CallTraceInspector, FrameOutcome};
pub use swap_event::{CapturedSwap, SwapEventCaptureHandle, SwapEventCaptureInspector, SwapFamily};

/// The composed inspector tuple baked into the production `BlockEvm` —
/// `AccessListCollector` paired with `(CallTraceInspector,
/// SwapEventCaptureInspector)`. revm's blanket `Inspector` impl covers
/// 2-tuples `(L, R)` only (`revm-inspector-42/src/inspector.rs:150`), so the
/// three-way composition is a nested tuple: `AccessListCollector` is `L`, and
/// the `CallTraceInspector`/`SwapEventCaptureInspector` pair is `R`.
/// Each member carries its own `Rc<RefCell<…>>` handle (mirroring
/// `AccessListCollector::new`'s `(Self, Handle)` shape) so the strategy drains
/// all three after `inspect_one` moves the tuple into the EVM.
///
/// NOT yet wired into `sim/evm/simulator.rs::BlockEvm` (the prototype is
/// test-only); the JHPW5W follow-on task flips the `BlockEvm` type parameter
/// from bare `AccessListCollector` to this alias.
pub type SimInspector = (
    super::access_list::AccessListCollector,
    (CallTraceInspector, SwapEventCaptureInspector),
);
