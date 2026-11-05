//! The session consumer — the ONE consume loop over the session's ordered
//! event stream, and the only place a batch becomes executor work (ADR-093
//! D5/D6).
//!
//! Both first-class consumers drive this module and nothing of their own:
//!
//! - the pure-Rust settlement bot attaches the driver's hand-off and spawns
//!   [`SessionConsumer::run`];
//! - the `PyO3` cockpit builds the same consumer over the same hand-off, and
//!   its Python side keeps only the calling-convention adaptation and the
//!   render drains.
//!
//! # The loop
//!
//! ```text
//! SessionEvent::Block(N)  →  the session's block clock (the ONE clock)
//! SessionEvent::Batch(b)  →  BatchWork (rows + payloads) → BatchExecutor
//!                         →  Batch outcome records (pulled by the adapter)
//! ```
//!
//! The clock comes from the stream's `Block` arm — the same producer call that
//! publishes the header edge — so a batch never advances the clock and a
//! block's view is never published after that block's batches.
//!
//! # The duties the loop owns (and the adapters do not)
//!
//! - **the block clock**: the dispatcher's block time + current block advance
//!   here, exactly once per accepted header, and the advisory fee-history
//!   sample rides the same edge;
//! - **stall detection's feed**: one heartbeat beat per RECEIVED stream event,
//!   so the core stall watchdog observes real stream activity (a silent source
//!   feeds nothing and trips it);
//! - **the loud-abort rule**: a stored leaf failure is re-raised in this
//!   caller's frame BEFORE the next batch is enqueued;
//! - **teardown**: [`SessionConsumer::run`] always closes the session, whether
//!   the loop ended, aborted, or never started.
//!
//! Rendering is deliberately absent: the loop publishes the executor's typed
//! records, and an adapter pulls them (a display fold, a liveness beat, or
//! nothing at all).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use degenbot_arbitrage::{BlockPriorityFees, CapturedSwap};
use degenbot_bot::arb_engine::AccessListRow;
use degenbot_bot::arb_engine::{
    BlockNotification, ConsumerClaim, EngineDriver, EventHandoff, Heartbeat, ResultBatch,
    SessionEvent, SessionStream, SimulatedPathResult, TeardownReport,
};
use degenbot_bot::arb_engine::{CapturedSwapRow, InlineSwapFamily};
use degenbot_core::eip_1559::{
    next_base_fee, DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR, DEFAULT_ELASTICITY_MULTIPLIER,
};
use degenbot_pools::solve::SolvePathResult;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::capture::SwapFamily;
use degenbot_submission::{fetch_fee_history, Dispatcher};
use tokio::sync::mpsc;

use crate::executor::{BatchExecutor, BatchWork};
use crate::row::{PayloadFailure, PayloadRow, RawResult};

/// The session block clock: the ONE clock a batch is consumed at.
///
/// `Block(N)` is the only writer (the stream's ordering guarantee), so the
/// value a batch is enqueued with is the session's current view — never the
/// batch's own `solve_block`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockClock {
    /// The latest accepted block number.
    pub current_block: u64,
    /// The latest accepted block timestamp (the EVM `block.timestamp`).
    pub block_timestamp: u64,
    /// The EIP-1559 base fee of the NEXT block.
    pub base_fee_next: u128,
}

impl BlockClock {
    /// Advance the clock from one accepted header.
    ///
    /// Returns whether the clock moved: a repeated or older header is not
    /// accepted (a rewind would re-consume an epoch's view).
    pub fn advance(&mut self, notification: &BlockNotification) -> bool {
        if notification.number < self.current_block {
            return false;
        }
        self.current_block = notification.number;
        self.block_timestamp = notification.timestamp;
        self.base_fee_next = next_base_fee(
            u128::from(notification.base_fee_per_gas.unwrap_or(0)),
            u128::from(notification.gas_used),
            u128::from(notification.gas_limit),
            None,
            DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR,
            DEFAULT_ELASTICITY_MULTIPLIER,
        );
        true
    }

