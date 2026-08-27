// Cache-lab seam (epic KIMRKS): instrumented CL-table cache strategies driven
// by the `cl_cache_lab` example. Strategies refill crossing tables + word
// profiles through the SAME production builders the bot uses and solve through
// the production `int_solve_cl_path_cached` entry, so every measurement is of
// the real code path (no parallel implementation).

#![expect(
    clippy::expect_used,
    clippy::must_use_candidate,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::type_complexity
)]

use hashbrown::HashMap;
use std::sync::Arc;

use alloy::primitives::U256;

use crate::mobius_v3_int::{
    build_cl_crossing_table, build_cl_word_profiles, build_cl_word_profiles_from_crossings,
    ClCrossingTable, ClProfileTable, IntTickRangeCrossing, IntV3TickRangeHop,
    IntV3TickRangeSequence,
};

#[derive(Clone, Debug, Default)]
pub struct BuildCounters {
    pub crossing_tables: u64,
    pub profile_tables: u64,
    pub sequence_rebuilds: u64,
    pub partial_rebuilds: u64,
    pub solves: u64,
}

#[derive(Clone, Debug)]
pub enum CacheEvent {
    Fresh,
    PriceMove { hop: usize },
    Liquidity { hop: usize, range: usize },
    TickCross { hop: usize },
    Restore,
}

pub type PreparedHop = (Arc<ClCrossingTable>, Arc<ClProfileTable>);

pub trait ClCacheStrategy {
    fn name(&self) -> &'static str;
    fn refill(&mut self, seqs: &[IntV3TickRangeSequence], event: &CacheEvent) -> Vec<PreparedHop>;
    fn counters(&self) -> &BuildCounters;
}

pub fn strategy_catalog() -> Vec<Box<dyn ClCacheStrategy + Send>> {
    vec![
        Box::new(FullRebuild::default()),
        Box::new(FusedEpochCache::default()),
        Box::new(PriceOverlayPatch::default()),
        Box::new(SegmentPrefixCache::default()),
        Box::new(DirtySuffixSegmentCache::default()),
        Box::new(SeqMemoProbe::default()),
        Box::new(ProfileSplitCache::default()),
        Box::new(CompositeSplitCache::default()),
    ]
}

fn state_key(seq: &IntV3TickRangeSequence) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    for r in &seq.ranges {
        let _ = write!(
            s,
            "{};{};{};{};{};{};",
            r.sqrt_price_x96,
            r.sqrt_price_lower_x96,
            r.sqrt_price_upper_x96,
            r.liquidity,
            r.gamma_numer,
            r.word_boundary_prices.len()
        );
    }
    s
}

fn struct_key(seq: &IntV3TickRangeSequence) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    for (i, r) in seq.ranges.iter().enumerate() {
        if i != 0 {
            let _ = write!(s, "{};", r.sqrt_price_x96);
        }
        let _ = write!(
            s,
            "{};{};{};{};{};",
            r.sqrt_price_lower_x96,
            r.sqrt_price_upper_x96,
            r.liquidity,
            r.gamma_numer,
            r.word_boundary_prices.len()
        );
    }
    s
}

fn ending_range(seq: &IntV3TickRangeSequence, k: usize) -> IntV3TickRangeHop {
    let r = &seq.ranges[k];
    if k == 0 {
        return r.clone();
    }
    let zfo = seq.ranges[0].zero_for_one;
    let entry = if zfo {
        seq.ranges[k - 1].sqrt_price_lower_x96
    } else {
        seq.ranges[k - 1].sqrt_price_upper_x96
    };
    IntV3TickRangeHop {
        liquidity: r.liquidity,
        sqrt_price_x96: entry,
        sqrt_price_lower_x96: r.sqrt_price_lower_x96,
        sqrt_price_upper_x96: r.sqrt_price_upper_x96,
        gamma_numer: r.gamma_numer,
        fee_denom: r.fee_denom,
        zero_for_one: r.zero_for_one,
        word_boundary_prices: r.word_boundary_prices.clone(),
    }
}

fn range_segment(seq: &IntV3TickRangeSequence, i: usize) -> Option<(U256, U256)> {
    let r = ending_range(seq, i);
    let scratch = IntV3TickRangeSequence::new(vec![r.clone(), r]).ok()?;
    let c = scratch.compute_crossing(1)?;
    Some((c.crossing_gross_input, c.crossing_output))
}

