//! The pending-transaction strategy seam.
//!
//! A [`PendingTxReaction`] reacts to one observed mempool transaction: it
//! admits the recovered pool post-states its identity cares about into the
//! trigger's fresh solver workspace, discovers candidate plays, prices them,
//! composes the artifact the driver simulates, and gates the final bid. The
//! driver owns the substrate around these stages (replay, extraction, the
//! bundle simulation, submission).
//!
//! [`ComposedIntent`] and [`Decided`] are the strategy-neutral artifacts the
//! driver threads between the stages; the trait itself names no strategy
//! identity.

use crate::backrun::BackrunConfig;
use crate::backrun_engine::BackrunSolver;
use alloy::primitives::{Bytes, U256};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm::journal_pools::PoolPostState;
use degenbot_simulation::sim::evm::{ScratchDb, ScratchEvm};

use crate::frame_pipeline::{BidEconomics, PipelineConfig, StageTrace};
use crate::market_context::MarketContext;

/// The candidate the driver simulates: the solved best composed at the
/// strategy's competitiveness ceiling, plus the profit evidence the trace
/// records.
pub struct ComposedIntent {
    /// The calldata the driver runs against the bundle-simulation gate.
    pub sim_calldata: Bytes,
    /// The solved gross profit (wei).
    pub profit_wei: u128,
    /// The solved optimal input (wei).
    pub optimal_input_wei: u128,
}

/// The strategy's decided outcome for one frame: the gate decision plus the
/// bid-able artifact the driver submits when one exists.
pub struct Decided {
    pub decision: crate::backrun::Decision,
    pub requested_bid: U256,
    pub submit_calldata: Option<Bytes>,
    pub economics: Option<BidEconomics>,
}

/// The frame's production context: the per-driver configuration and the
/// per-frame identity every stage reads, plus the frame's stage-evidence
/// buffer the frame module renders to the driver-owned offline-review
/// capture. The heavy frame surfaces (the frame-surviving runtime, the fresh
/// workspace, the scratch EVM, the read provider) stay stage arguments —
/// only the stages that read them name them.
pub struct FrameContext<'a> {
    /// The pipeline configuration (deployment, owner, economics).
    pub pl: &'a PipelineConfig,
    /// The strategy knobs.
    pub knobs: &'a BackrunConfig,
    /// The head the frame replays against (the admission seed block).
    pub head: u64,
    /// The frame's trace identity.
    pub trace_tx: &'a str,
    /// The stage-evidence buffer; the frame module renders it after each
    /// stage.
    pub trace: &'a mut StageTrace,
}

/// The decide stage's gate input: the evaluated frame, the composed artifact
/// the driver simulated, the bundle-simulation verdict over it, and the
/// frame's spent budget.
pub struct GateInput<'a, E> {
    pub evaluated: &'a E,
    pub composed: Option<&'a ComposedIntent>,
    pub sim_ok: bool,
    pub spent: U256,
}

/// One strategy that reacts to observed pending transactions.
///
/// Every stage reads [`FrameContext`] — the frame's config, identity, and
/// evidence buffer — plus its stage-specific input, so no stage threads a
/// parallel parameter list of the driver's seams. Stages report
/// instrumentation by appending typed events to [`FrameContext::trace`]; the
/// frame module renders them, and no strategy code touches the capture.
#[expect(
    async_fn_in_trait,
    reason = "consumed only through generic dispatch, never as a dyn object"
)]
pub trait PendingTxReaction {
    /// The pools admitted into this trigger's workspace.
    type Affected;
    /// The candidate plays discovery produced.
    type Intents;
    /// The priced outcome of evaluating those plays.
    type Evaluated;

    fn name(&self) -> &'static str;

    /// Admit what this strategy cares about from the recovered post-states
    /// into the trigger's fresh workspace. The admission seed block is
    /// [`FrameContext::head`].
    async fn admit(
        &mut self,
        cx: &mut FrameContext<'_>,
        ctx: &MarketContext,
        workspace: &mut BackrunSolver,
        states: &[PoolPostState],
    ) -> Self::Affected;

    /// `true` when nothing relevant was admitted (the driver observes the
    /// frame instead of running the remaining stages).
    fn affected_is_empty(affected: &Self::Affected) -> bool;

    /// Discover candidate plays over the admitted set.
    async fn discover(
        &mut self,
        cx: &mut FrameContext<'_>,
        ctx: &MarketContext,
        workspace: &mut BackrunSolver,
        scratch: &mut ScratchEvm<ScratchDb<'_>>,
        provider: &AlloyProvider,
        affected: &Self::Affected,
    ) -> Self::Intents;

    /// Evaluate the discovered intents inside the workspace.
    fn evaluate(
        &mut self,
        cx: &mut FrameContext<'_>,
        workspace: &mut BackrunSolver,
        intents: Self::Intents,
    ) -> Self::Evaluated;

    /// Compose the best evaluated candidate into the artifact the driver
    /// simulates, or `None` when nothing composes.
    fn compose(
        &mut self,
        cx: &mut FrameContext<'_>,
        evaluated: &Self::Evaluated,
    ) -> Option<ComposedIntent>;

    /// The final gate over the composed artifact and the driver's simulation
    /// verdict; returns the decision plus the submit artifact.
    fn decide(&self, cx: &mut FrameContext<'_>, gate: &GateInput<'_, Self::Evaluated>) -> Decided;
}
