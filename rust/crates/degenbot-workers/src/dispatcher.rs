//! The dispatcher: per-role bounded queues, one precedence grant loop, and
//! the slot host that grants leases ONLY along the T-table (design doc §3–§4).
//!
//! Precedence at lease time (design doc §4, reconciled):
//!
//! 1. pinned continuations (T6) — cycle-critical; the pin IS the key;
//! 2. sim-before-solve — queued `SimDriver` units drain before ANY new
//!    `Solver` queue intake when both contend for free slots (a queued sim
//!    preempts queue position, never a running walk);
//! 3. `Solver` queue intake (walk admission capped by the Solver share);
//! 4. `Resolve` chunks fill the remaining pooled capacity.
//!
//! `Merge` is pinned at boot and NEVER queued. Queues are bounded and
//! overflow LOUDLY (ADR-021 posture: classify, stop loudly, never silently
//! drop). The deadlock ledger carries over (§10): a unit whose results feed
//! a pipe is never abandoned silently — abandoning one mid-flight trips the
//! loud-abort tripwire (log at error + `std::process::abort`), mirroring the
//! executor discipline.

use degenbot_core::{op_error, op_info};
use std::collections::VecDeque;
use std::sync::Arc;

use crate::budget::{BudgetError, BudgetOverrides, FleetBudget};
use crate::gauges::{self as gauges_mod, RoleGaugeSample};
use crate::lane::LaneCtx;
use crate::posture::{
    FleetPosture, PostureChange, PostureOwner, PosturePolicy, PostureWatch, ThrottleSample,
};
use crate::role::{CordonClass, WorkerRole, ALL_ROLES, V1_ACTIVE_ROLES};
use crate::slot::{
    transition, PinKey, RejectedTransition, RejectionReason, SlotState, Transition,
    TransitionContext, UnitId, MERGE_PIN_KEY,
};

/// Worker-slot id within this host.
pub type SlotId = u64;

/// Warm allocator/L1/L2 arena token. Minted when a slot FIRST pins; reused
/// (warm identity) across every cycle while that pin lives; released only at
/// T9 — an arena is never live across a role switch (design doc §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaToken(u64);

impl ArenaToken {
    /// The stub token for pooled (non-pinned) seats: `0` is never minted by
    /// the host (`next_arena` starts at 1), so it can never collide with a
    /// warm identity.
    pub const DETACHED: Self = Self(0);
}

/// A unit of role work. The payload is a `'static + Send` RUST closure — the
/// fleet crate has no pyo3 in it and simulation never round-trips Python
/// (design doc §8), so no worker ever holds a GIL across role work by
/// construction.
pub struct Unit {
    /// Unit id (dispatch bookkeeping).
    pub id: UnitId,
    /// The role this unit executes under.
    pub role: WorkerRole,
    /// The pin key for a Solver bin unit.
    pub key: Option<PinKey>,
    /// Whether this unit's results feed a result pipe (merge/sidecar
    /// discipline: abandoning it mid-flight is a STRANDED PIPE).
    pub result_pipe: bool,
    /// The work payload.
    pub work: Box<dyn FnOnce(&LaneCtx) + Send>,
}

impl std::fmt::Debug for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Unit")
            .field("id", &self.id)
            .field("role", &self.role)
            .field("key", &self.key)
            .field("result_pipe", &self.result_pipe)
            .finish_non_exhaustive()
    }
}

impl Unit {
    /// Build a unit from a Rust closure.
    #[must_use]
    pub fn new(
        id: UnitId,
        role: WorkerRole,
        key: Option<PinKey>,
        result_pipe: bool,
        work: Box<dyn FnOnce(&LaneCtx) + Send>,
    ) -> Self {
        Self {
            id,
            role,
            key,
            result_pipe,
            work,
        }
    }

    /// An inert `work = || {}` unit for pool/dispatch tests.
    #[must_use]
    pub fn noop(id: UnitId, role: WorkerRole, key: Option<PinKey>) -> Self {
        Self::new(id, role, key, false, Box::new(|_ctx: &LaneCtx| {}))
    }
}

/// Why the fleet refused to boot.
///
/// Clone (FF-T1, BPHR6F): the boot-refusal parks in the executors'
/// process materializers and every later caller surfaces a CLONE of the
/// same sticky refusal — the typed error is cheap to hand out forever.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BootError {
    /// Budget sum check failed (fail-loud over-subscription).
    #[error("fleet budget refused: {0}")]
    Budget(#[from] BudgetError),
    /// A boot invariant (slot layout / the merge pin) did not hold.
    #[error("fleet boot invariant violated: {0}")]
    Invariant(&'static str),
}

/// The boot-frozen slot table geometry (2SIOHJ): ONE derivation behind the
/// fleet boot ordering — solver pin seats, sim seats, resolve seats, the
/// registration-intake station, then the merge sidecar at the LAST index.
/// Derived FIRST at boot from [`FleetBudget`] (before any slot cell,
/// per-role queue, or census row exists) and stored on the host; every
/// reader consumes this instead of re-deriving ranges from budget fields.
///
/// Frozen by design: [`FleetHost::resize_quota`] re-declares the LIVE
/// budget (queue bounds, admission shares) but never re-derives the table
/// — slot cells move only at an epoch boundary's T9 re-key — so a layout
/// read is always the boot truth and a budget read is always the live
/// admission truth. See the asymmetry note on [`FleetHost::queue_cap`].
// (`Copy` is unreachable here: the fields are `Range<usize>`, which is
// Clone-only — readers go through `FleetHost::layout()`, which hands out
// the small struct by clone.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotLayout {
    /// The LPT-bin Solver pin seats: one per structural bin (P6YXA6) —
    /// [`SlotLayout::of`] asserts this range's length equals the budget's
    /// `solver_pin_count` (pins == bins by construction).
    solver: std::ops::Range<usize>,
    /// The pooled `SimDriver` seats (duty-counted; fractional-remainder
    /// spenders).
    sim: std::ops::Range<usize>,
    /// The pooled `Resolve` seats (the fixed v1 seat).
    resolve: std::ops::Range<usize>,
    /// The registration intake station's `PoolStateUpdater` seats (PRG-3).
    poolupd: std::ops::Range<usize>,
    /// The merge sidecar's slot index — structurally the LAST index of
    /// the boot table (asserted in [`SlotLayout::of`]; boot pins it
    /// T1→T2→T4 immediately after construction).
    merge: usize,
}