fn assemble_crossings(
    seq: &IntV3TickRangeSequence,
    segs: &[(U256, U256)],
) -> Vec<IntTickRangeCrossing> {
    let n = seq.ranges.len();
    let mut gross = U256::ZERO;
    let mut output = U256::ZERO;
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        if k > 0 {
            let (g, o) = segs[k - 1];
            gross = gross.saturating_add(g);
            output = output.saturating_add(o);
        }
        out.push(IntTickRangeCrossing {
            crossing_gross_input: gross,
            crossing_output: output,
            ending_range: ending_range(seq, k),
        });
    }
    out
}

/// Exactness guard for the segment-patch path (PriceMove/Liquidity reuse).
///
/// The reassembled crossing table is byte-identical to a fresh
/// `compute_crossing` walk only when every reused cached segment still
/// describes a crossing of the CURRENT ranges. The cached table carries its
/// own shape record — `ending_range[k]` is range k with its bounds, fee, and
/// word-boundary listing; only the range-0 current price and the liquidity of
/// `liquidity_may_differ` (the just-jittered range, whose segment the caller
/// re-derives) may mislead. Compare all of it; any mismatch means the cache
/// was built against a differently-mutated sequence (e.g. carried over from a
/// previous path on the per-hop cache) and every prefix would inherit foreign
/// segments, so the caller must rebuild.
fn crossings_match_shape(
    seq: &IntV3TickRangeSequence,
    cached: &[IntTickRangeCrossing],
    liquidity_may_differ: Option<usize>,
) -> bool {
    if cached.len() != seq.ranges.len() {
        return false;
    }
    cached
        .iter()
        .zip(&seq.ranges)
        .enumerate()
        .all(|(k, (c, r))| {
            let er = &c.ending_range;
            let shape_ok = er.sqrt_price_lower_x96 == r.sqrt_price_lower_x96
                && er.sqrt_price_upper_x96 == r.sqrt_price_upper_x96
                && er.gamma_numer == r.gamma_numer
                && er.fee_denom == r.fee_denom
                && er.zero_for_one == r.zero_for_one
                && er.word_boundary_prices == r.word_boundary_prices;
            let liq_ok = er.liquidity == r.liquidity || liquidity_may_differ == Some(k);
            // Range 0's entry price must match too — a PriceMove on a prior
            // state leaves a stale seg[0] that would offset every prefix.
            let price_ok = k != 0 || er.sqrt_price_x96 == r.sqrt_price_x96;
            shape_ok && liq_ok && price_ok
        })
}

/// Rebuild the single profile at index `k` from that index's crossing entry.
fn rebuild_profile_at(
    crossings: &IntTickRangeCrossing,
    k: usize,
    old: Vec<Option<Arc<crate::mobius_v3_int::V3WordProfile>>>,
) -> Vec<Option<Arc<crate::mobius_v3_int::V3WordProfile>>> {
    let mut table = old;
    if k >= table.len() {
        return table;
    }
    let single = build_cl_word_profiles_from_crossings(std::slice::from_ref(crossings));
    if let Some(newp) = single.into_iter().next() {
        table[k] = newp;
    }
    table
}

fn rebuild_range0_profile(
    crossings: &IntTickRangeCrossing,
    old: Vec<Option<Arc<crate::mobius_v3_int::V3WordProfile>>>,
) -> Vec<Option<Arc<crate::mobius_v3_int::V3WordProfile>>> {
    rebuild_profile_at(crossings, 0, old)
}

fn profile_key(seq: &IntV3TickRangeSequence, k: usize) -> String {
    use std::fmt::Write as _;
    let r = ending_range(seq, k);
    let mut s = String::with_capacity(48);
    let _ = write!(
        s,
        "{};{};{};{};{};{};",
        r.sqrt_price_x96,
        r.sqrt_price_lower_x96,
        r.sqrt_price_upper_x96,
        r.liquidity,
        r.gamma_numer,
        r.word_boundary_prices.len()
    );
    s
}

fn add_delta_dir(cur: U256, old: U256, new: U256) -> U256 {
    if new >= old {
        cur.saturating_add(new - old)
    } else {
        cur.saturating_sub(old - new)
    }
}

