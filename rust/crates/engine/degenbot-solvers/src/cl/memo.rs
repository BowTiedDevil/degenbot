use alloy::primitives::U256;
use degenbot_math::v2::IntHopState;

use super::IntV3TickRangeSequence;

// ---------------------------------------------------------------------------
// Cross-block composition memo (walk_climb_fork follow-up)
// ---------------------------------------------------------------------------

/// One epoch's walk-composition memo accounting. `hits` = probes whose
/// fingerprint appeared in the PREVIOUS epoch (= solves a same-state
/// composition again, the usable cross-block reuse); `distinct` = unique
/// compositions probed in the current epoch (the census is maintained
/// whenever the memo is active — `memo_on` OR `stats_on` — so it is
/// populated in memo-on/stats-off runs too); `negatives_played` = probes
/// answered from a cached unprofitable entry (the inner walk skipped).
/// Every field except `negative_entries` is reset by `take_stats`:
/// `negative_entries` is a running gauge of the `None` values currently
/// cached and mirrors live cache state across epochs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkMemoStats {
    pub epoch: u64,
    pub probes: u64,
    pub hits: u64,
    pub distinct: u64,
    pub cache_plays: u64,
    pub negative_entries: u64,
    pub negatives_played: u64,
    pub probes_sims: u64,
    pub hits_sims: u64,
}

/// Three-state composition-memo probe: `Hit` replays a cached profitable
/// solution, `Negative` marks a cached unprofitable composition (the caller
/// skips the walk), `Miss` is an absent key or a disabled memo.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum MemoProbe {
    Hit((U256, U256, Vec<U256>)),
    Negative,
    Miss,
}

struct WalkMemoState {
    stats_on: bool,
    memo_on: bool,
    epoch: u64,
    prev: hashbrown::HashSet<u128>,
    curr: hashbrown::HashSet<u128>,
    prev_costs: hashbrown::HashMap<u128, u64>,
    curr_costs: hashbrown::HashMap<u128, u64>,
    cache: hashbrown::HashMap<u128, Option<(U256, U256, Vec<U256>)>>,
    /// Running count of `None` values in `cache`, kept exactly equal to it
    /// across every store and census eviction (replaces the O(cache) scan
    /// the old `take_stats` performed).
    negative_entries: u64,
    probes: u64,
    hits: u64,
    cache_plays: u64,
    negatives_played: u64,
    probes_sims: u64,
    hits_sims: u64,
}

impl Default for WalkMemoState {
    fn default() -> Self {
        Self {
            stats_on: false,
            memo_on: false,
            epoch: 0,
            prev: hashbrown::HashSet::new(),
            curr: hashbrown::HashSet::new(),
            prev_costs: hashbrown::HashMap::new(),
            curr_costs: hashbrown::HashMap::new(),
            cache: hashbrown::HashMap::new(),
            negative_entries: 0,
            probes: 0,
            hits: 0,
            cache_plays: 0,
            negatives_played: 0,
            probes_sims: 0,
            hits_sims: 0,
        }
    }
}

impl WalkMemoState {
    /// Consult the composition cache (call only under `memo_on`, after the
    /// census bookkeeping). One consult = one `cache_plays`; a cached
    /// negative additionally counts as `negatives_played`.
    fn consult_cache(&mut self, fp: u128) -> MemoProbe {
        let hit = self.cache.get(&fp).cloned();
        self.cache_plays += 1;
        match hit {
            Some(Some(entry)) => MemoProbe::Hit(entry),
            Some(None) => {
                self.negatives_played += 1;
                // Soundness: the cached-negative skip rides the same
                // exact-key invariant positive hits rely on —
                // `walk_path_fingerprint` folds the full per-range state,
                // cfg is fixed per engine lifetime, and env is a pure
                // derivation of hop state + cfg (path_bound_lines).
                // Identical inputs -> identical unprofitable outcome.
                MemoProbe::Negative
            }
            None => MemoProbe::Miss,
        }
    }
}

/// The engine's OWNED cross-block walk-composition handle:
/// an `Arc<WalkMemo>` passed into the CL solve entry — no global state, and
/// no environment read inside the solver (the enabled flags are constructor
/// fields; the owner builds them from its config, `build` at the
/// engine-construction boundary). Internal mutex is shared under rayon.
pub struct WalkMemo {
    inner: parking_lot::Mutex<WalkMemoState>,
}