impl SlotLayout {
    /// Derive the boot geometry from the budget — the FIRST boot step.
    ///
    /// # Errors
    /// [`BootError::Invariant`] when a v1-hosted role's range is EMPTY (a
    /// dead station — e.g. `pool_state_updater_slots = 0` — refuses to
    /// boot loudly, never hosts a station nobody can reach), when the
    /// merge sidecar would not land on the LAST index, or when the solver
    /// range drifts from the structural LPT bin count the budget sized.
    pub(crate) fn of(budget: &FleetBudget) -> Result<Self, BootError> {
        let solver_len = budget.solver_pin_count;
        let sim_len = budget.sim_slot_cap;
        let resolve_len = usize::try_from(budget.resolve_cpus).unwrap_or(1);
        let poolupd_len = budget.pool_state_updater_slots;
        if solver_len == 0 {
            return Err(BootError::Invariant(
                "the Solver pin range is empty — no LPT bin seat was sized",
            ));
        }
        if sim_len == 0 {
            return Err(BootError::Invariant(
                "the SimDriver slot range is empty — a dead station cannot host",
            ));
        }
        if resolve_len == 0 {
            return Err(BootError::Invariant(
                "the Resolve slot range is empty — a dead station cannot host",
            ));
        }
        if poolupd_len == 0 {
            return Err(BootError::Invariant(
                "the PoolStateUpdater slot range is empty — the registration \
                 intake station (PRG-3) is a dead station",
            ));
        }
        // pins == bins BY CONSTRUCTION (P6YXA6): the solver range is cut at
        // exactly the structural LPT bin count — the authority boot (and
        // the solve executor's seat array) sizes by. If a future edit ever
        // cuts the range from anything else, this refuses the boot instead
        // of seating bins on phantom seats.
        if solver_len != budget.solver_pin_count {
            return Err(BootError::Invariant(
                "solver seats must equal the structural LPT bin count (pins == bins, P6YXA6)",
            ));
        }
        let solver = 0..solver_len;
        let sim = solver.end..solver.end + sim_len;
        let resolve = sim.end..sim.end + resolve_len;
        let poolupd = resolve.end..resolve.end + poolupd_len;
        let total = poolupd.end + 1; // every hosted range + ONE sidecar slot
        let merge = total - 1; // the sidecar is structurally the LAST index
        if merge != poolupd.end
            || solver.contains(&merge)
            || sim.contains(&merge)
            || resolve.contains(&merge)
            || poolupd.contains(&merge)
        {
            return Err(BootError::Invariant(
                "the merge sidecar must be the LAST slot index, outside every hosted range",
            ));
        }
        Ok(Self {
            solver,
            sim,
            resolve,
            poolupd,
            merge,
        })
    }

    /// The layout's seat count for `role` (the census's per-role slot
    /// budget; the merge sidecar is the single `merge` seat).
    fn seats(&self, role: WorkerRole) -> usize {
        match role {
            WorkerRole::Solver => self.solver.len(),
            WorkerRole::SimDriver => self.sim.len(),
            WorkerRole::Resolve => self.resolve.len(),
            WorkerRole::PoolStateUpdater => self.poolupd.len(),
            WorkerRole::Merge => 1,
            _ => 0,
        }
    }
}

/// The per-role queue bound rule: 2× the role's seats (pipelining depth),
/// 4× for the chunked Resolve role; Merge is unbounded-by-type because it
/// is never queued (and declared-not-active roles hold nothing). ONE rule
/// for both readers: boot (layout seats) and [`FleetHost::queue_cap`]
/// (LIVE budget seats).
fn queue_cap_for(role: WorkerRole, seats: usize) -> usize {
    match role {
        WorkerRole::Solver | WorkerRole::SimDriver | WorkerRole::PoolStateUpdater => seats * 2,
        WorkerRole::Resolve => seats * 4,
        _ => 0,
    }
}

/// Why a queue enqueue was refused (all loud: ADR-021 classify-and-stop).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnqueueError {
    /// Declared role without v1 hosting (Known/planned gating in dispatch).
    #[error(
        "role {0:?} is declared but not v1-active; hosting is a later migration step (ADR-042 Q2)"
    )]
    RoleNotActive(WorkerRole),
    /// Merge is pinned at boot and never queued.
    #[error("merge is pinned at boot and never queued (design doc §4)")]
    MergeNeverQueued,
    /// Bounded queue at capacity.
    #[error("queue full: role {role:?} holds {len}/{cap} — loud overflow, never silent drop")]
    QueueFull {
        /// The overflowing role queue.
        role: WorkerRole,
        /// Current length.
        len: usize,
        /// The bound.
        cap: usize,
    },
    /// The posture holds this role's intake (cordon × deferrable).
    #[error("cordon holds intake for cordon-deferrable role {0:?}")]
    PostureHeld(WorkerRole),
}

/// The submit-seam refusal (LW-T5, Seam E): typed AT the submit surface —
/// admission-side only (a cordon never preempts a running unit: RAYPAR T3
/// never-yield mid-unit; the slot FSM itself stays untouched).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    /// The posture holds this role's intake. The payload is intact and the
    /// caller owns the retry/fallback decision (the serial arm is LW-T7).
    #[error(
        "submit refused: posture {posture:?} holds intake for role {role:?} \
         — admission-side only; running units never preempted (RAYPAR T3)"
    )]
    PostureHeld {
        /// The posture observed at submit.
        posture: FleetPosture,
        /// The refused role.
        role: WorkerRole,
    },
    /// The executor's host lane is closed (host gone) — loud, never a drop.
    #[error("submit refused: host channel closed")]
    PortClosed,
}

/// The submit-seam receipt (LW-T5, Seam E): a unit is NEVER dropped — the
/// unbounded host backlog absorbs overflow (§10 ledger); the receipt TELLS
/// the caller which path its unit took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitReceipt {
    /// `true` = the role queue was already at cap when admitted: the unit
    /// rides the unbounded host backlog and drains FIRST on the next pump.
    /// ADVISORY under mirror lag (the host publishes the length
    /// asynchronously): the FSM remains authoritative and no unit is ever
    /// dropped either way — the bit names the resource the unit rides.
    pub accepted_with_backlog: bool,
}

/// Why a host state operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// The T-table rejected the move.
    #[error("illegal transition: {0}")]
    Transition(#[from] RejectedTransition),
    /// Unknown slot id.
    #[error("unknown slot id {0}")]
    UnknownSlot(SlotId),
    /// A unit with in-flight result sends was abandoned mid-flight — the
    /// stranded-pipe tripwire fired (loud abort discipline).
    #[error("stranded result pipe on slot {0}: loud-abort tripwire fired")]
    StrandedPipe(SlotId),
}

/// What kind of dispatch grant this was (harness + telemetry surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantKind {
    /// A pinned slot's next-cycle unit (T6, same key).
    PinContinuation,
    /// A new Solver pin claim from the queue (T1 keyed lease).
    NewPinClaim,
    /// A pooled sim (T1; cordon floors intake).
    Sim,
    /// A pooled resolve chunk (T1).
    Resolve,
    /// A pooled registration-intake build unit (T1; Deferrable — a cordon
    /// holds intake entirely, in-flight units finish).
    PoolStateUpdate,
}

/// One dispatch grant (slot + unit id; the granted [`Unit`] travels
/// alongside in `dispatch`'s return value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grant {
    /// The slot the unit was granted to.
    pub slot: SlotId,
    /// The granted unit id.
    pub unit: UnitId,
    /// The precedence lane that produced this grant.
    pub kind: GrantKind,
}

/// What [`FleetHost::complete`] produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// The unit completed into a steady pin (T3/T4) on `key`.
    Pinned {
        /// The pin key.
        key: PinKey,
    },
    /// The unit completed back to the pooled set (T5).
    BackToIdle,
}

struct SlotCell {
    /// The idle pool this slot belongs to (dashboards render idle fleet
    /// slots per role even when no lease is active).
    home: WorkerRole,
    state: SlotState,
    arena: Option<ArenaToken>,
}