fn fresh_tables(
    seq: &IntV3TickRangeSequence,
    counters: &mut BuildCounters,
) -> (PreparedHop, Vec<(U256, U256)>) {
    let table = build_cl_crossing_table(seq);
    let mut segs = Vec::with_capacity(table.len().saturating_sub(1));
    for (k, c) in table.iter().enumerate().skip(1) {
        let prev = &table[k - 1];
        segs.push((
            c.crossing_gross_input
                .saturating_sub(prev.crossing_gross_input),
            c.crossing_output.saturating_sub(prev.crossing_output),
        ));
    }
    counters.crossing_tables += 1;
    counters.profile_tables += 1;
    let crossings = Arc::new(table);
    let profiles = Arc::new(build_cl_word_profiles(seq));
    ((Arc::clone(&crossings), profiles), segs)
}

// ---------------------------------------------------------------------------
// S0 full rebuild
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
        self.counters.crossing_tables = self.counters.crossing_tables.saturating_add(1);
        self.counters.profile_tables = self.counters.profile_tables.saturating_add(1);
        self.counters.solves += 1;
        Vec::new()
    }

    fn counters(&self) -> &BuildCounters {
        &self.counters
    }
}

// ---------------------------------------------------------------------------
// S1 fused epoch
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// S2 price overlay patch: PriceMove -> clone table + patch suffix by the
// range-0 crossing delta (constant add over all entries k>=1).
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct PriceOverlayPatch {
    hops: HashMap<usize, (String, String, PreparedHop)>,
    counters: BuildCounters,
}

impl ClCacheStrategy for PriceOverlayPatch {
    fn name(&self) -> &'static str {
        "S2_price_overlay_patch"
    }

    fn refill(&mut self, seqs: &[IntV3TickRangeSequence], event: &CacheEvent) -> Vec<PreparedHop> {
        let mut out = Vec::with_capacity(seqs.len());
        for (i, seq) in seqs.iter().enumerate() {
            let key = state_key(seq);
            if let Some((fk, _sk, t)) = self.hops.get(&i) {
                if *fk == key {
                    out.push(t.clone());
                    continue;
                }
            }
            let mut done = None;
            if let Some((_fk, sk, t)) = self.hops.get(&i).cloned() {
                if matches!(event, CacheEvent::PriceMove { hop } if *hop == i)
                    && sk == struct_key(seq)
                {
                    if let Some(c1) = seq.compute_crossing(1) {
                        let new_g = c1.crossing_gross_input;
                        let new_o = c1.crossing_output;
                        let old_g = t.0[1].crossing_gross_input;
                        let old_o = t.0[1].crossing_output;
                        let mut patched = (*t.0).clone();
                        for c in patched.iter_mut().skip(1) {
                            c.crossing_gross_input =
                                add_delta_dir(c.crossing_gross_input, old_g, new_g);
                            c.crossing_output = add_delta_dir(c.crossing_output, old_o, new_o);
                        }
                        patched[0].ending_range.sqrt_price_x96 =
                            seq.ranges.first().map_or(U256::ZERO, |r| r.sqrt_price_x96);
                        let profiles = Arc::new(rebuild_range0_profile(
                            patched.first().expect("non-empty"),
                            (*t.1).clone(),
                        ));
                        let t2 = (Arc::new(patched), profiles);
                        self.hops
                            .insert(i, (key.clone(), struct_key(seq), t2.clone()));
                        self.counters.partial_rebuilds += 1;
                        done = Some(t2);
                    }
                }
            }
            let t = match done {
                Some(t) => t,
                None => {
                    let (t, _segs) = fresh_tables(seq, &mut self.counters);
                    self.hops.insert(i, (key, struct_key(seq), t.clone()));
                    t
                }
            };
            out.push(t);
        }
        self.counters.solves += 1;
        out
    }

    fn counters(&self) -> &BuildCounters {
        &self.counters
    }
}

// ---------------------------------------------------------------------------
// S3 segment prefix: store per-range segments (bound key), reassemble the
// prefix table O(len) on PriceMove with only seg0 recomputed.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct SegmentPrefixCache {
    hops: HashMap<usize, (String, String, Vec<(U256, U256)>, PreparedHop)>,
    counters: BuildCounters,
}