    /// Whether a batch solved for `solve_block` arrived after the clock moved
    /// past it.
    ///
    /// A stale batch is still consumed — at the clock's current block — because
    /// the per-candidate staleness gate (the solve snapshot's state nonces) is
    /// what decides a candidate's fate; the batch count is reported so a lagging
    /// solve is visible instead of silently folded into the current block.
    #[must_use]
    pub const fn is_stale(&self, solve_block: u64) -> bool {
        solve_block < self.current_block
    }
}

/// The lock-free progress view of the consume loop, read by a run loop's
/// heartbeat task (and by a cockpit's diagnostics) without blocking or locking.
#[derive(Clone, Debug, Default)]
pub struct SessionProgress {
    batches: Arc<std::sync::atomic::AtomicU64>,
    blocks: Arc<std::sync::atomic::AtomicU64>,
    current_block: Arc<std::sync::atomic::AtomicU64>,
}

impl SessionProgress {
    /// A fresh, zeroed progress view.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one consumed batch at the clock's current block.
    ///
    /// A batch is not a block: streaming delivery fans one block out into many
    /// batches, so the batch count and the distinct block count stay separate.
    pub fn note_batch(&self, block: u64) {
        self.batches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let previous = self
            .current_block
            .swap(block, std::sync::atomic::Ordering::Relaxed);
        if block != previous {
            self.blocks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Record one accepted header.
    pub fn note_block(&self, block: u64) {
        let previous = self
            .current_block
            .swap(block, std::sync::atomic::Ordering::Relaxed);
        if block != previous {
            self.blocks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Batches consumed so far.
    #[must_use]
    pub fn batches(&self) -> u64 {
        self.batches.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Distinct block numbers the clock has advanced through.
    #[must_use]
    pub fn blocks(&self) -> u64 {
        self.blocks.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The clock's current block (0 before the first header).
    #[must_use]
    pub fn current_block(&self) -> u64 {
        self.current_block
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The advisory fee-history policy: every accepted header spawns one
/// percentiles sample into the dispatcher's ring.
///
/// A failed sample is advisory (the previous samples stay valid), exactly as
/// the driver-side leaf always behaved — a fee observation never fails a
/// session.
pub struct FeeHistoryPolicy {
    /// The provider the sample is read through.
    pub provider: Arc<AlloyProvider>,
    /// How many blocks the sample covers.
    pub block_count: u64,
    /// The reward percentiles (`eth_feeHistory`'s second argument).
    pub percentiles: Vec<f64>,
}

/// The session's clock handles: the coordination state the loop drives and the
/// optional fee-history sample.
pub struct SessionClock {
    /// The session's one dispatcher (block time, current block, the fee ring).
    pub dispatcher: Arc<Mutex<Dispatcher>>,
    /// The advisory fee-history sample, when this session records one.
    pub fee_history: Option<FeeHistoryPolicy>,
}

impl SessionClock {
    /// A clock over the session's dispatcher, recording no fee history.
    #[must_use]
    pub fn new(dispatcher: Arc<Mutex<Dispatcher>>) -> Self {
        Self {
            dispatcher,
            fee_history: None,
        }
    }

    /// Record the advisory fee-history sample per accepted header.
    #[must_use]
    pub fn with_fee_history(mut self, fee_history: FeeHistoryPolicy) -> Self {
        self.fee_history = Some(fee_history);
        self
    }
}

/// The consumer's session-scoped policy values.
pub struct ConsumerPolicy {
    /// Whether a natural stream end is the intended teardown.
    ///
    /// `true` for a session whose own stop closes the stream (the driver's
    /// `close`); `false` for a supervised session, where a stream end means the
    /// pump died outside a teardown and the end verdict says so loudly.
    pub allow_quiet_end: bool,
    /// The heartbeat the stall watchdog observes. Beat once per RECEIVED stream
    /// event.
    pub heartbeat: Option<Heartbeat>,
    /// The shared progress view a run loop reads.
    pub progress: SessionProgress,
}

impl ConsumerPolicy {
    /// A policy over a fresh progress view, with no heartbeat attached.
    #[must_use]
    pub fn new(allow_quiet_end: bool) -> Self {
        Self {
            allow_quiet_end,
            heartbeat: None,
            progress: SessionProgress::new(),
        }
    }
}

/// How a session's ordered stream ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsumerEnd {
    /// A natural stream end that this session's teardown intended.
    QuietEnd,
    /// A natural stream end this session did NOT intend — the pump stopped.
    PumpStopped,
}

impl ConsumerEnd {
    /// The stable spelling both driver shells exchange.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::QuietEnd => "quiet-end",
            Self::PumpStopped => "pump-stopped",
        }
    }
}

/// The consume loop's product: how many batches ran and how the stream ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsumerReport {
    /// Batches enqueued in arrival order.
    pub batches: u64,
    /// Distinct header ticks the clock advanced through.
    pub blocks: u64,
    /// Batches whose `solve_block` was behind the clock when they arrived.
    pub stale_batches: u64,
    /// Natural end-of-stream observations (always 1 once the stream closed).
    pub end_of_stream: u64,
    /// The terminal verdict; `None` when the loop aborted before the stream
    /// closed (the abort's own detail rides [`ConsumerError`]).
    pub end: Option<ConsumerEnd>,
}

/// A consumer-loop abort: the leaf failure that stopped the loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumerError {
    /// The batch index whose pass the failure belongs to (the batch count at
    /// the moment the failure surfaced).
    pub batch_index: u64,
    /// The leaf's own detail (never paraphrased).
    pub detail: String,
    /// The counts the loop reached before it stopped.
    pub report: ConsumerReport,
}

/// The session-attached consumer's product: the loop's report plus the ONE
/// shared teardown.
#[derive(Debug)]
pub struct SessionReport {
    /// The consume loop's report.
    pub consumer: ConsumerReport,
    /// The session's teardown report (the same value every close caller gets).
    pub teardown: TeardownReport,
}

/// A session-attached consumer's failure: the loop's abort (or the session's
/// own refusal) plus the teardown that always followed it.
#[derive(Debug)]
pub struct SessionFailure {
    /// The abort.
    pub error: ConsumerError,
    /// The teardown the failure path still ran (`None` only when no session was
    /// ever attached, so there was nothing to close).
    pub teardown: Option<TeardownReport>,
}

/// Where the consumer reads its ordered events from.
///
/// The production source is the driver's [`SessionStream`]; a scripted source is
/// an `mpsc` receiver, which is how the shared event-sequence oracle and the
/// `PyO3` seam drive the same consumer without a live pump.
pub trait SessionEventSource: Send {
    /// Await the next event; `None` is the stream's natural end.
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<SessionEvent>> + Send + '_>>;
}

impl SessionEventSource for SessionStream {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<SessionEvent>> + Send + '_>> {
        Box::pin(SessionStream::recv(self))
    }
}

impl SessionEventSource for mpsc::UnboundedReceiver<SessionEvent> {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<SessionEvent>> + Send + '_>> {
        Box::pin(mpsc::UnboundedReceiver::recv(self))
    }
}

/// The session half the consumer drives: the ordered stream, the one claim, and
/// the driver whose lifecycle the consumer owns end to end.
struct SessionHalf {
    stream: SessionStream,
    claim: Option<ConsumerClaim>,
    driver: Arc<EngineDriver>,
}

/// The session consumer: the consume loop plus the session it consumes.
pub struct SessionConsumer {
    executor: Arc<BatchExecutor>,
    clock_handles: SessionClock,
    policy: ConsumerPolicy,
    clock: BlockClock,
    session: Option<SessionHalf>,
}

impl SessionConsumer {
    /// Build the consumer over one executor and one session clock.
    #[must_use]
    pub fn new(executor: Arc<BatchExecutor>, clock: SessionClock, policy: ConsumerPolicy) -> Self {
        Self {
            executor,
            clock_handles: clock,
            policy,
            clock: BlockClock::default(),
            session: None,
        }
    }

