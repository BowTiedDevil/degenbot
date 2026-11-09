//! The bounded async candidate fan-out — the scheduler that owns candidate
//! execution: W worker threads, F total active EVM slots, and per-candidate
//! results returned by input index.
//!
//! The prepared-sequence fan-out a production dispatch drives has three facts
//! the sequential drivers cannot give it: candidates must overlap (each one's
//! cold reads suspend on evm2's async fiber instead of blocking), the footprint
//! must be bounded (a fan-out of a hundred candidates cannot hold a hundred
//! EVMs, and a candidate waiting in the queue must hold nothing at all), and the
//! results must come back keyed by the input they belong to even though they
//! complete in whatever order their reads settle.
//!
//! # What a slot owns, and what a worker owns
//!
//! A [`AsyncFanoutSlot`] owns ONE EVM: the engine configuration (spec, version,
//! registry, precompiles, interpreter runner), the block env it replays under,
//! and the accepted-state starting point every candidate it carries begins from.
//! An EVM is also a fiber stack (`evm2`'s async entrypoints run the synchronous
//! engine on a native fiber, and the stack is reserved on the EVM), so a slot is
//! the unit that can suspend: while its candidate waits on a cold read, the
//! slot's fiber is parked and the EVM is held by that suspension.
//!
//! A **worker** is an OS thread, and it owns everything its candidates touch: a
//! queue of candidate indices, a pool of idle slots, the active futures it polls,
//! and every EVM and fiber stack behind them. Concurrency here is real CPU
//! overlap, not suspension bookkeeping: a lane thread builds its own slot EVMs,
//! builds its own `transact_async` futures, and polls them on the thread that owns
//! them.
//!
//! That is exactly what evm2's local entrypoints ask for, not a bar to threading.
//! [`Evm::transact_async`] and [`Evm::read_account_info_async`] return non-`Send`
//! futures because the fiber stack lives IN the EVM — a half-run fiber must not
//! move between threads. A scoped thread that creates the EVM, creates the future,
//! polls it, and drops both on its own stack never moves anything across a thread,
//! so this scheduler needs no `Send` EVM, no `transact_async_send`, and no
//! `evm_is_send`: W lanes are W `std::thread::scope` threads holding only local
//! futures. The only thing required to be shareable is the data that genuinely
//! crosses a thread — the candidate slice, the candidate state source, the engine
//! configuration, and the read hub — borrowed `&`-shared, with `Sync` bounds on
//! [`CandidateStateSource`] and [`FanoutEngineConfig`] for it.
//!
//! Each lane thread enters the caller's runtime
//! ([`tokio::runtime::Handle::block_on`]) to drive its own local futures, so a
//! round's provider I/O reaches the same reactor the call came from. A fan-out of
//! ONE lane needs no thread at all: [`AsyncEvmFanout::run`] drives it on the
//! caller's own task.
//!
//! # Why one EVM per worker is insufficient
//!
//! If a worker owned one EVM, its lane would stall for the whole duration of
//! every cold read: the single fiber is parked, so the worker has nothing to
//! poll. The reads are the work — a candidate is mostly waiting — so a
//! one-EVM-per-worker pool degenerates into the sequential fan-out this module
//! replaces, with the added cost of the fiber dance. A worker must be able to
//! hold several parked slots at once and hand its free slot to the next queued
//! candidate the moment one settles. That is what the two knobs state
//! separately: `workers` (W) is how many lanes share the work and how the slot
//! budget is split, `slots` (F) is how many candidates may be in flight at
//! once, and `F > W` is the interesting configuration
//! ([`FanoutLimits::new`] keeps at least one slot per lane, so W lanes own F
//! slots between them, `F >= W`).
//!
//! # Reuse: what a candidate inherits and what it must not
//!
//! A reused slot keeps its engine configuration and its EVM — which is the
//! expensive part — and inherits NOTHING else. [`AsyncFanoutSlot::install`]
//! replaces the EVM's whole accepted-state layer
//! ([`Evm::set_database`] → `State::set_initial`), which drops the previous
//! candidate's accepted writes, its journal, its committed logs, its transient
//! (EIP-1153) storage, its warm sets, and its read caches, and installs the new
//! candidate's own initial database with that candidate's
//! [`SimulationOverrideParams`](crate::sim::SimulationOverrideParams) applied
//! over it (owner funding, declared code, settlement warmup). So two candidates
//! that share a slot share a spec and a block env, never a write: candidate B
//! cannot observe candidate A's SSTORE, its logs, its transient storage, or its
//! override-injected rows.
//!
//! The sequence runs IN the slot EVM, through
//! [`Evm2SimSession::run_sequence_async_in_place`]: `transact_async` and
//! `read_account_info_async` reuse that EVM's own engine configuration and fiber
//! stack across a candidate's steps AND across every candidate the slot carries,
//! so a reused slot is one EVM, one fiber stack, and one installation per
//! candidate. Nothing a slot already has is rebuilt per candidate — no EVM, no
//! stack, no throwaway — and a sequence's accepted overlay lands in the slot it
//! ran on.
//!
//! That last fact is the reset contract, and it is the slot owner's:
//! [`AsyncFanoutSlot::install`] replaces the whole accepted-state layer, so it is
//! what clears the previous candidate's accepted overlay (and with it the
//! journal, committed logs, transient storage, warm sets, and read caches) before
//! the next candidate starts. The fan-out installs before every sequence, so two
//! candidates that share a slot never share a write; the in-place verb's parity
//! with the throwaway driver is certified by the session's own tests.
//!
//! # Queued candidates hold nothing
//!
//! A candidate is prepared data ([`PreparedCandidate`]: its steps, its capture
//! mode, its override params) until it is ADMITTED. Admission is the moment the
//! worker draws a free slot (or builds one, up to the lane's capacity) and calls
//! [`AsyncFanoutSlot::install`]; only then does an EVM, a database, a fiber
//! stack, or a boxed future exist for it. A queue longer than the slot budget
//! costs indices, nothing more, and a fan-out smaller than the budget occupies
//! exactly as many slots as it has candidates.
//!
//! # Scheduling: poll, and drain when nothing can progress
//!
//! Each pass admits queued candidates into its lane's free slots and polls that
//! lane's active futures. A pass that made progress (admitted a candidate,
//! completed one, or woke one) runs again immediately. A pass that made none means
//! every active fiber of that lane is parked on a cold read, so the lane settles
//! ONE [`ReadHub::drain_round`] — the read lane's own round — and polls again.
//! That is the hub's documented caller shape, and it is why this module is the
//! hub's scheduler: with no runnable fiber the round IS the only progress left.
//!
//! # Drain ownership, and why an empty round is not a stall
//!
//! With W lanes concurrent a round is a SHARED event: the hub takes the queue at
//! its start, a read registered while that round is in flight joins it, and a
//! second concurrent drain therefore finds nothing queued and reports an empty
//! [`RoundReport`](super::read_hub::RoundReport). Read as "nothing can settle",
//! that empty report would let one lane's in-flight round starve every other lane
//! into a false [`FanoutError::Stalled`] — the lanes would count a wave of reads
//! being served as a stall. [`DrainPolicy`] selects how the lanes own rounds:
//!
//! - [`DrainPolicy::SessionWide`] is the committed baseline: one round is in
//!   flight for the whole fan-out. A lane takes the drain ROLE before it drains
//!   and holds it for the whole round (`HubDrain`) — an async mutex that guards
//!   the act of taking and settling a round and nothing else: no candidate,
//!   slot, queue, future, outcome, or gauge lives behind it, and no lock of any
//!   kind is held while a candidate is polled. Because the role spans the round,
//!   an empty report can only mean the queue really was empty — no other lane's
//!   round could have owned this lane's reads — and one round in flight per
//!   fan-out is also the shape the hub wants: overlapping rounds only split the
//!   same wave of reads across two packets.
//! - [`DrainPolicy::PerLane`] is the experimental alternative: every lane
//!   settles its own rounds, concurrently. The hub's single-flight rule already
//!   keeps one fetch per identity whatever lane drains it, so concurrent rounds
//!   never duplicate a fetch and never change a requester's result; what they
//!   change is which lane's round carries which wave of reads. An empty report
//!   is then ambiguous in exactly one way — another lane's round may own this
//!   lane's reads — and the coordinator resolves it by WAITING: a lane that
//!   drains empty while a neighbour's round is in flight waits for that round to
//!   settle (and then polls the reads it carried), so the wave counts as the
//!   neighbour's progress it is, never as this lane's march through
//!   [`MAX_IDLE_ROUNDS`] toward a false stall. An empty report with no round in
//!   flight is the unambiguous one: the queue really was empty.
//! - [`DrainPolicy::PerLaneUnguarded`] is [`DrainPolicy::PerLane`] with that
//!   wait removed: the negative probe's fault injection, which must reproduce
//!   the false stall the wait exists to prevent.
//!
//! A driver given `None` for the hub states that its candidates' databases
//! settle their reads themselves (an in-memory stack, or a test double that
//! self-wakes); it then has no round to drain and a parked-everything pass is a
//! stall. Either way the driver refuses to spin: a pass that finds nothing
//! yields the task before its nothing counts — a poll with no cooperative
//! budget left reports Pending whatever its read holds, so counting those
//! reports without a fresh budget manufactures stalls and spins — and after
//! [`MAX_IDLE_ROUNDS`] consecutive passes that neither progressed nor settled a
//! read, the run fails with [`FanoutError::Stalled`] naming the candidate
//! indices still waiting, instead of parking the caller forever.
//!
//! # Shutdown and cancellation
//!
//! Every thread [`AsyncEvmFanout::run`] starts is SCOPED to it: the lane threads
//! are `std::thread::scope` children, joined before `run` returns, so there is no
//! detached task, no background executor, and no thread whose lifetime is not the
//! call's. A lane owns its idle slots, its active futures, and its EVMs, and drops
//! all of them when it returns — no EVM outlives the fan-out that built it, and
//! every fiber stack is released on the thread that ran it.
//!
//! # What that costs, and how a caller cancels
//!
//! Joining lane threads is a synchronous region, so the future of a multi-lane
//! `run` cannot be dropped mid-fan-out: its grain is the whole fan-out. The one
//! exception is the single-lane path, which runs on the caller's own task and is
//! dropped like any other future — each active candidate's drop is then evm2's
//! documented cancellation path (the fiber is resumed with `Cancelled` and its
//! stack is recycled or released). Multi-lane callers cancel by not awaiting the
//! run at all, and a lane's futures still drop through that same path when the
//! lane returns or unwinds. A hub read whose requester went away is settled by its
//! own round into a dropped channel — the hub already treats a dropped receiver as
//! a requester that left.
//!
//! A multi-lane run also joins on the calling thread, so a `current_thread`
//! runtime's reactor is blocked for the run's duration. A stack whose rounds need
//! the reactor (live RPC) belongs on a multi-thread runtime, where the lanes'
//! provider I/O is driven by the runtime's other workers; a stack whose reads
//! settle synchronously (in-memory, or a test double) needs no driver at all.
//!
//! # Migrating a sequential fan-out onto this module
//!
//! Take the pieces in this order; nothing else in the session or hub changes.
//!
//! 1. Build one [`FanoutEngineConfig`] for the block
//!    ([`FanoutEngineConfig::production`] is the handle's own policy: the
//!    production spec with the nonce check removed) and carry the handle's
//!    interpreter runner onto it with [`FanoutEngineConfig::with_jit`].
//! 2. Implement [`CandidateStateSource`] for the caller's stack: it returns the
//!    candidate's fresh initial database. A stack whose cold reads park on the
//!    hub is [`ReadHubDb`] wrapped in evm2's `AsyncDb` (the hub adapter here,
//!    the hub's own batching behind it); a stack that settles its own reads is
//!    whatever `AsyncDatabase` it already builds. The source is asked once per
//!    admitted candidate, never per queued one, and every lane shares it through
//!    a `&`, so it must be `Sync`.
//! 3. Prepare one [`PreparedCandidate`] per opportunity, in the order results
//!    are wanted.
//! 4. Await [`AsyncEvmFanout::run`] with the fan-out's [`ReadHub`] (the same
//!    hub [`ReadHubDb`] reads through) when the stack's cold reads park on the
//!    hub — the lanes settle their own rounds — and read
//!    [`FanoutOutcome::outcomes`] by index — `outcomes[i]` is candidate `i`'s
//!    settled sequence or its refusal, whatever order they completed in
//!    ([`FanoutStats::completion_order`] reports that order when a caller wants
//!    to observe it).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use alloy::primitives::{Address, B256};