impl ClCacheStrategy for SegmentPrefixCache {
    fn name(&self) -> &'static str {
        "S3_segment_prefix"
    }

    fn refill(&mut self, seqs: &[IntV3TickRangeSequence], event: &CacheEvent) -> Vec<PreparedHop> {
        let mut out = Vec::with_capacity(seqs.len());
        for (i, seq) in seqs.iter().enumerate() {
            let key = state_key(seq);
            if let Some((fk, _sk, _segs, t)) = self.hops.get(&i) {
                if *fk == key {
                    out.push(t.clone());
                    continue;
                }
            }
            let mut done = None;
            if let Some((_fk, sk, segs, t)) = self.hops.get(&i).cloned() {
                if matches!(event, CacheEvent::PriceMove { hop } if *hop == i)
                    && sk == struct_key(seq)
                {
                    let mut segs = segs;
                    let patch_ok = if segs.is_empty() {
                        // Single-range sequence: nothing is crossed by a price
                        // move inside range 0, so the empty segment set stays
                        // valid — assembling below refreshes ending_range[0]'s
                        // price (and the range-0 profile).
                        true
                    } else if let Some(s0) = range_segment(seq, 0) {
                        segs[0] = s0;
                        true
                    } else {
                        false
                    };
                    if patch_ok {
                        let crossings = Arc::new(assemble_crossings(seq, &segs));
                        let profiles = Arc::new(rebuild_range0_profile(
                            crossings.first().expect("non-empty"),
                            (*t.1).clone(),
                        ));
                        let t2 = (Arc::clone(&crossings), profiles);
                        self.hops
                            .insert(i, (key.clone(), struct_key(seq), segs, t2.clone()));
                        self.counters.partial_rebuilds += 1;
                        done = Some(t2);
                    }
                }
            }
            let t = match done {
                Some(t) => t,
                None => {
                    let (t, segs) = fresh_tables(seq, &mut self.counters);
                    self.hops.insert(i, (key, struct_key(seq), segs, t.clone()));
                    t
                }
            };
            out.push(t);
        }
        self.counters.solves += 1;
        out
    }

    fn counters(&self) -> &BuildCounters {
        &self.counters
    }
}

// ---------------------------------------------------------------------------
// S4 dirty-suffix segment cache: S3 + a Liquidity event re-segments only the
// changed range index (the user-proposed tick-keyed shape).
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct DirtySuffixSegmentCache {
    hops: HashMap<usize, (String, String, Vec<(U256, U256)>, PreparedHop)>,
    counters: BuildCounters,
}

