//! The `Nominal ⇄ Cordoned` posture FSM (design doc §6) — the first
//! consumer of `degenbot.cgroup.throttled` (`cpu_budget::cgroup_throttle_delta`).
//!
//! Thresholds are typed, runtime-tunable config keys (the sign-off
//! amendment 2026-09-09: enter triggers, exit window, and cordon effects
//! calibrated from soak data via the operator channel — never share
//! arithmetic, which is [`crate::budget`]'s authority).
//!
//! Entering/exiting is LOUD: a transition fires a structured log line plus
//! the posture counters (never a silent degrade). Time is passed in as
//! monotonic milliseconds so the FSM is deterministic under test; callers
//! feed `Instant::now()` deltas from their throttle poller.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use degenbot_config::FleetConfig;
use parking_lot::{Mutex, RwLock};

use crate::role::CordonClass;

/// Process-level fleet posture. NOT a slot state (design doc §3.2): it
/// gates lease transitions, it never sheds a running unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FleetPosture {
    /// Normal operation: every dispatchable role leases freely.
    Nominal,
    /// Under cgroup throttling: no new leases for cordon-deferrable roles,
    /// sim intake floored; in-flight units COMPLETE (T7/T8), pins are never
    /// shed, the merge pin and ambient I/O are never cordoned.
    Cordoned,
}

/// Why the posture entered cordon (exported as the transition's cause).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EnterReason {
    /// ≥ `enter_events` throttle events within the rolling enter window.
    EventBurst {
        /// The events observed in the window.
        events: u64,
    },
    /// Throttled-time duty exceeded `duty_percent` over the duty window.
    DutySpike {
        /// Measured duty percent.
        duty_percent: f64,
    },
    /// A lane died mid-flight (FF-T4, Z6XTDX — the DECIDED option (a)
    /// input). Entered with its OWN exit discipline: see
    /// [`PostureCause::LaneDeath`].
    LaneDeath,
}

/// A typed non-throttle posture cause (FF-T4, Z6XTDX — the DECIDED
/// option (a): the input is typed AT the posture owner, not a sample
/// it has to infer from; the failure taxonomy stays CLOSED per
/// ADR-040's per-bucket reactions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostureCause {
    /// A lane died mid-flight: enter the cordon IMMEDIATELY (no sample
    /// hysteresis — the fleet is degraded NOW) with its OWN exit
    /// discipline: the clean window NEVER lifts a lane-death cordon
    /// (the lane is still dead) — the cordon is STICKY until a fresh
    /// process. In-flight paths get terminal receipts; the process
    /// stays alive (the cordoned posture holds deferrable intake and
    /// floors sim intake — the §6 cordon effects).
    LaneDeath,
}

/// The posture change a [`PostureStateMachine::observe`] tick produced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PostureChange {
    /// No transition (the FSM does not re-enter while already cordoned).
    Held,
    /// Nominal → Cordoned (loud: log at warn + counter).
    Entered(EnterReason),
    /// Cordoned → Nominal after the clean-window hysteresis (log at info).
    Exited,
}

/// Typed thresholds (Q5 amendment) — [`PosturePolicy::from_config`] is the
/// boot source; the operator channel re-derives from the same schema keys.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PosturePolicy {
    /// Enter trigger (a): >= this many throttle events in the enter window.
    pub enter_events: usize,
    /// Enter window (a): the rolling burst window.
    pub enter_window_ms: u64,
    /// Enter trigger (b): throttled-time duty percent over the duty window
    /// (`2.0` = >2%).
    pub duty_percent: f64,
    /// Enter trigger (b) window.
    pub duty_window_ms: u64,
    /// Exit: this much continuous clean time required after the last dirty
    /// sample (hysteresis prevents flapping; §6: 10 s).
    pub exit_clean_ms: u64,
    /// Cordon effect (b): sim intake cap while cordoned; `None` = half the
    /// slot cap (in-flight sims are never cancelled).
    pub sim_intake_floor_override: Option<usize>,
}

impl PosturePolicy {
    /// The design-doc §6 defaults (2 events / 1 s, >2% / 5 s, 10 s clean),
    /// used when a host boots without a typed config (hermetic runs).
    #[must_use]
    pub const fn doc_defaults() -> Self {
        Self {
            enter_events: 2,
            enter_window_ms: 1_000,
            duty_percent: 2.0,
            duty_window_ms: 5_000,
            exit_clean_ms: 10_000,
            sim_intake_floor_override: None,
        }
    }

    /// The typed-config projection — every threshold is a schema key.
    #[must_use]
    pub fn from_config(cfg: &FleetConfig) -> Self {
        Self {
            enter_events: cfg.cordon_enter_events,
            enter_window_ms: cfg.cordon_enter_window_ms,
            duty_percent: cfg.cordon_duty_percent,
            duty_window_ms: cfg.cordon_duty_window_ms,
            exit_clean_ms: cfg.cordon_exit_clean_ms,
            sim_intake_floor_override: cfg.cordon_sim_intake_floor,
        }
    }

    /// The cordon sim-intake cap: override or half the slot cap, floored
    /// at 1, never above the cap (§6 effect (b)).
    #[must_use]
    pub fn sim_intake_cap(&self, slot_cap: usize) -> usize {
        self.sim_intake_floor_override
            .unwrap_or(slot_cap / 2)
            .min(slot_cap)
            .max(1)
    }

    /// Apply a validated [`PosturePolicyPatch`] to `self`, producing the
    /// effective policy (the JCI2FW Part B re-tune channel's only write
    /// path: current policy + supplied fields). Pure — the caller feeds the
    /// result to [`PostureOwner::retune`]. Call [`PosturePolicyPatch::validate`]
    /// FIRST; this projection never checks semantics.
    #[must_use]
    pub fn patched_with(self, patch: PosturePolicyPatch) -> Self {
        Self {
            enter_events: patch.enter_events.unwrap_or(self.enter_events),
            enter_window_ms: patch.enter_window_ms.unwrap_or(self.enter_window_ms),
            duty_percent: patch.duty_percent.unwrap_or(self.duty_percent),
            duty_window_ms: patch.duty_window_ms.unwrap_or(self.duty_window_ms),
            exit_clean_ms: patch.exit_clean_ms.unwrap_or(self.exit_clean_ms),
            sim_intake_floor_override: match patch.sim_intake_floor_override {
                // Key absent: keep the current override.
                None => self.sim_intake_floor_override,
                // Key present: set it — `Some(v)` = explicit floor,
                // `None` = cleared (back to half the slot cap).
                Some(floor) => floor,
            },
        }
    }
}

