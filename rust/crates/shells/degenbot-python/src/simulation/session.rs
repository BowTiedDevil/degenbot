//! `SessionConsumer` — the `PyO3` seam over the core session consumer
//! (`degenbot_batch_executor::session`, ADR-093 D5/D6).
//!
//! The consume loop lives in the core: the block clock, the batch-work
//! derivation, the ordered lane enqueue, the loud-abort rule, the stall feed,
//! and the session close. This seam keeps only what a caller's frame owns —
//! argument extraction, the calling convention of an awaitable method, and the
//! exception mapping — so the cockpit drives the SAME loop the pure-Rust driver
//! drives (AGENTS.md: a driver shell translates, it does not co-implement).
//!
//! Two construction doors, one consumer type:
//!
//! - [`PySessionConsumer::scripted`] feeds a caller-supplied event sequence
//!   through the loop with no live pump. That is how the shared event-sequence
//!   oracle (`tests/standalone_parity/fixtures/session_consumption.json`)
//!   replays the SAME script the pure-Rust half replays.
//! - [`PySessionConsumer::new`] + [`PySessionConsumer::attach`] move the
//!   engine's ONE event hand-off into the consumer, so production cannot start
//!   without the attached-consumer proof the claim carries.
//!
//! The session moves (`resume`, `run`) are awaitables: the loop awaits a stream
//! and a driver close, both GIL-free for their whole body.

use std::sync::Arc;

use crate::bot::engine::PyArbEngine;
use crate::prelude::*;
use crate::rpc::async_provider::PyAsyncAlloyProvider;
use crate::simulation::batch::PyBatchExecutor;
use crate::simulation::dispatch::payload_row_from_dict;
use crate::submission::sim_pipeline::LEAF_FAILURE_PREFIX;
use degenbot_arbitrage::CapturedSwap;
use degenbot_batch_executor::session::{
    ConsumerErrorKind, ConsumerPolicy, ConsumerReport, FeeHistoryPolicy, SessionClock,
    SessionConsumer, SessionProgress,
};
use degenbot_bot::arb_engine::{
    AccessListRow, BlockNotification, CapturedSwapRow, Heartbeat, InlineSimFailure,
    InlineSwapFamily, ResultBatch, SessionEvent, SimulatedPathResult,
};
use degenbot_pools::solve::SolvePathResult;
use degenbot_simulation::sim::capture::SwapFamily;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use pyo3::PyTypeInfo;

pyo3::create_exception!(
    _ffi,
    SessionConsumerError,
    PyRuntimeError,
    "A session consume-loop abort: a pipeline leaf failure, or a lifecycle\n\
     refusal. A leaf failure's message is the leaf's own detail under the shared\n\
     loud-abort prefix; a refusal (no attached session, a spent claim, a driver\n\
     refusal) is prefix-free and carries the refusal's own text, because no leaf\n\
     ran. `batch_index` is the batch whose pass failed and `report` the counts\n\
     the loop reached before it stopped (the typed consumer report)."
);

/// The consume loop's counts, in the shape a caller reads.
#[derive(Clone, Copy, Debug, Default)]
struct ReportView {
    batches: u64,
    blocks: u64,
    stale_batches: u64,
    end_of_stream: u64,
    end: Option<&'static str>,
}

impl ReportView {
    /// Read one core report into the seam's view.
    fn of(report: &ConsumerReport) -> Self {
        Self {
            batches: report.batches,
            blocks: report.blocks,
            stale_batches: report.stale_batches,
            end_of_stream: report.end_of_stream,
            end: report
                .end
                .map(degenbot_batch_executor::session::ConsumerEnd::name),
        }
    }

    /// The view as the Python report dict.
    ///
    /// Owned (`Py<PyDict>`) rather than borrowed: the report crosses the
    /// awaitable's `Send` boundary, which a GIL-tied `Bound` cannot.
    fn to_py_dict(self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("batches", self.batches)?;
        dict.set_item("blocks", self.blocks)?;
        dict.set_item("stale_batches", self.stale_batches)?;
        dict.set_item("end_of_stream", self.end_of_stream)?;
        dict.set_item("end", self.end)?;
        Ok(dict.unbind())
    }
}