use ::evm2::bytecode::Bytecode;
use ::evm2::env::BlockEnvExt;
use ::evm2::ethereum::ethereum_tx_registry;
use ::evm2::evm::r#async::AsyncDatabase;
use ::evm2::evm::{AccountInfo, CacheDB, DynDatabase, InterpreterRunner};
use ::evm2::interpreter::Word;
use ::evm2::{BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Precompiles, SpecId, Version};

use degenbot_rpc::provider::AlloyProvider;

use super::alloy_async_db::account_info_from_read;
use super::read_hub::{ReadHubError, RoundReport};

use super::handle::EVM2_PRODUCTION_SPEC;
use super::read_hub::ReadHub;
use super::session::{Evm2CaptureMode, Evm2SimError, Evm2SimSequence, Evm2SimSession, Evm2SimStep};
use super::state_override::apply_simulation_overrides;
use crate::sim::SimulationOverrideParams;

/// How many consecutive passes a driver tolerates with no fiber runnable and no
/// read round settled before it reports [`FanoutError::Stalled`] instead of
/// parking the caller forever.
pub const MAX_IDLE_ROUNDS: usize = 8;

/// The fan-out's shape: W driver workers and F total active EVM slots (see the
/// module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanoutLimits {
    workers: usize,
    slots: usize,
}