/// A partial re-tune request over the six typed thresholds (JCI2FW Part B,
/// the operator channel's wire shape): every field is `None` = "key not
/// supplied — keep the current value". `sim_intake_floor_override` is
/// doubly-`Option`: the OUTER `None` is key-absent, and the inner
/// `Some(None)` is the operator supplying the key's `None` value (clear the
/// override, back to half the slot cap — the typed key itself is
/// `opt usize`).
///
/// The semantic rules (windows > 0, duty percent in range, floors >= 1,
/// non-empty patch) are encoded ONCE, in [`Self::validate`] — callers
/// REJECT, never clamp silently.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PosturePolicyPatch {
    /// `cordon_enter_events`: the burst trigger count.
    pub enter_events: Option<usize>,
    /// `cordon_enter_window_ms`: the burst window.
    pub enter_window_ms: Option<u64>,
    /// `cordon_duty_percent`: the duty trigger percent.
    pub duty_percent: Option<f64>,
    /// `cordon_duty_window_ms`: the duty window.
    pub duty_window_ms: Option<u64>,
    /// `cordon_exit_clean_ms`: the exit hysteresis.
    pub exit_clean_ms: Option<u64>,
    /// `cordon_sim_intake_floor`: outer `None` = key absent; inner
    /// `Some(None)` = clear the override; `Some(Some(n))` = explicit floor.
    pub sim_intake_floor_override: Option<Option<usize>>,
}

impl PosturePolicyPatch {
    /// Whether NO key was supplied (an empty patch — rejected by
    /// [`Self::validate`]; a re-tune that changes nothing must never look
    /// like a successful one).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.enter_events.is_none()
            && self.enter_window_ms.is_none()
            && self.duty_percent.is_none()
            && self.duty_window_ms.is_none()
            && self.exit_clean_ms.is_none()
            && self.sim_intake_floor_override.is_none()
    }

    /// The ONE encoding of the re-tune's semantic rules. Every violation is
    /// a typed [`PostureRetuneError`] naming the offending key and value —
    /// the channel rejects, it never clamps.
    ///
    /// # Errors
    ///
    /// [`PostureRetuneError::EmptyPatch`] when no key was supplied, or the
    /// per-key range error for the first offending value.
    pub fn validate(&self) -> Result<(), PostureRetuneError> {
        if self.is_empty() {
            return Err(PostureRetuneError::EmptyPatch);
        }
        if self.enter_events.is_some_and(|v| v < 1) {
            return Err(PostureRetuneError::EnterEvents(
                self.enter_events.unwrap_or_default(),
            ));
        }
        if self.enter_window_ms.is_some_and(|v| v == 0) {
            return Err(PostureRetuneError::EnterWindow(0));
        }
        if let Some(duty) = self.duty_percent {
            // The sane inclusive range: a measured duty percent is a
            // (0.0, 100.0] quantity — 0 would cordon on ANY throttled
            // microsecond and >100 or non-finite cannot be a duty.
            if !(duty > 0.0 && duty <= 100.0) {
                return Err(PostureRetuneError::DutyPercent(duty));
            }
        }
        if self.duty_window_ms.is_some_and(|v| v == 0) {
            return Err(PostureRetuneError::DutyWindow(0));
        }
        if self.exit_clean_ms.is_some_and(|v| v == 0) {
            return Err(PostureRetuneError::ExitClean(0));
        }
        if let Some(Some(floor)) = self.sim_intake_floor_override {
            if floor < 1 {
                return Err(PostureRetuneError::SimIntakeFloor(floor));
            }
        }
        Ok(())
    }
}

/// Why the re-tune channel refused a [`PosturePolicyPatch`] (JCI2FW Part
/// B). The rules live once, in [`PosturePolicyPatch::validate`]; the wire
/// layer maps these to its typed channel error verbatim.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum PostureRetuneError {
    /// No threshold key was supplied — an empty patch is refused.
    #[error("at least one cordon threshold key is required (empty patch)")]
    EmptyPatch,
    /// `cordon_enter_events` below the floor of 1.
    #[error("cordon_enter_events must be >= 1, got {0}")]
    EnterEvents(usize),
    /// `cordon_enter_window_ms` must be a positive window.
    #[error("cordon_enter_window_ms must be > 0 ms, got {0} ms")]
    EnterWindow(u64),
    /// `cordon_duty_percent` outside the sane (0.0, 100.0] range.
    #[error("cordon_duty_percent must be in (0.0, 100.0], got {0}")]
    DutyPercent(f64),
    /// `cordon_duty_window_ms` must be a positive window.
    #[error("cordon_duty_window_ms must be > 0 ms, got {0} ms")]
    DutyWindow(u64),
    /// `cordon_exit_clean_ms` must be a positive window.
    #[error("cordon_exit_clean_ms must be > 0 ms, got {0} ms")]
    ExitClean(u64),
    /// `cordon_sim_intake_floor` below the floor of 1.
    #[error("cordon_sim_intake_floor must be >= 1, got {0}")]
    SimIntakeFloor(usize),
}

/// One throttle-poll delta: `cgroup_throttle_delta()`'s counters plus the
/// elapsed wall time of the poll interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleSample {
    /// `nr_throttled` delta since the last sample.
    pub events: u64,
    /// `throttled_usec` delta since the last sample.
    pub throttled_usec: u64,
    /// Elapsed wall time (µs) of the poll interval backing the deltas.
    pub elapsed_usec: u64,
}

impl ThrottleSample {
    /// A clean sample: no throttle events, no throttled time.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.events == 0 && self.throttled_usec == 0
    }
}

/// Loud-transition counters (exported with the posture metrics so
/// thresholds are tuned against measurements, §6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PostureCounters {
    /// Cordons entered.
    pub entered: u64,
    /// Cordons exited.
    pub exited: u64,
    /// Lease grants denied while cordoned (deferrable intake held + sim
    /// intake suppression above the floor).
    pub intake_suppressed: u64,
    /// Lane-death causes observed (FF-T4 — the sticky cordons, incl.
    /// upgrades of an existing throttle cordon).
    pub lane_deaths: u64,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    now_ms: u64,
    sample: ThrottleSample,
}

/// The posture state machine. Deterministic: time is monotonic ms fed by
/// the caller.
#[derive(Debug)]
pub struct PostureStateMachine {
    state: FleetPosture,
    policy: PosturePolicy,
    samples: VecDeque<Sample>,
    last_unclean_ms: Option<u64>,
    counters: PostureCounters,
    /// A lane-death cordon is STICKY (FF-T4): once set, the clean-window
    /// exit refuses to lift the cordon — the lane is still dead. Only
    /// a fresh process clears it (the operator restart path); there is
    /// deliberately no in-process clear API (a sticky cordon that
    /// silently un-sticks is exactly the silent-narrow class §10
    /// forbids).
    lane_death_hold: bool,
}