/// The consume loop's typed abort as the Python exception surface.
///
/// The core's [`ConsumerErrorKind`] classifies the message and the shell never
/// re-classifies: a leaf failure maps under the shared loud-abort prefix with
/// the leaf's detail after it, while a refusal
/// ([`ConsumerErrorKind::NoSession`], [`ConsumerErrorKind::ClaimSpent`],
/// [`ConsumerErrorKind::DriverRefusedTheMove`]) maps prefix-free, carrying the
/// refusal's own text — no leaf ran. The batch index and the report ride as
/// attributes, so a caller reads typed facts instead of parsing the text.
fn abort_error(
    py: Python<'_>,
    kind: ConsumerErrorKind,
    detail: &str,
    batch_index: u64,
    report: ReportView,
) -> PyErr {
    let message = match kind {
        ConsumerErrorKind::LeafFailure => format!("{LEAF_FAILURE_PREFIX}{detail}"),
        ConsumerErrorKind::NoSession
        | ConsumerErrorKind::ClaimSpent
        | ConsumerErrorKind::DriverRefusedTheMove => detail.to_owned(),
    };
    let Ok(instance) = SessionConsumerError::type_object(py).call1((message.clone(),)) else {
        // Building the typed instance is the whole point of the call; if the
        // type is not callable the plain error still carries the contract.
        return PyRuntimeError::new_err(message);
    };
    if let Err(err) = instance.setattr("batch_index", batch_index) {
        return err;
    }
    match report.to_py_dict(py) {
        Ok(report) => {
            if let Err(err) = instance.setattr("report", report) {
                return err;
            }
        }
        Err(err) => return err,
    }
    PyErr::from_value(instance)
}

/// Where the consumer reads its ordered events from.
///
/// The origin rides every slot move: `resume` takes the consumer out for its
/// await and restores it WITH the origin, so a scripted consumer refused
/// `resume` still consumes its script for the follow-on `run`.
enum EventOrigin {
    /// The `#[new]` door's placeholder: no stream has moved in yet.
    ///
    /// It guards the ATTACH transition only. An unattached `resume` / `run`
    /// stays the core's refusal (its own `unattached` constructor), so the
    /// shipped no-session message and its mapping path are unchanged.
    Pending,
    /// Production: the engine's one hand-off, moved in by `attach`.
    Attached,
    /// The oracle: a caller-supplied script, queued before the loop starts.
    Scripted(tokio::sync::mpsc::UnboundedReceiver<SessionEvent>),
}

/// A lifecycle move a caller asks the slot to make.
#[derive(Clone, Copy, Debug)]
enum SlotMove {
    /// The engine's hand-off moves in (`Pending -> Attached`).
    Attach,
    /// The claim's one use (`Fresh -> Running -> Fresh`).
    Resume,
    /// The consume loop (`Fresh -> Spent`, restored on a delegated refusal).
    Run,
}

impl SlotMove {
    /// The move's name, as a slot refusal spells it.
    fn name(self) -> &'static str {
        match self {
            Self::Attach => "attach",
            Self::Resume => "resume",
            Self::Run => "run",
        }
    }

    /// Why the slot refuses this move, or `Ok` when it admits it.
    ///
    /// One reason per illegal pair, so no refusal borrows another's text.
    fn admit(self, slot: &ConsumerSlot) -> Result<(), &'static str> {
        match self {
            Self::Attach => match slot {
                ConsumerSlot::Fresh(_, EventOrigin::Pending) => Ok(()),
                ConsumerSlot::Fresh(_, EventOrigin::Attached) => {
                    Err("one session has one consumer")
                }
                ConsumerSlot::Fresh(_, EventOrigin::Scripted(_)) => {
                    Err("a scripted consumer cannot attach an engine hand-off")
                }
                ConsumerSlot::Running(_) => Err("a move is already in flight"),
                ConsumerSlot::Spent => Err("a spent consumer cannot attach"),
            },
            Self::Resume => match slot {
                ConsumerSlot::Fresh(_, _) => Ok(()),
                ConsumerSlot::Running(_) => Err("a move is already in flight"),
                ConsumerSlot::Spent => Err("a spent consumer cannot resume"),
            },
            Self::Run => match slot {
                ConsumerSlot::Fresh(_, _) => Ok(()),
                ConsumerSlot::Running(_) => Err("a move is already in flight"),
                ConsumerSlot::Spent => Err("a spent consumer cannot run"),
            },
        }
    }
}