impl WalkMemo {
    #[must_use]
    pub fn new(memo_on: bool, stats_on: bool) -> Self {
        Self {
            inner: parking_lot::Mutex::new(WalkMemoState {
                stats_on,
                memo_on,
                ..WalkMemoState::default()
            }),
        }
    }

    /// Whether either memo gate is on (lets the entry skip the fingerprint +
    /// probe/store entirely when disabled).
    #[must_use]
    pub(super) fn active(&self) -> bool {
        let st = self.lock();
        st.memo_on || st.stats_on
    }

    /// Acquire the interior state lock. The backing mutex is a
    /// `parking_lot::Mutex`, which never poisons: the std poison-recovery
    /// arm (`unwrap_or_else(PoisonError::into_inner)`) is gone with the type
    /// swap, `lock()` is infallible, and the guard releases on drop exactly
    /// as before.
    fn lock(&self) -> parking_lot::MutexGuard<'_, WalkMemoState> {
        self.inner.lock()
    }

    /// Advance the cross-block epoch and swap the composition census (call
    /// at block-lifecycle start — replaces the global set-epoch accessor).
    ///
    /// The composition cache is bounded by the census, not a fixed entry
    /// cap: after the prev/curr swap the cache is pruned to fingerprints
    /// probed in the current or previous epoch, so it holds roughly two
    /// epochs of distinct probed compositions. Anything not probed in the
    /// current or previous epoch is dead by the `hits` definition (a hit
    /// requires previous-epoch census membership) — the census IS the reuse
    /// window. An entry stored without a probe never joins the census and
    /// is evicted at the next epoch advance.
    pub fn begin_block(&self, epoch: u64) {
        let mut st = self.lock();
        if epoch == st.epoch {
            return;
        }
        st.epoch = epoch;
        st.prev = std::mem::take(&mut st.curr);
        let reserve = st.prev.len();
        st.curr.reserve(reserve);
        st.prev_costs = std::mem::take(&mut st.curr_costs);
        let reserve_n = st.prev.len();
        st.curr_costs.reserve(reserve_n);
        // Census-scoped eviction replaces the old wholesale clear at 4096
        // entries, which killed every hot entry mid-epoch (under rayon) the
        // moment a 4097th distinct composition arrived. After the swap
        // `curr` is empty, so this keeps the last epoch's probed
        // compositions and drops everything older. Evicted `None`s
        // decrement the negative gauge so it keeps mirroring the cache.
        let WalkMemoState {
            prev,
            curr,
            cache,
            negative_entries,
            ..
        } = &mut *st;
        cache.retain(|k, v| {
            let keep = prev.contains(k) || curr.contains(k);
            if !keep && v.is_none() {
                *negative_entries -= 1;
            }
            keep
        });
    }

    /// Take (and reset) the per-epoch accounting counters. `negative_entries`
    /// is NOT among the reset counters: it is a running gauge mirroring the
    /// live cache (the number of `None` values currently stored), maintained
    /// across stores and census evictions.
    #[must_use]
    pub fn take_stats(&self) -> WalkMemoStats {
        let mut st = self.lock();
        let out = WalkMemoStats {
            epoch: st.epoch,
            probes: st.probes,
            hits: st.hits,
            distinct: st.curr.len() as u64,
            cache_plays: st.cache_plays,
            negative_entries: st.negative_entries,
            negatives_played: st.negatives_played,
            probes_sims: st.probes_sims,
            hits_sims: st.hits_sims,
        };
        st.probes = 0;
        st.hits = 0;
        st.cache_plays = 0;
        st.negatives_played = 0;
        st.probes_sims = 0;
        st.hits_sims = 0;
        out
    }

    /// Three-state composition-cache probe. Census bookkeeping happens
    /// FIRST, so a played negative still lands the fingerprint in the
    /// epoch's census.
    pub(super) fn probe(&self, fp: u128) -> MemoProbe {
        let mut st = self.lock();
        // The census insert runs whenever the memo is active (the caller
        // probes only under `active()`, i.e. memo_on OR stats_on), NOT just
        // under stats: a memo_on/stats_off engine must still maintain the
        // census, or the begin_block retain would wipe the whole cache
        // every epoch. One HashSet insert per probe is the accepted cost.
        st.curr.insert(fp);
        if st.stats_on {
            st.probes += 1;
            let hit_now = st.prev.contains(&fp);
            if hit_now {
                st.hits += 1;
                st.hits_sims += st.prev_costs.get(&fp).copied().unwrap_or(0);
            }
            if st.memo_on {
                return st.consult_cache(fp);
            }
            return MemoProbe::Miss;
        }
        if st.memo_on {
            return st.consult_cache(fp);
        }
        MemoProbe::Miss
    }

    pub(super) fn note_cost(&self, fp: u128, sims: u64) {
        let mut st = self.lock();
        if !st.stats_on {
            return;
        }
        st.probes_sims += sims;
        st.curr_costs.insert(fp, sims);
    }

    pub(super) fn store(&self, fp: u128, result: Option<&(U256, U256, Vec<U256>)>) {
        let mut st = self.lock();
        if !st.memo_on {
            return;
        }
        // No wholesale cap: the cache is bounded by the census working set
        // (begin_block evicts everything not probed in the current or
        // previous epoch), so a burst of new compositions can never clear
        // hot entries mid-epoch.
        let old = st.cache.insert(fp, result.cloned());
        // Running negative gauge, kept exactly equal to the number of `None`
        // values in the cache across every transition: fresh None +1,
        // Some→None +1, None→Some −1, Some→Some 0.
        if result.is_none() {
            if !matches!(old, Some(None)) {
                st.negative_entries += 1;
            }
        } else if matches!(old, Some(None)) {
            st.negative_entries -= 1;
        }
    }
}

