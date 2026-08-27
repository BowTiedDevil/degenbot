//! Cache-lab seam (epic KIMRKS): instrumented CL-table cache strategies driven
//! by the `cl_cache_lab` example. Strategies refill crossing tables + word
//! profiles through the SAME production builders the bot uses and solve through
//! the production `int_solve_cl_path_cached` entry, so every measurement is of
//! the real code path (no parallel implementation).

use std::collections::HashMap;
use std::sync::Arc;

use alloy::primitives::U256;

use crate::mobius_v3_int::{
    build_cl_crossing_table, build_cl_word_profiles, ClCrossingTable, ClProfileTable,
    IntV3TickRangeSequence,
};

/// Rebuild accounting per strategy (solver-agnostic numbers a report can sum).
#[derive(Clone, Debug, Default)]
pub struct BuildCounters {
    /// Full crossing-table builds (`build_cl_crossing_table`).
    pub crossing_tables: u64,
    /// Dense word-profile table builds.
    pub profile_tables: u64,
    /// Sequence-level rebuilds (strategy-specific semantic).
    pub sequence_rebuilds: u64,
    /// Solves executed through a strategy's prepared tables.
    pub solves: u64,
}

/// What changed in the pool state between refills (the driver's event class).
#[derive(Clone, Debug)]
pub enum CacheEvent {
    /// The environment moved to a fresh state the strategy has never seen.
    Fresh,
    /// `sqrt_price_x96` of hop `hop`'s current range moved without crossing a
    /// tick (liquidity map untouched).
    PriceMove { hop: usize },
    /// A liquidity position changed on hop `hop` (structure event).
    Liquidity { hop: usize },
    /// The current tick re-anchored (structure event; window shifted).
    TickCross { hop: usize },
    /// State restored to an earlier snapshot.
    Restore,
}

/// Prepared per-hop tables a strategy hands to `int_solve_cl_path_cached`.
pub type PreparedHop = (Arc<ClCrossingTable>, Arc<ClProfileTable>);

/// One cache strategy in the lab catalog.
pub trait ClCacheStrategy {
    /// Printable strategy name.
    #[must_use]
    fn name(&self) -> &'static str;

    /// Refill (or reuse) the crossing/profile tables for the current state,
    /// given the event class that produced it. Returns tables parallel to
    /// `seqs` (one per hop).
    fn refill(&mut self, seqs: &[IntV3TickRangeSequence], event: &CacheEvent) -> Vec<PreparedHop>;

    /// Cumulative rebuild counters.
    #[must_use]
    fn counters(&self) -> &BuildCounters;
}

/// The registry the lab example enumerates: every strategy the catalog tests.
#[must_use]
pub fn strategy_catalog() -> Vec<Box<dyn ClCacheStrategy + Send>> {
    vec![
        Box::new(FullRebuild::default()),
        Box::new(FusedEpochCache::default()),
    ]
}

// ---------------------------------------------------------------------------
// S0 — full rebuild per solve (today's offline `int_solve_cl_path` shape)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct FullRebuild {
    counters: BuildCounters,
}

impl ClCacheStrategy for FullRebuild {
    fn name(&self) -> &'static str {
        "S0_full_rebuild"
    }

    fn refill(
        &mut self,
        _seqs: &[IntV3TickRangeSequence],
        _event: &CacheEvent,
    ) -> Vec<PreparedHop> {
        // The offline entry rebuilds inside the solve; model that cost so the
        // counter row is comparable with the caching arms.
        self.counters.crossing_tables += 1;
        self.counters.profile_tables += 1;
        self.counters.solves += 1;
        Vec::new() // sentinel: driver detects empty and uses int_solve_cl_path
    }

    fn counters(&self) -> &BuildCounters {
        &self.counters
    }
}

// ---------------------------------------------------------------------------
// S1 — fused epoch cache (today's production bot behaviour)
// ---------------------------------------------------------------------------

/// Rebuild a hop's tables only when its state changed since the last refill.
/// `last_state` is a structural hash of the hop's ranges (including price),
/// so ANY mutation re-projects that hop — the whole-pool nonce-equivalent.
#[derive(Default)]
pub struct FusedEpochCache {
    tables: HashMap<(usize, String), PreparedHop>,
    counters: BuildCounters,
}