/// The consumer's lifecycle: the consumer and its event origin, one in-flight
/// move at a time, and nothing after the loop ran.
///
/// A refused move leaves the slot fresh, and the claim/attachment facts stay
/// core-owned: a delegated refusal (`resume` / `run` with no session, a spent
/// claim, a driver refusal) restores [`Self::Fresh`] with the origin it took, so
/// the caller may dispatch the follow-on move the core's own contract admits.
enum ConsumerSlot {
    /// The consumer may move; nothing is in flight.
    Fresh(SessionConsumer, EventOrigin),
    /// An awaitable holds the consumer. The origin stays behind, so the restore
    /// is origin-preserving and a concurrent move is refused instead of aliasing
    /// the loop.
    Running(EventOrigin),
    /// The consume loop ran: nothing moves again.
    Spent,
}

impl ConsumerSlot {
    /// The state a refusal message names.
    fn state(&self) -> &'static str {
        match self {
            Self::Fresh(_, EventOrigin::Pending) => "pending (no engine hand-off attached)",
            Self::Fresh(_, EventOrigin::Attached) => "attached",
            Self::Fresh(_, EventOrigin::Scripted(_)) => "scripted",
            Self::Running(_) => "running",
            Self::Spent => "spent",
        }
    }

    /// The refusal for a move this slot cannot make: one message per illegal
    /// transition, naming the move and the state it found.
    ///
    /// Shell-local by construction: the core never models a slot transition, so
    /// the message carries no core kind and never traverses the loud-abort
    /// mapping that prefixes a leaf failure.
    fn refuse(&self, moved: SlotMove, why: &str) -> PyErr {
        PyRuntimeError::new_err(format!(
            "the session consumer slot refuses {}: it is {}; {why}",
            moved.name(),
            self.state()
        ))
    }

    /// Take the consumer out for `resume`, leaving its origin behind in
    /// [`Self::Running`].
    ///
    /// A refused take leaves the slot untouched.
    fn take_for_resume(&mut self) -> PyResult<SessionConsumer> {
        if let Err(why) = SlotMove::Resume.admit(self) {
            return Err(self.refuse(SlotMove::Resume, why));
        }
        let Self::Fresh(consumer, origin) = std::mem::replace(self, Self::Spent) else {
            unreachable!("the admission check just proved the slot fresh");
        };
        *self = Self::Running(origin);
        Ok(consumer)
    }

    /// Put `resume`'s consumer back, with the origin it left behind.
    fn restore(&mut self, consumer: SessionConsumer) {
        let Self::Running(origin) = std::mem::replace(self, Self::Spent) else {
            debug_assert!(false, "a restore follows its own take");
            return;
        };
        *self = Self::Fresh(consumer, origin);
    }

    /// Take the consumer AND its origin for the terminal `run`.
    ///
    /// The loop reads the origin's receiver itself, so the origin cannot stay
    /// behind; the slot goes [`Self::Spent`] at dispatch (a concurrent or later
    /// move is refused as spent) and a delegated refusal restores the pair.
    fn take_for_run(&mut self) -> PyResult<(SessionConsumer, EventOrigin)> {
        if let Err(why) = SlotMove::Run.admit(self) {
            return Err(self.refuse(SlotMove::Run, why));
        }
        let Self::Fresh(consumer, origin) = std::mem::replace(self, Self::Spent) else {
            unreachable!("the admission check just proved the slot fresh");
        };
        Ok((consumer, origin))
    }
}

/// The session consume loop (the core's `SessionConsumer`) plus its one
/// stream-activity heartbeat and progress view.
#[pyclass(name = "SessionConsumer", module = "degenbot._ffi.simulation")]
pub struct PySessionConsumer {
    slot: Arc<parking_lot::Mutex<ConsumerSlot>>,
    heartbeat: Heartbeat,
    progress: SessionProgress,
}