impl FanoutLimits {
    /// `workers` lanes (one OS thread each) carrying `slots` candidates in flight
    /// between them.
    ///
    /// Both are clamped to at least one, and the slot budget is never smaller
    /// than the lane count: a lane that could not hold a slot would never run
    /// anything, so [`Self::effective_workers`] reports the lanes the budget
    /// actually funds.
    #[must_use]
    pub const fn new(workers: usize, slots: usize) -> Self {
        let workers = if workers == 0 { 1 } else { workers };
        let slots = if slots == 0 { 1 } else { slots };
        Self { workers, slots }
    }

    /// The configured lane count (W).
    #[must_use]
    pub const fn workers(&self) -> usize {
        self.workers
    }

    /// The total active slot budget (F).
    #[must_use]
    pub const fn slots(&self) -> usize {
        self.slots
    }

    /// The lanes the slot budget funds: `min(W, F)`.
    #[must_use]
    pub const fn effective_workers(&self) -> usize {
        if self.workers < self.slots {
            self.workers
        } else {
            self.slots
        }
    }

    /// The slots lane `worker` owns: the budget split evenly over
    /// [`Self::effective_workers`], the remainder to the first lanes. The parts
    /// sum to [`Self::slots`].
    #[must_use]
    pub const fn lane_capacity(&self, worker: usize) -> usize {
        let lanes = self.effective_workers();
        if worker >= lanes {
            return 0;
        }
        let base = self.slots / lanes;
        let remainder = self.slots % lanes;
        let extra = if worker < remainder { 1 } else { 0 };
        base + extra
    }
}

/// The engine configuration every slot of one fan-out is built from — the
/// reusable half of a slot (see the module docs).
///
/// The configuration is the handle's policy shape: a spec, the [`Version`] whose
/// feature set the engine honors (the production policy removes the nonce check
/// because a bundle call is `eth_call`-shaped), the block env the sequences
/// replay under, and the optional process-shared interpreter runner (an
/// `Arc<dyn InterpreterRunner>`, itself `Send + Sync`, so the configuration is
/// shared by every lane thread without a copy of the runner).
pub struct FanoutEngineConfig {
    spec: SpecId,
    version: Version,
    block_env: BlockEnvExt,
    jit: Option<Arc<dyn InterpreterRunner<BaseEvmTypes>>>,
}

impl FanoutEngineConfig {
    /// The production policy at `spec`, with the nonce check removed (the
    /// shape [`super::build_block_evm2`] builds).
    #[must_use]
    pub fn production_at(spec: SpecId, block_env: BlockEnvExt) -> Self {
        let mut version = Version::new(spec);
        version.features.remove(EvmFeatures::NONCE_CHECK);
        Self {
            spec,
            version,
            block_env,
            jit: None,
        }
    }

    /// The production policy at the production spec
    /// ([`EVM2_PRODUCTION_SPEC`]).
    #[must_use]
    pub fn production(block_env: BlockEnvExt) -> Self {
        Self::production_at(EVM2_PRODUCTION_SPEC, block_env)
    }

    /// Carry the process-shared interpreter runner onto every slot this
    /// configuration builds.
    #[must_use]
    pub fn with_jit(mut self, runner: Option<Arc<dyn InterpreterRunner<BaseEvmTypes>>>) -> Self {
        self.jit = runner;
        self
    }

    /// The spec the slots run at.
    #[must_use]
    pub const fn spec(&self) -> SpecId {
        self.spec
    }

    /// The block env the sequences replay under.
    #[must_use]
    pub const fn block_env(&self) -> &BlockEnvExt {
        &self.block_env
    }

    /// The installed interpreter runner (`None` = the stock interpreter).
    #[must_use]
    pub fn jit(&self) -> Option<&Arc<dyn InterpreterRunner<BaseEvmTypes>>> {
        self.jit.as_ref()
    }

    /// One candidate's accepted-state starting point: the candidate's fresh
    /// initial database under a `CacheDB` carrying its own overrides.
    fn armed_state<'a>(
        database: impl DynDatabase + 'a,
        overrides: &SimulationOverrideParams,
    ) -> impl DynDatabase + 'a {
        let mut state = CacheDB::new(database);
        apply_simulation_overrides(&mut state, overrides);
        state
    }
}

/// One reusable EVM slot: an EVM plus the engine configuration it was built
/// from (see the module docs).
pub struct AsyncFanoutSlot<'a> {
    worker: usize,
    config: FanoutEngineConfig,
    evm: Evm<'a, BaseEvmTypes>,
    installs: usize,
}