/// THE one pin representation is the slot table itself: a cell in
/// [`SlotState::Pinned`], written ONLY by the T-table (P-SLOT: the FSM's
/// state is the truth). This renderer derives the `(key, slot)` pin view
/// from it, in SLOT-INDEX order.
///
/// Order contract (deliberate normalization, DNZQ5G): the deleted `pins`
/// mirror was MRU-ordered (`complete()`'s retain+push); the derived view
/// is slot-index ordered. No caller observes pin order — the `pins()`
/// accessor had zero callers repo-wide, and continuation grants are
/// per-key to per-key seats, so grant order among distinct keys carries
/// no semantics. Slot-index order is the documented, test-pinned
/// contract.
fn pinned_slots(slots: &[SlotCell]) -> impl Iterator<Item = (PinKey, SlotId)> + '_ {
    slots
        .iter()
        .enumerate()
        .filter_map(|(i, cell)| match cell.state {
            SlotState::Pinned { key, .. } => Some((key, u64::try_from(i).unwrap_or(SlotId::MAX))),
            _ => None,
        })
}

/// Take the first queued unit for `key` (the pin IS the key: continuation
/// grants go only to their own pin's seat). A free function over the queue
/// slice so the derived-pin iteration in [`FleetHost::dispatch`] can hold
/// the slot table immutably while the queue alone is mutated (disjoint
/// field borrows — no per-pass pin-snapshot allocation).
fn take_solver_unit_for(queue: &mut VecDeque<Unit>, key: PinKey) -> Option<Unit> {
    let pos = queue.iter().position(|u| u.key == Some(key))?;
    queue.remove(pos)
}

/// The fleet host: slots, queues, posture, budget, telemetry — harness-
/// driven core with no production callers yet (design doc §11; hosting of
/// the real engines is F3–F5).
pub struct FleetHost {
    budget: FleetBudget,
    /// The boot plan (FF-T2, MEBF4V): the tiered authority that resolved
    /// this boot (id + binding + the oversubscription mark + the detected
    /// budget). Boot-frozen like the layout; the census rows and
    /// `runtime_status` read it.
    plan: crate::plan::FleetPlan,
    /// The boot-frozen slot table geometry (2SIOHJ): derived FIRST at
    /// boot, before any cell/queue/census row; see [`SlotLayout`].
    layout: SlotLayout,
    /// THE shared fleet posture owner (JCI2FW Part A): the host consults it
    /// everywhere it used to consult a host-local machine (enqueue gate,
    /// admission thresholds, the T7 shed trigger) so every host — and the
    /// process throttle feed — see ONE posture.
    posture: &'static PostureOwner,
    /// The host's subscription to the owner's transition feed: the T7 shed
    /// trigger (a transition INTO Cordoned — or a Cordoned snapshot at a
    /// grant-pass boundary — drains the deferrable in-flight units).
    posture_watch: PostureWatch,
    slots: Vec<SlotCell>,
    queues: [VecDeque<Unit>; 8],
    epoch_boundary: bool,
    next_arena: u64,
    overflow_count: u64,
    tripwire: Arc<dyn Fn(&str) + Send + Sync>,
}

impl std::fmt::Debug for FleetHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FleetHost")
            .field("budget", &self.budget)
            .field("plan", &self.plan.binding)
            .field("layout", &self.layout)
            .field("posture", &self.posture.current())
            .field("slots", &self.slots.len())
            .finish_non_exhaustive()
    }
}

/// Boot description for [`FleetHost::boot`].
#[derive(Debug, Clone, Copy)]
pub struct FleetBoot {
    /// Fractional cgroup quota (cores), from
    /// [`crate::quota::fractional_cpu_budget`].
    pub quota_cpus: f64,
    /// The fleet host-binding profile (FF-T2, MEBF4V): `auto` resolves
    /// the tier from the budget; `pinned`/`serial` force a binding.
    pub profile: degenbot_config::FleetProfile,
    /// Terminal typed overrides.
    pub overrides: BudgetOverrides,
    /// Posture thresholds (typed config).
    pub posture: PosturePolicy,
    /// The shared fleet posture owner (JCI2FW Part A). `None` (production)
    /// installs `posture` into the PROCESS owner first-wins at boot and
    /// consults that — exactly one posture per process. Hermetic tests
    /// MUST inject a fresh [`PostureOwner::new`] owner here (leaked to
    /// `'static`): posture leaking across tests is a failure class
    /// (7KAPBB).
    pub owner: Option<&'static PostureOwner>,
}

impl FleetBoot {
    /// Defaults for tests/hermetic runs: plain fractional quota detection,
    /// no overrides, config-default thresholds.
    #[must_use]
    pub fn from_config(cfg: &degenbot_config::BotConfig) -> Self {
        Self {
            quota_cpus: crate::budget::detected_quota_cpus(&cfg.fleet),
            profile: cfg.runtime.fleet_profile,
            overrides: BudgetOverrides::from_config(cfg),
            posture: PosturePolicy::from_config(&cfg.fleet),
            owner: None,
        }
    }
}

impl PartialEq for FleetBoot {
    fn eq(&self, other: &Self) -> bool {
        // CONFIG equality only (the boot-stamp ledger's key, R2/R3): the
        // owner handle is runtime plumbing, never config.
        self.quota_cpus == other.quota_cpus
            && self.profile == other.profile
            && self.overrides == other.overrides
            && self.posture == other.posture
    }
}