impl ClCacheStrategy for DirtySuffixSegmentCache {
    fn name(&self) -> &'static str {
        "S4_dirty_suffix_segments"
    }

    fn refill(&mut self, seqs: &[IntV3TickRangeSequence], event: &CacheEvent) -> Vec<PreparedHop> {
        let liq_range = match event {
            CacheEvent::Liquidity { hop, range } => Some((*hop, *range)),
            _ => None,
        };
        let mut out = Vec::with_capacity(seqs.len());
        for (i, seq) in seqs.iter().enumerate() {
            let key = state_key(seq);
            if let Some((fk, _sk, _segs, t)) = self.hops.get(&i) {
                if *fk == key {
                    out.push(t.clone());
                    continue;
                }
            }
            let mut done = None;
            if let Some((_fk, sk, segs, t)) = self.hops.get(&i).cloned() {
                if matches!(event, CacheEvent::PriceMove { hop } if *hop == i)
                    && sk == struct_key(seq)
                {
                    let mut segs = segs;
                    let patch_ok = if segs.is_empty() {
                        // Single-range sequence: nothing is crossed by a price
                        // move inside range 0, so the empty segment set stays
                        // valid — assembling below refreshes ending_range[0]'s
                        // price (and the range-0 profile).
                        true
                    } else if let Some(s0) = range_segment(seq, 0) {
                        segs[0] = s0;
                        true
                    } else {
                        false
                    };
                    if patch_ok {
                        let crossings = Arc::new(assemble_crossings(seq, &segs));
                        let profiles = Arc::new(rebuild_range0_profile(
                            crossings.first().expect("non-empty"),
                            (*t.1).clone(),
                        ));
                        let t2 = (Arc::clone(&crossings), profiles);
                        self.hops
                            .insert(i, (key.clone(), struct_key(seq), segs, t2.clone()));
                        self.counters.partial_rebuilds += 1;
                        done = Some(t2);
                    }
                } else if let Some((h, range)) = liq_range {
                    if h != i || !crossings_match_shape(seq, &t.0, Some(range)) {
                        // Event targeted another hop, or the cached shape no
                        // longer matches this sequence (segments carried over
                        // from another path would offset every prefix) —
                        // rebuild below.
                    } else {
                        let mut segs = segs;
                        if range < segs.len() {
                            if let Some(ns) = range_segment(seq, range) {
                                segs[range] = ns;
                            }
                        } // trailing range: no segment slot; prefixes unchanged
                        let crossings = Arc::new(assemble_crossings(seq, &segs));
                        let profiles = Arc::new(rebuild_profile_at(
                            crossings.get(range).expect("range in table"),
                            range,
                            (*t.1).clone(),
                        ));
                        let t2 = (Arc::clone(&crossings), profiles);
                        self.hops
                            .insert(i, (key.clone(), struct_key(seq), segs, t2.clone()));
                        self.counters.partial_rebuilds += 1;
                        done = Some(t2);
                    }
                }
            }
            let t = match done {
                Some(t) => t,
                None => {
                    let (t, segs) = fresh_tables(seq, &mut self.counters);
                    self.hops.insert(i, (key, struct_key(seq), segs, t.clone()));
                    t
                }
            };
            out.push(t);
        }
        self.counters.solves += 1;
        out
    }

    fn counters(&self) -> &BuildCounters {
        &self.counters
    }
}

// ---------------------------------------------------------------------------
// S5 sequence-memo probe
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct SeqMemoProbe {
    seen: HashMap<usize, String>,
    tables: HashMap<(usize, String), PreparedHop>,
    counters: BuildCounters,
}

impl ClCacheStrategy for SeqMemoProbe {
    fn name(&self) -> &'static str {
        "S5_seq_memo_probe"
    }

    fn refill(&mut self, seqs: &[IntV3TickRangeSequence], _event: &CacheEvent) -> Vec<PreparedHop> {
        let mut out = Vec::with_capacity(seqs.len());
        for (i, seq) in seqs.iter().enumerate() {
            let key = state_key(seq);
            let first_sight = self.seen.insert(i, key.clone()) != Some(key.clone());
            if first_sight {
                self.counters.sequence_rebuilds += 1;
            }
            if let Some(t) = self.tables.get(&(i, key.clone())).cloned() {
                out.push(t);
                continue;
            }
            let crossings = Arc::new(build_cl_crossing_table(seq));
            let profiles = Arc::new(build_cl_word_profiles(seq));
            self.counters.crossing_tables += 1;
            self.counters.profile_tables += 1;
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

// ---------------------------------------------------------------------------
// S6 profile split: crossings fused-epoch cached, per-index profiles keyed by
// the ending-range identity (incl. entry sqrt) refreshed only when changed.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct ProfileSplitCache {
    crossings: HashMap<(usize, String), Arc<ClCrossingTable>>,
    profiles: HashMap<(usize, String), Vec<Option<Arc<crate::mobius_v3_int::V3WordProfile>>>>,
    last_profile_keys: HashMap<usize, Vec<String>>,
    counters: BuildCounters,
}

impl ClCacheStrategy for ProfileSplitCache {
    fn name(&self) -> &'static str {
        "S6_profile_split"
    }

    fn refill(&mut self, seqs: &[IntV3TickRangeSequence], _event: &CacheEvent) -> Vec<PreparedHop> {
        let mut out = Vec::with_capacity(seqs.len());
        for (i, seq) in seqs.iter().enumerate() {
            let key = state_key(seq);
            let crossings = match self.crossings.get(&(i, key.clone())) {
                Some(c) => Arc::clone(c),
                None => {
                    let c = Arc::new(build_cl_crossing_table(seq));
                    self.counters.crossing_tables += 1;
                    self.crossings.insert((i, key.clone()), Arc::clone(&c));
                    c
                }
            };
            let mut table: Vec<Option<Arc<crate::mobius_v3_int::V3WordProfile>>> =
                Vec::with_capacity(seq.ranges.len());
            let mut new_keys = Vec::with_capacity(seq.ranges.len());
            let prev_t = self.profiles.get(&(i, key.clone())).cloned();
            let prev_keys = self.last_profile_keys.get(&i).cloned();
            for (k, c) in crossings.iter().enumerate() {
                let kk = profile_key(seq, k);
                new_keys.push(kk.clone());
                if prev_keys.as_ref().is_some_and(|pk| pk.get(k) == Some(&kk)) {
                    table.push(
                        prev_t
                            .as_ref()
                            .and_then(|t| t.get(k))
                            .and_then(Clone::clone),
                    );
                } else {
                    let pf = build_cl_word_profiles_from_crossings(std::slice::from_ref(c));
                    self.counters.profile_tables += 1;
                    table.push(pf.into_iter().next().flatten());
                }
            }
            self.last_profile_keys.insert(i, new_keys);
            self.profiles.insert((i, key), table.clone());
            out.push((crossings, Arc::new(table)));
        }
        self.counters.solves += 1;
        out
    }

    fn counters(&self) -> &BuildCounters {
        &self.counters
    }
}

