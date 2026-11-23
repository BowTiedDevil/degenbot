//! The epoch's delivery seam: the merged result's inline-sim payload store and
//! the streaming single-result emission.
//!
//! The delivery policy itself lives in [`super::delivery_policy`]; this module
//! owns the two steps the merge phase takes THROUGH it — the payload the
//! delivery diff ships with the batch, and the immediate per-result emission
//! when the streaming stance is on. Machine-direct free functions over the
//! epoch's [`EpochContext`].

use super::EpochContext;
use crate::arb_engine::admission_floor::AdmissionSeam;
use crate::arb_engine::delivery_policy::DeliveryPolicy;
use crate::arb_engine::inline_sim::SimulatedPathResult;
use ::degenbot_pools::solve::SolvePathResult;
use degenbot_core::diag;
use degenbot_substrate::BlockMetadata;

/// The merged result's delivery steps, in the merge's own order: store (or
/// drop) the inline-sim payload the delivery diff ships with the batch, then
/// emit the single-result batch when the streaming stance is on.
pub(super) fn deliver_merged_result(
    ctx: &EpochContext<'_>,
    delivery: &mut DeliveryPolicy,
    solve_block: u64,
    metadata: &BlockMetadata,
    pid: u64,
    result: &SolvePathResult,
    payload: Option<SimulatedPathResult>,
) {
    // SIMPIPE2 T3: the inline-sim payload — store it so the delivery
    // diff ships it with the batch (`None` = the path re-solved without a
    // payload this cycle — stance off or hook failure — so any stale
    // entry MUST drop). The merge emits the terminal
    // verdict as an EVENT on the enclosing merge span (both arms hold
    // `degenbot.arb.merge` here; the detached sidecar re-enters the solve
    // span). The former `degenbot.bundle.simulate` marker span collided
    // with the real EVM-sim spans of the same name and flooded every
    // block trace with 90-300 microsecond lookalikes. The span name now
    // belongs to simulation work only (worker seam + the FFI seam in
    // degenbot-arbitrage/simulator.rs).
    // The settle verdict for BOTH payload arms: a `None` payload (no sim landed
    // this cycle — stance off, the admission floor, or an inline degrade) is the
    // same diagnosis problem as a failed one, so it is named here rather than
    // read off the absence of a payload.
    let verdict = match &payload {
        None => "no_payload",
        Some(p) if p.failure.is_some() => "not_profitable",
        Some(_) => "profitable",
    };
    // DEBUG-gated: on the settled arms this event duplicates the
    // `[path] profitable solve` event above (same path.id/profit) and the
    // Python-side `[sim]` summary; the settle verdict is also observable as an
    // event on the enclosing `degenbot.arb.merge` OTel span. Re-enable with
    // `RUST_LOG=degenbot_bot=debug`. The `no_payload` arm has no other line at
    // this seam — the entry is dropped from the payload store here, so this is
    // where an un-simulated candidate is named.
    diag!(domain = solver, { path.id = pid, verdict, expected_profit = %result.profit, sim.seam = "inline_payload_store" },
        "inline payload settle"
    );
    hotpath::measure_block!("merge.payload_store", {
        match payload {
            Some(p) => {
                ctx.inline_payloads.insert(pid, p);
            }
            None => {
                ctx.inline_payloads.remove(&pid);
            }
        }
    });
    // T3: DEGENBOT_STREAMING_DELIVERY - each above-threshold
    // merged result is emitted IMMEDIATELY (before the slowest path can
    // possibly delay it). The per-entry emission composes with the
    // debounce sweep, which still owns expired/removed + the end-of-cycle
    // metadata batch.
    let payload_now = ctx.inline_payloads.get(&pid).map(|e| e.value().clone());
    if ctx.streaming_delivery {
        // The same counting-seam ownership as the batch sweep: an armed inline
        // gate already decided this candidate's floor, so its suppression is
        // counted at that seam and not again here.
        let seam = AdmissionSeam::of_cycle(ctx.inline_sim_enabled, ctx.inline_sim.is_some());
        hotpath::measure_block!("merge.delivery_emit", {
            delivery.emit_single_result_batch(
                crate::arb_engine::SolveStamp {
                    block: solve_block,
                    anchor: ctx.cursor.coordinates().results_anchor,
                },
                metadata,
                pid,
                result,
                payload_now.as_ref(),
                seam,
            );
        });
    }
}