    /// Attach the session's one hand-off and its driver.
    ///
    /// The hand-off's claim is moved into [`Self::resume`], so production cannot
    /// start without the attached-consumer proof the claim carries.
    pub fn attach(&mut self, handoff: EventHandoff, driver: Arc<EngineDriver>) {
        let (stream, claim) = handoff.split();
        self.session = Some(SessionHalf {
            stream,
            claim: Some(claim),
            driver,
        });
    }

    /// The clock as the loop last accepted it.
    #[must_use]
    pub fn clock(&self) -> BlockClock {
        self.clock
    }

    /// The shared progress view (the loop's own handle).
    #[must_use]
    pub fn progress(&self) -> SessionProgress {
        self.policy.progress.clone()
    }

    /// Start production: the claim's one use is the driver's `run`.
    ///
    /// A refusal closes the session before returning: a consumer that could not
    /// start must not leave a session running with nobody consuming it.
    ///
    /// # Errors
    ///
    /// [`SessionFailure`] when no session is attached, the claim was already
    /// used, or the driver refused the move (the refusal's own text is kept).
    pub async fn resume(&mut self) -> Result<(), SessionFailure> {
        let Some(half) = self.session.as_mut() else {
            return Err(unattached("resume"));
        };
        let Some(claim) = half.claim.take() else {
            return Err(unattached("resume (the claim was already used)"));
        };
        match half.driver.run(claim).await {
            Ok(()) => Ok(()),
            Err(error) => {
                let teardown = half.driver.close().await;
                Err(SessionFailure {
                    error: ConsumerError {
                        batch_index: 0,
                        detail: error.to_string(),
                        report: ConsumerReport::default(),
                    },
                    teardown: Some(teardown),
                })
            }
        }
    }