impl PostureStateMachine {
    /// A fresh machine in [`FleetPosture::Nominal`].
    #[must_use]
    pub fn new(policy: PosturePolicy) -> Self {
        Self {
            state: FleetPosture::Nominal,
            policy,
            samples: VecDeque::new(),
            last_unclean_ms: None,
            counters: PostureCounters::default(),
            lane_death_hold: false,
        }
    }

    /// Current posture.
    #[must_use]
    pub const fn state(&self) -> FleetPosture {
        self.state
    }

    /// Loud-transition counters (metrics export surface).
    #[must_use]
    pub const fn counters(&self) -> &PostureCounters {
        &self.counters
    }

    /// The active policy (operator-channel re-tune reads it through here).
    #[must_use]
    pub const fn policy(&self) -> &PosturePolicy {
        &self.policy
    }

    /// Swap the policy (the operator channel's re-tune). Pure: the state
    /// and the trailing sample window are KEPT — a retune never fabricates
    /// samples, so the next [`Self::observe`] re-derives the posture under
    /// the new thresholds. Semantic validation of the new policy is the
    /// re-tune caller's job (the JCI2FW Part B channel).
    pub fn set_policy(&mut self, policy: PosturePolicy) {
        self.policy = policy;
    }

    /// Feed one throttle-poll delta at `now_ms`. Returns whether the
    /// posture transitioned (and why) — the caller surfaces that loudly.
    pub fn observe(&mut self, now_ms: u64, sample: ThrottleSample) -> PostureChange {
        self.samples.push_back(Sample { now_ms, sample });
        self.prune(now_ms);
        if !sample.is_clean() {
            self.last_unclean_ms = Some(now_ms);
        }

        match self.state {
            FleetPosture::Nominal => self.maybe_enter(now_ms),
            FleetPosture::Cordoned => self.maybe_exit(now_ms),
        }
    }

    /// Feed one typed non-throttle cause (FF-T4, Z6XTDX — the DECIDED
    /// option (a) input). A [`PostureCause::LaneDeath`] enters the
    /// cordon from ANY state with its OWN exit discipline: the cordon
    /// is sticky (the clean window never lifts it — see
    /// [`Self::maybe_exit`]), in-flight paths get terminal receipts at
    /// the detection site, and the process stays alive under the §6
    /// cordon effects. Idempotent while the hold is already set (the
    /// counter still counts every cause).
    pub fn observe_cause(&mut self, cause: PostureCause) -> PostureChange {
        match cause {
            PostureCause::LaneDeath => {
                self.counters.lane_deaths += 1;
                if self.lane_death_hold {
                    return PostureChange::Held;
                }
                self.lane_death_hold = true;
                if self.state == FleetPosture::Cordoned {
                    // An existing (throttle) cordon UPGRADES to the
                    // sticky hold: no state transition, but the cause
                    // is new and loud.
                    tracing::warn!(
                        target: "degenbot::fleet",
                        lane_deaths = self.counters.lane_deaths,
                        "[fleet-posture] lane-death HOLD upgrades an existing cordon — sticky, clean-window exit disabled"
                    );
                    return PostureChange::Held;
                }
                self.enter(EnterReason::LaneDeath)
            }
        }
    }

    /// A lease grant was denied because of the posture (counter only — the
    /// dispatcher supplies the typed error).
    pub const fn note_intake_suppressed(&mut self) {
        self.counters.intake_suppressed += 1;
    }

    /// Whether the posture admits new lease intake for `class` right now
    /// (§6: `Never` and `SimPool` lease freely — sim is only intake-FLOORED;
    /// `Deferrable` is held while cordoned).
    #[must_use]
    pub const fn admits_lease(&self, class: CordonClass) -> bool {
        !matches!(
            (self.state, class),
            (FleetPosture::Cordoned, CordonClass::Deferrable)
        )
    }

    /// The sim intake cap in the current posture (§6 effect (b)).
    #[must_use]
    pub fn sim_intake_cap(&self, slot_cap: usize) -> usize {
        match self.state {
            FleetPosture::Nominal => slot_cap,
            FleetPosture::Cordoned => self.policy.sim_intake_cap(slot_cap),
        }
    }

    fn prune(&mut self, now_ms: u64) {
        let window = self.policy.duty_window_ms.max(self.policy.enter_window_ms);
        while let Some(front) = self.samples.front() {
            if now_ms.saturating_sub(front.now_ms) > window {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    fn maybe_enter(&mut self, now_ms: u64) -> PostureChange {
        // Trigger (a): event burst inside the enter window.
        let burst: u64 = self
            .samples
            .iter()
            .filter(|s| now_ms.saturating_sub(s.now_ms) <= self.policy.enter_window_ms)
            .map(|s| s.sample.events)
            .sum();
        if burst >= u64::try_from(self.policy.enter_events.max(1)).unwrap_or(1) {
            return self.enter(EnterReason::EventBurst { events: burst });
        }
        // Trigger (b): duty percent over the trailing duty window.
        let (events, throttled_usec, elapsed_usec) = self.duty_window_totals();
        let _ = events;
        if elapsed_usec > 0 {
            #[expect(
                clippy::cast_precision_loss,
                reason = "duty percent is an f64 metric by definition (µs/µs ratio)"
            )]
            let duty_percent = throttled_usec as f64 / elapsed_usec as f64 * 100.0;
            if duty_percent > self.policy.duty_percent {
                return self.enter(EnterReason::DutySpike { duty_percent });
            }
        }
        PostureChange::Held
    }

    fn duty_window_totals(&self) -> (u64, u64, u64) {
        self.samples
            .iter()
            .fold((0, 0, 0), |(ev, th, el), Sample { sample, .. }| {
                (
                    ev + sample.events,
                    th + sample.throttled_usec,
                    el + sample.elapsed_usec,
                )
            })
    }

    fn enter(&mut self, reason: EnterReason) -> PostureChange {
        self.state = FleetPosture::Cordoned;
        self.counters.entered += 1;
        tracing::warn!(
            target: "degenbot::fleet",
            reason = ?reason,
            entered = self.counters.entered,
            "[fleet-posture] cordon ENTER — deferrable intake held, sim intake floored; in-flight units complete"
        );
        PostureChange::Entered(reason)
    }

    fn maybe_exit(&mut self, now_ms: u64) -> PostureChange {
        // The lane-death hold is STICKY (FF-T4): the clean window never
        // lifts it — the lane is still dead. Throttle samples keep
        // feeding the machine (harmless bookkeeping); only a fresh
        // process clears the hold.
        if self.lane_death_hold {
            return PostureChange::Held;
        }
        // Exit: `exit_clean_ms` of clean time since the last dirty sample
        // (hysteresis; §6: 10 s of clean windows).
        let dirty_recently = self
            .last_unclean_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < self.policy.exit_clean_ms);
        if dirty_recently {
            return PostureChange::Held;
        }
        self.state = FleetPosture::Nominal;
        self.counters.exited += 1;
        tracing::info!(
            target: "degenbot::fleet",
            exited = self.counters.exited,
            clean_ms = self.policy.exit_clean_ms,
            "[fleet-posture] cordon EXIT after clean-window hysteresis"
        );
        PostureChange::Exited
    }
}

