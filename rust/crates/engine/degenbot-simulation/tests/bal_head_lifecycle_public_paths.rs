//! The lifecycle's public surface is one definition reachable through two
//! paths: the module that owns it (`evm2::head_lifecycle`) and the
//! representation module that reexports it (`evm2::delta_snapshot`), which is
//! the path every existing consumer already names.
//!
//! This file is the external compile/use evidence for that split: it is a
//! separate crate from the library, so a moved or missing path is a build error
//! here rather than a surprise in a downstream consumer. The type ascriptions
//! are the second half of the evidence — each pair is the SAME type, never a
//! parallel redefinition that would type-check while breaking a consumer's own
//! types.

/// The owner module: the definition site.
mod owner {
    pub use degenbot_simulation::sim::evm2::head_lifecycle::{
        BalHeadDriver, HeadCold, HeadCompletion, HeadOutcome, HeadReadiness, HeadStats, HeadTicket,
        HeadView,
    };
}

/// The reexport: the path every consumer already names.
mod reexported {
    pub use degenbot_simulation::sim::evm2::delta_snapshot::{
        BalHeadDriver, HeadCold, HeadCompletion, HeadOutcome, HeadReadiness, HeadStats, HeadTicket,
        HeadView,
    };
}

/// Every public lifecycle type is reachable at its historical
/// `evm2::delta_snapshot` path and at its owning `evm2::head_lifecycle` path, and
/// the two names denote the same type — the split moved no consumer's import.
#[test]
fn the_lifecycle_types_are_one_definition_behind_both_public_paths() {
    fn same<T>(value: Option<T>) -> Option<T> {
        value
    }

    fn to_reachable(value: Option<reexported::BalHeadDriver>) -> Option<owner::BalHeadDriver> {
        value
    }

    let _: Option<owner::BalHeadDriver> = same::<reexported::BalHeadDriver>(None);
    let _: Option<owner::HeadCold> = same::<reexported::HeadCold>(None);
    let _: Option<owner::HeadCompletion> = same::<reexported::HeadCompletion>(None);
    let _: Option<owner::HeadOutcome> = same::<reexported::HeadOutcome>(None);
    let _: Option<owner::HeadReadiness> = same::<reexported::HeadReadiness>(None);
    let _: Option<owner::HeadStats> = same::<reexported::HeadStats>(None);
    let _: Option<owner::HeadTicket> = same::<reexported::HeadTicket>(None);
    let _: Option<owner::HeadView> = same::<reexported::HeadView>(None);

    let _: Option<owner::BalHeadDriver> = to_reachable(None);
}

/// The driver's whole caller-facing surface is nameable through the reexported
/// path: the synchronous entry points are taken as function POINTERS (so a
/// moved, renamed, or reshaped method fails to compile here), and the
/// asynchronous ones are called from one generic-free async fn whose type IS the
/// evidence.
#[test]
fn the_reexported_driver_offers_the_whole_public_surface() {
    use alloy::primitives::{Address, B256};
    use degenbot_rpc::provider::AlloyProvider;
    use degenbot_simulation::sim::evm2::delta_snapshot::{
        BalHeadDriver, DeltaReport, HeadCold, HeadCompletion, HeadOutcome, HeadReadiness,
        HeadStats, HeadTicket, HeadView, RelevanceSet, Retention,
    };
    use degenbot_simulation::sim::evm2::read_hub::PreparedBal;
    use degenbot_simulation::sim::ReadAnchor;
    use std::sync::Arc;

    let new: fn(&AlloyProvider) -> BalHeadDriver = BalHeadDriver::new;
    let with_config: fn(&AlloyProvider, u32, Retention) -> BalHeadDriver =
        BalHeadDriver::with_config;
    let retention: fn(&BalHeadDriver) -> Retention = BalHeadDriver::retention;
    let max_depth: fn(&BalHeadDriver) -> u32 = BalHeadDriver::max_depth;
    let deliver: fn(&BalHeadDriver, ReadAnchor, B256) -> HeadTicket = BalHeadDriver::deliver;
    let apply: fn(&BalHeadDriver, HeadTicket, &PreparedBal, &[Address]) -> HeadOutcome =
        BalHeadDriver::apply;
    let apply_members: fn(&BalHeadDriver, HeadTicket, &PreparedBal, &RelevanceSet) -> HeadOutcome =
        BalHeadDriver::apply_members;
    let snapshot: fn(&BalHeadDriver, ReadAnchor) -> HeadView = BalHeadDriver::snapshot;
    let completion: fn(&BalHeadDriver, ReadAnchor) -> Option<HeadCompletion> =
        BalHeadDriver::completion;
    let stats: fn(&BalHeadDriver) -> HeadStats = BalHeadDriver::stats;
    let delivered: fn(&BalHeadDriver) -> Option<ReadAnchor> = BalHeadDriver::delivered;
    let installed: fn(&BalHeadDriver) -> Option<ReadAnchor> = BalHeadDriver::installed;
    let previous: fn(&BalHeadDriver) -> Option<ReadAnchor> = BalHeadDriver::previous;
    let last_report: fn(&BalHeadDriver) -> Option<Arc<DeltaReport>> = BalHeadDriver::last_report;
    let is_ready: fn(&HeadReadiness) -> bool = HeadReadiness::is_ready;
    let cold_reason: for<'a> fn(&'a HeadReadiness) -> Option<&'a HeadCold> =
        HeadReadiness::cold_reason;

    let _ = (
        new,
        with_config,
        retention,
        max_depth,
        deliver,
        apply,
        apply_members,
        snapshot,
        completion,
        stats,
        delivered,
        installed,
        previous,
        last_report,
        is_ready,
        cold_reason,
        async_surface,
    );
}

/// The three asynchronous entry points, called through the reexported path: the
/// await-ed return types are what a caller's own signatures name.
async fn async_surface(
    driver: &degenbot_simulation::sim::evm2::delta_snapshot::BalHeadDriver,
    ticket: degenbot_simulation::sim::evm2::delta_snapshot::HeadTicket,
    anchor: degenbot_simulation::sim::ReadAnchor,
) {
    use degenbot_simulation::sim::evm2::delta_snapshot::{HeadOutcome, HeadView};
    use degenbot_simulation::sim::evm2::read_hub::PreparedBal;

    let _: PreparedBal = driver.prepare(ticket).await;
    let _: HeadOutcome = driver.advance(ticket, &[]).await;
    let _: HeadView = driver.wait_for_head(anchor).await;
}
