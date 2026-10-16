//! The epoch's non-dispatch verdicts: the admission draw that mints the
//! [`CycleTicket`], and the shed / skipped-empty arms the epoch takes before
//! any dispatch.
//!
//! The draw is the epoch ledger's SINGLE consumption decision, and the ticket
//! is the only carrier of its zero-budget verdict, so the shed arm reads a
//! value that cannot outlive the cycle it belongs to. The draw runs from the
//! Resolved stage row, before the epoch exists, so it takes the admission
//! stance and the machine as parameters; the two arms are machine-direct free
//! functions over the epoch's [`EpochContext`], with the cycle's arm latch
//! passed in because it is a cycle transient, not epoch work.

use super::{CycleArm, CycleOutcome, CycleTicket, EpochContext, ResolveCensus};
use crate::arb_engine::detached_cycle::DetachedCycle;
use degenbot_core::op_info;
use degenbot_pools::solve::affected_keys::AffectedKey;
use degenbot_substrate::EpochDelta;
use hashbrown::HashSet;

/// The admission DRAW - the SINGLE consumption decision of the
/// epoch ledger. `budget = max(0, target - in-flight)` keys freshest-first,
/// the overflow RETAINED for a later cycle; a zero budget is the SHED
/// verdict, carried on the returned [`CycleTicket`] (never stashed on the
/// cycle) so only the epoch that consumes that ticket can act on it.
///
/// The draw precedes the epoch (the Resolved stage row mints the ticket the
/// Solved row's epoch consumes), so the stance and the machine arrive as
/// parameters rather than through an [`EpochContext`].
pub(super) fn draw(
    delta: &EpochDelta,
    head: u64,
    solve_admission: bool,
    admission_target_depth: u64,
    admission_retention_blocks: u64,
    detached_cycle: &mut DetachedCycle,
) -> CycleTicket {
    if !solve_admission {
        return CycleTicket {
            affected: delta.take_keys(),
            draw_zero: false,
        };
    }
    let outstanding = detached_cycle
        .outstanding
        .load(std::sync::atomic::Ordering::Relaxed);
    let budget =
        usize::try_from(admission_target_depth.saturating_sub(outstanding)).unwrap_or(usize::MAX);
    let cutoff = head.saturating_sub(admission_retention_blocks);
    let expired = delta.expire_older_than(cutoff);
    detached_cycle.note_leads_expired(expired);
    CycleTicket {
        affected: delta.draw_freshest(budget),
        draw_zero: budget == 0,
    }
}

/// The shed arm: a zero-budget draw consumed nothing, so this epoch submits
/// NOTHING - no `begin_cycle`, no seq tick, no submission. The keys stay in
/// the ledger for a later cycle (carry) and the solved-block cursor advances
/// exactly like the skipped-empty bookkeeping pass.
pub(super) fn shed_epoch(
    ctx: &mut EpochContext<'_>,
    last_arm: &mut Option<CycleArm>,
    affected: &[AffectedKey],
    affected_path_ids: &HashSet<u64>,
    solve_block: u64,
) -> CycleOutcome {
    ctx.detached_cycle.shed();
    op_info!(
        domain = solver,
        block_number = solve_block,
        paths.affected = affected_path_ids.len(),
        in_flight = ctx
            .detached_cycle
            .outstanding
            .load(std::sync::atomic::Ordering::Relaxed),
        target_depth = ctx.admission_target_depth,
        "SHED: draw-time zero budget — nothing submitted; keys retained for carry"
    );
    // monotone advance on the block cursor (the
    // skipped_empty bookkeeping contract).
    ctx.cursor.advance_solved(solve_block);
    let arm = CycleArm::Shed {
        keys_affected: affected_path_ids.len(),
        in_flight: ctx
            .detached_cycle
            .outstanding
            .load(std::sync::atomic::Ordering::Relaxed),
        target_depth: ctx.admission_target_depth,
    };
    *last_arm = Some(arm);
    CycleOutcome {
        solved_block: solve_block,
        census: ResolveCensus {
            affected: affected.len() as u64,
            resolved: 0,
            same_state: 0,
            projections: 0,
        },
        arm,
    }
}

/// The skipped-empty arm: no affected paths (and no pending registration
/// carry), so the pass is bookkeeping-only and the solved-block cursor still
/// advances.
pub(super) fn skipped_empty_epoch(
    ctx: &mut EpochContext<'_>,
    last_arm: &mut Option<CycleArm>,
    affected: &[AffectedKey],
    solve_block: u64,
) -> CycleOutcome {
    // Cold-start trace: keys with NO registered paths reach here as a
    // bookkeeping-only pass (span exists, no dispatch) — stamp so the
    // cycle span never reads as arm-less.
    // monotone advance on the block cursor.
    ctx.cursor.advance_solved(solve_block);
    let arm = CycleArm::SkippedEmpty { keys_affected: 0 };
    *last_arm = Some(arm);
    CycleOutcome {
        solved_block: solve_block,
        census: ResolveCensus {
            affected: affected.len() as u64,
            resolved: 0,
            same_state: 0,
            projections: 0,
        },
        arm,
    }
}