impl PySessionConsumer {
    /// Build the core consumer over one session executor and clock.
    fn build(
        executor: &PyBatchExecutor,
        origin: EventOrigin,
        allow_quiet_end: bool,
        fee_history: Option<FeeHistoryPolicy>,
    ) -> Self {
        let heartbeat = Heartbeat::new();
        let progress = SessionProgress::new();
        let policy = ConsumerPolicy {
            allow_quiet_end,
            heartbeat: Some(heartbeat.clone()),
            progress: progress.clone(),
        };
        let mut clock = SessionClock::new(executor.dispatcher_arc())
            .with_suppression(executor.suppression_arc());
        if let Some(fee_history) = fee_history {
            clock = clock.with_fee_history(fee_history);
        }
        let consumer = SessionConsumer::new(executor.executor_arc(), clock, policy);
        Self {
            slot: Arc::new(parking_lot::Mutex::new(ConsumerSlot::Fresh(
                consumer, origin,
            ))),
            heartbeat,
            progress,
        }
    }

    /// The advisory fee-history sample the session records per accepted header.
    fn fee_history(
        provider: Option<&PyAsyncAlloyProvider>,
        block_count: u64,
    ) -> Option<FeeHistoryPolicy> {
        provider.map(|provider| FeeHistoryPolicy {
            provider: provider.provider_arc(),
            block_count,
            percentiles: Self::core_percentiles(),
        })
    }

    /// The advisory sample's reward percentiles: the foundation pair, read at
    /// build time so no cached or mirrored pair can redirect the sample.
    fn core_percentiles() -> Vec<f64> {
        degenbot_core::fee_percentiles::PRIORITY_FEE_PERCENTILES
            .iter()
            .map(|percentile| f64::from(u32::try_from(*percentile).unwrap_or(u32::MAX)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fee policy's pair is the foundation pair at build time - a
    /// hardcoded or cached pair at this seam would silently redirect every
    /// `eth_feeHistory` sample away from the declared pair.
    #[test]
    fn the_fee_policy_reads_the_core_percentile_pair() {
        let expected: Vec<f64> = degenbot_core::fee_percentiles::PRIORITY_FEE_PERCENTILES
            .iter()
            .map(|percentile| f64::from(u32::try_from(*percentile).unwrap_or(u32::MAX)))
            .collect();
        assert_eq!(PySessionConsumer::core_percentiles(), expected);
    }
}

#[pymethods]
impl PySessionConsumer {
    /// Build the session consumer over a session executor, unattached.
    ///
    /// Production builds this shape and then moves the engine's hand-off into
    /// it with :meth:`attach`; the slot starts `Pending` (no stream has moved
    /// in), which is what makes a second attach and a scripted attach refusals
    /// rather than silent replacements. `allow_quiet_end` is the session's end
    /// policy:
    /// `False` for a supervised session, where a natural stream end means the
    /// pump died outside its own teardown and the end verdict says so. A
    /// `fee_history_provider` opts the session into the advisory per-header
    /// `eth_feeHistory` sample (the core's declared percentile pair).
    #[new]
    #[pyo3(signature = (
        executor,
        *,
        allow_quiet_end = false,
        fee_history_provider = None,
        fee_history_blocks = 1,
    ))]
    fn new(
        executor: &PyBatchExecutor,
        allow_quiet_end: bool,
        fee_history_provider: Option<&PyAsyncAlloyProvider>,
        fee_history_blocks: u64,
    ) -> Self {
        Self::build(
            executor,
            EventOrigin::Pending,
            allow_quiet_end,
            Self::fee_history(fee_history_provider, fee_history_blocks),
        )
    }