impl FleetHost {
    /// Boot the fleet host: derive the budget (fail-fast), derive the
    /// boot-frozen [`SlotLayout`] from it FIRST (2SIOHJ — the ONE boot
    /// ordering authority: every hosted range non-empty, the merge sidecar
    /// pinned to the LAST index, solver seats == LPT bins), then build the
    /// slot table / per-role queues / census rows FROM that layout, pin
    /// the merge sidecar (T1→T2→T4, exactly one), and self-register every
    /// hosted role into the worker census (§7).
    ///
    /// # Errors
    /// [`BudgetError`] fail-fasts on over-subscription / pinned-role floor;
    /// [`BootError::Invariant`] on a dead hosted station or a broken
    /// layout invariant.
    pub fn boot(boot: FleetBoot) -> Result<Self, BootError> {
        // FF-T2 (MEBF4V): the PLAN is the first boot step — the tiered
        // host authority (LW-T4's one floor generalized into ordered tiers).
        // ONE boot log line names it (id + binding + budget); the serial
        // tier refuses with its own typed refusal until the arm lands
        // (FF-T4) — never a silent narrow.
        let plan = crate::plan::plan(boot.quota_cpus, boot.profile, &boot.overrides)?;
        op_info!(domain = pump, plan = plan.id,
            binding = %plan.binding,
            budget_cpus = plan.budget_cpus,
            oversubscribed = plan.oversubscribed,
            "boot plan resolved"
        );
        // FF-T4 (Z6XTDX): BOTH bindings boot — the projection is
        // binding-derived (pinned: the floor-checked budget; serial: the
        // one-solver-seat tier) and the executors' binding seam
        // instantiates the seat model over the SAME slot FSM.
        let budget = plan.projected_budget(&boot.overrides)?;
        // ONE process-level fleet posture owner (JCI2FW Part A): the
        // boot's policy installs the process owner first-wins; hermetic
        // boots inject their own owner and never touch the global.
        let posture: &'static PostureOwner = boot
            .owner
            .unwrap_or_else(|| crate::posture::install_process_owner(boot.posture));
        let posture_watch = posture.subscribe();

        // THE boot ordering (2SIOHJ): the SlotLayout is derived FIRST —
        // every v1-hosted range is checked non-empty (a dead station
        // refuses the boot loudly) and the merge sidecar is pinned to the
        // LAST index HERE, before anything is built.
        let layout = SlotLayout::of(&budget)?;

        // Slot cells FROM the layout: solver pins, sim slots, resolve, the
        // registration intake station (PRG-3), then the merge sidecar (its
        // dedicated slot, pinned below before anything else can claim it).
        let mut slots = Vec::with_capacity(layout.merge + 1);
        for _ in layout.solver.clone() {
            slots.push(SlotCell {
                home: WorkerRole::Solver,
                state: SlotState::Idle,
                arena: None,
            });
        }
        for _ in layout.sim.clone() {
            slots.push(SlotCell {
                home: WorkerRole::SimDriver,
                state: SlotState::Idle,
                arena: None,
            });
        }
        for _ in layout.resolve.clone() {
            slots.push(SlotCell {
                home: WorkerRole::Resolve,
                state: SlotState::Idle,
                arena: None,
            });
        }
        for _ in layout.poolupd.clone() {
            slots.push(SlotCell {
                home: WorkerRole::PoolStateUpdater,
                state: SlotState::Idle,
                arena: None,
            });
        }
        slots.push(SlotCell {
            home: WorkerRole::Merge,
            state: SlotState::Idle,
            arena: None,
        });

        // Per-role queues FROM the layout: pre-sized by the same bound rule
        // `queue_cap` states over the LIVE budget (at boot the live budget
        // IS the layout's budget) — capacity only, never a second bound.
        let mut queues: [VecDeque<Unit>; 8] = Default::default();
        for role in ALL_ROLES {
            if let Some(idx) = role.index_in_all_roles().map(usize::from) {
                if let Some(queue) = queues.get_mut(idx) {
                    *queue = VecDeque::with_capacity(queue_cap_for(role, layout.seats(role)));
                }
            }
        }

        let mut host = Self {
            budget,
            plan,
            layout,
            posture,
            posture_watch,
            slots,
            queues,
            epoch_boundary: false,
            next_arena: 1,
            overflow_count: 0,
            tripwire: Arc::new(loud_abort),
        };

        host.register_census();

        // The merge pin: exactly one, pinned at boot (T4) — per-path sends
        // land in a pipe somebody drinks from. The slot is the layout's
        // merge index — the LAST table slot (2SIOHJ).
        let merge_slot = host.merge_slot_id();
        let merge_claim = || Unit::noop(0, WorkerRole::Merge, Some(MERGE_PIN_KEY));
        host.lease_claim(merge_slot, WorkerRole::Merge, Some(MERGE_PIN_KEY))
            .map_err(|_| BootError::Invariant("the fresh merge slot rejected its claim (T1)"))?;
        host.start(merge_slot, &merge_claim())
            .map_err(|_| BootError::Invariant("the merge claim start (T2) failed"))?;
        host.complete(merge_slot)
            .map_err(|_| BootError::Invariant("the merge pin conversion (T4) failed"))?;
        Ok(host)
    }

    /// The boot plan (FF-T2): the tiered authority that resolved this
    /// boot — id, binding, the oversubscription mark, and the detected
    /// budget. Boot-frozen; `runtime_status` and the census read it.
    #[must_use]
    pub fn plan(&self) -> &crate::plan::FleetPlan {
        &self.plan
    }

    /// The merge sidecar's slot: the boot-frozen [`SlotLayout`] merge
    /// index — structurally the LAST slot of the boot table (the pin
    /// itself is claimed T1→T2→T4 immediately after boot construction —
    /// that conversion is the standing proof of the cell's state). 2SIOHJ:
    /// the layout owns this read; no re-derivation from the table length.
    fn merge_slot_id(&self) -> SlotId {
        u64::try_from(self.layout().merge).unwrap_or(SlotId::MAX)
    }

    fn register_census(&self) {
        use degenbot_core::worker_census::{register, WorkerCensusEntry};
        for role in V1_ACTIVE_ROLES {
            register(WorkerCensusEntry {
                resource: role.census_resource(),
                kind: role.census_kind(),
                count: self.role_slot_budget(role),
                thread_name: role.thread_name(),
                sizing: role.census_sizing(),
                // FF-T2: how the row's work binds to host threads — the
                // fleet roles are the pinned binding's dedicated seats
                // (the serial binding maps them onto shared threads as
                // logical lanes when it lands, FF-T4).
                binding: self.plan.binding.label(),
            });
        }
    }

    /// The per-role slot budget, read FROM the boot-frozen [`SlotLayout`]
    /// (2SIOHJ) — the census rows are layout-built and byte-identical to
    /// the old budget reads (the merge sidecar is the single `merge`
    /// seat, which the derive fixes at one `merge_cpus`).
    fn role_slot_budget(&self, role: WorkerRole) -> usize {
        self.layout.seats(role)
    }

    // ---- observation surface --------------------------------------------------

    /// The derived budget table (re-derived only via `resize_quota`).
    #[must_use]
    pub const fn budget(&self) -> &FleetBudget {
        &self.budget
    }

    /// The boot-frozen slot table geometry (2SIOHJ): derived once at boot
    /// and never re-derived by [`FleetHost::resize_quota`].
    #[must_use]
    pub(crate) fn layout(&self) -> SlotLayout {
        self.layout.clone()
    }

    /// Current posture (read through the shared owner).
    #[must_use]
    pub fn posture(&self) -> FleetPosture {
        self.posture.current()
    }

    /// Whether the shared posture admits lease intake for `role` right
    /// now (the same `admits_lease` predicate the enqueue gate and the
    /// T-table ctx consult — one source of truth, no hand mirrors).
    #[must_use]
    pub fn posture_admits_role(&self, role: WorkerRole) -> bool {
        self.posture.admits_lease(role.cordon_class())
    }

    /// The sticky lane-death hold (read-through). The Faulted transition
    /// (TB4QGX T6) keys on THIS typed latch — never on elapsed cordon time.
    #[must_use]
    pub fn lane_death_held(&self) -> bool {
        self.posture.lane_death_held()
    }

    /// The sim intake cap in the current posture (read through the shared
    /// owner — cordon floors it per §6 effect (b)).
    #[must_use]
    pub fn sim_intake_cap(&self, slot_cap: usize) -> usize {
        self.posture.sim_intake_cap(slot_cap)
    }

    /// The full slot state map.
    #[must_use]
    pub fn slot_states(&self) -> Vec<(SlotId, SlotState)> {
        self.slots
            .iter()
            .enumerate()
            .map(|(i, cell)| (u64::try_from(i).unwrap_or(SlotId::MAX), cell.state))
            .collect()
    }

    /// One slot's state (`None` = unknown slot id).
    #[must_use]
    pub fn slot_state(&self, slot: SlotId) -> Option<SlotState> {
        let idx = usize::try_from(slot).ok()?;
        self.slots.get(idx).map(|c| c.state)
    }

    /// The slot's warm arena token: identity is STABLE across cycles while
    /// the pin lives; `None` once released via T9 (§3.4).
    #[must_use]
    pub fn arena(&self, slot: SlotId) -> Option<ArenaToken> {
        let idx = usize::try_from(slot).ok()?;
        self.slots.get(idx).and_then(|c| c.arena)
    }