// ---------------------------------------------------------------------------
// S7 composite: S4-crossings + S6-profils.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct CompositeSplitCache {
    segs: HashMap<usize, (String, String, Vec<(U256, U256)>, Arc<ClCrossingTable>)>,
    profiles: HashMap<(usize, String), Vec<Option<Arc<crate::mobius_v3_int::V3WordProfile>>>>,
    last_profile_keys: HashMap<usize, Vec<String>>,
    counters: BuildCounters,
}

impl ClCacheStrategy for CompositeSplitCache {
    fn name(&self) -> &'static str {
        "S7_composite_split"
    }

    fn refill(&mut self, seqs: &[IntV3TickRangeSequence], event: &CacheEvent) -> Vec<PreparedHop> {
        let liq_range = match event {
            CacheEvent::Liquidity { hop, range } => Some((*hop, *range)),
            _ => None,
        };
        let mut out = Vec::with_capacity(seqs.len());
        for (i, seq) in seqs.iter().enumerate() {
            let key = state_key(seq);
            if let Some((fk, _sk, _segs, t)) = self.segs.get(&i) {
                if *fk == key {
                    let t = Arc::clone(t);
                    let table = self.split_profiles(i, &key, seq, &t);
                    out.push((t, Arc::new(table)));
                    continue;
                }
            }
            let crossings = {
                let mut built = None;
                if let Some((_fk, sk, segs, t)) = self.segs.get(&i).cloned() {
                    if matches!(event, CacheEvent::PriceMove { hop } if *hop == i)
                        && sk == struct_key(seq)
                    {
                        let mut segs = segs;
                        let patch_ok = if segs.is_empty() {
                            // Single-range sequence: the empty segment set stays
                            // valid; assembling refreshes ending_range[0]'s price.
                            true
                        } else if let Some(s0) = range_segment(seq, 0) {
                            segs[0] = s0;
                            true
                        } else {
                            false
                        };
                        if patch_ok {
                            let c = Arc::new(assemble_crossings(seq, &segs));
                            self.segs
                                .insert(i, (key.clone(), struct_key(seq), segs, Arc::clone(&c)));
                            self.counters.partial_rebuilds += 1;
                            built = Some(c);
                        }
                    } else if let Some((h, range)) = liq_range {
                        if h != i
                            || seq.ranges.len() != segs.len() + 1
                            || range > segs.len()
                            || !crossings_match_shape(seq, &t, Some(range))
                        {
                            // Event targeted another hop, segment/sequence
                            // lengths disagree, or the cached shape no longer
                            // matches this sequence — rebuild below.
                        } else {
                            // Mid range: re-derive exactly one segment. The
                            // trailing range (range == segs.len()) owns no
                            // segment slot and its liquidity cannot move any
                            // crossing-amount prefix — reuse every segment;
                            // assemble_crossings re-derives the final
                            // ending_range (fresh liquidity) from the seq.
                            let mut segs = segs;
                            if range < segs.len() {
                                if let Some(ns) = range_segment(seq, range) {
                                    segs[range] = ns;
                                }
                            }
                            let c = Arc::new(assemble_crossings(seq, &segs));
                            self.segs
                                .insert(i, (key.clone(), struct_key(seq), segs, Arc::clone(&c)));
                            self.counters.partial_rebuilds += 1;
                            built = Some(c);
                        }
                    }
                }
                match built {
                    Some(c) => c,
                    None => {
                        let table = build_cl_crossing_table(seq);
                        let mut segs = Vec::with_capacity(table.len().saturating_sub(1));
                        for (k, c) in table.iter().enumerate().skip(1) {
                            let prev = &table[k - 1];
                            segs.push((
                                c.crossing_gross_input
                                    .saturating_sub(prev.crossing_gross_input),
                                c.crossing_output.saturating_sub(prev.crossing_output),
                            ));
                        }
                        self.counters.crossing_tables += 1;
                        let c = Arc::new(table);
                        self.segs
                            .insert(i, (key.clone(), struct_key(seq), segs, Arc::clone(&c)));
                        c
                    }
                }
            };
            let table = self.split_profiles(i, &key, seq, &crossings);
            out.push((crossings, Arc::new(table)));
        }
        self.counters.solves += 1;
        out
    }

    fn counters(&self) -> &BuildCounters {
        &self.counters
    }
}