impl std::fmt::Debug for WalkMemo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalkMemo").finish_non_exhaustive()
    }
}

/// Bilinear mixing of a `U256` into a `u64` accumulator (deterministic,
/// order-sensitive; folded per range across every state field). 128-bit pair
/// of accumulators bounds collision risk without hashing-cost contention on
/// the rayon pool (no SipHash per solve).
fn mix_u256(acc: &mut u64, v: U256) {
    let l = v.into_limbs();
    *acc = acc
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(l[0])
        .wrapping_add(l[1].rotate_left(29))
        .wrapping_add(l[2].rotate_left(17))
        .wrapping_add(l[3].rotate_left(11))
        .rotate_left(7)
        ^ 0xD6E8_FEB8_6659_FD93;
}

/// Fold one CL sequence's per-range state into the two lanes — the EXACT
/// mixing body [`walk_path_fingerprint`] folds per position: the
/// empty-sequence tag, the full per-range state (liquidity, prices, gamma,
/// swap direction) into lane_a, and the derived gross/cross accumulators +
/// every word-boundary price into lane_b. The `gross`/`cross` accumulators
/// are threaded by the caller: path-wide running sums carried across every
/// sequence position (function-scope in the original inline body), so this
/// helper neither owns nor resets them and multi-sequence folds are
/// bit-identical to the pre-refactor inline body — the verbatim-copy
/// identity test in the `tests` module below pins that against the
/// original source (commit ca5fbf08f). The mixed entry point
/// ([`walk_mixed_path_fingerprint`]) shares this helper so the collision
/// discipline stays uniform — no forked mixing logic.
fn fold_sequence_ranges(
    lane_a: &mut u64,
    lane_b: &mut u64,
    gross: &mut U256,
    cross: &mut U256,
    seq: &IntV3TickRangeSequence,
) {
    if seq.ranges.is_empty() {
        mix_u256(lane_a, U256::from(0xDEADu64));
        mix_u256(lane_b, U256::from(0xBEEFu64));
        return;
    }
    for r in &seq.ranges {
        mix_u256(lane_a, U256::from(r.gamma_numer));
        mix_u256(lane_a, U256::from(r.fee_denom));
        mix_u256(lane_a, r.sqrt_price_lower_x96);
        mix_u256(lane_a, r.sqrt_price_upper_x96);
        mix_u256(lane_a, U256::from(r.liquidity));
        mix_u256(lane_a, r.sqrt_price_x96);
        mix_u256(lane_a, U256::from(u64::from(r.zero_for_one)));
        *gross = gross.wrapping_add(U256::from(r.liquidity));
        *cross = cross.wrapping_add(r.sqrt_price_x96);
        mix_u256(lane_b, *gross);
        mix_u256(lane_b, *cross);
    }
    for r in &seq.ranges {
        for price in &r.word_boundary_prices {
            mix_u256(lane_b, *price);
        }
    }
}