    /// Mint (idempotently) the slot's warm arena at the T2 grant seam
    /// (LW-T2): a lane ctx handed to a unit at grant time must ALWAYS
    /// carry the warm identity — minted on the first pin claim, stable
    /// across cycles, released at T9 (never live across a role switch).
    /// `None` for unknown slots.
    #[must_use]
    pub fn ensure_arena(&mut self, slot: SlotId) -> Option<ArenaToken> {
        let idx = usize::try_from(slot).ok()?;
        let cell = self.slots.get_mut(idx)?;
        if cell.arena.is_none() {
            // First pin claim: mint on the spot — NOT deferred to the first
            // completion — so a lane ctx handed at grant time always carries
            // the warm identity (still released at T9).
            cell.arena = Some(ArenaToken(self.next_arena));
            self.next_arena += 1;
        }
        cell.arena
    }

    /// Slot id of the (unique) merge pin: the boot-frozen layout's LAST
    /// index (boot pins it T1→T2→T4 before anything else can claim it).
    /// Post-boot this is infallible — the table was built FROM this same
    /// layout — so the `checked_sub`/`Option` dance is gone (2SIOHJ) and
    /// callers read a plain `SlotId`.
    #[must_use]
    pub fn merge_slot(&self) -> SlotId {
        self.merge_slot_id()
    }

    /// Slot pinned for `key`, if any — DERIVED over the slot table's
    /// `Pinned` cells (the one pin representation; no mirror).
    #[must_use]
    pub fn pin_slot(&self, key: PinKey) -> Option<SlotId> {
        pinned_slots(&self.slots)
            .find(|(k, _)| *k == key)
            .map(|(_, s)| s)
    }

    /// Queue length for a role (declared roles hold nothing).
    #[must_use]
    pub fn queue_len(&self, role: WorkerRole) -> usize {
        role.index_in_all_roles()
            .map(usize::from)
            .and_then(|i| self.queues.get(i))
            .map_or(0, VecDeque::len)
    }

    /// Loud-overflow counter (metric export surface for the ADR-021 posture).
    #[must_use]
    pub const fn overflow_count(&self) -> u64 {
        self.overflow_count
    }

    /// The per-role gauge table (busy/idle per role).
    #[must_use]
    pub fn role_gauges(&self) -> Vec<RoleGaugeSample> {
        self.gauge_rows()
    }

    // ---- posture feed -----------------------------------------------------------

    /// Feed a throttle delta to the SHARED posture owner (JCI2FW Part A —
    /// the host owns no machine anymore). A transition INTO Cordoned
    /// immediately sheds cordon-deferrable in-flight units to Draining
    /// (T7) — they always complete (T8); pinned walks and the merge pin
    /// are never shed. The shed is driven by the owner's transition feed
    /// (the boot-time [`PostureWatch` subscription]), so a transition
    /// published by ANY feeder (this host, the process throttle feed, a
    /// future retune) sheds this host promptly.
    pub fn observe_throttle(&mut self, now_ms: u64, sample: ThrottleSample) -> PostureChange {
        let change = self.posture.observe_throttle(now_ms, sample);
        self.shed_if_cordoned();
        change
    }

    /// The T7 shed trigger, driven by the shared owner: drain the watch
    /// (a transition INTO Cordoned) OR honor a Cordoned snapshot at a
    /// grant-pass boundary (check-before-each-grant — covers transitions
    /// fed by other hosts / the process feed between this host's own
    /// feeds). Idempotent: Draining/pinned/merge units are never touched,
    /// and the scan is a no-op while Nominal.
    fn shed_if_cordoned(&mut self) {
        let posture = self
            .posture_watch
            .take_if_changed()
            .unwrap_or_else(|| self.posture.current());
        if matches!(posture, FleetPosture::Cordoned) {
            for slot in 0..self.slots.len() {
                let slot = u64::try_from(slot).unwrap_or(SlotId::MAX);
                let Some(state) = self.slot_state(slot) else {
                    continue;
                };
                let Some(role) = state.role() else { continue };
                let sheddable =
                    matches!(state, SlotState::Running { .. } | SlotState::Leased { .. })
                        && role.cordon_class() == CordonClass::Deferrable;
                if sheddable {
                    let _ = self.apply_transition(slot, Transition::BeginDraining);
                }
            }
        }
    }

    // ---- epoch / quota lifecycle -----------------------------------------------

    /// Begin an epoch boundary: pin release (T9) is ONLY legal between this
    /// and [`FleetHost::end_epoch`] — quota re-detection and config
    /// overrides drive it, never a mid-cycle event (design doc §3.3 T9).
    pub fn begin_epoch(&mut self) {
        self.epoch_boundary = true;
    }

    /// End the epoch boundary.
    pub fn end_epoch(&mut self) {
        self.epoch_boundary = false;
    }

    /// Release the pin on `key` (T9, epoch boundary only) and drop its warm
    /// arena — an arena is never live across a role switch (§3.4).
    ///
    /// # Errors
    /// [`HostError::Transition`] (the FSM's `MidCyclePin`) off-boundary.
    pub fn release_pin(&mut self, key: PinKey) -> Result<SlotId, HostError> {
        // Derived find over the renderer (DNZQ5G): the pin lives in the
        // cell; no mirror to retain-clear and no merge_pin to reset.
        let slot = self
            .pin_slot(key)
            .ok_or(HostError::UnknownSlot(SlotId::MAX))?;
        self.apply_transition(slot, Transition::ReleasePin)?;
        if let Some(cell) = usize::try_from(slot)
            .ok()
            .and_then(|i| self.slots.get_mut(i))
        {
            cell.arena = None;
        }
        Ok(slot)
    }

    /// Quota resize: re-derive the budget under the new quota (fail-loud).
    /// Sum is re-checked by construction; pin re-keying happens at an epoch
    /// boundary (T9) that the CALLER opens — the host never re-keys a pin
    /// mid-cycle.
    ///
    /// # Errors
    /// [`BudgetError`] if the new quota cannot host the fleet.
    pub fn resize_quota(
        &mut self,
        new_quota_cpus: f64,
        new_overrides: &BudgetOverrides,
    ) -> Result<(), BudgetError> {
        let next = self.budget.resize(new_quota_cpus, new_overrides)?;
        self.budget = next;
        // 2SIOHJ: `self.layout` stays INTENTIONALLY frozen here — the slot
        // table does not resize under a live quota (cells move only at the
        // epoch-boundary T9 re-key). The new budget drives the LIVE
        // admission arithmetic (`queue_cap`, intake caps) while the layout
        // keeps serving the boot table geometry; see the asymmetry note on
        // [`FleetHost::queue_cap`] before "unifying" the two.
        op_info!(
            domain = pump,
            quota = new_quota_cpus,
            solver_pins = self.budget.solver_pin_count,
            sim_driver_slots = self.budget.sim_slot_cap,
            "quota re-detected — shares re-declared, sum re-checked"
        );
        Ok(())
    }

    // ---- the dispatch core -------------------------------------------------------

    /// Enqueue a unit into its role's bounded queue. Loud rejections:
    /// declared-not-active roles, merge-never-queued, bounded-queue
    /// overflow (counted + `error!`, ADR-021), posture-held deferrable
    /// intake.
    ///
    /// # Errors
    /// [`EnqueueError`] — every variant is a loud refusal, never a drop.
    pub fn enqueue(&mut self, unit: Unit) -> Result<(), EnqueueError> {
        self.try_enqueue(unit).map_err(|(err, _)| err)
    }