// ---- the ONE process-level fleet posture owner (JCI2FW Part A) ------------

/// Watch-style subscription to the fleet posture feed — the workers-crate
/// equivalent of a `tokio::sync::watch` receiver (the crate carries no
/// tokio; this is the `parking_lot` pattern its other feeds use). One
/// producer (the [`PostureOwner`]), many independent consumers: every
/// `FleetHost` subscribes at boot for the T7 shed trigger, tests subscribe
/// to observe transitions, and the Part B operator channel will drive the
/// owner directly. A consumer sees the latest posture and REAL transitions
/// only — a `Held` tick or a no-effect retune never raises an edge.
#[derive(Debug)]
pub struct PostureWatch {
    shared: Arc<FeedShared>,
    seen: AtomicU64,
}

impl PostureWatch {
    /// The latest posture (reads through — never consumes the edge).
    #[must_use]
    pub fn current(&self) -> FleetPosture {
        self.shared.state.read().posture
    }

    /// Did a transition occur since the last drain? (Does not consume.)
    #[must_use]
    pub fn has_changed(&self) -> bool {
        self.shared.state.read().seq != self.seen.load(Ordering::Relaxed)
    }

    /// Drain: the posture if a transition occurred since the last drain,
    /// else `None` (consuming the edge — exactly-once per transition).
    #[must_use]
    pub fn take_if_changed(&self) -> Option<FleetPosture> {
        let state = self.shared.state.read();
        if state.seq == self.seen.load(Ordering::Relaxed) {
            return None;
        }
        self.seen.store(state.seq, Ordering::Relaxed);
        Some(state.posture)
    }
}

/// The broadcast cell behind the feed: the posture plus a monotonic
/// transition sequence (the edge counter the watches diff against).
#[derive(Debug)]
struct FeedShared {
    state: RwLock<FeedState>,
}

#[derive(Debug, Clone, Copy)]
struct FeedState {
    posture: FleetPosture,
    seq: u64,
}

/// The ONE owner of a fleet posture state machine — a shared, thread-safe
/// shell around the pure [`PostureStateMachine`]. Every consumer (every
/// `FleetHost`, the block pump's throttle feed, tests) consults THE SAME
/// instance, so there is exactly one posture per process (per hermetic
/// test scope) and no host-local mirrors to drift apart.
///
/// Sync: the machine sits behind a `parking_lot::Mutex` — every consult is
/// a short read-through critical section, never held across an await
/// (the crate has none); the transition feed is the [`PostureWatch`]
/// broadcast above.
#[derive(Debug)]
pub struct PostureOwner {
    machine: Mutex<PostureStateMachine>,
    broadcast: Arc<FeedShared>,
}

impl PostureOwner {
    /// A fresh owner in [`FleetPosture::Nominal`]. Hermetic tests build
    /// their own owner and inject it via `FleetBoot::owner` — NEVER the
    /// process global ([`process`]/[`install_process_owner`]); posture
    /// leaking across tests is a failure class (7KAPBB).
    #[must_use]
    pub fn new(policy: PosturePolicy) -> Self {
        Self {
            machine: Mutex::new(PostureStateMachine::new(policy)),
            broadcast: Arc::new(FeedShared {
                state: RwLock::new(FeedState {
                    posture: FleetPosture::Nominal,
                    seq: 0,
                }),
            }),
        }
    }

    /// Feed one throttle-poll delta. Publishes to the feed ONLY on a real
    /// transition (`Held` ticks are silent — a subscriber never sees a
    /// spurious edge).
    ///
    /// # Feeder-site contract (TB4QGX T3)
    /// A BOT-side caller of this method MUST also wake the fleet hosts on a
    /// non-`Held` change (the degenbot-bot host waker). This crate cannot
    /// know about host channels (layering), so the wake is the caller's
    /// obligation. The resulting hint is untrusted: hosts re-read the live
    /// owner.
    pub fn observe_throttle(&self, now_ms: u64, sample: ThrottleSample) -> PostureChange {
        let (change, posture) = {
            let mut machine = self.machine.lock();
            let change = machine.observe(now_ms, sample);
            (change, machine.state())
        };
        if !matches!(change, PostureChange::Held) {
            self.publish(posture);
        }
        change
    }

    /// Feed one typed non-throttle cause (FF-T4, Z6XTDX). Publishes to
    /// the feed on a real transition like [`Self::observe_throttle`]
    /// (an idempotent hold-upgrade returns `Held` and stays silent —
    /// the detection site owns the loud lane-death log).
    ///
    /// # Feeder-site contract (TB4QGX T3)
    /// A BOT-side caller of this method MUST also wake the fleet hosts on a
    /// non-`Held` change; see [`Self::observe_throttle`].
    pub fn observe_cause(&self, cause: PostureCause) -> PostureChange {
        let (change, posture) = {
            let mut machine = self.machine.lock();
            let change = machine.observe_cause(cause);
            (change, machine.state())
        };
        if !matches!(change, PostureChange::Held) {
            self.publish(posture);
        }
        change
    }

    /// The current posture.
    #[must_use]
    pub fn current(&self) -> FleetPosture {
        self.machine.lock().state()
    }

    /// The active policy (read-through; the Part B operator channel reads
    /// and re-tunes through here).
    #[must_use]
    pub fn policy(&self) -> PosturePolicy {
        *self.machine.lock().policy()
    }

    /// Subscribe a watch: the receiver starts at the CURRENT posture with
    /// no pending edge (it observes only transitions from here on).
    #[must_use]
    pub fn subscribe(&self) -> PostureWatch {
        let seen = self.broadcast.state.read().seq;
        PostureWatch {
            shared: Arc::clone(&self.broadcast),
            seen: AtomicU64::new(seen),
        }
    }