/// Fold one V2 hop state into the two lanes: every field
/// `PieceView::constant_product` consumes — the constant-product view reads
/// exactly the four `IntHopState` fields (`reserve_in`, `reserve_out`,
/// `gamma_numer`, `fee_denom`) through `swap` / `swap_exact_out` / the
/// shifted-piece anchor, so all four go into lane_a, and the reserve pair
/// (the hop's extractable-output capacity) again into lane_b. This is the
/// key-completeness requirement against env: `PathBoundLines` folds the V2
/// reserves and fee params via `hop_lines_and_cap` → `mobius_lines`, so the
/// fingerprint must fold exactly what determines the solve outcome.
fn fold_v2_hop(lane_a: &mut u64, lane_b: &mut u64, hop: &IntHopState) {
    mix_u256(lane_a, hop.reserve_in);
    mix_u256(lane_a, hop.reserve_out);
    mix_u256(lane_a, hop.gamma_numer);
    mix_u256(lane_a, hop.fee_denom);
    mix_u256(lane_b, hop.reserve_in);
    mix_u256(lane_b, hop.reserve_out);
}

/// 128-bit content fingerprint of the path composition: one lane folds hop
/// order and the full per-range state (liquidity, prices, gamma, swap
/// direction), the other folds the derived capacity fields (gross/output
/// pairs) and every word-boundary price, so two distinct compositions that
/// collapse one lane cannot collapse both. The crossing tables + word
/// profiles are pure deterministic derivations of the sequence, so this
/// fingerprint is the EXACT correctness key for a cached result.
#[must_use]
pub fn walk_path_fingerprint(sequences: &[&IntV3TickRangeSequence]) -> u128 {
    let mut lane_a: u64 = 0xCBF2_9CE4_8422_2325;
    let mut lane_b: u64 = 0x6E99_B980_B247_C7F6;
    let mut gross = U256::ZERO;
    let mut cross = U256::ZERO;
    for (i, seq) in sequences.iter().enumerate() {
        mix_u256(&mut lane_a, U256::from((i as u64).wrapping_add(3)));
        mix_u256(&mut lane_b, U256::from((i as u64).wrapping_add(5)));
        fold_sequence_ranges(&mut lane_a, &mut lane_b, &mut gross, &mut cross, seq);
    }
    (u128::from(lane_a) << 64) | u128::from(lane_b)
}

/// 128-bit content fingerprint of a MIXED V2+CL path composition — the
/// mixed entry's (`solve_mixed_piecewise`) exact correctness key for the
/// cross-block memo, same contract as [`walk_path_fingerprint`]: the
/// crossing tables + word profiles are pure derivations of the sequence,
/// cfg is fixed per engine lifetime, and env (PathBoundLines) is a pure
/// derivation of hop state + cfg (it folds the V2 reserves and fee params
/// via `hop_lines_and_cap` → `mobius_lines`), so folding every position's
/// determining state makes the key exact for mixed compositions.
///
/// Per position it folds the hop index (the same `(i + 3)` / `(i + 5)` lane
/// mixing [`walk_path_fingerprint`] uses), a V2/CL discriminator tag, and
/// the hop state: V2 via [`fold_v2_hop`] (every field
/// `PieceView::constant_product` consumes), CL via [`fold_sequence_ranges`]
/// (the same per-range folding the all-CL key performs — no forked mixing),
/// with the `gross`/`cross` accumulators threaded path-wide across the CL
/// positions exactly as [`walk_path_fingerprint`] threads them; V2
/// positions contribute nothing to them (the original CL-only accumulation
/// discipline). The mixed keys are transient in-memory memo keys — a value
/// shift here is inert.
///
/// The all-CL and mixed fingerprints cover different composition spaces
/// (both entries may share one `WalkMemo` handle — fingerprint identity is
/// composition identity), so no cross-compatibility between the two keys is
/// required or intended. The u128 two-lane output discipline is kept.
#[must_use]
pub fn walk_mixed_path_fingerprint(
    v2_hops: &[Option<IntHopState>],
    cl_sequences: &[Option<&IntV3TickRangeSequence>],
    hop_order: &[bool], // true = V2, false = CL
) -> u128 {
    let mut lane_a: u64 = 0xCBF2_9CE4_8422_2325;
    let mut lane_b: u64 = 0x6E99_B980_B247_C7F6;
    let mut gross = U256::ZERO;
    let mut cross = U256::ZERO;
    for (i, &is_v2) in hop_order.iter().enumerate() {
        mix_u256(&mut lane_a, U256::from((i as u64).wrapping_add(3)));
        mix_u256(&mut lane_b, U256::from((i as u64).wrapping_add(5)));
        mix_u256(&mut lane_a, U256::from(u64::from(is_v2)));
        // A `None` state is a structurally invalid position (the entry
        // refuses it before probing); fold a distinct absent tag so it
        // cannot share a key with any present state or empty sequence.
        if is_v2 {
            if let Some(hop) = &v2_hops[i] {
                fold_v2_hop(&mut lane_a, &mut lane_b, hop);
            } else {
                mix_u256(&mut lane_a, U256::from(0xFACEu64));
                mix_u256(&mut lane_b, U256::from(0xFEEDu64));
            }
        } else if let Some(seq) = cl_sequences[i] {
            fold_sequence_ranges(&mut lane_a, &mut lane_b, &mut gross, &mut cross, seq);
        } else {
            mix_u256(&mut lane_a, U256::from(0xFACEu64));
            mix_u256(&mut lane_b, U256::from(0xFEEDu64));
        }
    }
    (u128::from(lane_a) << 64) | u128::from(lane_b)
}