    /// The lossless refusal seam (§10 never-drop): like [`FleetHost::
    /// enqueue`], but a refusal returns the unit BACK next to the typed
    /// error. The pooled-intake hosts need this under the SHARED posture
    /// owner (JCI2FW Part A): the owner is fed from the throttle-poller
    /// thread, so a cordon can onset between a caller's admission check
    /// and this gate — the refusing gate must not swallow the payload
    /// (the caller parks it in its unbounded backlog instead).
    ///
    /// # Errors
    /// `(EnqueueError, Unit)` — the loud refusal PLUS the unit back.
    pub fn try_enqueue(&mut self, unit: Unit) -> Result<(), (EnqueueError, Unit)> {
        if !unit.role.v1_active() {
            return Err((EnqueueError::RoleNotActive(unit.role), unit));
        }
        if unit.role == WorkerRole::Merge {
            return Err((EnqueueError::MergeNeverQueued, unit));
        }
        if unit.role.cordon_class() == CordonClass::Deferrable
            && !self.posture.admits_lease(unit.role.cordon_class())
        {
            self.posture.note_intake_suppressed();
            return Err((EnqueueError::PostureHeld(unit.role), unit));
        }
        let cap = self.queue_cap(unit.role);
        let len = self
            .role_queue_mut(unit.role)
            .as_deref()
            .map_or(0, VecDeque::len);
        if len >= cap {
            self.overflow_count += 1;
            op_error!(
                domain = pump,
                role = unit.role.label(),
                len,
                cap,
                overflows = self.overflow_count,
                "queue FULL — loud overflow (ADR-021: classify, stop, never silently drop)"
            );
            return Err((
                EnqueueError::QueueFull {
                    role: unit.role,
                    len,
                    cap,
                },
                unit,
            ));
        }
        if let Some(queue) = self.role_queue_mut(unit.role) {
            queue.push_back(unit);
        }
        Ok(())
    }

    fn role_queue_mut(&mut self, role: WorkerRole) -> Option<&mut VecDeque<Unit>> {
        let idx = usize::from(role.index_in_all_roles()?);
        self.queues.get_mut(idx)
    }

    fn role_queue(&self, role: WorkerRole) -> Option<&VecDeque<Unit>> {
        let idx = usize::from(role.index_in_all_roles()?);
        self.queues.get(idx)
    }

    fn take_from_role(&mut self, role: WorkerRole) -> Option<Unit> {
        self.role_queue_mut(role)?.pop_front()
    }

    /// Drain a role's queued (not-yet-granted) units, returning how many
    /// were removed. The Faulted arm (TB4QGX T6) resolves them terminally;
    /// granted in-flight units are untouched (they complete naturally).
    pub fn drain_role_queue(&mut self, role: WorkerRole) -> usize {
        self.role_queue_mut(role).map_or(0, |queue| {
            let drained = queue.len();
            queue.clear();
            drained
        })
    }

    fn return_unit(&mut self, unit: Unit) {
        if let Some(queue) = self.role_queue_mut(unit.role) {
            queue.push_front(unit);
        }
    }

    /// The per-role queue bound: 2× the role's slot budget (pipelining
    /// depth); Merge is unbounded-by-type because it is never queued.
    ///
    /// RESIZE-QUOTA ASYMMETRY (2SIOHJ — read before "fixing" this to read
    /// `self.layout`): this bound INTENTIONALLY reads the LIVE budget,
    /// not the boot-frozen [`SlotLayout`]. [`FleetHost::resize_quota`]
    /// re-declares the budget under a new quota while the slot table (and
    /// its layout) stays boot-frozen — cells move only at an epoch
    /// boundary's T9 re-key — so a layout-sourced bound would keep
    /// enforcing the BOOT quota's queue depth after a resize while
    /// admission must follow the LIVE shares. The split is the design:
    /// layout = table geometry (seat identity, lease targets), live
    /// budget = admission arithmetic (queue bounds, intake caps, the
    /// solver admission share).
    #[must_use]
    pub fn queue_cap(&self, role: WorkerRole) -> usize {
        let seats = match role {
            WorkerRole::Solver => self.budget.solver_pin_count,
            WorkerRole::SimDriver => self.budget.sim_slot_cap,
            WorkerRole::Resolve => usize::try_from(self.budget.resolve_cpus).unwrap_or(1),
            WorkerRole::PoolStateUpdater => self.budget.pool_state_updater_slots,
            _ => 0,
        };
        queue_cap_for(role, seats)
    }

    /// The one precedence grant loop (design doc §4). Returns granted
    /// (grant, unit) pairs; the caller executes the unit's Rust closure on
    /// its own worker, then reports via [`FleetHost::start`] / [`FleetHost::complete`]
    /// / [`FleetHost::shed`]. A pinned continuation's `start` applies T6.
    #[must_use]
    pub fn dispatch(&mut self) -> Vec<(Grant, Unit)> {
        // Check-before-each-grant: drain the shared owner's transition
        // feed (shed if the fleet is Cordoned) BEFORE granting — a cordon
        // that onsets between this host's feeds still holds intake (the
        // gate below reads the owner live) and sheds deferrable
        // in-flight units on this pass (T7).
        self.shed_if_cordoned();
        let mut grants = Vec::new();

        // 1. Pinned continuations (T6): cycle-critical, keyed to their pin.
        //    Iterate the DERIVED pin table (slot-index order, DNZQ5G): no
        //    mirror and no per-pass allocation — the old `self.pins.clone()`
        //    heap copy is gone; the renderer reads the cells the FSM wrote,
        //    and nothing below mutates slot states (queue-only mutation,
        //    hence the disjoint field borrows).
        let solver_queue = WorkerRole::Solver.index_in_all_roles().map(usize::from);
        for (key, slot) in pinned_slots(&self.slots) {
            let is_solver_pin = matches!(
                self.slot_state(slot),
                Some(SlotState::Pinned {
                    role: WorkerRole::Solver,
                    ..
                })
            );
            if !is_solver_pin || self.pin_queue_len(key) == 0 {
                continue;
            }
            let Some(unit) = solver_queue
                .and_then(|idx| self.queues.get_mut(idx))
                .and_then(|q| take_solver_unit_for(q, key))
            else {
                continue;
            };
            grants.push((
                Grant {
                    slot,
                    unit: unit.id,
                    kind: GrantKind::PinContinuation,
                },
                unit,
            ));
        }

        // 2. sim-before-solve: queued sims drain before ANY new Solver
        //    queue intake, pooled slots permitting, cordon intake-cap aware.
        let sim_intake_cap = self.posture.sim_intake_cap(self.budget.sim_slot_cap);
        let mut sim_busy = self.count_leased_or_running(WorkerRole::SimDriver);
        while sim_busy < sim_intake_cap {
            let Some(idle) = self.first_idle_slot() else {
                break;
            };
            let Some(unit) = self.take_from_role(WorkerRole::SimDriver) else {
                break;
            };
            if self.lease(idle, WorkerRole::SimDriver, None).is_err() {
                self.return_unit(unit);
                break;
            }
            grants.push((
                Grant {
                    slot: idle,
                    unit: unit.id,
                    kind: GrantKind::Sim,
                },
                unit,
            ));
            sim_busy += 1;
        }

        // 3. Solver queue intake: new pin claims, admission-capped by the
        //    Solver CPU share (a gated bin parks — §5 note). Only reached
        //    after the sim queue drained: sim-before-solve at lease time.
        //    A unit whose key is HOT (pinned or in flight on its seat) is
        //    skipped here — it is granted only via T6 onto its OWN seat by
        //    the continuation lane; granting a hot key cold would seat one
        //    bin on two workers (the pin IS the key, §3.4).
        let admission_cap = usize::try_from(self.budget.solver_cpus).unwrap_or(1);
        while self.count_leased_or_running(WorkerRole::Solver) < admission_cap {
            let Some(idle) = self.first_idle_slot() else {
                break;
            };
            let Some(pos) = self.first_cold_solver_pos() else {
                break;
            };
            let Some(unit) = self
                .role_queue_mut(WorkerRole::Solver)
                .and_then(|q| q.remove(pos))
            else {
                break;
            };
            let key = unit.key;
            if self.lease(idle, WorkerRole::Solver, key).is_err() {
                self.return_unit(unit);
                break;
            }
            grants.push((
                Grant {
                    slot: idle,
                    unit: unit.id,
                    kind: GrantKind::NewPinClaim,
                },
                unit,
            ));
        }

        // 4. Resolve chunks fill the remaining pooled capacity.
        while let Some(idle) = self.first_idle_slot() {
            let Some(unit) = self.take_from_role(WorkerRole::Resolve) else {
                break;
            };
            if self.lease(idle, WorkerRole::Resolve, None).is_err() {
                self.return_unit(unit);
                break;
            }
            grants.push((
                Grant {
                    slot: idle,
                    unit: unit.id,
                    kind: GrantKind::Resolve,
                },
                unit,
            ));
        }

        // 5. The registration intake station (PRG-3): strictly BEHIND
        //    solve/sim/resolve precedence.
        self.dispatch_pool_state_updates(&mut grants);

        self.export_gauges();
        grants
    }