    /// Build the consumer over a scripted event sequence (the shared oracle).
    ///
    /// Each event is the engine stream's own Python shape: a block dict
    /// (`kind="block"` + the notification fields) or a result-batch dict
    /// (`kind="batch"` + `fresh`/`updated` six-tuples and `payloads`), so the
    /// script a test replays is the surface the engine handed Python.
    ///
    /// # Errors
    ///
    /// `ValueError`: an event is not the engine's stream shape.
    #[staticmethod]
    #[pyo3(signature = (executor, events, *, allow_quiet_end = true))]
    fn scripted(
        executor: &PyBatchExecutor,
        events: &Bound<'_, PyList>,
        allow_quiet_end: bool,
    ) -> PyResult<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        for event in events.iter() {
            tx.send(session_event_from_py(&event)?)
                .map_err(|_| PyRuntimeError::new_err("the scripted session channel closed"))?;
        }
        drop(tx);
        Ok(Self::build(
            executor,
            EventOrigin::Scripted(rx),
            allow_quiet_end,
            None,
        ))
    }

    /// Move the engine's ONE event hand-off (and its driver) into this
    /// consumer.
    ///
    /// The hand-off travels with the claim that proves a consumer holds it, so
    /// the consumer's :meth:`resume` is the only way production starts. One
    /// session has one consumer, and the attach transition is guarded here: a
    /// consumer that already holds a hand-off, a scripted consumer, one with a
    /// move in flight, and a spent one are each refused by name. The slot is
    /// checked BEFORE the engine's hand-off is taken, so a refused attach
    /// destroys nothing — the other engine's stream stays claimable.
    ///
    /// # Errors
    ///
    /// `RuntimeError`: the slot refuses the attach transition, or the engine's
    /// hand-off is already claimed.
    fn attach(&self, engine: &PyArbEngine) -> PyResult<()> {
        let mut slot = self.slot.lock();
        if let Err(why) = SlotMove::Attach.admit(&slot) {
            return Err(slot.refuse(SlotMove::Attach, why));
        }
        let handoff = engine.take_handoff().ok_or_else(|| {
            PyRuntimeError::new_err(
                "the engine's event stream is already claimed; one consumer hand-off \
                 per session",
            )
        })?;
        let driver = engine.driver_arc();
        let ConsumerSlot::Fresh(consumer, origin) = &mut *slot else {
            unreachable!("the admission check just proved the slot pending");
        };
        consumer.attach(handoff, driver);
        *origin = EventOrigin::Attached;
        Ok(())
    }

    /// Start production: the claim's one use is the driver's `run`.
    ///
    /// The core closes the session on a refusal too, so a consumer that could
    /// not start never leaves a session running with nobody consuming it. The
    /// refusal itself is the core's (the claim is core-owned); the slot restores
    /// the consumer with its event origin, so a scripted consumer refused here
    /// still consumes its script when the caller turns to :meth:`run`.
    ///
    /// # Errors
    ///
    /// `RuntimeError`: the slot refuses the move (a move is already in flight,
    /// or the consumer already ran).
    /// `SessionConsumerError` (a `RuntimeError`) carrying the core's refusal —
    /// the driver's refusal, a spent claim, or no attached session. A refusal is
    /// prefix-free: its message is the refusal's own text.
    fn resume<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let slot = Arc::clone(&self.slot);
        crate::ambient_runtime::future_into_py(py, async move {
            let mut consumer = slot.lock().take_for_resume()?;
            let outcome = consumer.resume().await;
            slot.lock().restore(consumer);
            match outcome {
                Ok(()) => Ok(()),
                Err(failure) => Err(Python::attach(|py| {
                    abort_error(
                        py,
                        failure.error.kind,
                        &failure.error.detail,
                        failure.error.batch_index,
                        ReportView::of(&failure.error.report),
                    )
                })),
            }
        })
    }

    /// Consume the session's ordered stream and report; close runs on every
    /// path (the core owns it).
    ///
    /// The product is the loop's counts plus the typed end verdict
    /// (`quiet-end` / `pump-stopped`). The executor's record stream is quiesced
    /// by the loop's own exit, so a concurrent drain ends by itself.
    ///
    /// An unattached consumer's `run` is the core's refusal, not the slot's: the
    /// core's `unattached("run")` text (no attached session to run) survives
    /// verbatim, and the refused move restores the slot fresh with its origin.
    ///
    /// # Errors
    ///
    /// `RuntimeError`: the slot refuses the move (a move is already in flight,
    /// or the consumer already ran).
    /// `SessionConsumerError` (a `RuntimeError`): the ordered lane stored a leaf
    /// failure, or the core refused the move (no attached session). A leaf
    /// failure's message is the leaf's own text under the shared loud-abort
    /// prefix, re-raised in this caller's frame before any further batch reached
    /// the lane; a refusal's message is the refusal's own text, prefix-free.
    fn run<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let slot = Arc::clone(&self.slot);
        crate::ambient_runtime::future_into_py(py, async move {
            let (mut consumer, mut origin) = slot.lock().take_for_run()?;
            let outcome = match &mut origin {
                EventOrigin::Scripted(rx) => consumer
                    .consume(rx)
                    .await
                    .map(|report| ReportView::of(&report))
                    .map_err(|error| (error.kind, error.detail, error.batch_index, error.report)),
                EventOrigin::Attached | EventOrigin::Pending => consumer
                    .run()
                    .await
                    .map(|report| ReportView::of(&report.consumer))
                    .map_err(|failure| {
                        (
                            failure.error.kind,
                            failure.error.detail,
                            failure.error.batch_index,
                            failure.error.report,
                        )
                    }),
            };
            match outcome {
                Ok(view) => Python::attach(|py| view.to_py_dict(py)),
                Err((kind, detail, batch_index, report)) => {
                    // The refused move leaves the slot fresh: the claim and
                    // attachment facts are core-owned, so the caller may dispatch
                    // the move the core's contract admits.
                    *slot.lock() = ConsumerSlot::Fresh(consumer, origin);
                    Err(Python::attach(|py| {
                        abort_error(py, kind, &detail, batch_index, ReportView::of(&report))
                    }))
                }
            }
        })
    }

    /// The stream-activity beats this consumer fed (one per RECEIVED event).
    #[getter]
    fn beats(&self) -> u64 {
        self.heartbeat.seq()
    }

    /// The consume loop's lock-free progress view: `(batches, blocks,
    /// current_block)`.
    #[getter]
    fn progress(&self) -> (u64, u64, u64) {
        (
            self.progress.batches(),
            self.progress.blocks(),
            self.progress.current_block(),
        )
    }
}