// ---------------------------------------------------------------------------
// Fingerprint identity tests (bit-identity proof against the pre-refactor
// inline body)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ::degenbot_pools::int_v3_hop::IntV3TickRangeHop;

    /// VERBATIM copy of the pre-refactor inline fingerprint body.
    ///
    /// Provenance: `git show ca5fbf08f:rust/crates/engine/degenbot-solvers/src/cl/memo.rs`
    /// (the `walk_path_fingerprint` body at the commit before the
    /// `fold_sequence_ranges` factoring landed). Deliberately NOT refactored
    /// to share the helper: its whole value is being an independent original,
    /// so the identity tests below prove the refactored
    /// [`walk_path_fingerprint`] is bit-identical to the inline body it
    /// replaced.
    fn walk_path_fingerprint_original(sequences: &[&IntV3TickRangeSequence]) -> u128 {
        let mut lane_a: u64 = 0xCBF2_9CE4_8422_2325;
        let mut lane_b: u64 = 0x6E99_B980_B247_C7F6;
        let mut gross = U256::ZERO;
        let mut cross = U256::ZERO;
        for (i, seq) in sequences.iter().enumerate() {
            mix_u256(&mut lane_a, U256::from((i as u64).wrapping_add(3)));
            mix_u256(&mut lane_b, U256::from((i as u64).wrapping_add(5)));
            if seq.ranges.is_empty() {
                mix_u256(&mut lane_a, U256::from(0xDEADu64));
                mix_u256(&mut lane_b, U256::from(0xBEEFu64));
                continue;
            }
            for r in &seq.ranges {
                mix_u256(&mut lane_a, U256::from(r.gamma_numer));
                mix_u256(&mut lane_a, U256::from(r.fee_denom));
                mix_u256(&mut lane_a, r.sqrt_price_lower_x96);
                mix_u256(&mut lane_a, r.sqrt_price_upper_x96);
                mix_u256(&mut lane_a, U256::from(r.liquidity));
                mix_u256(&mut lane_a, r.sqrt_price_x96);
                mix_u256(&mut lane_a, U256::from(u64::from(r.zero_for_one)));
                gross = gross.wrapping_add(U256::from(r.liquidity));
                cross = cross.wrapping_add(r.sqrt_price_x96);
                mix_u256(&mut lane_b, gross);
                mix_u256(&mut lane_b, cross);
            }
            for r in &seq.ranges {
                for price in &r.word_boundary_prices {
                    mix_u256(&mut lane_b, *price);
                }
            }
        }
        (u128::from(lane_a) << 64) | u128::from(lane_b)
    }

    /// Fixture range with arbitrary (fingerprint-opaque) small Q128.96-style
    /// prices; no semantic validity is required — only that every folded
    /// field is deterministic.
    fn make_hop(
        liquidity: u128,
        sp: u64,
        lower: u64,
        upper: u64,
        wb: Vec<U256>,
    ) -> IntV3TickRangeHop {
        IntV3TickRangeHop {
            liquidity,
            sqrt_price_x96: U256::from(sp),
            sqrt_price_lower_x96: U256::from(lower),
            sqrt_price_upper_x96: U256::from(upper),
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
            word_boundary_prices: wb,
        }
    }

    /// Direct struct construction (not `IntV3TickRangeSequence::new`, which
    /// rejects the empty range vector the empty-sequence fixtures need).
    fn make_seq(ranges: Vec<IntV3TickRangeHop>) -> IntV3TickRangeSequence {
        IntV3TickRangeSequence { ranges }
    }

    /// Bit-identity assertion against the verbatim original body.
    fn assert_matches_original(sequences: &[&IntV3TickRangeSequence]) {
        assert_eq!(
            walk_path_fingerprint(sequences),
            walk_path_fingerprint_original(sequences),
            "walk_path_fingerprint diverged from the original inline body (ca5fbf08f)"
        );
    }

    #[test]
    fn fingerprint_identity_one_sequence() {
        let s1 = make_seq(vec![
            make_hop(1_000, 1 << 62, 1 << 60, 1 << 63, vec![]),
            make_hop(
                2_000,
                (1 << 62) + 7,
                (1 << 60) + 3,
                (1 << 63) + 5,
                vec![U256::from(1u128) << 96],
            ),
        ]);
        assert_matches_original(&[&s1]);
    }

    #[test]
    fn fingerprint_identity_two_sequences() {
        let s1 = make_seq(vec![make_hop(1_000, 1 << 62, 1 << 60, 1 << 63, vec![])]);
        let s2 = make_seq(vec![make_hop(
            3_000,
            (1 << 62) + 7,
            (1 << 60) + 3,
            (1 << 63) + 5,
            vec![U256::from(1u128) << 96],
        )]);
        assert_matches_original(&[&s1, &s2]);
    }

    #[test]
    fn fingerprint_identity_three_sequences() {
        let s1 = make_seq(vec![make_hop(1_000, 1 << 62, 1 << 60, 1 << 63, vec![])]);
        let s2 = make_seq(vec![make_hop(
            3_000,
            (1 << 62) + 7,
            (1 << 60) + 3,
            (1 << 63) + 5,
            vec![U256::from(1u128) << 96],
        )]);
        let s3 = make_seq(vec![make_hop(
            5_000,
            (1 << 62) + 11,
            (1 << 60) + 9,
            (1 << 63) + 13,
            vec![],
        )]);
        assert_matches_original(&[&s1, &s2, &s3]);
    }

    #[test]
    fn fingerprint_identity_empty_sequence_in_middle() {
        let s1 = make_seq(vec![make_hop(1_000, 1 << 62, 1 << 60, 1 << 63, vec![])]);
        let empty = make_seq(vec![]);
        let s2 = make_seq(vec![make_hop(
            3_000,
            (1 << 62) + 7,
            (1 << 60) + 3,
            (1 << 63) + 5,
            vec![U256::from(1u128) << 96],
        )]);
        assert_matches_original(&[&s1, &empty, &s2]);
    }

    #[test]
    fn fingerprint_identity_equal_vs_different_liquidity() {
        let s1 = make_seq(vec![make_hop(7_000, 1 << 62, 1 << 60, 1 << 63, vec![])]);
        let equal = make_seq(vec![make_hop(
            7_000,
            (1 << 62) + 7,
            (1 << 60) + 3,
            (1 << 63) + 5,
            vec![],
        )]);
        let different = make_seq(vec![make_hop(
            9_000,
            (1 << 62) + 7,
            (1 << 60) + 3,
            (1 << 63) + 5,
            vec![],
        )]);
        let equal_path = [&s1, &equal];
        let different_path = [&s1, &different];
        assert_matches_original(&equal_path);
        assert_matches_original(&different_path);
        assert_ne!(
            walk_path_fingerprint(&equal_path),
            walk_path_fingerprint(&different_path)
        );
    }

    /// Golden pin: the exact u128 [`walk_path_fingerprint`] returns for the
    /// fixed two-sequence fixture below.
    ///
    /// STABILITY CONTRACT: this value must stay put across refactors —
    /// `ClSolveTables::source_fingerprint` is built from this function's
    /// output (and paired against it downstream), and the cross-block memo
    /// (`WalkMemo` cache/census) keys on it. A changed value silently
    /// invalidates every paired consumer and every memoized key.
    #[test]
    fn fingerprint_golden_two_sequence_fixture() {
        let s1 = make_seq(vec![make_hop(
            10_000,
            1 << 62,
            1 << 60,
            1 << 63,
            vec![U256::from(1u128) << 96],
        )]);
        let s2 = make_seq(vec![make_hop(
            20_000,
            (1 << 62) + 7,
            (1 << 60) + 3,
            (1 << 63) + 5,
            vec![],
        )]);
        // Pinned from this exact fixture at the tree state that restored the
        // original (ca5fbf08f) inline-body accumulation discipline. Hex split
        // is lane_a << 64 | lane_b.
        assert_eq!(
            walk_path_fingerprint(&[&s1, &s2]),
            0x831f_878b_e447_c38b_e028_fbcb_79ab_9c70u128
        );
    }
}
