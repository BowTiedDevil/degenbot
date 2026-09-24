//! FF-T5: "degenbot.runtime_status()" - budget, plan, census.
//!
//! The operator's live-process view: what the fleet booted as (the plan:
//! binding, oversubscription, the tier refusal it fell from), what it
//! runs (the projected budget: seats and shares), and who is executing
//! (the worker census rows, with the lane-to-thread binding per
//! resource). Sourced from the construction-stamped boot (the pure plan
//! function re-derives it), else the live default-profile projection
//! ("fleet_booted" names the view).

use pyo3::prelude::*;
use pyo3::types::PyDict;

use degenbot_bot::arb_engine::fleet_status::fleet_runtime_status;

/// The runtime fleet status (FF-T5): budget, plan, census - a dict with
/// `fleet_booted`, `profile`, `quota_cpus`, `binding`,
/// `oversubscribed`, `tier_refused`, `budget` (the projected seat/share
/// table), and `census` (the worker rows, with the binding per
/// resource).
#[pyfunction]
#[must_use]
pub fn runtime_status(py: Python<'_>) -> Py<PyDict> {
    let status = fleet_runtime_status();
    let dict = PyDict::new(py);

    let _ = dict.set_item("fleet_booted", status.fleet_booted);
    let _ = dict.set_item("profile", profile_str(status.profile));
    let _ = dict.set_item("quota_cpus", status.quota_cpus);
    let _ = dict.set_item(
        "binding",
        match status.binding {
            Some(degenbot_workers::plan::Binding::Pinned) => "pinned",
            Some(degenbot_workers::plan::Binding::Serial) => "serial",
            None => "refused",
        },
    );
    let _ = dict.set_item("oversubscribed", status.oversubscribed);
    let _ = dict.set_item("tier_refused", status.tier_refused.clone());

    let budget = PyDict::new(py);
    if let Some(b) = status.budget.as_ref() {
        let _ = budget.set_item("quota_cpus", b.quota_cpus);
        let _ = budget.set_item("quota_floor", b.quota_floor);
        let _ = budget.set_item("reserve_cpus", b.reserve_cpus);
        let _ = budget.set_item("ambient_cpus", b.ambient_cpus);
        let _ = budget.set_item("resolve_cpus", b.resolve_cpus);
        let _ = budget.set_item("merge_cpus", b.merge_cpus);
        let _ = budget.set_item("solver_cpus", b.solver_cpus);
        let _ = budget.set_item("solver_pin_count", b.solver_pin_count);
        let _ = budget.set_item("sim_slot_cap", b.sim_slot_cap);
        let _ = budget.set_item("pool_state_updater_slots", b.pool_state_updater_slots);
    }
    let _ = dict.set_item("budget", budget);

    let census: Vec<Bound<'_, PyDict>> = status
        .census
        .iter()
        .map(|entry| {
            let row = PyDict::new(py);
            let _ = row.set_item("resource", entry.resource);
            let _ = row.set_item("kind", entry.kind);
            let _ = row.set_item("count", entry.count);
            let _ = row.set_item("thread_name", entry.thread_name);
            let _ = row.set_item("sizing", entry.sizing);
            let _ = row.set_item("binding", entry.binding);
            row
        })
        .collect();
    let _ = dict.set_item("census", census);

    dict.into()
}

fn profile_str(profile: degenbot_config::FleetProfile) -> &'static str {
    match profile {
        degenbot_config::FleetProfile::Auto => "auto",
        degenbot_config::FleetProfile::Pinned => "pinned",
        degenbot_config::FleetProfile::Serial => "serial",
    }
}