impl<'a> AsyncFanoutSlot<'a> {
    /// Build the slot's EVM over `database` with `overrides` applied, as the
    /// first candidate's installation.
    #[must_use]
    pub fn build(
        worker: usize,
        config: &FanoutEngineConfig,
        database: impl DynDatabase + 'a,
        overrides: &SimulationOverrideParams,
    ) -> Self {
        let mut evm = Evm::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(config.spec, config.version),
            config.spec,
            config.block_env,
            ethereum_tx_registry(config.spec),
            FanoutEngineConfig::armed_state(database, overrides),
            Precompiles::base(config.spec),
        );
        if let Some(runner) = &config.jit {
            evm.set_shared_interpreter_runner(Arc::clone(runner));
        }
        Self {
            worker,
            config: FanoutEngineConfig {
                spec: config.spec,
                version: config.version,
                block_env: config.block_env,
                jit: config.jit.clone(),
            },
            evm,
            installs: 1,
        }
    }

    /// Install one candidate's accepted-state starting point: the EVM's
    /// accepted overlay, journal, logs, transient storage, warm sets, and read
    /// caches are all replaced by `database` (with `overrides` applied over it),
    /// while the engine configuration and the EVM itself are retained. This is
    /// the whole reuse contract (see the module docs).
    pub fn install(
        &mut self,
        database: impl DynDatabase + 'a,
        overrides: &SimulationOverrideParams,
    ) {
        self.evm
            .set_database(FanoutEngineConfig::armed_state(database, overrides));
        self.installs += 1;
    }

    /// The lane that owns this slot.
    #[must_use]
    pub const fn worker(&self) -> usize {
        self.worker
    }

    /// How many candidates have been installed over this slot (1 for the build,
    /// +1 per [`Self::install`]) — the reuse count.
    #[must_use]
    pub const fn installs(&self) -> usize {
        self.installs
    }

    /// The slot's engine configuration.
    #[must_use]
    pub const fn config(&self) -> &FanoutEngineConfig {
        &self.config
    }

    /// The slot's EVM.
    #[must_use]
    pub const fn evm(&self) -> &Evm<'a, BaseEvmTypes> {
        &self.evm
    }

    /// The slot's EVM, mutably — the seam a test (or a caller holding a slot
    /// outside the fan-out) drives directly.
    pub const fn evm_mut(&mut self) -> &mut Evm<'a, BaseEvmTypes> {
        &mut self.evm
    }
}

/// One prepared candidate: the ordered sequence to run, how its captured steps
/// capture, and the override rows its own initial database carries.
///
/// A candidate is inert data until it is admitted into a slot (see the module
/// docs): it holds no EVM, no database, and no fiber.
pub struct PreparedCandidate {
    /// The ordered steps, exactly as [`Evm2SimSession::run_sequence_async`]
    /// consumes them.
    pub steps: Vec<Evm2SimStep>,
    /// How the sequence's captured steps capture.
    pub mode: Evm2CaptureMode,
    /// The per-candidate overrides installed over its fresh database.
    pub overrides: SimulationOverrideParams,
}

impl PreparedCandidate {
    /// A `Seam`-capture candidate over `steps` with `overrides`.
    #[must_use]
    pub fn new(steps: Vec<Evm2SimStep>, overrides: SimulationOverrideParams) -> Self {
        Self {
            steps,
            mode: Evm2CaptureMode::Seam,
            overrides,
        }
    }

    /// The same candidate with another capture mode.
    #[must_use]
    pub fn with_mode(mut self, mode: Evm2CaptureMode) -> Self {
        self.mode = mode;
        self
    }
}

/// Where a candidate's fresh initial database comes from.
///
/// The source is asked ONCE per admitted candidate, at the moment its lane
/// installs it into a slot — never when the candidate is queued. The returned
/// database is the candidate's accepted-state starting point; the fan-out
/// applies the candidate's [`SimulationOverrideParams`] over it and drops it
/// when the next candidate is installed into the same slot.
///
/// Every lane thread shares ONE source through a `&`, so [`AsyncEvmFanout::run`]
/// requires it to be [`Sync`] (`&(dyn CandidateStateSource + Sync)`) while the
/// returned database stays thread-local: a boxed `DynDatabase` is handed to the
/// lane that asked for it and never crosses a thread.
pub trait CandidateStateSource<'a> {
    /// A fresh initial database for `candidate` (an index into the fan-out's
    /// candidate slice).
    fn database(&self, candidate: usize) -> Box<dyn DynDatabase + 'a>;
}

impl<'a, F> CandidateStateSource<'a> for F
where
    F: Fn(usize) -> Box<dyn DynDatabase + 'a>,
{
    fn database(&self, candidate: usize) -> Box<dyn DynDatabase + 'a> {
        self(candidate)
    }
}

/// What one candidate's run settled: its sequence, or the refusal that ended it.
#[derive(Debug)]
pub enum CandidateOutcome {
    /// The sequence settled.
    Settled(Evm2SimSequence),
    /// The sequence was refused (a refusing step, or a mode the async verb
    /// cannot carry). The refusal belongs to this candidate only.
    Refused(Evm2SimError),
}

impl CandidateOutcome {
    /// The settled sequence, when the candidate settled.
    #[must_use]
    pub const fn sequence(&self) -> Option<&Evm2SimSequence> {
        match self {
            Self::Settled(sequence) => Some(sequence),
            Self::Refused(_) => None,
        }
    }

    /// The refusal, when the candidate was refused.
    #[must_use]
    pub const fn refusal(&self) -> Option<&Evm2SimError> {
        match self {
            Self::Settled(_) => None,
            Self::Refused(error) => Some(error),
        }
    }
}

/// What one fan-out run did — the measured shape of the schedule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FanoutStats {
    /// The lanes — OS threads — the run used
    /// ([`FanoutLimits::effective_workers`]).
    pub workers: usize,
    /// The total active slot budget the run was given.
    pub slots: usize,
    /// The EVs the run built. Bounded by `slots`, never by the candidate count:
    /// a queued candidate allocates none, and a reused slot allocates none.
    pub slots_created: usize,
    /// The largest number of candidates the run had in flight at one moment,
    /// across every lane — the achieved active-slot bound, read from the shared
    /// gauge rather than summed from the lanes' own maxima.
    pub peak_active_slots: usize,
    /// The candidate installations performed (`>= slots_created`; the excess is
    /// slot reuse).
    pub installs: usize,
    /// The read rounds that carried at least one read, summed over the lanes.
    pub rounds_drained: usize,
    /// The input indices in the order they completed, stamped as each lane
    /// finished them — the schedule's own evidence, since
    /// [`FanoutOutcome::outcomes`] is index-keyed regardless.
    pub completion_order: Vec<usize>,
}

/// One fan-out's results, keyed by input index.
#[derive(Debug)]
pub struct FanoutOutcome {
    /// `outcomes[i]` is candidate `i`'s settled sequence or its own refusal,
    /// whatever order the candidates completed in.
    pub outcomes: Vec<CandidateOutcome>,
    /// What the run's schedule did (see [`FanoutStats`]).
    pub stats: FanoutStats,
}