/// Translate one scripted Python event into the session's own event value.
///
/// The shapes are the engine stream's: a block dict, or a result-batch dict
/// whose `fresh`/`updated` rows are the engine's six-tuples and whose
/// `payloads` map is the result-channel payload dict.
fn session_event_from_py(event: &Bound<'_, PyAny>) -> PyResult<SessionEvent> {
    let dict = event
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("scripted session events must be dicts"))?;
    let kind: String = dict
        .get_item("kind")?
        .ok_or_else(|| PyValueError::new_err("scripted session event missing 'kind'"))?
        .extract()?;
    match kind.as_str() {
        "block" => Ok(SessionEvent::Block(BlockNotification {
            number: field_u64(dict, "number")?,
            timestamp: field_u64(dict, "timestamp")?,
            base_fee_per_gas: optional_u64(dict, "base_fee_per_gas")?,
            gas_used: field_u64(dict, "gas_used")?,
            gas_limit: field_u64(dict, "gas_limit")?,
        })),
        "batch" => {
            let mut batch = ResultBatch {
                solve_block: field_u64(dict, "solve_block")?,
                timestamp: optional_u64(dict, "timestamp")?.unwrap_or_default(),
                base_fee_per_gas: optional_u64(dict, "base_fee_per_gas")?,
                gas_used: optional_u64(dict, "gas_used")?.unwrap_or_default(),
                gas_limit: optional_u64(dict, "gas_limit")?.unwrap_or_default(),
                fresh: rows_from_py(dict, "fresh")?,
                updated: rows_from_py(dict, "updated")?,
                expired: id_list(dict, "expired")?,
                removed: id_list(dict, "removed")?,
                payloads: hashbrown::HashMap::default(),
            };
            if let Some(payloads) = dict.get_item("payloads")? {
                if !payloads.is_none() {
                    let payloads = payloads
                        .cast::<PyDict>()
                        .map_err(|_| PyValueError::new_err("payloads must be a dict"))?;
                    for (path_id, value) in payloads.iter() {
                        let path_id: u64 = path_id.extract()?;
                        let row = value
                            .cast::<PyDict>()
                            .map_err(|_| PyValueError::new_err("payload entries must be dicts"))?;
                        let payload = simulated_path_result_from_dict(row)?;
                        if payload.path_id != path_id {
                            return Err(PyValueError::new_err(format!(
                                "payload dict path_id {} disagrees with its key {path_id}",
                                payload.path_id
                            )));
                        }
                        batch.payloads.insert(payload.path_id, payload);
                    }
                }
            }
            Ok(SessionEvent::Batch(batch))
        }
        other => Err(PyValueError::new_err(format!(
            "unknown scripted session event kind {other:?} (expected block|batch)"
        ))),
    }
}