    /// The registration intake station's grant step (PRG-3): `PoolStateUpdater`
    /// grants run strictly BEHIND solve/sim/resolve precedence (the deferrable
    /// role drains the leftovers). Cordon is enforced at ENQUEUE time —
    /// Deferrable intake is held while cordoned, so this loop sees no queued
    /// units then; in-flight units finish normally (never cancelled, §6).
    fn dispatch_pool_state_updates(&mut self, grants: &mut Vec<(Grant, Unit)>) {
        let poolupd_cap = self.budget.pool_state_updater_slots;
        let mut poolupd_busy = self.count_leased_or_running(WorkerRole::PoolStateUpdater);
        while poolupd_busy < poolupd_cap {
            let Some(idle) = self.first_idle_slot() else {
                break;
            };
            let Some(unit) = self.take_from_role(WorkerRole::PoolStateUpdater) else {
                break;
            };
            if self
                .lease(idle, WorkerRole::PoolStateUpdater, None)
                .is_err()
            {
                self.return_unit(unit);
                break;
            }
            grants.push((
                Grant {
                    slot: idle,
                    unit: unit.id,
                    kind: GrantKind::PoolStateUpdate,
                },
                unit,
            ));
            poolupd_busy += 1;
        }
    }

    /// Position of the FIRST queued Solver unit whose key is COLD — not
    /// pinned and not in flight on its seat. Hot-keyed units wait for their
    /// own seat's T6 continuation (a hot key granted cold would seat one
    /// bin on two workers, breaking the one-seat-per-bin contract).
    fn first_cold_solver_pos(&self) -> Option<usize> {
        self.role_queue(WorkerRole::Solver)?
            .iter()
            .position(|u| !self.solver_key_is_hot(u.key))
    }

    /// Whether a keyed Solver unit currently has a claimed seat — ONE pass
    /// over the slot table (formerly a `pin_slot` probe plus a second
    /// scan): a live `Pinned` cell (the derived pin table — no mirror) or
    /// an in-flight Leased/Running unit carrying the same key on a Solver
    /// seat. Hot-keyed units wait for their own seat's T6 continuation (a
    /// hot key granted cold would seat one bin on two workers).
    fn solver_key_is_hot(&self, key: Option<PinKey>) -> bool {
        let Some(key) = key else {
            return false;
        };
        self.slots.iter().any(|c| {
            matches!(
                c.state,
                SlotState::Pinned { key: k, .. }
                | SlotState::Leased {
                    role: WorkerRole::Solver,
                    key: Some(k),
                }
                | SlotState::Running {
                    role: WorkerRole::Solver,
                    key: Some(k),
                } if k == key
            )
        })
    }

    fn pin_queue_len(&self, key: PinKey) -> usize {
        self.role_queue(WorkerRole::Solver)
            .map_or(0, |q| q.iter().filter(|u| u.key == Some(key)).count())
    }
    // (index conversions: usize::from(u8) is infallible)

    fn first_idle_slot(&self) -> Option<SlotId> {
        let pos = self.slots.iter().position(|c| c.state == SlotState::Idle)?;
        u64::try_from(pos).ok()
    }

    fn count_leased_or_running(&self, role: WorkerRole) -> usize {
        self.slots
            .iter()
            .filter(|c| {
                matches!(
                    c.state,
                    SlotState::Leased { .. } | SlotState::Running { .. }
                ) && c.state.role() == Some(role)
            })
            .count()
    }

    // ---- T-table application --------------------------------------------------------

    fn apply_transition(&mut self, slot: SlotId, t: Transition) -> Result<SlotState, HostError> {
        let idx = usize::try_from(slot).map_err(|_| HostError::UnknownSlot(slot))?;
        let from = self
            .slots
            .get(idx)
            .ok_or(HostError::UnknownSlot(slot))?
            .state;
        let ctx = TransitionContext {
            at_epoch_boundary: self.epoch_boundary,
            posture_admits_role: from
                .role()
                .is_none_or(|r| self.posture.admits_lease(r.cordon_class())),
        };
        match transition(from, t, ctx) {
            Ok(to) => {
                self.slots[idx].state = to;
                Ok(to)
            }
            Err(rejected) => {
                op_error!(domain = pump, slot,
                    from = ?rejected.from,
                    transition = ?rejected.transition,
                    reason = %rejected.reason,
                    "transition REJECTED — off the T-table"
                );
                Err(HostError::Transition(rejected))
            }
        }
    }

    fn lease(
        &mut self,
        slot: SlotId,
        role: WorkerRole,
        key: Option<PinKey>,
    ) -> Result<(), HostError> {
        self.apply_transition(slot, Transition::Lease { role, key })
            .map(|_| ())
    }

    /// Commit a unit onto a slot: T2 (Leased → Running) or T6 (Pinned →
    /// Running, key-matched at the host — the pin IS the key, dispatched
    /// only to its own slot).
    ///
    /// # Errors
    /// [`HostError::Transition`] off the table.
    pub fn start(&mut self, slot: SlotId, unit: &Unit) -> Result<(), HostError> {
        let state = self.slot_state(slot).ok_or(HostError::UnknownSlot(slot))?;
        if let SlotState::Pinned { key, .. } = state {
            if unit.key != Some(key) {
                return Err(HostError::Transition(RejectedTransition {
                    from: state,
                    transition: Transition::Start { unit: unit.id },
                    reason: RejectionReason::NoLegalRow,
                }));
            }
        }
        self.apply_transition(slot, Transition::Start { unit: unit.id })
            .map(|_| ())
    }

