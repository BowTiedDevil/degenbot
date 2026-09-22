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
use alloy::primitives::{Address, Bytes, B256, U256};
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_pools::TickInfo;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm::journal_pools::PoolPostState;
use degenbot_simulation::sim::evm::{ScratchDb, ScratchEvm};
use hashbrown::HashMap as HbMap;

use crate::frame_pipeline::{BidEconomics, PipelineConfig};
use crate::market_context::MarketContext;

/// A V3 tick window source for anchor admission: connector state read from
/// the same chain view the frames replay over. Implemented for the frame
/// scratch DB (production) and by test mocks (offline pins).
pub trait V3TickWindow {
    /// The in-range initialized ticks around `current_tick`.
    fn tick_window(
        &self,
        pool: Address,
        layout: ClSlotLayout,
        tick_spacing: i32,
        current_tick: i32,
        head: u64,
    ) -> HbMap<i32, TickInfo>;

    /// The V4 twin: the in-range initialized ticks around `current_tick`,
    /// read at the `PoolManager` singleton through the `poolId`-derived
    /// bases. The default is empty so offline mocks need no V4 state.
    fn v4_tick_window(
        &self,
        _manager: Address,
        _pool_id: B256,
        _tick_spacing: i32,
        _current_tick: i32,
        _head: u64,
    ) -> HbMap<i32, TickInfo> {
        HbMap::default()
    }
}

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

/// One strategy that reacts to observed pending transactions.
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
    /// into the trigger's fresh workspace.
    fn admit(
        &mut self,
        ctx: &MarketContext,
        workspace: &mut BackrunSolver,
        states: &[PoolPostState],
        seed_block: u64,
        trace_tx: &str,
        tick_window: Option<&dyn V3TickWindow>,
    ) -> Self::Affected;

    /// `true` when nothing relevant was admitted (the driver observes the
    /// frame instead of running the remaining stages).
    fn affected_is_empty(affected: &Self::Affected) -> bool;

    /// Discover candidate plays over the admitted set.
    #[expect(
        clippy::too_many_arguments,
        reason = "the stage threads the driver's distinct seams"
    )]
    async fn discover(
        &mut self,
        ctx: &MarketContext,
        workspace: &mut BackrunSolver,
        scratch: &mut ScratchEvm<ScratchDb<'_>>,
        provider: &AlloyProvider,
        affected: &Self::Affected,
        head: u64,
        trace_tx: &str,
    ) -> Self::Intents;

    /// Evaluate the discovered intents inside the workspace.
    fn evaluate(
        &mut self,
        workspace: &mut BackrunSolver,
        intents: Self::Intents,
        pl: &PipelineConfig,
        trace_tx: &str,
    ) -> Self::Evaluated;

    /// Compose the best evaluated candidate into the artifact the driver
    /// simulates, or `None` when nothing composes.
    fn compose(
        &mut self,
        evaluated: &Self::Evaluated,
        pl: &PipelineConfig,
        trace_tx: &str,
    ) -> Option<ComposedIntent>;

    /// The final gate over the composed artifact and the driver's simulation
    /// verdict; returns the decision plus the submit artifact.
    #[expect(
        clippy::too_many_arguments,
        reason = "the gate reads the driver's verdict and the composed artifact"
    )]
    fn decide(
        &self,
        knobs: &BackrunConfig,
        pl: &PipelineConfig,
        evaluated: &Self::Evaluated,
        composed: Option<&ComposedIntent>,
        sim_ok: bool,
        spent: U256,
        trace_tx: &str,
    ) -> Decided;
}
