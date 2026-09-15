//! FF-T5 (NT7HJC): the runtime fleet status - budget, plan, census.
//!
//! `degenbot.runtime_status()` (the pyo3 leaf reads this) answers the
//! operator's first three questions about a live process: what did the
//! fleet boot as (the plan: binding, oversubscription, the tier refusal
//! it fell from), what does it run (the projected budget: seats and
//! shares), and who is executing (the worker census rows, with the
//! lane-to-thread binding per resource).
//!
//! Sources of truth, in order: the CONSTRUCTION-STAMPED boot (YI5NGB -
//! the boot the first engine construction derived from its own config;
//! every later construction rides it), else the LIVE-detected quota with
//! the default profile (the pre-construction projection - the pure plan
//! function re-derives what the host WOULD run; `fleet_booted` names
//! which view it is).
use degenbot_config::FleetProfile;
use degenbot_core::op_warn;
use degenbot_workers::budget::FleetBudget;
use degenbot_workers::dispatcher::FleetBoot;
use degenbot_workers::plan::{Binding, FleetPlan};
use std::sync::OnceLock;
/// The resolved runtime status: the plan + the projected budget + the
/// census, from the stamped boot or the live default projection.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetRuntimeStatus {
    /// Whether an engine CONSTRUCTED and stamped a fleet boot (the
    /// authoritative view); false = the live default-profile projection
    /// (pre-construction).
    pub fleet_booted: bool,
    /// The boot's profile (auto / pinned / serial).
    pub profile: FleetProfile,
    /// The boot's quota (cores) - stamped at construction, else
    /// live-detected.
    pub quota_cpus: f64,
    /// The resolved lane-to-thread binding. None = the boot is
    /// UNHOSTABLE (the plan refused: below the host floor) - the
    /// `tier_refused` string names why.
    pub binding: Option<Binding>,
    /// Whether the projection runs marked-oversubscribed (a forced
    /// pinned profile on a sub-floor host).
    pub oversubscribed: bool,
    /// The tier refusal the plan fell from, rendered `NAME: message` — the
    /// closed family name (`BudgetError::name()`) is the greppable
    /// vocabulary, the Display sentence keeps the detected quota, the
    /// floor, and the hint. A serial-tier host carries the
    /// `QuotaTooSmallForPinnedRoles` that placed it there; a forced-pinned
    /// oversubscribed host carries the pinned-floor refusal it overrode
    /// (FF-T5 addendum, 452GZC). None when the plan refused nothing — or
    /// the boot plan itself refused (binding None).
    pub tier_refused: Option<String>,
    /// The projected budget the fleet runs (the plan's projection).
    /// None on a refused boot.
    pub budget: Option<FleetBudget>,
    /// The worker census rows (who is executing, with the binding per
    /// resource).
    pub census: Vec<degenbot_core::worker_census::WorkerCensusEntry>,
}
/// Compute the runtime status (FF-T5): budget, plan, census - from the
/// stamped boot when an engine constructed, else the live
/// default-profile projection (`fleet_booted` names the view).
///
/// # Panics
///
/// Never in practice: the stamped boot already BOOTED (its plan is
/// legal by construction), and the live default projection derives
/// from the detected quota (above the host floor whenever a fleet can
/// boot at all). The expects name the invariant, not a
/// caller-reachable arm.
#[must_use]
pub fn fleet_runtime_status() -> FleetRuntimeStatus {
    let registry = crate::arb_engine::seat_host::FleetBootRegistry::process();
    // candidate 4 (YUMQU3): the canonical process boot is the registry's
    // first-wins latch — whichever registry role (sim/registration) installed
    // first — never a role module's private static.
    let boot: FleetBoot = registry.process_boot().unwrap_or_else(live_default_boot);
    let fleet_booted = registry.boot_installed();
    // The status NEVER panics: an unhostable boot (below the host floor
    // - possible only on the pre-construction live view) is DATA (the
    // refused view: no binding, no budget, the refusal string).
    let plan: Option<(FleetPlan, FleetBudget)> =
        degenbot_workers::plan::plan(boot.quota_cpus, boot.profile, &boot.overrides)
            .ok()
            .and_then(|plan| {
                plan.projected_budget(&boot.overrides)
                    .ok()
                    .map(|budget| (plan, budget))
            });
    let (binding, oversubscribed, tier_refused, budget) = match plan {
        Some((plan, budget)) => (
            Some(plan.binding),
            plan.oversubscribed,
            tier_refused_string(&plan),
            Some(budget),
        ),
        None => (None, false, None, None),
    };
    FleetRuntimeStatus {
        fleet_booted,
        profile: boot.profile,
        quota_cpus: boot.quota_cpus,
        binding,
        oversubscribed,
        tier_refused,
        budget,
        census: degenbot_core::worker_census::snapshot(),
    }
}
/// The `tier_refused` string (FF-T5 addendum, 452GZC): the typed refusal's
/// closed family NAME plus its Display sentence — an operator (and the
/// test gates) greps the family; the sentence keeps the detected quota,
/// the floor, and the hint.
fn tier_refused_string(plan: &FleetPlan) -> Option<String> {
    plan.tier_refusal
        .as_ref()
        .map(|refusal| format!("{}: {refusal}", refusal.name()))
}
/// The pre-construction view: the LIVE-detected quota with the default
/// profile (the pure plan function projects what the host WOULD run).
fn live_default_boot() -> FleetBoot {
    FleetBoot {
        profile: FleetProfile::Auto,
        quota_cpus: degenbot_workers::quota::fractional_cpu_budget(),
        overrides: degenbot_workers::budget::BudgetOverrides::default(),
        posture: degenbot_workers::posture::PosturePolicy::doc_defaults(),
        owner: None,
    }
}
/// The process's resolved fleet profile summary (FF-T5, NT7HJC): the
/// one-line answer to "what did the fleet boot as", recorded ONCE at the
/// construction-stamp install (first engine wins, like every stance
/// static) and exported as the `degenbot_fleet_profile` metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FleetProfileSummary {
    /// The boot's profile (auto / pinned / serial).
    pub profile: &'static str,
    /// The resolved lane-to-thread binding (pinned / serial / refused).
    pub binding: &'static str,
    /// Whether the projection runs marked-oversubscribed.
    pub oversubscribed: bool,
}
/// The process-global profile summary (set once, at the stamp install).
static FLEET_PROFILE_SUMMARY: OnceLock<FleetProfileSummary> = OnceLock::new();
/// Record the profile summary at the construction-stamp install (FF-T5):
/// resolve the plan for the stamped boot (pure), set the process-global
/// summary (first writer wins - `install_boot` never overrides), export it
/// through the instruments (the `degenbot_fleet_profile` metric; absent
/// pre-pipeline, recorded at build if the pipeline came first), and fire
/// the PRODUCTION ALERT when a production host resolves to serial (the
/// 2-5-core tier: a degradation signal the operator must see, never a
/// silent narrow - the small-host arm is real, and its use is LOUD).
pub fn record_fleet_profile_at_install(boot: &FleetBoot) {
    let summary = match degenbot_workers::plan::plan(boot.quota_cpus, boot.profile, &boot.overrides)
    {
        Ok(plan) => FleetProfileSummary {
            profile: profile_label(boot.profile),
            binding: match plan.binding {
                degenbot_workers::plan::Binding::Pinned => "pinned",
                degenbot_workers::plan::Binding::Serial => "serial",
            },
            oversubscribed: plan.oversubscribed,
        },
        Err(_) => FleetProfileSummary {
            profile: profile_label(boot.profile),
            binding: "refused",
            oversubscribed: false,
        },
    };
    let summary = *FLEET_PROFILE_SUMMARY.get_or_init(|| summary);
    crate::instruments::note_fleet_profile(&summary);
    if summary.binding == "serial" {
        op_warn!(domain = solver, profile = summary.profile,
            quota_cpus = boot.quota_cpus,
            "PRODUCTION ALERT: this host resolved to the SERIAL tier              (2-5 cores: one cycle lane per host) - the small-host arm is a              degradation signal, never a silent narrow (FF-T5, NT7HJC)"
        );
    }
}
/// The process's resolved profile summary (None before the first engine
/// construction stamped a boot).
#[must_use]
pub fn fleet_profile_summary() -> Option<FleetProfileSummary> {
    FLEET_PROFILE_SUMMARY.get().copied()
}
fn profile_label(profile: FleetProfile) -> &'static str {
    match profile {
        FleetProfile::Auto => "auto",
        FleetProfile::Pinned => "pinned",
        FleetProfile::Serial => "serial",
    }
}
#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use degenbot_workers::budget::BudgetError;
    /// The `tier_refused` string composes the family NAME with the floor
    /// sentence: greppable (`QuotaTooSmallForPinnedRoles ...`) AND
    /// explanatory (the detected quota and the floor ride). No refusal, no
    /// string.
    #[test]
    fn the_tier_refused_string_names_the_family_it_fell_from() {
        let refused = FleetPlan {
            id: degenbot_workers::plan::PLAN_ID,
            binding: Binding::Pinned,
            oversubscribed: true,
            budget_cpus: 4.0,
            tier_refusal: Some(BudgetError::QuotaTooSmallForPinnedRoles {
                quota: 4.0,
                required: 6,
            }),
        };
        let rendered = tier_refused_string(&refused).expect("the refusal renders");
        assert!(
            rendered.contains("QuotaTooSmallForPinnedRoles"),
            "the string names the typed family: {rendered}"
        );
        assert!(
            rendered.contains("pinned-role floor"),
            "the string keeps the floor sentence: {rendered}"
        );
        let clean = FleetPlan {
            tier_refusal: None,
            ..refused
        };
        assert!(tier_refused_string(&clean).is_none());
    }
    /// candidate4 pin 2 - RED at HEAD (the registry does not exist yet;
    /// `fleet_runtime_status` reads `fleet_registration_executor`'s module
    /// statics). TARGET (post-T2): `fleet_status` reads the ONE
    /// `seat_host::FleetBootRegistry`; whichever role installs first owns the
    /// canonical process boot, stamped exactly once, and
    /// `fleet_runtime_status()` reports the SAME boot facts regardless of
    /// install order.
    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "exact equality is intended: both sides read the same canonical FleetBoot"
    )]
    fn candidate4_runtime_status_ignores_install_order_first_wins_once() {
        let registry = crate::arb_engine::seat_host::FleetBootRegistry::process();
        // The keyed accessors expose each role's descriptor (the boot stamp
        // rides `BootRole`), so the registry owns both rows.
        assert_eq!(
            registry.sim().descriptor().boot_role,
            crate::arb_engine::boot_stamp::BootRole::Sim
        );
        assert_eq!(
            registry.registration().descriptor().boot_role,
            crate::arb_engine::boot_stamp::BootRole::Registration
        );
        // First-wins, stamped exactly once: the registry's canonical boot is
        // the SAME value `fleet_runtime_status()` reports - never the
        // registration module's private static.
        let status = fleet_runtime_status();
        assert_eq!(
            status.fleet_booted,
            registry.boot_installed(),
            "the runtime status must read the registry's first-wins latch"
        );
        if let Some(canonical) = registry.process_boot() {
            assert_eq!(status.quota_cpus, canonical.quota_cpus);
            assert_eq!(status.profile, canonical.profile);
        }
    }
    /// ADAPTER pin: pins the `runtime_status` PRG-5 gate semantics
    /// source-independently (both sides must read the same owner after the
    /// reroute): the status
    /// `fleet_booted` mirrors the registration-boot latch, the projected
    /// budget co-varies with the resolved binding, and the `tier_refused`
    /// renderer keeps the typed family NAME + floor sentence (the port of the
    /// existing `the_tier_refused_string_names_the_family_it_fell_from`).
    #[test]
    fn candidate4_preserve_runtime_status_semantics() {
        // PRG-5 gate: `fleet_runtime_status().fleet_booted` mirrors the
        // process registration-boot latch, pre- and post-install.
        let gate = crate::arb_engine::fleet_intake::registration_boot_installed();
        let status = fleet_runtime_status();
        assert_eq!(
            status.fleet_booted, gate,
            "the runtime status must mirror the PRG-5 registration-boot gate"
        );
        // A resolved binding implies a projected budget; a refused plan
        // carries neither (the status never panics on an unhostable view).
        assert_eq!(
            status.binding.is_some(),
            status.budget.is_some(),
            "binding and projected budget co-vary"
        );
        // The tier-refusal renderer (ported so it survives the T2 reroute).
        let refused = FleetPlan {
            id: degenbot_workers::plan::PLAN_ID,
            binding: Binding::Pinned,
            oversubscribed: true,
            budget_cpus: 4.0,
            tier_refusal: Some(BudgetError::QuotaTooSmallForPinnedRoles {
                quota: 4.0,
                required: 6,
            }),
        };
        let rendered = tier_refused_string(&refused).expect("the refusal renders");
        assert!(
            rendered.contains("QuotaTooSmallForPinnedRoles"),
            "the string names the typed family: {rendered}"
        );
        assert!(
            rendered.contains("pinned-role floor"),
            "the string keeps the floor sentence: {rendered}"
        );
        let clean = FleetPlan {
            tier_refusal: None,
            ..refused
        };
        assert!(tier_refused_string(&clean).is_none());
    }
}