    /// Consume the attached session's stream, then close the session.
    ///
    /// Close runs on EVERY path — a clean `quiet-end`, a loud abort, and a
    /// stream that had already closed before the loop started — so no consumer
    /// exit leaves a session running.
    ///
    /// # Errors
    ///
    /// [`SessionFailure`] when the loop aborted; the teardown is still reported.
    pub async fn run(&mut self) -> Result<SessionReport, SessionFailure> {
        let Some(half) = self.session.take() else {
            return Err(unattached("run"));
        };
        // The claim's work is the attachment proof: `resume` already consumed
        // it, so the pattern drops it here without a `drop` call.
        let SessionHalf {
            mut stream,
            claim: _,
            driver,
        } = half;
        let outcome = self.consume(&mut stream).await;
        let teardown = driver.close().await;
        match outcome {
            Ok(consumer) => Ok(SessionReport { consumer, teardown }),
            Err(error) => Err(SessionFailure {
                error,
                teardown: Some(teardown),
            }),
        }
    }

    /// The consume loop over one event source.
    ///
    /// # Errors
    ///
    /// [`ConsumerError`] when the ordered lane stored a leaf failure: the detail
    /// is re-raised HERE, in the caller's frame, before any further batch is
    /// enqueued.
    pub async fn consume(
        &mut self,
        source: &mut dyn SessionEventSource,
    ) -> Result<ConsumerReport, ConsumerError> {
        let mut report = ConsumerReport::default();
        loop {
            let Some(event) = source.recv().await else {
                report.end_of_stream = 1;
                report.end = Some(if self.policy.allow_quiet_end {
                    ConsumerEnd::QuietEnd
                } else {
                    ConsumerEnd::PumpStopped
                });
                // Nothing else can enqueue, so quiescing the lane is what makes
                // the terminal verdict truthful: a leaf failure still in flight
                // is surfaced here rather than after the report said the stream
                // ran clean, and the record stream's tail is published for the
                // adapter's drain (which then observes its own end).
                return match self.quiesce().await {
                    Ok(()) => Ok(report),
                    Err(failure) => Err(ConsumerError {
                        batch_index: report.batches,
                        detail: failure.detail,
                        report,
                    }),
                };
            };
            // One beat per RECEIVED event: the stall watchdog observes actual
            // stream activity, never a timer of this loop's own.
            if let Some(heartbeat) = &self.policy.heartbeat {
                heartbeat.beat();
            }
            // The loud-abort rule's placement: between receipt and handling, so
            // a stored leaf failure stops the loop BEFORE this event's batch
            // reaches the lane. The lane is already stopping on that failure;
            // draining it here publishes what completed and keeps the record
            // stream's end with this loop's end.
            if let Some(error) = self.stored_failure(&report) {
                let _ = self.quiesce().await;
                return Err(error);
            }
            match event {
                SessionEvent::Block(notification) => self.accept_block(&notification, &mut report),
                SessionEvent::Batch(batch) => self.enqueue_batch(&batch, &mut report),
            }
        }
    }

