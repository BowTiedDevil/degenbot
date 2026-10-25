//! The epoch's delivery seam: the merged result's inline-sim payload store and
//! the streaming single-result emission.
//!
//! The delivery policy itself lives in [`super::delivery_policy`]; this module
//! owns the two steps the merge phase takes THROUGH it — the payload the
//! delivery diff ships with the batch, and the immediate per-result emission
//! when the streaming stance is on. Machine-direct free functions over the
//! epoch's [`EpochContext`].

use super::EpochContext;
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
    if let Some(p) = &payload {
        let verdict = if p.failure.is_some() {
            "not_profitable"
        } else {
            "profitable"
        };
        // DEBUG-gated: this event duplicates the
        // `[path] profitable solve` event above (same path.id/profit) and
        // the Python-side `[sim]` summary; the settle verdict is also
        // observable as an event on the enclosing `degenbot.arb.merge`
        // OTel span. Re-enable with `RUST_LOG=degenbot_bot=debug`.
        diag!(domain = solver, { path.id = pid, verdict, expected_profit = %result.profit, sim.seam = "inline_payload_store" },
            "inline payload settle"
        );
    }
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
        hotpath::measure_block!("merge.delivery_emit", {
            delivery.emit_single_result_batch(
                solve_block,
                metadata,
                pid,
                result,
                payload_now.as_ref(),
            );
        });
    }
}