    /// Complete the in-flight unit: T3/T4 (pinnable roles convert to a warm
    /// pin, minting/reusing its arena), T5 (pooled roles return to idle), or
    /// T8 (a shed unit's `SeatDone` retires the drain back to idle).
    ///
    /// # Errors
    /// [`HostError::Transition`] off the table.
    pub fn complete(&mut self, slot: SlotId) -> Result<Completion, HostError> {
        let state = self.slot_state(slot).ok_or(HostError::UnknownSlot(slot))?;
        let to = match state {
            SlotState::Running {
                role: WorkerRole::SimDriver | WorkerRole::Resolve | WorkerRole::PoolStateUpdater,
                ..
            } => self.apply_transition(slot, Transition::CompleteToIdle)?,
            SlotState::Running {
                role: WorkerRole::Solver | WorkerRole::Merge,
                ..
            } => self.apply_transition(slot, Transition::CompleteToPinned)?,
            // T8: a shed in-flight unit's SeatDone lands here (the seat
            // always finishes — T7's "the unit always completes" contract);
            // the drain retires the slot to Idle. Routing it anywhere else
            // is a loud completion refusal → stranded receipt pipe abort.
            SlotState::Draining { .. } => self.apply_transition(slot, Transition::DrainComplete)?,
            _ => {
                return Err(HostError::Transition(RejectedTransition {
                    from: state,
                    transition: Transition::CompleteToIdle,
                    reason: RejectionReason::NoLegalRow,
                }));
            }
        };
        match to {
            SlotState::Idle => {
                self.export_gauges();
                Ok(Completion::BackToIdle)
            }
            SlotState::Pinned { key, .. } => {
                // The FSM's CompleteToPinned write above IS the pin
                // registration: the derived renderer reads the cell, so
                // there is no mirror to update (DNZQ5G deleted the
                // three-way pins/merge_pin bookkeeping).
                // ONE arena mint path: ensure_arena (idempotent; minted on
                // the first pin, reused warm across cycles, DETACHED(0)
                // never minted).
                let _ = self.ensure_arena(slot);
                self.export_gauges();
                Ok(Completion::Pinned { key })
            }
            _ => Err(HostError::Transition(RejectedTransition {
                from: state,
                transition: Transition::CompleteToIdle,
                reason: RejectionReason::NoLegalRow,
            })),
        }
    }

    /// Shed a running/leased unit: T7 (cordon onset / resize — the in-flight
    /// unit ALWAYS completes), then [`FleetHost::drain_done`] for T8.
    ///
    /// # Errors
    /// [`HostError::Transition`] off the table.
    pub fn shed(&mut self, slot: SlotId) -> Result<(), HostError> {
        self.apply_transition(slot, Transition::BeginDraining)
            .map(|_| ())
    }

    /// Draining done: T8 back to idle.
    ///
    /// # Errors
    /// [`HostError::Transition`] off the table.
    pub fn drain_done(&mut self, slot: SlotId) -> Result<(), HostError> {
        self.apply_transition(slot, Transition::DrainComplete)
            .map(|_| ())
    }

    /// Lease a pin claim onto a specific idle slot (Solver bins / the merge
    /// sidecar): T1 with the pinnable-key constraints.
    ///
    /// # Errors
    /// [`HostError::Transition`] off the table.
    pub fn lease_claim(
        &mut self,
        slot: SlotId,
        role: WorkerRole,
        key: Option<PinKey>,
    ) -> Result<(), HostError> {
        self.lease(slot, role, key)
    }

    /// The stranded-pipe tripwire: abandoning a unit whose results feed a
    /// pipe WITHOUT draining it first hits the loud-abort path (log at
    /// error + `std::process::abort`) — the executor discipline carried
    /// over verbatim (design doc §10.3). The harness injects an observing
    /// tripwire via [`FleetHost::with_tripwire_observer`].
    ///
    /// # Errors
    /// Always: [`HostError::StrandedPipe`] (the default handler aborts
    /// before returning).
    pub fn strand_unit(&mut self, slot: SlotId) -> Result<(), HostError> {
        op_error!(domain = pump, slot,
            "stranded result pipe: a dead host abandons a unit with in-flight result sends — loud abort (design doc §10.3)"
        );
        (self.tripwire)("stranded result pipe: unit abandoned mid-drain");
        Err(HostError::StrandedPipe(slot))
    }

    /// Install a tripwire observer (the default is the loud abort; test-
    /// declared only, mirroring the conformance-stub pattern).
    #[must_use]
    pub fn with_tripwire_observer(mut self, tripwire: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        self.tripwire = tripwire;
        self
    }

    // ---- gauge export -------------------------------------------------------------

    fn gauge_rows(&self) -> Vec<RoleGaugeSample> {
        let (mut idle, mut leased, mut running, mut pinned, mut draining) =
            ([0_u64; 8], [0_u64; 8], [0_u64; 8], [0_u64; 8], [0_u64; 8]);
        for cell in &self.slots {
            let idx = cell.home.index_in_all_roles().map_or(0, usize::from);
            match cell.state {
                SlotState::Idle => idle[idx] += 1,
                SlotState::Leased { .. } => leased[idx] += 1,
                SlotState::Running { .. } => running[idx] += 1,
                SlotState::Pinned { .. } => pinned[idx] += 1,
                SlotState::Draining { .. } => draining[idx] += 1,
            }
        }
        gauges_mod::sample_table(&idle, &leased, &running, &pinned, &draining)
    }

    fn export_gauges(&self) {
        gauges_mod::export_dashboard(&self.gauge_rows());
    }
}

/// The default loud-abort tripwire (executor discipline, §10.3): the error
/// is logged by [`FleetHost::strand_unit`], then the process aborts —
/// swallowing the error is never an option.
fn loud_abort(_reason: &str) {
    std::process::abort();
}

// ---------------------------------------------------------------------------
// Panic-verdict policy (QR3NUS, Seam D): a unit panic whose results feed a
// pipe is expressible as DATA — a policy object decides between converting
// the panic to typed per-path failure records (the seat survives, decision
// A) and the loud structural abort. The unit runner consults the verdict;
// per-path outcome synthesis lives with the caller that owns the pid list
// (the arb_engine solve-lane adapter).
// ---------------------------------------------------------------------------

/// What a [`PanicVerdict`] prescribes when a submitted unit panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanicAction {
    /// Convert the panic to typed per-path failure records; the seat keeps
    /// taking its pinned units (QR3NUS decision A).
    RecordAndContinue,
    /// Loud structural abort (strict ADR-021 posture). Reserved for
    /// dev/prod wiring that demands hard failure — never installed under
    /// test.
    Abort,
}

/// Policy object consulted when a submitted unit panics with
/// `result_pipe: true` (abandoning its results would strand the pipe,
/// design doc §10).
pub trait PanicVerdict: Send + Sync + 'static {
    /// Prescribe the action for a panicking unit. The payload names the
    /// unit and the seat that was executing it.
    #[must_use]
    fn on_unit_panic(&self, unit: u64, seat: u64) -> PanicAction;
}

/// Seat-survives policy (QR3NUS decision A): the panic becomes typed
/// failure records on the result pipe, and the seat keeps serving its pin.
pub struct SeatSurvivesPolicy;

impl PanicVerdict for SeatSurvivesPolicy {
    fn on_unit_panic(&self, _unit: u64, _seat: u64) -> PanicAction {
        PanicAction::RecordAndContinue
    }
}

/// Strict abort policy (ADR-021 posture): unit panics abort the process.
/// Dev/prod wiring only — never installed under test.
pub struct AbortingPolicy;

impl PanicVerdict for AbortingPolicy {
    fn on_unit_panic(&self, _unit: u64, _seat: u64) -> PanicAction {
        PanicAction::Abort
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod harness;