    /// Drain the ordered lane: no further batch can be enqueued, so the lane's
    /// stored failure (if any) is this session's terminal verdict, and the
    /// record stream's tail is published for the adapter's drain.
    async fn quiesce(&self) -> Result<(), degenbot_submission::PipelineFailure> {
        self.executor.shutdown().await
    }

    /// Take the lane's stored failure, if any, rendered for this loop's counts.
    fn stored_failure(&self, report: &ConsumerReport) -> Option<ConsumerError> {
        self.executor
            .raise_if_failed()
            .err()
            .map(|failure| ConsumerError {
                batch_index: report.batches,
                detail: failure.detail,
                report: *report,
            })
    }

    /// Accept one header: the session's ONE block clock.
    fn accept_block(&mut self, notification: &BlockNotification, report: &mut ConsumerReport) {
        if !self.clock.advance(notification) {
            // A repeated or older header never rewinds the clock (and never
            // re-publishes an epoch's view).
            return;
        }
        report.blocks += 1;
        {
            let mut dispatcher = self
                .clock_handles
                .dispatcher
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            dispatcher.record_block_time(notification.number, notification.timestamp);
            dispatcher.advance_block(notification.number);
        }
        if let Some(fee) = &self.clock_handles.fee_history {
            let dispatcher = Arc::clone(&self.clock_handles.dispatcher);
            let provider = Arc::clone(&fee.provider);
            let percentiles = fee.percentiles.clone();
            let block_count = fee.block_count;
            let block = notification.number;
            // Advisory and fire-and-forget: a failed sample leaves the previous
            // ones valid, so the block clock is never parked on an RPC.
            degenbot_core::runtime::get_runtime().spawn(async move {
                let _ = fetch_fee_history(&provider, &dispatcher, block_count, block, &percentiles)
                    .await;
            });
        }
        self.policy.progress.note_block(self.clock.current_block);
    }

    /// Enqueue one batch's work at the session's current clock.
    fn enqueue_batch(&mut self, batch: &ResultBatch, report: &mut ConsumerReport) {
        if self.clock.is_stale(batch.solve_block) {
            report.stale_batches += 1;
        }
        let mut work = batch_work(batch, &self.clock);
        work.block_priority_fees = self.priority_fees();
        self.executor.enqueue(work);
        report.batches += 1;
        self.policy.progress.note_batch(self.clock.current_block);
    }

