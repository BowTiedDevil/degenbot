//! The single authoritative routing table for decoded CL pool events.
//!
//! FUWYUR/UO3JM4 root cause: a decoded event's fate was decided in three
//! places with divergent partial policies (`LogDispatcher::dispatch`'s
//! APPLY-MISS funnel, `process_backfill_logs`, and the inline quarantine/
//! unregistered arms inside the `apply_*` methods). Each site knew a subset
//! of the policy; the funnel's "unregistered ⇒ no-op" inference silently
//! dropped live liquidity events that the buffering machinery was built to
//! stage. This module collapses that knowledge into ONE exhaustive match
//! keyed on `(phase, pool presence, event kind)` — adding a variant to any
//! axis is a compile error here, which is the entire point.
//!
//! Callers (dispatcher, backfill applier) become transport: decode → ask the
//! table → execute the returned [`RouteAction`]. No caller may pre-judge an
//! event's fate from its own cheaper copy of the policy.

use degenbot_pools::v3_state::RegistrationLifecycle;

/// Which phase of the bot lifecycle produced the event — selects the buffer
/// family and whether direct application is even legal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Snapshot→WS gap replay (`eth_getLogs(S+1..W)`) before/around crawl:
    /// events land in the never-expired backfill buffer for staged
    /// application at registration.
    Backfill,
    /// Live websocket stream: events land in the cutoff-gated pump buffer or
    /// apply directly.
    Live,
}

/// Pool presence as seen by the router: `RegistrationLifecycle` covers only
/// registered pools; "not in `BotState` at all" is its own row of the table
/// (the FUWYUR row), not an `Option` the caller gets to interpret.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PoolPresence {
    /// No `BotState` entry yet — crawl has not reached this pool.
    Unregistered,
    /// Registered `Tracked` pool in the two-step verify-then-live lifecycle
    /// (6N7XVR): events defer to the pump buffer so the pin cannot outrun
    /// `last_complete_block`.
    Quarantined,
    /// Registered and verified: steady-state direct application.
    Live,
}

impl PoolPresence {
    /// Map a registration lookup to presence. `None` = not in `BotState`.
    #[must_use]
    pub fn from_lifecycle(lifecycle: Option<RegistrationLifecycle>) -> Self {
        match lifecycle {
            Some(RegistrationLifecycle::Quarantined) => Self::Quarantined,
            Some(RegistrationLifecycle::Live) => Self::Live,
            None => Self::Unregistered,
        }
    }
}

/// What the event does to state, at table resolution time. Scalar-refresh
/// events (V2 Sync, V3/V4 Swap) rewrite head scalars; liquidity events
/// (V3 `Mint`/`Burn`, V4 `ModifyLiquidity`) mutate `tick_data` — the distinction
/// matters because tick data CANNOT be retro-supplied by a later DB-row
/// load, while scalars can (that trust assumption is named explicitly on
/// the `NoOp` rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// V2 Sync / V3 Swap / V4 Swap: scalar payload, re-seedable from DB row.
    ScalarRefresh,
    /// V3 `Mint`/`Burn` / V4 `ModifyLiquidity`: tick-map mutation, must not be lost.
    TickMutation,
}

/// Which physical buffer receives a staged event. The dual-buffer split is
/// load-bearing: backfill drains fully at registration while the pump buffer
/// drains only up to the tombstone cutoff plus a `set_live` tail flush
/// (3M5PO5/YLYJM2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BufferKind {
    /// Never-expired snapshot-gap buffer; drained by `apply_backfill_buffer_*`
    /// during registration.
    Backfill,
    /// Live WS buffer; drained by `apply_pump_buffer_*` up to the completeness
    /// cutoff, retained tail flushed at `set_*_pool_live`.
    Pump,
}

/// Why an event was deliberately dropped. Dropping requires naming a reason —
/// the compiler-enforced antidote to the FUWYUR silent drop, where "no-op"
/// was inferred instead of chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoOpReason {
    /// Unregistered pool, scalar-refresh event: the payload is re-seeded
    /// wholesale from the DB row at registration. TRUST ASSUMPTION: the row
    /// is fresh enough. Liquidity events NEVER take this path — they stage
    /// into a buffer instead (tick data cannot be retro-supplied).
    ScalarReseedAtRegistration,
}

/// Execution result after a caller routed one event through [`route_action`]
/// and applied the returned [`RouteAction`]. `Applied` carries the affected
/// `pool_id` so callers can run subscriber notify; buffered/no-op outcomes
/// carry no id (nothing was mutated).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Applied directly to registered pool state.
    Applied(u64),
    /// Staged into the named buffer.
    Buffered(BufferKind),
    /// Deliberately dropped (reason names the trust assumption).
    NoOp(NoOpReason),
}

/// The verdict for one decoded event. Total: no implicit drop path exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteAction {
    /// Apply directly to registered pool state now.
    ApplyDirect,
    /// Stage into the named buffer for later application.
    Buffer(BufferKind),
    /// Deliberately do nothing; the reason names the trust assumption.
    Drop(NoOpReason),
}