impl ClCacheStrategy for FusedEpochCache {
    fn name(&self) -> &'static str {
        "S1_fused_epoch"
    }

    fn refill(&mut self, seqs: &[IntV3TickRangeSequence], _event: &CacheEvent) -> Vec<PreparedHop> {
        let mut out = Vec::with_capacity(seqs.len());
        for (i, seq) in seqs.iter().enumerate() {
            let key = state_key(seq);
            // Fused epoch policy: rebuild a hop only for a state it has never
            // seen; ANY mutation (price or structure) yields a new key. A
            // Restore key was already stored, so it hits without rebuilding.
            if let Some(t) = self.tables.get(&(i, key.clone())).cloned() {
                out.push(t);
                continue;
            }
            let crossings = Arc::new(build_cl_crossing_table(seq));
            let profiles = Arc::new(build_cl_word_profiles(seq));
            self.counters.crossing_tables += 1;
            self.counters.profile_tables += 1;
            self.counters.sequence_rebuilds += 1;
            self.tables
                .insert((i, key), (Arc::clone(&crossings), Arc::clone(&profiles)));
            out.push((crossings, profiles));
        }
        self.counters.solves += 1;
        out
    }

    fn counters(&self) -> &BuildCounters {
        &self.counters
    }
}

/// Cheap deterministic multi-line string key of a hop's ranges: includes
/// price, bounds, liquidity and word-boundary counts so any mutation differs.
fn state_key(seq: &IntV3TickRangeSequence) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    let push_part = |s: &mut String, v: &U256| {
        // format! hex is fine here — refill is not the measured hot path.
        let _ = write!(s, "{v:x};");
    };
    for r in &seq.ranges {
        push_part(&mut s, &r.sqrt_price_x96);
        push_part(&mut s, &r.sqrt_price_lower_x96);
        push_part(&mut s, &r.sqrt_price_upper_x96);
        let _ = write!(s, "{};{};{};", r.liquidity, r.gamma_numer, r.fee_denom);
        let _ = write!(s, "{};", r.word_boundary_prices.len());
    }
    s
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]
    use super::*;
    use crate::mobius_v3_int::IntV3TickRangeHop;

    fn two_range_seq() -> IntV3TickRangeSequence {
        let mk = |liq: u128, lo: U256, hi: U256, price: U256, zfo: bool| IntV3TickRangeHop {
            liquidity: liq,
            sqrt_price_x96: price,
            sqrt_price_lower_x96: lo,
            sqrt_price_upper_x96: hi,
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: zfo,
            word_boundary_prices: Vec::new(),
        };
        IntV3TickRangeSequence::new(vec![
            mk(
                1_000,
                U256::from(90u64),
                U256::from(110u64),
                U256::from(100u64),
                false,
            ),
            mk(
                1_100,
                U256::from(110u64),
                U256::from(130u64),
                U256::from(110u64),
                false,
            ),
        ])
        .unwrap()
    }

    #[test]
    fn fused_epoch_cache_reuses_tables_for_identical_state() {
        let seq = two_range_seq();
        let mut s = FusedEpochCache::default();
        let first = s.refill(std::slice::from_ref(&seq), &CacheEvent::Fresh);
        assert_eq!(s.counters().crossing_tables, 1);
        let second = s.refill(std::slice::from_ref(&seq), &CacheEvent::Restore);
        assert_eq!(s.counters().crossing_tables, 1, "identical state must hit");
        let p0 = std::sync::Arc::as_ptr(&first[0].0);
        let p1 = std::sync::Arc::as_ptr(&second[0].0);
        assert_eq!(p0, p1, "same allocation reused");
    }

    #[test]
    fn catalog_registers_baseline_and_fused_first() {
        let catalog = strategy_catalog();
        assert_eq!(catalog[0].name(), "S0_full_rebuild");
        assert_eq!(catalog[1].name(), "S1_fused_epoch");
        assert!(catalog.len() >= 2);
    }
}