impl FanoutOutcome {
    /// Candidate `index`'s settled sequence, if it settled.
    #[must_use]
    pub fn sequence(&self, index: usize) -> Option<&Evm2SimSequence> {
        self.outcomes.get(index)?.sequence()
    }

    /// Every refused candidate, with its input index.
    pub fn refusals(&self) -> impl Iterator<Item = (usize, &Evm2SimError)> {
        self.outcomes
            .iter()
            .enumerate()
            .filter_map(|(index, outcome)| outcome.refusal().map(|error| (index, error)))
    }
}

/// Why a fan-out run could not produce outcomes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FanoutError {
    /// No active candidate could progress and no read round settled, for
    /// [`MAX_IDLE_ROUNDS`] consecutive passes: the named candidates are still
    /// waiting and nothing the driver can do would wake them.
    #[error(
        "the fan-out stalled: candidates {waiting:?} are waiting and no read round can settle"
    )]
    Stalled {
        /// The candidate indices still waiting, in input order.
        waiting: Vec<usize>,
    },
}

/// Counts wakes so a driver pass can tell "every fiber parked on a read" from
/// "a fiber asked to be polled again" (see the module docs).
#[derive(Default)]
struct WakeCounter(AtomicUsize);

impl WakeCounter {
    /// Read and reset the wake count.
    fn take(&self) -> usize {
        self.0.swap(0, Ordering::Relaxed)
    }
}

impl futures_util::task::ArcWake for WakeCounter {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// What one candidate's future hands back when it completes: its input index,
/// the slot it owned, and its outcome.
type SlotCompletion<'a> = (usize, AsyncFanoutSlot<'a>, CandidateOutcome);

/// One in-flight candidate: its index and the future that owns its slot.
struct Active<'j, 'a> {
    candidate: usize,
    future: Pin<Box<dyn Future<Output = SlotCompletion<'a>> + 'j>>,
}

/// One lane's settled work: its candidates' outcomes stamped in completion order,
/// and the slots it built and installed.
struct LaneOutcome {
    /// `(stamp, candidate, outcome)` — the stamp orders completions across lanes.
    completions: Vec<(usize, usize, CandidateOutcome)>,
    slots_created: usize,
    installs: usize,
}

/// The readings every lane shares: how many candidates are in flight right now
/// (so the peak is a real temporal peak across the lanes, not a sum of per-lane
/// maxima) and the stamp that orders completions across lanes.
#[derive(Default)]
struct RunGauge {
    active: AtomicUsize,
    peak: AtomicUsize,
    completed: AtomicUsize,
}

impl RunGauge {
    /// A candidate was admitted: count it in flight and raise the peak.
    fn admitted(&self) {
        let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak.fetch_max(active, Ordering::Relaxed);
    }

    /// A candidate completed: take it out of flight and stamp its completion.
    fn completed(&self) -> usize {
        self.active.fetch_sub(1, Ordering::Relaxed);
        self.completed.fetch_add(1, Ordering::Relaxed)
    }

    /// The largest number of candidates that were in flight at one moment.
    fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }
}

/// How the fan-out's lanes own rounds over their shared [`ReadHub`] (see the
/// module docs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DrainPolicy {
    /// The committed baseline: one round is in flight for the whole fan-out,
    /// the drain role taken per round.
    #[default]
    SessionWide,
    /// The experimental alternative: every lane settles its own rounds
    /// concurrently, and a lane that drains empty while a neighbour's round is
    /// in flight waits for that round instead of counting the wave it is
    /// serving as its own stall.
    PerLane,
    /// [`Self::PerLane`] with that wait removed — the negative probe's fault
    /// injection, which must reproduce the false [`FanoutError::Stalled`] the
    /// wait exists to prevent.
    PerLaneUnguarded,
}

/// What one settled round did (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// The round carried reads and settled them.
    Carried,
    /// The round dispatched nothing while a neighbour's round was in flight,
    /// and this lane waited for that round to settle — the wait that keeps a
    /// wave of reads being served from counting as "nothing can settle".
    Coordinated,
    /// The round dispatched nothing and no round was in flight: the queue
    /// really was empty.
    Idle,
}

/// The drain coordination every lane shares (see the module docs).
pub struct HubDrain<'h, 'p> {
    hub: &'h ReadHub<'p>,
    /// The ownership the lanes take over rounds.
    policy: DrainPolicy,
    /// The drain role ([`DrainPolicy::SessionWide`]): one lane at a time takes
    /// the queue and settles the round. It guards that act and nothing else —
    /// no lane state, no candidate, no outcome, and no gauge lives behind it,
    /// and it is never held while a candidate is polled.
    ///
    /// Holding it across the whole round is what makes an empty report
    /// unambiguous: a lane drains only when no other round is in flight, so
    /// "empty" means the queue really was empty — never "a neighbour's round
    /// owns this lane's reads".
    role: tokio::sync::Mutex<()>,
    /// The rounds in flight right now ([`DrainPolicy::PerLane`]), and the wake
    /// a settling round publishes. An empty report consults it before counting
    /// idle: a neighbour still in flight is the one case where "nothing queued"
    /// does not mean "nothing can settle".
    flight: tokio::sync::watch::Sender<usize>,
    /// Rounds that carried at least one read — the run's measured read shape.
    rounds: AtomicUsize,
}

impl<'h, 'p> HubDrain<'h, 'p> {
    /// The coordinator over `hub`, handing the lanes `policy`'s ownership.
    pub fn new(hub: &'h ReadHub<'p>, policy: DrainPolicy) -> Self {
        let (flight, _installed) = tokio::sync::watch::channel(0usize);
        Self {
            hub,
            policy,
            role: tokio::sync::Mutex::new(()),
            flight,
            rounds: AtomicUsize::new(0),
        }
    }

    /// The ownership the lanes take over rounds.
    pub const fn policy(&self) -> DrainPolicy {
        self.policy
    }

    /// Take the policy's ownership and settle one round, reporting what the
    /// round did.
    pub async fn settle(&self) -> DrainOutcome {
        self.settle_with(|_report| {}).await
    }