impl CompositeSplitCache {
    fn split_profiles(
        &mut self,
        i: usize,
        key: &str,
        seq: &IntV3TickRangeSequence,
        crossings: &Arc<ClCrossingTable>,
    ) -> Vec<Option<Arc<crate::mobius_v3_int::V3WordProfile>>> {
        let mut table: Vec<Option<Arc<crate::mobius_v3_int::V3WordProfile>>> =
            Vec::with_capacity(seq.ranges.len());
        let mut new_keys = Vec::with_capacity(seq.ranges.len());
        let prev_t = self.profiles.get(&(i, key.to_string())).cloned();
        let prev_keys = self.last_profile_keys.get(&i).cloned();
        for (k, c) in crossings.iter().enumerate() {
            let kk = profile_key(seq, k);
            new_keys.push(kk.clone());
            if prev_keys.as_ref().is_some_and(|pk| pk.get(k) == Some(&kk)) {
                table.push(
                    prev_t
                        .as_ref()
                        .and_then(|t| t.get(k))
                        .and_then(Clone::clone),
                );
            } else {
                let pf = build_cl_word_profiles_from_crossings(std::slice::from_ref(c));
                self.counters.profile_tables += 1;
                table.push(pf.into_iter().next().flatten());
            }
        }
        self.last_profile_keys.insert(i, new_keys);
        self.profiles.insert((i, key.to_string()), table.clone());
        table
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::print_stderr)]
    use super::*;

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
                U256::from(105u64),
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
    fn assembled_crossings_match_full_build_after_liquidity_patch() {
        let seq0 = two_range_seq();
        let mut seq1 = seq0.clone();
        seq1.ranges[0].liquidity += 100;
        let mut counters = BuildCounters::default();
        let (_t, segs0) = fresh_tables(&seq0, &mut counters);
        let mut segs = segs0;
        let ns = range_segment(&seq1, 0).expect("current-range segment");
        segs[0] = ns;
        let assembled = assemble_crossings(&seq1, &segs);
        let reference = build_cl_crossing_table(&seq1);
        for (k, c) in assembled.iter().enumerate() {
            eprintln!(
                "k={k} assembled=({},{}) ref=({},{})",
                c.crossing_gross_input,
                c.crossing_output,
                reference[k].crossing_gross_input,
                reference[k].crossing_output
            );
            assert_eq!(
                (c.crossing_gross_input, c.crossing_output),
                (
                    reference[k].crossing_gross_input,
                    reference[k].crossing_output
                ),
                "range {k} diverges after liquidity patch"
            );
        }
    }

    #[test]
    fn catalog_registers_all_eight_strategies_in_order() {
        let catalog = strategy_catalog();
        let names: Vec<_> = catalog.iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            vec![
                "S0_full_rebuild",
                "S1_fused_epoch",
                "S2_price_overlay_patch",
                "S3_segment_prefix",
                "S4_dirty_suffix_segments",
                "S5_seq_memo_probe",
                "S6_profile_split",
                "S7_composite_split",
            ]
        );
    }
}