/// THE routing table. Every `(Phase, PoolPresence, EventKind)` combination
/// has exactly one row; adding any axis variant without adding rows fails to
/// compile here. This function is the ONLY component allowed to decide an
/// event's fate — callers execute, they do not pre-judge.
///
/// Row notes:
/// - Backfill × Quarantined × `ScalarRefresh` routes to the PUMP buffer (not
///   backfill): it mirrors the historical behavior of `apply_v3_swap`'s
///   quarantine arm, which buffers via `buffer_pump`. Preserved verbatim;
///   revisit only with a soak-backed reason.
/// - Backfill × {Unregistered, Quarantined} × `TickMutation` stages into the
///   BACKFILL buffer: drained fully at registration, matching
///   `buffer_backfill_v3_liquidity_update`.
/// - Live × Unregistered × `TickMutation` is the FUWYUR row: it MUST stage
///   into the pump buffer. It used to be an implicit drop.
// The per-cell rows are deliberate: this IS the table — merging arms would
// hide which cell each behavior belongs to.
#[expect(clippy::match_same_arms)]
#[must_use]
pub fn route_action(phase: Phase, presence: PoolPresence, kind: EventKind) -> RouteAction {
    match (phase, presence, kind) {
        // Registered-and-live pools apply directly in every phase.
        (_, PoolPresence::Live, _) => RouteAction::ApplyDirect,

        // Backfill phase: tick mutations always stage (backfill buffer);
        // scalars for unregistered pools rely on the row re-seed.
        (Phase::Backfill, PoolPresence::Unregistered, EventKind::TickMutation) => {
            RouteAction::Buffer(BufferKind::Backfill)
        }
        (Phase::Backfill, PoolPresence::Unregistered, EventKind::ScalarRefresh) => {
            RouteAction::Drop(NoOpReason::ScalarReseedAtRegistration)
        }
        (Phase::Backfill, PoolPresence::Quarantined, EventKind::TickMutation) => {
            RouteAction::Buffer(BufferKind::Backfill)
        }
        (Phase::Backfill, PoolPresence::Quarantined, EventKind::ScalarRefresh) => {
            RouteAction::Buffer(BufferKind::Pump)
        }

        // Live phase: everything for a not-yet-steady pool defers to the
        // pump buffer; unregistered scalar refreshes still lean on the row
        // re-seed (documented trust).
        (Phase::Live, PoolPresence::Unregistered, EventKind::TickMutation) => {
            RouteAction::Buffer(BufferKind::Pump)
        }
        (Phase::Live, PoolPresence::Unregistered, EventKind::ScalarRefresh) => {
            RouteAction::Drop(NoOpReason::ScalarReseedAtRegistration)
        }
        (Phase::Live, PoolPresence::Quarantined, EventKind::TickMutation) => {
            RouteAction::Buffer(BufferKind::Pump)
        }
        (Phase::Live, PoolPresence::Quarantined, EventKind::ScalarRefresh) => {
            RouteAction::Buffer(BufferKind::Pump)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exhaustiveness is compile-time enforced, but this walks every cell of
    /// the 2×3×2 table anyway so a future refactor can't quietly narrow the
    /// domain (e.g., swap an axis for an Option and lose a row).
    #[test]
    fn route_table_covers_every_cell() {
        let phases = [Phase::Backfill, Phase::Live];
        let presences = [
            PoolPresence::Unregistered,
            PoolPresence::Quarantined,
            PoolPresence::Live,
        ];
        let kinds = [EventKind::ScalarRefresh, EventKind::TickMutation];
        for &p in &phases {
            for &presence in &presences {
                for &k in &kinds {
                    // Merely calling asserts no panic path; the match arms are
                    // the real contract.
                    let _ = route_action(p, presence, k);
                }
            }
        }
    }

    /// THE FUWYUR ROW: a live-phase tick mutation for a not-yet-registered
    /// pool MUST stage into the pump buffer. It used to be an implicit drop
    /// at the dispatcher funnel — this test exists so nobody can reintroduce
    /// that inference here.
    #[test]
    fn fuwyur_row_live_unregistered_tick_mutation_buffers() {
        assert_eq!(
            route_action(
                Phase::Live,
                PoolPresence::Unregistered,
                EventKind::TickMutation
            ),
            RouteAction::Buffer(BufferKind::Pump),
            "FUWYUR regression: live-window tick mutations for unregistered pools \
             must stage into the pump buffer, never drop"
        );
    }

    /// Drops may only target scalar refreshes — tick mutations are never
    /// droppable anywhere in the table (they cannot be retro-supplied).
    #[test]
    fn drops_only_target_scalar_refresh_events() {
        for &p in &[Phase::Backfill, Phase::Live] {
            for &presence in &[
                PoolPresence::Unregistered,
                PoolPresence::Quarantined,
                PoolPresence::Live,
            ] {
                assert_ne!(
                    route_action(p, presence, EventKind::TickMutation),
                    RouteAction::Drop(NoOpReason::ScalarReseedAtRegistration),
                    "tick mutations are never droppable ({p:?}, {presence:?})"
                );
            }
        }
    }

    /// Live pools always apply directly, regardless of phase or kind.
    #[test]
    fn live_presence_always_applies_direct() {
        for &p in &[Phase::Backfill, Phase::Live] {
            for &k in &[EventKind::ScalarRefresh, EventKind::TickMutation] {
                assert_eq!(
                    route_action(p, PoolPresence::Live, k),
                    RouteAction::ApplyDirect
                );
            }
        }
    }

    /// Presence mapping: `None` lifecycle is Unregistered (the FUWYUR blind
    /// spot made a first-class value), not something callers get to interpret.
    #[test]
    fn none_lifecycle_maps_to_unregistered() {
        assert_eq!(
            PoolPresence::from_lifecycle(None),
            PoolPresence::Unregistered
        );
        assert_eq!(
            PoolPresence::from_lifecycle(Some(RegistrationLifecycle::Quarantined)),
            PoolPresence::Quarantined
        );
        assert_eq!(
            PoolPresence::from_lifecycle(Some(RegistrationLifecycle::Live)),
            PoolPresence::Live
        );
    }
}
