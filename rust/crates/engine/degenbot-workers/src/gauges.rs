//! Per-role busy/idle gauge surface : the activation
//! dashboard's fleet source, plus the `instruments.rs`-mirror seam.
//!
//! Same hook pattern as `degenbot_core::worker_census`: a plain function
//! pointer installed once by the layer above (the bot's `instruments.rs`
//! builds the `OTel` gauges and counts role occupancy from these samples —
//! the Prometheus families `degenbot_fleet_role_busy_total` /
//! `degenbot_fleet_role_workers{state}` render from THIS shape, so the
//! dashboard never grows a second fleet source). The host re-exports after
//! every transition batch.

use std::sync::OnceLock;

use crate::role::WorkerRole;
use crate::role::ALL_ROLES;

/// One per-role occupancy sample (per-state slot counts). `busy` =
/// everything that is not idle — leased, running, pinned, or draining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleGaugeSample {
    /// The role this row describes.
    pub role: WorkerRole,
    /// Slots parked: no lease, zero CPU.
    pub idle: u64,
    /// Slots with a granted dispatch not yet started on the unit.
    pub leased: u64,
    /// Slots executing a unit.
    pub running: u64,
    /// Slots holding a steady across-cycle pin (warm arenas intact).
    pub pinned: u64,
    /// Slots completing their in-flight unit under shed/cordon.
    pub draining: u64,
}

impl RoleGaugeSample {
    /// Busy occupancy — the activation dashboard's headline per role.
    #[must_use]
    pub const fn busy(&self) -> u64 {
        self.leased + self.running + self.pinned + self.draining
    }

    /// Total slots observed for this role (`busy + idle` — the gauge pair
    /// the activation dashboard plots).
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.busy() + self.idle
    }
}

type DashboardHook = fn(&[RoleGaugeSample]);

static DASHBOARD_HOOK: OnceLock<DashboardHook> = OnceLock::new();

/// Install the gauge exporter (called above this crate by instruments.rs at
/// metrics init, BEFORE any host boots). First installation wins — the
/// fleet exports through the bot layer, it does not replace it.
/// Returns `true` when THIS call installed the hook (first wins).
pub fn set_dashboard_hook(hook: DashboardHook) -> bool {
    DASHBOARD_HOOK.set(hook).is_ok()
}

/// Re-export the full per-role table through the installed hook. No-op
/// until the hook is installed (one branch per observation, same
/// discipline as the census).
pub fn export_dashboard(samples: &[RoleGaugeSample]) {
    if let Some(hook) = DASHBOARD_HOOK.get() {
        hook(samples);
    }
}

/// The full 8-role table from per-state counts, indexed by role position in
/// [`ALL_ROLES`] (idle counts follow the slot's HOME role so idle fleet
/// slots still appear on the dashboard).
#[must_use]
pub fn sample_table(
    idle: &[u64; 8],
    leased: &[u64; 8],
    running: &[u64; 8],
    pinned: &[u64; 8],
    draining: &[u64; 8],
) -> Vec<RoleGaugeSample> {
    ALL_ROLES
        .iter()
        .enumerate()
        .map(|(i, role)| RoleGaugeSample {
            role: *role,
            idle: idle[i],
            leased: leased[i],
            running: running[i],
            pinned: pinned[i],
            draining: draining[i],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_is_everything_not_idle() {
        let s = RoleGaugeSample {
            role: WorkerRole::Solver,
            idle: 2,
            leased: 1,
            running: 3,
            pinned: 4,
            draining: 2,
        };
        assert_eq!(s.busy(), 10);
        assert_eq!(s.total(), 12);
    }

    #[test]
    fn sample_table_covers_all_eight_roles_in_table_order() {
        let table = sample_table(
            &[1, 2, 3, 4, 0, 0, 0, 0],
            &[0; 8],
            &[0; 8],
            &[0; 8],
            &[0; 8],
        );
        assert_eq!(table.len(), ALL_ROLES.len());
        for (row, role) in table.iter().zip(ALL_ROLES) {
            assert_eq!(row.role, role);
        }
    }

    #[test]
    fn dashboard_hook_receives_the_full_busy_idle_table() {
        static SEEN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        fn hook(samples: &[RoleGaugeSample]) {
            let _ = SEEN.set(samples.len());
        }
        // The hook is process-global first-wins: assert the funnel only
        // when THIS test installed it (parallel sibling tests may have won).
        if set_dashboard_hook(hook) {
            let table = sample_table(&[1; 8], &[0; 8], &[0; 8], &[0; 8], &[0; 8]);
            export_dashboard(&table);
            assert_eq!(SEEN.get(), Some(&8), "hook receives all 8 role rows");
        }
        // The busy/idle gauge PAIR exists for every role (the activation
        // dashboard's requirement).
        let table = sample_table(&[1; 8], &[0; 8], &[0; 8], &[0; 8], &[0; 8]);
        for row in &table {
            assert_eq!(row.busy() + row.idle, row.total());
        }
    }
}
