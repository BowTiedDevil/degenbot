//! Executor A/B probe fixtures : the harness support the
//! fleet parity/identity fixtures and the workload-partition binning
//! property draw on (the heavy-CL capture-corpus loader, the shared
//! solve-cycle fixture, and the production LPT bin packer).
//!
//! Extracted from the retired grab file so the fixtures live in the module tree (test-only) rather than
//! inside the twin-collapse residue.
// Fixture + harness support for the fleet probe surface (LW-T9: the
// legacy-stance A/B probe arms are DELETED with the stance — the fleet
// is one executor, so there is nothing to A/B). What remains: the heavy-CL
// capture-corpus loader, the production cost proxy's bin packer and the
// shared-cycle fixture used by the fleet parity/identity fixtures.
// Fixture parse replicates rust/crates/degenbot-solvers/examples/rayon_scale_probe.rs.
use super::BotState;
use crate::arb_engine::solve_cycle::PathTimesHeap;
use crate::arb_engine::solve_cycle::SolveCycleShared;
use crate::arb_engine::workload_partition::{lpt_partition, path_cost_proxy};
use crate::arb_engine::BlockMetadata;
use alloy::primitives::U256;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::mobius_v3_int::{build_cl_crossing_table, build_cl_word_profiles};
use hashbrown::HashMap;
use serde_json::Value;
use std::sync::Arc;
fn fixture_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("DEGENBOT_PROBE_FIXTURE") {
        return std::path::PathBuf::from(p);
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl")
}
/// Zst-aware corpus load for the BCA77G parity fixtures: the packaged
/// `heavy_cl_solve_captures.jsonl.zst` decodes transparently via
/// `capture_fixture::read_fixture` (same corpus the probe measures).
pub(in crate::arb_engine) fn load_corpus_fixture(
) -> Vec<Arc<::degenbot_solvers::mixed::ResolvedMixedPath>> {
    load_corpus()
}
fn u256(s: &str) -> Result<U256, String> {
    s.trim().parse::<U256>().map_err(|e| e.to_string())
}
fn range(v: &Value) -> Result<IntV3TickRangeHop, String> {
    let wbp = v
        .get("word_boundary_prices")
        .and_then(Value::as_array)
        .ok_or("word_boundary_prices")?
        .iter()
        .map(|w| w.as_str().ok_or_else(|| "wbp".to_string()).and_then(u256))
        .collect::<Result<Vec<_>, String>>()?;
    Ok(IntV3TickRangeHop {
        liquidity: v
            .get("liquidity")
            .and_then(Value::as_str)
            .ok_or("liquidity")?
            .parse::<u128>()
            .map_err(|e| e.to_string())?,
        sqrt_price_x96: u256(
            v.get("sqrt_price_x96")
                .and_then(Value::as_str)
                .ok_or("sp")?,
        )?,
        sqrt_price_lower_x96: u256(
            v.get("sqrt_price_lower_x96")
                .and_then(Value::as_str)
                .ok_or("spl")?,
        )?,
        sqrt_price_upper_x96: u256(
            v.get("sqrt_price_upper_x96")
                .and_then(Value::as_str)
                .ok_or("spu")?,
        )?,
        gamma_numer: v
            .get("gamma_numer")
            .and_then(Value::as_u64)
            .ok_or("gamma")?,
        fee_denom: v.get("fee_denom").and_then(Value::as_u64).ok_or("fee")?,
        zero_for_one: v
            .get("zero_for_one")
            .and_then(Value::as_bool)
            .ok_or("zfo")?,
        word_boundary_prices: wbp,
    })
}
pub(in crate::arb_engine) fn load_corpus() -> Vec<Arc<::degenbot_solvers::mixed::ResolvedMixedPath>>
{
    let path = fixture_path();
    // Zst-aware (packaged fixtures decode transparently; regenerated
    // plain captures still win the resolution order).
    let content = ::degenbot_solvers::capture_fixture::read_fixture(&path);
    let mut items = Vec::new();
    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(doc) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Ok(hops_v) = doc.get("hops").and_then(Value::as_array).ok_or("hops") else {
            continue;
        };
        let mut hops = Vec::new();
        for hop in hops_v {
            let Ok(ra) = hop.as_array().ok_or("hop") else {
                continue;
            };
            let mut ranges = Vec::new();
            for r in ra {
                if let Ok(rh) = range(r) {
                    ranges.push(rh);
                }
            }
            let seq = IntV3TickRangeSequence { ranges };
            hops.push(::degenbot_solvers::mixed::ResolvedHop::V3 {
                word_profiles: Arc::from(build_cl_word_profiles(&seq)),
                crossing_table: Arc::from(build_cl_crossing_table(&seq)),
                int_seq: Arc::new(seq),
            });
        }
        items.push(Arc::new(::degenbot_solvers::mixed::ResolvedMixedPath {
            hops,
            valid: true,
            state_nonces: Vec::new(),
            max_update_block: 0,
        }));
    }
    assert!(!items.is_empty(), "fixture must load at least one path");
    items
}
pub(in crate::arb_engine) fn probe_ctx() -> Arc<SolveCycleShared> {
    Arc::new(SolveCycleShared {
        solve_block: 0,
        epoch: 0,
        metadata: BlockMetadata::default(),
        runtime: ::degenbot_solvers::runtime::SolveRuntimeConfig::default(),
        gate_capture: None,
        walk_memo: Arc::new(::degenbot_solvers::mobius_v3_int::WalkMemo::new(
            false, false,
        )),
        prefix_cache: Arc::new(::degenbot_solvers::profit_envelope::PrefixCache::new()),
        min_profit: ::alloy::primitives::U256::ZERO,
        capture: None,
        capture_mixed: None,
        path_times: parking_lot::Mutex::new(PathTimesHeap::new()),
        gate_total: parking_lot::Mutex::new(
            ::degenbot_solvers::profit_envelope::GateStats::default(),
        ),
        solve_cpu_us: std::sync::atomic::AtomicU64::new(0),
        walk_pieces_total: std::sync::atomic::AtomicU64::new(0),
        walk_sims_total: std::sync::atomic::AtomicU64::new(0),
        walk_word_steps_total: std::sync::atomic::AtomicU64::new(0),
        walk_refine_sims_total: std::sync::atomic::AtomicU64::new(0),
        walk_ternary_total: std::sync::atomic::AtomicU64::new(0),
        walk_grid_total: std::sync::atomic::AtomicU64::new(0),
        sims_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        gate_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        core: Arc::new(crate::bot_core::state_lock::StateLock::new(BotState::new())),
        pool_refs: Vec::new(),
        worker_clamp: false,
        inline_sim: None,
        #[cfg(test)]
        test_solve_delay: None,
        #[cfg(test)]
        test_solve_panic: None,
    })
}
/// LPT bins per the production cost fn (empty measured-history = first-cycle
/// structural cost, exactly like an engine cold bucket).
pub(in crate::arb_engine) fn prod_lpt_bins(
    items: &[Arc<::degenbot_solvers::mixed::ResolvedMixedPath>],
    threads: usize,
) -> Vec<Vec<usize>> {
    lpt_partition(items.len(), threads, |i| path_cost_proxy(&items[i]))
}