    /// The dispatcher's latest recorded priority-fee percentiles, if any.
    fn priority_fees(&self) -> Option<BlockPriorityFees> {
        let dispatcher = self
            .clock_handles
            .dispatcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        dispatcher
            .block_priority_fees()
            .last_key_value()
            .map(|(&block, fees)| BlockPriorityFees {
                block,
                p10: alloy::primitives::U256::from(*fees.get(&10).unwrap_or(&0)),
                p50: alloy::primitives::U256::from(*fees.get(&50).unwrap_or(&0)),
            })
    }
}

/// A session move the consumer could not make.
fn unattached(method: &str) -> SessionFailure {
    SessionFailure {
        error: ConsumerError {
            batch_index: 0,
            detail: format!("the session consumer has no attached session to {method}"),
            report: ConsumerReport::default(),
        },
        teardown: None,
    }
}

/// Translate one engine solve row into the executor's raw row.
///
/// The one seam extraction both consumers share: `U256` values saturate into
/// `u128` (the executor's threshold arithmetic is `u128`).
#[must_use]
pub fn raw_row(path_id: u64, result: &SolvePathResult, solve_block: u64) -> RawResult {
    let saturate = |value: &alloy::primitives::U256| u128::try_from(*value).unwrap_or(u128::MAX);
    RawResult {
        path_id,
        optimal_input: saturate(&result.optimal_input),
        profit: saturate(&result.profit),
        hop_outputs: result.hop_outputs.iter().map(saturate).collect(),
        consumed_inputs: result.consumed_inputs.iter().map(saturate).collect(),
        solve_block,
        state_nonces: result.state_nonces.clone(),
    }
}

/// Translate one engine inline-sim payload into the executor's payload row.
///
/// A payload row's resolve miss is the assembly stage's loud-abort arm, so this
/// extraction never fabricates a path.
#[must_use]
pub fn payload_row(sim: &SimulatedPathResult) -> PayloadRow {
    PayloadRow {
        path_id: sim.path_id,
        gross_profit: sim.gross_profit,
        net_profit: sim.net_profit,
        gas_used: sim.gas_used,
        priority_fee: sim.priority_fee,
        base_fee_next: sim.base_fee_next,
        execute_calldata: alloy::primitives::Bytes::from(sim.execute_calldata.clone()),
        access_list: sim.access_list.as_deref().map(access_list),
        failure: sim.failure.as_ref().map(|failure| PayloadFailure {
            bucket: failure.bucket.clone(),
            fail_index: failure.fail_index,
            revert_data: alloy::primitives::Bytes::from(failure.revert_data.clone()),
        }),
        // The sim's own capture rides the row: a failed payload's dump then
        // shows the ACTUAL swap amounts beside the request's expected ones.
        captured_swaps: sim.captured_swaps.iter().map(captured_swap).collect(),
    }
}

/// The engine primitive access-list rows back into the RPC access-list shape.
fn access_list(rows: &[AccessListRow]) -> alloy::rpc::types::AccessList {
    alloy::rpc::types::AccessList(
        rows.iter()
            .map(|row| alloy::rpc::types::AccessListItem {
                address: row.address,
                storage_keys: row
                    .storage_keys
                    .iter()
                    .map(|key| alloy::primitives::B256::from(*key))
                    .collect(),
            })
            .collect(),
    )
}

/// The engine's primitive swap mirror back into the sim core's decoded swap.
fn captured_swap(row: &CapturedSwapRow) -> CapturedSwap {
    CapturedSwap {
        emitter: row.emitter,
        family: match row.family {
            InlineSwapFamily::V2 => SwapFamily::V2,
            InlineSwapFamily::V3 => SwapFamily::V3,
            InlineSwapFamily::V4 => SwapFamily::V4,
        },
        amount0: row.amount0,
        amount1: row.amount1,
        sqrt_price_x96: row.sqrt_price_x96,
        liquidity: row.liquidity,
        tick: row.tick,
    }
}

/// Build one batch's executor work from a consumed result batch and the session
/// clock.
///
/// The `fresh` and `updated` engine rows join the raw batch; the inline-sim
/// payloads join in path-id order (a deterministic record order for the drain).
/// The payload rows' path ids ARE the batch-local payload-served set the
/// assembly reads — per-entry presence decides, so a mixed batch only degrades
/// the payload-less entries.
#[must_use]
pub fn batch_work(batch: &ResultBatch, clock: &BlockClock) -> BatchWork {
    let rows: Vec<RawResult> = batch
        .fresh
        .iter()
        .chain(batch.updated.iter())
        .map(|(path_id, result)| raw_row(*path_id, result, batch.solve_block))
        .collect();
    let mut payload_ids: Vec<u64> = batch.payloads.keys().copied().collect();
    payload_ids.sort_unstable();
    let payloads = payload_ids
        .iter()
        .filter_map(|id| batch.payloads.get(id).map(payload_row))
        .collect();
    BatchWork {
        rows,
        payloads,
        current_block: clock.current_block,
        base_fee_next: clock.base_fee_next,
        block_timestamp: clock.block_timestamp,
        block_priority_fees: None,
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::default_trait_access,
    reason = "the consumer's unit tests drive scripted sources and assert on the typed product; the payload map's hashbrown value type is not nameable without a direct dep"
)]
mod tests {
    use super::*;

    use std::time::Duration;

    use degenbot_submission::{SimFuture, SimLeaf, SubmitFuture, SubmitLeaf};

    use crate::executor::SimStageOutput;

    /// A source that never yields — the stall watchdog's own scenario.
    struct SilentSource;

    impl SessionEventSource for SilentSource {
        fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<SessionEvent>> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }

    fn empty_batch(solve_block: u64) -> ResultBatch {
        ResultBatch {
            solve_block,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
            fresh: Vec::new(),
            updated: Vec::new(),
            expired: Vec::new(),
            removed: Vec::new(),
            payloads: Default::default(),
        }
    }

    fn block(number: u64, timestamp: u64) -> BlockNotification {
        BlockNotification {
            number,
            timestamp,
            base_fee_per_gas: Some(1_000_000_000),
            gas_used: 15_000_000,
            gas_limit: 30_000_000,
        }
    }

    /// The fake sim leaf: it fails on the marked block and signals every pass,
    /// so a test can sequence the next event after that pass.
    struct SignallingSim {
        fail_on: Option<u64>,
        passed: Arc<tokio::sync::Notify>,
    }

    impl SimLeaf<BatchWork, SimStageOutput> for SignallingSim {
        fn simulate<'a>(&'a self, work: &'a BatchWork) -> SimFuture<'a, SimStageOutput> {
            let fail_on = self.fail_on;
            let passed = Arc::clone(&self.passed);
            let block = work.current_block;
            Box::pin(async move {
                passed.notify_one();
                if fail_on == Some(block) {
                    return Err(format!("sim leaf failed at block {block}"));
                }
                Ok(Some(SimStageOutput {
                    outcomes: Vec::new(),
                    submit_candidates: Vec::new(),
                }))
            })
        }
    }

    struct NoopSubmit;

