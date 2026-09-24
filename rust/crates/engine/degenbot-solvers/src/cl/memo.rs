use alloy::primitives::U256;

use super::IntV3TickRangeSequence;

// ---------------------------------------------------------------------------
// Cross-block composition memo (walk_climb_fork follow-up; F4YJL8)
// ---------------------------------------------------------------------------

/// One epoch's walk-composition memo accounting. `hits` = probes whose
/// fingerprint appeared in the PREVIOUS epoch (= solves a same-state
/// composition again, the usable cross-block reuse); `distinct` = unique
/// compositions in the current epoch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkMemoStats {
    pub epoch: u64,
    pub probes: u64,
    pub hits: u64,
    pub distinct: u64,
    pub cache_plays: u64,
    pub negative_entries: u64,
    pub probes_sims: u64,
    pub hits_sims: u64,
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
    probes: u64,
    hits: u64,
    cache_plays: u64,
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
            probes: 0,
            hits: 0,
            cache_plays: 0,
            probes_sims: 0,
            hits_sims: 0,
        }
    }
}

/// The engine's OWNED cross-block walk-composition handle (SU7MAE T3 / Q12a):
/// an `Arc<WalkMemo>` passed into the CL solve entry — no global state, and
/// no environment read inside the solver (the enabled flags are constructor
/// fields; the owner builds them from its config, `from_env` at the
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
    }

    /// Take (and reset) the per-epoch accounting counters.
    #[must_use]
    pub fn take_stats(&self) -> WalkMemoStats {
        let mut st = self.lock();
        let out = WalkMemoStats {
            epoch: st.epoch,
            probes: st.probes,
            hits: st.hits,
            distinct: st.curr.len() as u64,
            cache_plays: st.cache_plays,
            negative_entries: st.cache.values().filter(|v| v.is_none()).count() as u64,
            probes_sims: st.probes_sims,
            hits_sims: st.hits_sims,
        };
        st.probes = 0;
        st.hits = 0;
        st.cache_plays = 0;
        st.probes_sims = 0;
        st.hits_sims = 0;
        out
    }

    pub(super) fn probe(&self, fp: u128) -> Option<(U256, U256, Vec<U256>)> {
        let mut st = self.lock();
        if st.stats_on {
            st.probes += 1;
            let hit_now = st.prev.contains(&fp);
            if hit_now {
                st.hits += 1;
                st.hits_sims += st.prev_costs.get(&fp).copied().unwrap_or(0);
            }
            st.curr.insert(fp);
            if st.memo_on {
                let hit = st.cache.get(&fp).cloned();
                st.cache_plays += 1;
                if let Some(entry) = hit.flatten() {
                    return Some(entry);
                }
                return None;
            }
            return None;
        }
        if st.memo_on {
            let hit = st.cache.get(&fp).cloned();
            st.cache_plays += 1;
            if let Some(entry) = hit.flatten() {
                return Some(entry);
            }
            return None;
        }
        None
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
        if st.cache.len() >= 4096 {
            st.cache.clear();
        }
        st.cache.insert(fp, result.cloned());
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
/// order + per-range liquidity/prices/gamma, the other folds the derived
/// capacity fields (gross/output pairs) so two distinct compositions that
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
            gross = gross.wrapping_add(U256::from(r.liquidity));
            cross = cross.wrapping_add(r.sqrt_price_x96);
            mix_u256(&mut lane_b, gross);
            mix_u256(&mut lane_b, cross);
        }
        for r in &seq.ranges {
            let mut gf = U256::from(r.gamma_numer)
                .saturating_mul(U256::from(r.word_boundary_prices.len() as u64))
                .saturating_add(U256::from(r.liquidity));
            if !r.word_boundary_prices.is_empty() {
                gf = gf.saturating_add(r.word_boundary_prices[0]);
            }
            mix_u256(&mut lane_b, gf);
        }
    }
    (u128::from(lane_a) << 64) | u128::from(lane_b)
}