    /// [`Self::settle`], with `charge` run once the round settled and before the
    /// ownership ends: the benchmark's modeled round trip, which a lane waiting
    /// on this round waits out along with it.
    pub async fn settle_with(&self, charge: impl FnOnce(&RoundReport)) -> DrainOutcome {
        let report = match self.policy {
            DrainPolicy::SessionWide => {
                // The role ends with this block, before the caller polls a candidate.
                let _role = self.role.lock().await;
                let report = self.hub.drain_round().await;
                charge(&report);
                report
            }
            DrainPolicy::PerLane | DrainPolicy::PerLaneUnguarded => {
                self.flight.send_modify(|in_flight| *in_flight += 1);
                let report = self.hub.drain_round().await;
                charge(&report);
                self.flight.send_modify(|in_flight| *in_flight -= 1);
                if report.is_empty() && self.policy == DrainPolicy::PerLane {
                    return self.await_neighbour_rounds().await;
                }
                report
            }
        };
        if report.is_empty() {
            DrainOutcome::Idle
        } else {
            self.rounds.fetch_add(1, Ordering::Relaxed);
            DrainOutcome::Carried
        }
    }

    /// Wait until no round is in flight, reporting [`DrainOutcome::Coordinated`]
    /// when one was: the wait a false stall would otherwise be counted from.
    async fn await_neighbour_rounds(&self) -> DrainOutcome {
        // Subscribed before the first read of the gauge, so a round settling
        // between that read and this wait is still observed — a missed wake
        // would park the lane past the round that can free it.
        let mut settled = self.flight.subscribe();
        let mut waited = false;
        loop {
            if *self.flight.borrow() == 0 {
                return if waited {
                    DrainOutcome::Coordinated
                } else {
                    DrainOutcome::Idle
                };
            }
            waited = true;
            if settled.changed().await.is_err() {
                return DrainOutcome::Coordinated;
            }
        }
    }

    /// The rounds that carried at least one read.
    fn rounds(&self) -> usize {
        self.rounds.load(Ordering::Relaxed)
    }
}

/// One driver lane and the thread it runs on: the candidates assigned to it, the
/// slots it currently owns idle, and the candidates it has in flight.
struct LaneDriver<'j, 'a, 's, 'g> {
    worker: usize,
    candidates: &'j [PreparedCandidate],
    state: &'s (dyn CandidateStateSource<'a> + Sync),
    config: &'s FanoutEngineConfig,
    gauge: &'g RunGauge,
    capacity: usize,
    queue: VecDeque<usize>,
    idle: Vec<AsyncFanoutSlot<'a>>,
    active: Vec<Active<'j, 'a>>,
    slots_created: usize,
    installs: usize,
    completions: Vec<(usize, usize, CandidateOutcome)>,
}