/// The engine's six-tuple solve rows: `(path_id, optimal_input, profit,
/// hop_outputs, consumed_inputs, state_nonces)`.
fn rows_from_py(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Vec<(u64, SolvePathResult)>> {
    let Some(rows) = dict.get_item(key)? else {
        return Ok(Vec::new());
    };
    if rows.is_none() {
        return Ok(Vec::new());
    }
    rows.try_iter()?
        .map(|row| {
            let row = row?;
            let tuple = row
                .cast::<PyTuple>()
                .map_err(|_| PyValueError::new_err(format!("{key} rows must be tuples")))?;
            if tuple.len() != 6 {
                return Err(PyValueError::new_err(format!(
                    "{key} rows must be (path_id, optimal_input, profit, hop_outputs, \
                     consumed_inputs, state_nonces)"
                )));
            }
            let listed = |index: usize| -> PyResult<Vec<alloy::primitives::U256>> {
                tuple
                    .get_item(index)?
                    .try_iter()?
                    .map(|value| Ok(alloy::primitives::U256::from(value?.extract::<u128>()?)))
                    .collect()
            };
            Ok((
                tuple.get_item(0)?.extract::<u64>()?,
                SolvePathResult {
                    optimal_input: alloy::primitives::U256::from(
                        tuple.get_item(1)?.extract::<u128>()?,
                    ),
                    profit: alloy::primitives::U256::from(tuple.get_item(2)?.extract::<u128>()?),
                    hop_outputs: listed(3)?,
                    consumed_inputs: listed(4)?,
                    state_nonces: tuple
                        .get_item(5)?
                        .try_iter()?
                        .map(|value| value?.extract::<u64>())
                        .collect::<PyResult<Vec<u64>>>()?,
                    ..Default::default()
                },
            ))
        })
        .collect()
}

/// A path-id list field (`expired` / `removed`).
fn id_list(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Vec<u64>> {
    let Some(ids) = dict.get_item(key)? else {
        return Ok(Vec::new());
    };
    if ids.is_none() {
        return Ok(Vec::new());
    }
    ids.try_iter()?.map(|id| id?.extract::<u64>()).collect()
}

/// A required `u64` event field.
fn field_u64(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<u64> {
    dict.get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("scripted session event missing '{key}'")))?
        .extract()
}

/// An optional `u64` event field (`None`/absent = `None`).
fn optional_u64(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<u64>> {
    dict.get_item(key)?
        .filter(|value| !value.is_none())
        .map(|value| value.extract::<u64>())
        .transpose()
}

/// The result-channel payload dict back into the engine's inline-sim payload.
///
/// A scripted batch arrives in the engine's own payload-dict shape, so the ONE
/// parser (`payload_row_from_dict`) reads it and this inversion restores the
/// engine value the consumer's `payload_row` extraction consumes. The two
/// translations are inverse, so a scripted payload reaches the executor exactly
/// as a live one does.
fn simulated_path_result_from_dict(row: &Bound<'_, PyDict>) -> PyResult<SimulatedPathResult> {
    let hop_count = optional_u64(row, "hop_count")?;
    let row = payload_row_from_dict(row)?;
    Ok(SimulatedPathResult {
        path_id: row.path_id,
        gross_profit: row.gross_profit,
        net_profit: row.net_profit,
        gas_used: row.gas_used,
        priority_fee: row.priority_fee,
        base_fee_next: row.base_fee_next,
        execute_calldata: row.execute_calldata.to_vec(),
        access_list: row.access_list.as_ref().map(|list| {
            list.0
                .iter()
                .map(|item| AccessListRow {
                    address: item.address,
                    storage_keys: item
                        .storage_keys
                        .iter()
                        .map(|key| alloy::primitives::U256::from_be_bytes(key.0))
                        .collect(),
                })
                .collect()
        }),
        captured_swaps: row
            .captured_swaps
            .iter()
            .map(|swap: &CapturedSwap| CapturedSwapRow {
                emitter: swap.emitter,
                family: match swap.family {
                    SwapFamily::V2 => InlineSwapFamily::V2,
                    SwapFamily::V3 => InlineSwapFamily::V3,
                    SwapFamily::V4 => InlineSwapFamily::V4,
                },
                amount0: swap.amount0,
                amount1: swap.amount1,
                sqrt_price_x96: swap.sqrt_price_x96,
                liquidity: swap.liquidity,
                tick: swap.tick,
            })
            .collect(),
        hop_count: usize::try_from(hop_count.unwrap_or_default()).unwrap_or_default(),
        failure: row.failure.as_ref().map(|failure| InlineSimFailure {
            fail_index: failure.fail_index,
            revert_data: failure.revert_data.to_vec(),
            bucket: failure.bucket.clone(),
        }),
    })
}
