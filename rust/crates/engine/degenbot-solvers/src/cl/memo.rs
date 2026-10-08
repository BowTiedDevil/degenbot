use alloy::primitives::U256;

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
    inner: std::sync::Mutex<WalkMemoState>,
}

impl WalkMemo {
    #[must_use]
    pub fn new(memo_on: bool, stats_on: bool) -> Self {
        Self {
            inner: std::sync::Mutex::new(WalkMemoState {
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

    fn lock(&self) -> std::sync::MutexGuard<'_, WalkMemoState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
