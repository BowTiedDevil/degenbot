//! The quarantine gate: one pool id excluded from solve resolution.
//!
//! Quarantine sits in the projection dispatcher rather than in each family's
//! state type, so a tainted surface is excluded by one set regardless of which
//! design the pool registered under. Every transition bumps the pool's
//! `state_nonce` — the gate's whole value is that a cached hop projection or an
//! in-flight solver snapshot taken before the transition cannot be used after
//! it, and the nonce is what invalidates those.
//!
//! Maintains the `degenbot.engine.quarantined_pools` scrape gauge so the
//! excluded set is observable without reading state.

use super::BotState;

impl BotState {
    /// ADR-040 quarantine seam: exclude the pool from solve resolution
    /// (tainted surface containment). Idempotent; first transition bumps the
    /// pool's `state_nonce` so every cached projection + in-flight solver
    /// snapshot invalidates. Returns `true` when this call CHANGED the state
    /// (a `false` return is a no-op or an unknown pool id — callers log).
    /// Maintains the `degenbot.engine.quarantined_pools` scrape gauge.
    pub fn quarantine_pool(&mut self, pool_id: u64) -> bool {
        if !self.registry.pools.contains_key(&pool_id) {
            return false;
        }
        if !self.registry.quarantined_pools.insert(pool_id) {
            return false;
        }
        if let Some(entry) = self.registry.pools.get_mut(&pool_id) {
            entry.bump_state_nonce();
        }
        if let Some(p) = crate::telemetry_port::pipeline() {
            p.set_quarantined_pools(self.registry.quarantined_pools.len());
        }
        true
    }

    /// ADR-040 quarantine release: re-admit the pool to solve resolution and
    /// dirty its nonce so stale `Invalid(Quarantined)` cache entries cannot
    /// stick. Returns `true` when this call CHANGED the state.
    pub fn release_pool(&mut self, pool_id: u64) -> bool {
        if !self.registry.quarantined_pools.remove(&pool_id) {
            return false;
        }
        if let Some(entry) = self.registry.pools.get_mut(&pool_id) {
            entry.bump_state_nonce();
        }
        if let Some(p) = crate::telemetry_port::pipeline() {
            p.set_quarantined_pools(self.registry.quarantined_pools.len());
        }
        true
    }

    /// ADR-040: is the pool currently quarantined (excluded from solve)?
    #[must_use]
    pub fn is_pool_quarantined(&self, pool_id: u64) -> bool {
        self.registry.quarantined_pools.contains(&pool_id)
    }

    /// ADR-040: current quarantine depth (the scrape gauge's backing count).
    #[must_use]
    pub fn quarantined_pool_count(&self) -> usize {
        self.registry.quarantined_pools.len()
    }
}