    impl SubmitLeaf<BatchWork, SimStageOutput> for NoopSubmit {
        fn submit<'a>(
            &'a self,
            _work: &'a BatchWork,
            _outcome: &'a SimStageOutput,
        ) -> SubmitFuture<'a> {
            Box::pin(async { Ok(()) })
        }
    }

    fn fake_executor(fail_on: Option<u64>, passed: Arc<tokio::sync::Notify>) -> Arc<BatchExecutor> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(BatchExecutor::from_leaves(
            1,
            Arc::new(SignallingSim { fail_on, passed }),
            Arc::new(NoopSubmit),
            tx,
            rx,
        ))
    }

    fn consumer(
        executor: Arc<BatchExecutor>,
        allow_quiet_end: bool,
    ) -> (SessionConsumer, Heartbeat) {
        let heartbeat = Heartbeat::new();
        let policy = ConsumerPolicy {
            allow_quiet_end,
            heartbeat: Some(heartbeat.clone()),
            progress: SessionProgress::new(),
        };
        (
            SessionConsumer::new(
                executor,
                SessionClock::new(Arc::new(Mutex::new(Dispatcher::for_block(0)))),
                policy,
            ),
            heartbeat,
        )
    }

    /// The loud-abort rule's placement: a stored leaf failure stops the loop
    /// BEFORE the next batch reaches the lane. The source is sequenced on the
    /// leaf's own signal, so the ordering is the contract, not a race.
    #[tokio::test]
    async fn a_stored_leaf_failure_aborts_before_the_next_enqueue() {
        let passed = Arc::new(tokio::sync::Notify::new());
        let executor = fake_executor(Some(100), Arc::clone(&passed));
        let (mut consumer, _heartbeat) = consumer(Arc::clone(&executor), true);
        let (tx, mut rx) = mpsc::unbounded_channel();

        tx.send(SessionEvent::Block(block(100, 1))).unwrap();
        tx.send(SessionEvent::Batch(empty_batch(100))).unwrap();
        // The next event waits for the failing batch's pass through the leaf, so
        // the loop's check has a stored failure to find.
        tokio::spawn(async move {
            passed.notified().await;
            // The leaf's signal fires as its future resolves; the lane stores the
            // failure on the next poll of its own loop, so yield the scheduler
            // past that store before the next event can be read.
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(SessionEvent::Batch(empty_batch(100))).unwrap();
            drop(tx);
        });

        let error = consumer
            .consume(&mut rx)
            .await
            .expect_err("the leaf failed");
        assert_eq!(
            error.batch_index, 1,
            "the failure surfaced after the one batch"
        );
        assert_eq!(error.detail, "sim leaf failed at block 100");
        assert_eq!(
            executor.enqueued(),
            1,
            "the second batch never reached the lane"
        );
    }

    /// The stall watchdog observes ACTUAL stream activity: a silent source
    /// feeds no beat, so the core detection trips while the loop stays parked.
    #[tokio::test]
    async fn a_silent_source_feeds_no_beat_and_the_stall_watchdog_trips() {
        let executor = fake_executor(None, Arc::new(tokio::sync::Notify::new()));
        let (mut consumer, heartbeat) = consumer(executor, true);
        let mut source = SilentSource;

        let detection = degenbot_bot::arb_engine::SessionEndDetection::stall(
            heartbeat.clone(),
            Duration::from_millis(50),
        );
        let facts = detection.facts().clone();
        let mut loop_future = Box::pin(consumer.consume(&mut source));

        let cause = tokio::select! {
            cause = facts.wait() => cause,
            report = &mut loop_future => panic!("the silent source must not end: {report:?}"),
        };
        assert_eq!(
            cause,
            degenbot_bot::arb_engine::SessionEndCause::StallWatchdogTripped
        );
        assert_eq!(heartbeat.seq(), 0, "no event arrived, so nothing beat");
    }

    /// Every flowing event beats: one block and one batch feed the watchdog, so
    /// the window that trips a silent source stays alive here.
    #[tokio::test]
    async fn stream_activity_feeds_the_watchdog() {
        let executor = fake_executor(None, Arc::new(tokio::sync::Notify::new()));
        let (mut consumer, heartbeat) = consumer(executor, true);
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(SessionEvent::Block(block(100, 1))).unwrap();
        tx.send(SessionEvent::Batch(empty_batch(100))).unwrap();
        drop(tx);

        let detection = degenbot_bot::arb_engine::SessionEndDetection::stall(
            heartbeat.clone(),
            Duration::from_secs(1),
        );
        assert_eq!(detection.facts().current(), None);

        let report = consumer.consume(&mut rx).await.expect("no leaf failed");
        assert_eq!(heartbeat.seq(), 2, "one beat per received event");
        assert_eq!(report.blocks, 1);
        assert_eq!(report.batches, 1);
    }

    /// The clock is the stream's `Block` arm and nothing else: a batch never
    /// advances it, so a batch's own `solve_block` cannot move the session's
    /// view (and a stale batch is counted, then consumed at the clock's block).
    #[tokio::test]
    async fn the_block_arm_owns_the_clock_and_a_stale_batch_is_counted() {
        let executor = fake_executor(None, Arc::new(tokio::sync::Notify::new()));
        let (mut consumer, _heartbeat) = consumer(executor, true);
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(SessionEvent::Block(block(100, 1_700_000_100)))
            .unwrap();
        tx.send(SessionEvent::Batch(empty_batch(99))).unwrap();
        tx.send(SessionEvent::Batch(empty_batch(101))).unwrap();
        drop(tx);

        let report = consumer.consume(&mut rx).await.expect("no leaf failed");
        assert_eq!(report.blocks, 1);
        assert_eq!(report.batches, 2);
        assert_eq!(
            report.stale_batches, 1,
            "block 99's batch is behind the clock"
        );
        assert_eq!(
            consumer.clock().current_block,
            100,
            "a batch never moves the clock"
        );
    }

    /// Teardown's end verdict is the policy value's: the same stream end is a
    /// quiet end for a session that stops itself and a loud pump stop for a
    /// supervised one.
    #[tokio::test]
    async fn the_end_verdict_follows_the_quiet_end_policy() {
        for (allow_quiet_end, expected) in [
            (true, ConsumerEnd::QuietEnd),
            (false, ConsumerEnd::PumpStopped),
        ] {
            let executor = fake_executor(None, Arc::new(tokio::sync::Notify::new()));
            let (mut consumer, _heartbeat) = consumer(executor, allow_quiet_end);
            let (tx, mut rx) = mpsc::unbounded_channel::<SessionEvent>();
            drop(tx);
            let report = consumer.consume(&mut rx).await.expect("no leaf failed");
            assert_eq!(report.end, Some(expected));
            assert_eq!(report.end_of_stream, 1);
        }
    }
}