impl<'j, 'a, 's, 'g> LaneDriver<'j, 'a, 's, 'g>
where
    'a: 'j,
{
    /// Lane `worker` of `limits`' shape: candidate `i` belongs to lane
    /// `i % effective_workers`.
    fn new(
        worker: usize,
        candidates: &'j [PreparedCandidate],
        state: &'s (dyn CandidateStateSource<'a> + Sync),
        config: &'s FanoutEngineConfig,
        limits: &FanoutLimits,
        gauge: &'g RunGauge,
    ) -> Self {
        let lanes = limits.effective_workers();
        let mut queue = VecDeque::new();
        for index in (worker..candidates.len()).step_by(lanes) {
            queue.push_back(index);
        }
        Self {
            worker,
            candidates,
            state,
            config,
            gauge,
            capacity: limits.lane_capacity(worker),
            queue,
            idle: Vec::new(),
            active: Vec::new(),
            slots_created: 0,
            installs: 0,
            completions: Vec::new(),
        }
    }

    /// Whether every candidate this lane owns has been admitted and completed.
    fn done(&self) -> bool {
        self.queue.is_empty() && self.active.is_empty()
    }

    /// The indices of this lane's candidates not yet completed (queued or in
    /// flight), in input order.
    fn waiting(&self) -> Vec<usize> {
        let mut waiting: Vec<usize> = self.queue.iter().copied().collect();
        waiting.extend(self.active.iter().map(|active| active.candidate));
        waiting.sort_unstable();
        waiting
    }

    /// One pass: admit into free slots, then poll every active future. Returns
    /// whether the pass started a candidate or completed one.
    fn pump(&mut self, cx: &mut Context<'_>) -> bool {
        let admitted = self.admit();
        let completed = self.poll(cx);
        admitted || completed
    }

    /// Admit queued candidates into this lane's free slots.
    fn admit(&mut self) -> bool {
        let mut admitted = false;
        while self.active.len() < self.capacity {
            let Some(index) = self.queue.pop_front() else {
                break;
            };
            self.admit_one(index);
            admitted = true;
        }
        admitted
    }

    /// Install `index` into this lane: draw a free slot (or build the lane's next
    /// one) and hand the slot to the candidate's future.
    fn admit_one(&mut self, index: usize) {
        let job = &self.candidates[index];
        // The source is asked only now — a queued candidate never built one.
        let database = self.state.database(index);
        let slot = if let Some(mut slot) = self.idle.pop() {
            slot.install(database, &job.overrides);
            slot
        } else {
            self.slots_created += 1;
            AsyncFanoutSlot::build(self.worker, self.config, database, &job.overrides)
        };
        self.installs += 1;
        self.active.push(Active {
            candidate: index,
            future: slot_future(slot, index, job),
        });
        self.gauge.admitted();
    }

    /// Poll this lane's active futures, collecting every completion. Completions
    /// are removed highest-index first, so each `swap_remove` only ever moves an
    /// already-polled, still active future into the slot being vacated.
    fn poll(&mut self, cx: &mut Context<'_>) -> bool {
        let mut ready: Vec<(usize, usize, AsyncFanoutSlot<'a>, CandidateOutcome)> = Vec::new();
        for position in 0..self.active.len() {
            let future = self.active[position].future.as_mut();
            if let Poll::Ready((index, slot, outcome)) = future.poll(cx) {
                ready.push((position, index, slot, outcome));
            }
        }
        let completed = !ready.is_empty();
        for (position, index, slot, outcome) in ready.into_iter().rev() {
            self.active.swap_remove(position);
            self.idle.push(slot);
            let stamp = self.gauge.completed();
            self.completions.push((stamp, index, outcome));
        }
        completed
    }

    /// Drive this lane to completion: admit and poll, and settle a hub round
    /// whenever nothing in this lane can progress (see the module docs).
    async fn drive(&mut self, drain: Option<&HubDrain<'_, '_>>) -> Result<(), FanoutError> {
        let counter = Arc::new(WakeCounter::default());
        let waker = futures_util::task::waker(Arc::clone(&counter));
        let mut cx = Context::from_waker(&waker);
        let mut idle_rounds = 0;
        while !self.done() {
            if self.pump(&mut cx) {
                idle_rounds = 0;
                continue;
            }
            if counter.take() > 0 {
                idle_rounds = 0;
                // A wake is not always a read: the cooperative poll budget wakes
                // the task when it runs out, and a poll without budget reports
                // Pending whatever its read holds. The lane yields for a fresh
                // budget instead of spinning on budget wakes.
                tokio::task::yield_now().await;
                continue;
            }
            let settled = match drain {
                Some(hub) => hub.settle().await,
                None => DrainOutcome::Idle,
            };
            match settled {
                DrainOutcome::Carried | DrainOutcome::Coordinated => idle_rounds = 0,
                DrainOutcome::Idle => {
                    // The same fresh-budget yield before anything counts as
                    // idle: a budget-starved pass is not a pass that proved
                    // nothing can settle.
                    tokio::task::yield_now().await;
                    idle_rounds += 1;
                    if idle_rounds > MAX_IDLE_ROUNDS {
                        return Err(FanoutError::Stalled {
                            waiting: self.waiting(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// This lane's settled work.
    fn finish(self) -> LaneOutcome {
        LaneOutcome {
            completions: self.completions,
            slots_created: self.slots_created,
            installs: self.installs,
        }
    }
}

/// Merge the lanes' settled work into the index-keyed outcome.
fn assemble(
    lanes: Vec<LaneOutcome>,
    candidates: usize,
    limits: FanoutLimits,
    gauge: &RunGauge,
    rounds_drained: usize,
) -> FanoutOutcome {
    let mut slots_created = 0;
    let mut installs = 0;
    let mut completions: Vec<(usize, usize, CandidateOutcome)> = Vec::new();
    for lane in lanes {
        slots_created += lane.slots_created;
        installs += lane.installs;
        completions.extend(lane.completions);
    }
    // The cross-lane completion order, as stamped when each candidate settled.
    completions.sort_by_key(|(stamp, _, _)| *stamp);
    let mut settled: Vec<Option<CandidateOutcome>> = (0..candidates).map(|_| None).collect();
    let mut completion_order = Vec::with_capacity(completions.len());
    for (_, index, outcome) in completions {
        settled[index] = Some(outcome);
        completion_order.push(index);
    }
    let outcomes: Vec<CandidateOutcome> = settled.into_iter().flatten().collect();
    debug_assert_eq!(
        outcomes.len(),
        candidates,
        "every candidate settles before the run assembles"
    );
    FanoutOutcome {
        outcomes,
        stats: FanoutStats {
            workers: limits.effective_workers(),
            slots: limits.slots(),
            slots_created,
            peak_active_slots: gauge.peak(),
            installs,
            rounds_drained,
            completion_order,
        },
    }
}

/// Build one candidate's future: install already happened, so this owns the slot
/// and runs the sequence verb IN PLACE on the slot's own EVM — the same EVM, the
/// same fiber stack, and the same engine configuration for every candidate the
/// slot carries (see the module docs).
fn slot_future<'j, 'a>(
    mut slot: AsyncFanoutSlot<'a>,
    candidate: usize,
    job: &'j PreparedCandidate,
) -> Pin<Box<dyn Future<Output = SlotCompletion<'a>> + 'j>>
where
    'a: 'j,
{
    Box::pin(async move {
        // The slot's own block env: the in-place verb runs under the configuration
        // the slot was built with, and its reset belongs to the next `install`.
        let block_env = *slot.config().block_env();
        let settled = Evm2SimSession::with_block_env(slot.evm_mut(), block_env)
            .run_sequence_async_in_place(&job.steps, job.mode)
            .await;
        let outcome = match settled {
            Ok(sequence) => CandidateOutcome::Settled(sequence),
            Err(error) => CandidateOutcome::Refused(error),
        };
        (candidate, slot, outcome)
    })
}

/// The bounded async fan-out: W worker threads and F slots over the in-place
/// async sequence verb (see the module docs).
pub struct AsyncEvmFanout {
    limits: FanoutLimits,
    config: FanoutEngineConfig,
    policy: DrainPolicy,
}

impl AsyncEvmFanout {
    /// A fan-out with `limits`' shape building every slot from `config`, its
    /// lanes draining under [`DrainPolicy::SessionWide`].
    #[must_use]
    pub fn new(limits: FanoutLimits, config: FanoutEngineConfig) -> Self {
        Self {
            limits,
            config,
            policy: DrainPolicy::SessionWide,
        }
    }

    /// The same fan-out with its lanes draining under `policy` (see the module
    /// docs).
    #[must_use]
    pub fn with_drain_policy(mut self, policy: DrainPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The drain ownership the lanes take over their shared hub.
    #[must_use]
    pub const fn drain_policy(&self) -> DrainPolicy {
        self.policy
    }

    /// The configured shape.
    #[must_use]
    pub const fn limits(&self) -> FanoutLimits {
        self.limits
    }

    /// The engine configuration every slot is built from.
    #[must_use]
    pub const fn config(&self) -> &FanoutEngineConfig {
        &self.config
    }

    /// Run every candidate, returning their outcomes by input index.
    ///
    /// `hub` is the read lane the candidates' cold reads park on: whenever no
    /// active candidate of a lane can progress, that lane settles one
    /// [`ReadHub::drain_round`] and polls again. `None` states that the
    /// candidates' databases settle their reads themselves, so there is no
    /// round to drain.
    ///
    /// The lanes are the threads: W `std::thread::scope` threads, each owning its
    /// own slots, its own EVMs, and its own local (non-`Send`) futures, joined
    /// before this returns. W = 1 runs on the caller's own task (see the module
    /// docs for the cancellation grain this buys and costs, and for the drain
    /// ownership that keeps one lane's in-flight round from stalling another).
    ///
    /// `state` is borrowed `&`-shared by every lane, which is why it must be
    /// [`Sync`] (as is the [`FanoutEngineConfig`] the slots are built from).
    ///
    /// # Errors
    ///
    /// [`FanoutError::Stalled`] when no candidate progressed and no round
    /// settled for [`MAX_IDLE_ROUNDS`] consecutive passes.
    pub async fn run<'j, 'a>(
        &self,
        candidates: &'j [PreparedCandidate],
        state: &(dyn CandidateStateSource<'a> + Sync),
        hub: Option<&ReadHub<'_>>,
    ) -> Result<FanoutOutcome, FanoutError>
    where
        'a: 'j,
    {
        let lanes_used = self.limits.effective_workers();
        let gauge = RunGauge::default();
        let drain = hub.map(|hub| HubDrain::new(hub, self.policy));
        if candidates.is_empty() {
            return Ok(assemble(
                Vec::new(),
                candidates.len(),
                self.limits,
                &gauge,
                drain.as_ref().map_or(0, HubDrain::rounds),
            ));
        }
        let lanes = if lanes_used == 1 {
            // One lane needs no thread: the caller's task IS the lane.
            let mut lane =
                LaneDriver::new(0, candidates, state, &self.config, &self.limits, &gauge);
            lane.drive(drain.as_ref()).await?;
            vec![lane.finish()]
        } else {
            self.drive_on_threads(candidates, state, drain.as_ref(), &gauge, lanes_used)?
        };
        Ok(assemble(
            lanes,
            candidates.len(),
            self.limits,
            &gauge,
            drain.as_ref().map_or(0, HubDrain::rounds),
        ))
    }

    /// Run one lane per OS thread and join them before returning (see the module
    /// docs).
    fn drive_on_threads<'j, 'a>(
        &self,
        candidates: &'j [PreparedCandidate],
        state: &(dyn CandidateStateSource<'a> + Sync),
        drain: Option<&HubDrain<'_, '_>>,
        gauge: &RunGauge,
        lanes_used: usize,
    ) -> Result<Vec<LaneOutcome>, FanoutError>
    where
        'a: 'j,
    {
        // The ambient handle is what keeps a lane's provider I/O working: the
        // lane's local (non-`Send`) futures are driven on its own thread under the
        // caller's runtime context, so a round's transport future reaches the
        // reactor the call came from.
        let handle = tokio::runtime::Handle::current();
        let config = &self.config;
        let limits = &self.limits;
        let mut finished: Vec<Result<LaneOutcome, FanoutError>> = Vec::with_capacity(lanes_used);
        std::thread::scope(|scope| {
            let joins: Vec<_> = (0..lanes_used)
                .map(|worker| {
                    let handle = handle.clone();
                    scope.spawn(move || {
                        // The lane, its slots, and its futures are built on the
                        // thread that polls them, so nothing non-`Send` crosses.
                        let mut lane =
                            LaneDriver::new(worker, candidates, state, config, limits, gauge);
                        handle.block_on(lane.drive(drain)).map(|()| lane.finish())
                    })
                })
                .collect();
            for join in joins {
                match join.join() {
                    Ok(result) => finished.push(result),
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }
        });
        finished.into_iter().collect()
    }
}

/// Why a [`ReadHubDb`] read has no answer.
#[derive(Debug, thiserror::Error)]
pub enum ReadHubDbError {
    /// The hub's read failed: the member's own refusal, or its round's
    /// transport failure (see [`ReadHubError`]).
    #[error(transparent)]
    Read(#[from] ReadHubError),
    /// The block-hash read is not a hub read (block hashes are per-chain facts
    /// the hub never caches), so it went straight to the provider and failed.
    #[error("RPC read failed: {0}")]
    Rpc(#[from] degenbot_core::errors::ProviderError),
    /// `eth_getBlockByNumber` returned null for a requested block.
    #[error("block {0} not found")]
    BlockNotFound(u64),
    /// The account read eager-loads code with the account, so evm2 never asks
    /// for it by hash. Reported as a typed refusal rather than a panic inside a
    /// fiber.
    #[error("the account read eager-loads code, so get_code_by_hash is unreachable")]
    CodeByHashUnreachable,
}

/// The [`AsyncDatabase`] face of a [`ReadHub`]: every cold read registers
/// against the hub's anchor and resolves in a round the scheduler drains.
///
/// This is the small adapter the fan-out's candidates need, not a second read
/// lane: the value shapes are the hub's (`read_storage`'s word,
/// `read_account`'s balance/nonce/code triple), the account row goes through the
/// shared all-empty rule ([`account_info_from_read`]), and nothing here memoizes,
/// batches, or retries — the hub owns all three. The one read the hub never
/// serves is the block hash, which goes to the provider directly.
///
/// A read registered here parks until a [`ReadHub::drain_round`] settles it, so
/// this adapter is only usable inside a stack whose driver drains rounds — the
/// fan-out's own idle pass ([`AsyncEvmFanout::run`]).
pub struct ReadHubDb<'h, 'p> {
    hub: &'h ReadHub<'p>,
    provider: &'p AlloyProvider,
}

impl<'h, 'p> ReadHubDb<'h, 'p> {
    /// The adapter over `hub`, with `provider` answering the block-hash reads
    /// the hub does not cache.
    #[must_use]
    pub const fn new(hub: &'h ReadHub<'p>, provider: &'p AlloyProvider) -> Self {
        Self { hub, provider }
    }

    /// The hub every read registers against.
    #[must_use]
    pub const fn hub(&self) -> &'h ReadHub<'p> {
        self.hub
    }
}

impl AsyncDatabase for ReadHubDb<'_, '_> {
    type Error = ReadHubDbError;

    fn get_account(
        &mut self,
        address: Address,
    ) -> impl Future<Output = Result<Option<AccountInfo>, Self::Error>> + Send + '_ {
        let hub = self.hub;
        async move {
            let read = hub.read_account(address).await?;
            Ok(account_info_from_read(&read))
        }
    }

    fn get_code_by_hash(
        &mut self,
        code_hash: B256,
    ) -> impl Future<Output = Result<Bytecode, Self::Error>> + Send + '_ {
        // Named (not `_`) so the returned future carries the request it refused.
        let _ = code_hash;
        async move { Err(ReadHubDbError::CodeByHashUnreachable) }
    }

    fn get_storage(
        &mut self,
        address: Address,
        key: Word,
    ) -> impl Future<Output = Result<Word, Self::Error>> + Send + '_ {
        let hub = self.hub;
        async move { hub.read_storage(address, key).await.map_err(Into::into) }
    }

    fn get_block_hash(
        &mut self,
        number: Word,
    ) -> impl Future<Output = Result<B256, Self::Error>> + Send + '_ {
        let provider = self.provider;
        async move {
            let height = number.to::<u64>();
            let block = provider.get_block(height).await?;
            let block = block.ok_or(ReadHubDbError::BlockNotFound(height))?;
            Ok(block.header.hash)
        }
    }
}