    /// Swap the policy (the Part B operator channel's entry point). The
    /// swap is atomic under the machine lock and keeps the state + the
    /// trailing sample window; the feed re-publishes the current posture
    /// so it mirrors the machine post-swap (a no-op unless the posture
    /// itself changed — the feed carries only real transitions, and the
    /// next `observe_throttle` re-derives the posture under the new
    /// thresholds). Semantic validation of the new policy is the caller's
    /// job.
    pub fn retune(&self, new_policy: PosturePolicy) {
        let posture = {
            let mut machine = self.machine.lock();
            machine.set_policy(new_policy);
            machine.state()
        };
        self.publish(posture);
    }

    /// Whether the posture admits new lease intake for `class` right now
    /// (read-through — the dispatcher's enqueue/T-table gates and the seat
    /// hosts' admission all consult this).
    #[must_use]
    pub fn admits_lease(&self, class: CordonClass) -> bool {
        self.machine.lock().admits_lease(class)
    }

    /// Count a lease grant denied because of the posture (read-through —
    /// the tuning loop's suppression metric).
    pub fn note_intake_suppressed(&self) {
        self.machine.lock().note_intake_suppressed();
    }

    /// The sim intake cap in the current posture (read-through).
    #[must_use]
    pub fn sim_intake_cap(&self, slot_cap: usize) -> usize {
        self.machine.lock().sim_intake_cap(slot_cap)
    }

    /// Loud-transition counters snapshot (the tuning loop's metrics).
    #[must_use]
    pub fn counters(&self) -> PostureCounters {
        *self.machine.lock().counters()
    }

    /// Mirror the machine's state into the feed — compare-then-publish, so
    /// the sequence (and every subscriber's edge) moves ONLY on a real
    /// posture change.
    fn publish(&self, posture: FleetPosture) {
        let mut state = self.broadcast.state.write();
        if state.posture != posture {
            state.posture = posture;
            state.seq = state.seq.wrapping_add(1);
        }
    }
}

/// The process-level posture owner (the KAHU5W holder pattern): ONE
/// instance per process, installed by the FIRST fleet boot (first-wins —
/// later installs log at debug and return the existing owner).
static PROCESS_OWNER: OnceLock<PostureOwner> = OnceLock::new();

/// Install the process-level owner with `policy` (first-wins: the first
/// fleet boot wins; later calls log at debug and return the existing
/// owner). Production installs happen at `FleetHost::boot` — BEFORE the
/// first throttle feed reaches [`process`]. Hermetic tests never call
/// this: they inject fresh owners via `FleetBoot::owner`.
#[must_use]
pub fn install_process_owner(policy: PosturePolicy) -> &'static PostureOwner {
    if PROCESS_OWNER.set(PostureOwner::new(policy)).is_err() {
        tracing::debug!(
            target: "degenbot::fleet",
            "[fleet-posture] process owner already installed — first-wins, keeping the existing owner"
        );
    }
    process()
}

/// The process-level owner, or a doc-default owner when nothing was
/// installed yet (the holder's default stance: tests and standalone
/// constructions observe schema defaults). `FleetBoot` without an injected
/// owner installs its policy here first-wins at boot time, which is why a
/// production feed never lands on the doc-default stance.
#[must_use]
pub fn process() -> &'static PostureOwner {
    PROCESS_OWNER.get_or_init(|| PostureOwner::new(PosturePolicy::doc_defaults()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PosturePolicy {
        PosturePolicy {
            enter_events: 2,
            enter_window_ms: 1_000,
            duty_percent: 2.0,
            duty_window_ms: 5_000,
            exit_clean_ms: 10_000,
            sim_intake_floor_override: None,
        }
    }

    fn sm() -> PostureStateMachine {
        PostureStateMachine::new(policy())
    }

    fn sample(events: u64, throttled_usec: u64, elapsed_usec: u64) -> ThrottleSample {
        ThrottleSample {
            events,
            throttled_usec,
            elapsed_usec,
        }
    }

    #[test]
    fn nominal_is_the_boot_state() {
        assert_eq!(sm().state(), FleetPosture::Nominal);
    }

    /// FF-T4 (Z6XTDX) — the DECIDED option (a): a typed `PostureCause`
    /// enters the cordon IMMEDIATELY (no sample hysteresis).
    #[test]
    fn a_lane_death_cordons_immediately() {
        let mut m = sm();
        assert_eq!(
            m.observe_cause(PostureCause::LaneDeath),
            PostureChange::Entered(EnterReason::LaneDeath)
        );
        assert_eq!(m.state(), FleetPosture::Cordoned);
        assert_eq!(m.counters().lane_deaths, 1);
        assert_eq!(m.counters().entered, 1);
    }

    /// The lane-death cordon is STICKY: the clean window never lifts it
    /// (the lane is still dead) — its own exit discipline, deliberately
    /// different from the throttle hysteresis.
    #[test]
    fn the_lane_death_cordon_is_sticky_across_clean_windows() {
        let mut m = sm();
        m.observe_cause(PostureCause::LaneDeath);
        // A long run of clean samples past the full exit window: the
        // throttle hysteresis would EXIT here — the lane-death hold
        // refuses (never a silent un-cordon).
        for t in (0..30_u64).map(|i| 10_000 + i * 1_000) {
            assert_eq!(
                m.observe(t, sample(0, 0, 1_000_000)),
                PostureChange::Held,
                "a clean window must never lift a lane-death cordon"
            );
        }
        assert_eq!(m.state(), FleetPosture::Cordoned);
        assert_eq!(m.counters().exited, 0, "the sticky cordon never exits");
    }

    /// A lane death UPGRADES an existing throttle cordon: no state
    /// transition (already Cordoned), but the hold turns sticky and the
    /// cause is counted — loud, never silent.
    #[test]
    fn a_lane_death_upgrades_a_throttle_cordon_to_sticky() {
        let mut m = sm();
        // Enter via the throttle hysteresis first.
        m.observe(100, sample(2, 0, 1_000_000));
        assert_eq!(m.state(), FleetPosture::Cordoned);
        assert_eq!(m.counters().entered, 1);
        // The lane death upgrades the hold.
        assert_eq!(
            m.observe_cause(PostureCause::LaneDeath),
            PostureChange::Held,
            "no state transition — the cordon was already up"
        );
        assert_eq!(m.counters().lane_deaths, 1);
        assert_eq!(m.counters().entered, 1, "no second enter counted");
        // And the upgrade is sticky: clean windows no longer lift it.
        for t in (0..30_u64).map(|i| 10_000 + i * 1_000) {
            m.observe(t, sample(0, 0, 1_000_000));
        }
        assert_eq!(m.state(), FleetPosture::Cordoned);
        assert_eq!(m.counters().exited, 0);
    }

    /// Idempotent while the hold is already set (every cause counts).
    #[test]
    fn repeated_lane_deaths_count_but_do_not_re_enter() {
        let mut m = sm();
        m.observe_cause(PostureCause::LaneDeath);
        for _ in 0..3 {
            assert_eq!(
                m.observe_cause(PostureCause::LaneDeath),
                PostureChange::Held
            );
        }
        assert_eq!(m.counters().lane_deaths, 4);
        assert_eq!(m.counters().entered, 1);
    }

    /// The cordon effects hold under a lane-death cause exactly as under
    /// a throttle cause (the §6 vocabulary: deferrable intake held,
    /// sim intake floored, in-flight units complete).
    #[test]
    fn lane_death_cordon_effects_match_the_throttle_vocabulary() {
        let mut m = sm();
        m.observe_cause(PostureCause::LaneDeath);
        assert!(!m.admits_lease(CordonClass::Deferrable));
        assert!(m.admits_lease(CordonClass::Never));
        assert!(m.admits_lease(CordonClass::SimPool));
        // The sim intake floor (a Cordoned cap of 4 slots -> 2, the
        // same shape a throttle cordon produces).
        assert_eq!(m.sim_intake_cap(4), 2);
    }

    /// The owner publishes the lane-death transition to the feed (a
    /// subscriber sees the Cordoned edge exactly once).
    #[test]
    fn the_owner_publishes_the_lane_death_transition() {
        let owner = PostureOwner::new(policy());
        let watch = owner.subscribe();
        assert_eq!(watch.current(), FleetPosture::Nominal);
        owner.observe_cause(PostureCause::LaneDeath);
        assert_eq!(watch.current(), FleetPosture::Cordoned);
        assert_eq!(owner.current(), FleetPosture::Cordoned);
    }

    #[test]
    fn enters_on_event_burst_within_the_window() {
        let mut m = sm();
        // One lone event inside the window: below the threshold.
        assert_eq!(m.observe(100, sample(1, 0, 1_000_000)), PostureChange::Held);
        // The second event inside the same trailing 1 s window -> enter,
        // loudly typed.
        let change = m.observe(600, sample(1, 0, 500_000));
        assert_eq!(
            change,
            PostureChange::Entered(EnterReason::EventBurst { events: 2 })
        );
        assert_eq!(m.state(), FleetPosture::Cordoned);
        assert_eq!(m.counters().entered, 1);
        // Already cordoned: further dirty samples do not re-enter.
        assert!(matches!(
            m.observe(700, sample(1, 0, 100_000)),
            PostureChange::Held
        ));
        assert_eq!(m.counters().entered, 1);
    }

    #[test]
    fn burst_outside_the_enter_window_does_not_cordon() {
        let mut m = sm();
        m.observe(0, sample(1, 0, 1_000_000));
        // The 1 s window expired; two lone events > 1 s apart never burst.
        m.observe(3_000, sample(1, 0, 2_000_000));
        m.observe(3_100, sample(0, 0, 100_000));
        assert_eq!(m.state(), FleetPosture::Nominal);
    }

    #[test]
    fn enters_on_duty_spike_over_the_duty_window() {
        let mut m = sm();
        // A 1 s poll interval whose throttled time was 50% — measured over
        // the trailing 5 s window that is 50% duty, far above the 2%
        // threshold: the duty trigger fires on the first observation.
        let change = m.observe(1_000, sample(0, 500_000, 1_000_000));
        assert!(matches!(
            change,
            PostureChange::Entered(EnterReason::DutySpike { .. })
        ));
        assert_eq!(m.state(), FleetPosture::Cordoned);
        assert_eq!(m.counters().entered, 1);
    }

    #[test]
    fn a_rising_duty_only_crosses_once_the_trailing_total_exceeds_the_threshold() {
        let mut m = sm();
        // Sub-threshold ticks (0.05% each) hold the posture...
        for t in 1..5_u64 {
            assert_eq!(
                m.observe(t * 1_000, sample(0, 500, 1_000_000)),
                PostureChange::Held
            );
        }
        // ...until one more dirty tick crosses the window total above 2%.
        let change = m.observe(5_000, sample(0, 5_000_000, 1_000_000));
        assert!(matches!(
            change,
            PostureChange::Entered(EnterReason::DutySpike { .. })
        ));
    }

    #[test]
    fn sub_threshold_duty_never_cordons() {
        let mut m = sm();
        // 1% duty (design doc: steady state 0.18%) for ten windows.
        for t in 0..10_u64 {
            m.observe((t + 1) * 1_000, sample(0, 10_000, 1_000_000));
        }
        assert_eq!(m.state(), FleetPosture::Nominal);
    }

    #[test]
    fn exits_only_after_the_full_clean_hysteresis() {
        let mut m = sm();
        m.observe(100, sample(2, 0, 100_000)); // burst enter
        assert_eq!(m.state(), FleetPosture::Cordoned);
        // Clean time below the hysteresis keeps the cordon.
        let mut t = 200;
        while t < 10_000 {
            assert_eq!(m.observe(t, sample(0, 0, 1_000_000)), PostureChange::Held);
            t += 1_000;
        }
        assert_eq!(m.state(), FleetPosture::Cordoned);
        // Past 10 s clean: exit.
        let change = m.observe(10_200, sample(0, 0, 100_000));
        assert_eq!(change, PostureChange::Exited);
        assert_eq!(m.state(), FleetPosture::Nominal);
        assert_eq!(m.counters().exited, 1);
    }

    #[test]
    fn a_dirty_window_restarts_the_clean_clock() {
        let mut m = sm();
        m.observe(0, sample(2, 0, 100_000));
        let mut t = 1_000;
        while t < 9_000 {
            m.observe(t, sample(0, 0, 1_000_000));
            t += 1_000;
        }
        // A lone dirty sample resets the clean-window clock.
        m.observe(9_000, sample(1, 0, 100_000));
        assert_eq!(m.state(), FleetPosture::Cordoned);
        let mut t = 10_000;
        while t < 18_000 {
            m.observe(t, sample(0, 0, 1_000_000));
            t += 1_000;
        }
        assert_eq!(m.state(), FleetPosture::Cordoned, "only 9 s clean");
        m.observe(19_100, sample(0, 0, 100_000));
        assert_eq!(
            m.state(),
            FleetPosture::Nominal,
            "10 s clean since the reset"
        );
    }

    #[test]
    fn cordon_effects_match_the_sign_off_table() {
        let mut m = sm();
        // Nominal: full sim cap; deferrable admitted.
        assert_eq!(m.sim_intake_cap(4), 4);
        assert!(m.admits_lease(CordonClass::Never));
        assert!(m.admits_lease(CordonClass::SimPool));
        assert!(m.admits_lease(CordonClass::Deferrable));

        m.observe(0, sample(2, 0, 100_000));
        assert_eq!(m.state(), FleetPosture::Cordoned);
        // Cordon: deferrable held, sim floored at half the cap, Never free.
        assert!(!m.admits_lease(CordonClass::Deferrable));
        assert!(m.admits_lease(CordonClass::SimPool));
        assert!(m.admits_lease(CordonClass::Never));
        assert_eq!(m.sim_intake_cap(4), 2, "floor = half the slot cap");
        assert_eq!(m.sim_intake_cap(1), 1, "floored at one");
    }

    #[test]
    fn override_intake_floor_wins_and_caps_at_the_slot_cap() {
        let p = PosturePolicy {
            sim_intake_floor_override: Some(7),
            ..policy()
        };
        assert_eq!(p.sim_intake_cap(4), 4, "nothing above the slot cap");
        let p = PosturePolicy {
            sim_intake_floor_override: Some(0),
            ..policy()
        };
        assert_eq!(p.sim_intake_cap(4), 1, "floored at one");
    }

    #[test]
    fn suppressed_intake_is_counted_for_the_tuning_loop() {
        let mut m = sm();
        m.note_intake_suppressed();
        m.note_intake_suppressed();
        assert_eq!(m.counters().intake_suppressed, 2);
    }

    #[test]
    fn typed_config_projects_all_the_q5_amendment_thresholds() {
        let cfg = degenbot_config::BotConfig::default();
        let p = PosturePolicy::from_config(&cfg.fleet);
        // Defaults mirror design doc §6: 2 events / 1 s, >2% / 5 s, 10 s.
        assert_eq!(p.enter_events, 2);
        assert_eq!(p.enter_window_ms, 1_000);
        assert!((p.duty_percent - 2.0).abs() < 1e-9, "duty default is 2%");
        assert_eq!(p.duty_window_ms, 5_000);
        assert_eq!(p.exit_clean_ms, 10_000);
        assert_eq!(p.sim_intake_floor_override, None);
    }

    // ---- PostureOwner (JCI2FW Part A) -------------------------------------

    #[test]
    fn owner_transitions_publish_only_on_change() {
        let owner = PostureOwner::new(policy());
        let watch = owner.subscribe();
        // A clean sample: no transition, no publication.
        owner.observe_throttle(0, sample(0, 0, 1_000));
        assert!(!watch.has_changed());
        assert_eq!(watch.current(), FleetPosture::Nominal);
        // One lone event below the threshold: still no edge.
        owner.observe_throttle(100, sample(1, 0, 1_000));
        assert!(!watch.has_changed());
        // The bursting event crosses: exactly one edge, and the drain
        // consumes it (exactly-once per transition).
        owner.observe_throttle(600, sample(1, 0, 500_000));
        assert!(watch.has_changed());
        assert_eq!(watch.take_if_changed(), Some(FleetPosture::Cordoned));
        assert_eq!(watch.take_if_changed(), None, "one edge per transition");
        // Already cordoned: dirty ticks are Held — never re-published.
        owner.observe_throttle(700, sample(1, 0, 100_000));
        assert!(!watch.has_changed());
        assert_eq!(watch.current(), FleetPosture::Cordoned);
    }

    #[test]
    fn owner_current_reflects_the_machine_including_exit_hysteresis() {
        let owner = PostureOwner::new(policy());
        assert_eq!(owner.current(), FleetPosture::Nominal);
        owner.observe_throttle(0, sample(3, 0, 1_000));
        assert_eq!(owner.current(), FleetPosture::Cordoned);
        assert_eq!(owner.counters().entered, 1);
        // The full clean hysteresis lifts the cordon (10 s of clean ticks).
        let mut now = 1_000;
        loop {
            owner.observe_throttle(now, sample(0, 0, 1_000));
            if owner.current() == FleetPosture::Nominal {
                break;
            }
            now += 1_000;
            assert!(now <= 60_000, "the cordon never lifted");
        }
        assert_eq!(owner.counters().exited, 1);
    }

    #[test]
    fn first_wins_process_install_keeps_the_existing_owner() {
        let first = install_process_owner(policy());
        let second = install_process_owner(PosturePolicy {
            enter_events: 99,
            ..policy()
        });
        assert!(
            std::ptr::eq(first, second),
            "first-wins: a later install returns the existing owner"
        );
        assert!(std::ptr::eq(first, process()));
        assert_eq!(
            second.policy().enter_events,
            first.policy().enter_events,
            "the losing install's policy never landed"
        );
    }

    #[test]
    fn retune_swaps_thresholds_and_the_next_observe_rederives() {
        let owner = PostureOwner::new(policy());
        let watch = owner.subscribe();
        // One lone event: sub-threshold under the boot policy.
        owner.observe_throttle(0, sample(1, 0, 1_000_000));
        assert_eq!(owner.current(), FleetPosture::Nominal);
        // Retune to a 1-event trigger: no posture change, no edge — but
        // the next lone (clean) sample cordons under the new thresholds.
        owner.retune(PosturePolicy {
            enter_events: 1,
            ..policy()
        });
        assert_eq!(owner.policy().enter_events, 1, "the retune swapped");
        assert!(!watch.has_changed(), "a no-effect retune is not an edge");
        owner.observe_throttle(2_000, sample(1, 0, 100_000));
        assert_eq!(owner.current(), FleetPosture::Cordoned);
        assert_eq!(watch.take_if_changed(), Some(FleetPosture::Cordoned));
    }

    #[test]
    fn two_owners_are_fully_independent_hermetic_isolation() {
        let a = PostureOwner::new(policy());
        let b = PostureOwner::new(policy());
        let watch_a = a.subscribe();
        let watch_b = b.subscribe();
        // A cordons; b never hears about it.
        a.observe_throttle(0, sample(3, 0, 1_000));
        assert_eq!(a.current(), FleetPosture::Cordoned);
        assert_eq!(b.current(), FleetPosture::Nominal);
        assert_eq!(watch_a.take_if_changed(), Some(FleetPosture::Cordoned));
        assert_eq!(
            watch_b.take_if_changed(),
            None,
            "no posture leaks across owners"
        );
        // b's own feed stays silent on a clean tick, and the feeds stay
        // independent in both directions.
        b.observe_throttle(1_000, sample(0, 0, 1_000));
        assert_eq!(watch_b.take_if_changed(), None);
        assert_eq!(watch_a.take_if_changed(), None);
    }

    #[test]
    fn owner_read_throughs_match_the_machine_semantics() {
        let owner = PostureOwner::new(PosturePolicy {
            sim_intake_floor_override: Some(1),
            ..policy()
        });
        assert!(owner.admits_lease(CordonClass::Deferrable));
        assert_eq!(owner.sim_intake_cap(8), 8, "nominal intake is the cap");
        owner.observe_throttle(0, sample(3, 0, 1_000));
        assert!(!owner.admits_lease(CordonClass::Deferrable));
        assert!(owner.admits_lease(CordonClass::Never));
        assert!(owner.admits_lease(CordonClass::SimPool));
        assert_eq!(owner.sim_intake_cap(8), 1, "the cordon floor override");
        assert_eq!(owner.counters().entered, 1);
    }

    // ---- the operator re-tune patch (JCI2FW Part B) -----------------------

    #[test]
    fn an_empty_patch_is_rejected() {
        assert!(PosturePolicyPatch::default().validate().is_err());
        assert!(PosturePolicyPatch::default().is_empty());
        assert_eq!(
            PosturePolicyPatch::default().validate(),
            Err(PostureRetuneError::EmptyPatch),
            "the empty-patch refusal is its own typed error"
        );
    }

    #[test]
    fn every_threshold_rule_is_a_typed_rejection() {
        assert_eq!(
            PosturePolicyPatch {
                enter_events: Some(0),
                ..PosturePolicyPatch::default()
            }
            .validate(),
            Err(PostureRetuneError::EnterEvents(0)),
            "enter_events >= 1"
        );
        assert_eq!(
            PosturePolicyPatch {
                enter_window_ms: Some(0),
                ..PosturePolicyPatch::default()
            }
            .validate(),
            Err(PostureRetuneError::EnterWindow(0)),
            "windows must be > 0 ms"
        );
        assert_eq!(
            PosturePolicyPatch {
                duty_window_ms: Some(0),
                ..PosturePolicyPatch::default()
            }
            .validate(),
            Err(PostureRetuneError::DutyWindow(0))
        );
        assert_eq!(
            PosturePolicyPatch {
                exit_clean_ms: Some(0),
                ..PosturePolicyPatch::default()
            }
            .validate(),
            Err(PostureRetuneError::ExitClean(0))
        );
        // The sane inclusive duty range (0.0, 100.0]: 0, negatives, >100,
        // and non-finite values are all refused, never clamped.
        for duty in [0.0, -1.0, 100.5, f64::NAN, f64::INFINITY] {
            let rejected = PosturePolicyPatch {
                duty_percent: Some(duty),
                ..PosturePolicyPatch::default()
            }
            .validate();
            assert!(
                matches!(rejected, Err(PostureRetuneError::DutyPercent(_))),
                "duty {duty} must be refused as DutyPercent, got {rejected:?}"
            );
        }
        assert_eq!(
            PosturePolicyPatch {
                sim_intake_floor_override: Some(Some(0)),
                ..PosturePolicyPatch::default()
            }
            .validate(),
            Err(PostureRetuneError::SimIntakeFloor(0)),
            "an explicit floor must be >= 1"
        );
    }

    #[test]
    fn boundary_values_are_admitted() {
        // The inclusive edges pass: 1 event, 1 ms windows, the 100.0% duty
        // ceiling, a floor of exactly 1, and a CLEARED floor.
        let validated = PosturePolicyPatch {
            enter_events: Some(1),
            enter_window_ms: Some(1),
            duty_percent: Some(100.0),
            duty_window_ms: Some(1),
            exit_clean_ms: Some(1),
            sim_intake_floor_override: Some(Some(1)),
        }
        .validate();
        assert_eq!(
            validated,
            Ok(()),
            "inclusive bounds are legal (1 event, 1 ms windows, 100.0% duty, floor 1)"
        );
        let cleared = PosturePolicyPatch {
            sim_intake_floor_override: Some(None),
            ..PosturePolicyPatch::default()
        }
        .validate();
        assert_eq!(
            cleared,
            Ok(()),
            "clearing the floor is a legal one-key patch"
        );
    }

    #[test]
    fn patched_with_touches_only_supplied_keys() {
        let base = policy();
        let patched = base.patched_with(PosturePolicyPatch {
            enter_events: Some(7),
            ..PosturePolicyPatch::default()
        });
        assert_eq!(patched.enter_events, 7, "the supplied key landed");
        assert_eq!(patched.enter_window_ms, base.enter_window_ms);
        assert!(
            (patched.duty_percent - base.duty_percent).abs() < f64::EPSILON,
            "an absent key keeps the current duty percent"
        );
        assert_eq!(patched.duty_window_ms, base.duty_window_ms);
        assert_eq!(patched.exit_clean_ms, base.exit_clean_ms);
        assert_eq!(
            patched.sim_intake_floor_override, base.sim_intake_floor_override,
            "an absent key keeps the current value"
        );
    }

    #[test]
    fn patched_with_distinguishes_floor_set_clear_and_absent() {
        let base = PosturePolicy {
            sim_intake_floor_override: Some(3),
            ..policy()
        };
        // Key absent: the current override is kept.
        assert_eq!(
            base.patched_with(PosturePolicyPatch::default())
                .sim_intake_floor_override,
            Some(3)
        );
        // Key present with a value: the override is set.
        assert_eq!(
            base.patched_with(PosturePolicyPatch {
                sim_intake_floor_override: Some(Some(1)),
                ..PosturePolicyPatch::default()
            })
            .sim_intake_floor_override,
            Some(1)
        );
        // Key present with the key's None value: the override is CLEARED
        // (back to half the slot cap at read time).
        assert_eq!(
            base.patched_with(PosturePolicyPatch {
                sim_intake_floor_override: Some(None),
                ..PosturePolicyPatch::default()
            })
            .sim_intake_floor_override,
            None
        );
    }

    #[test]
    fn a_validated_patch_retunes_the_owner_end_to_end() {
        let owner = PostureOwner::new(policy());
        let patch = PosturePolicyPatch {
            duty_percent: Some(5.0),
            ..PosturePolicyPatch::default()
        };
        assert_eq!(patch.validate(), Ok(()), "the channel validated the patch");
        let effective = owner.policy().patched_with(patch);
        owner.retune(effective);
        assert!(
            (owner.policy().duty_percent - 5.0).abs() < f64::EPSILON,
            "the retuned duty percent is live"
        );
        assert_eq!(owner.policy().enter_events, policy().enter_events);
    }
}
