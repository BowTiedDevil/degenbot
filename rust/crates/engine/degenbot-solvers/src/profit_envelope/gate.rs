use crate::runtime::SolveRuntimeConfig;
use alloy::primitives::{aliases::I512, U256, U512};
use degenbot_core::diag;
#[cfg(not(feature = "hotpath"))]
use degenbot_core::op_warn;

use super::capture::{DegeneratePathEvent, DegeneratePathSink};
use super::derive::{
    cl_table_key, classify_cl_rejection, content_mix_u256, hop_lines_and_cap,
    hop_lines_and_cap_cached, HopCacheKey, HopMath, PrefixCache,
};
use super::kernel::{
    approx_cmp_ceil, approx_cmp_ratio, ceil_div, compose_envelopes_exact, concave_max, i512_to_f64,
    narrow, trace_boundary, Line, COMPOSE_TARGET_BITS,
};

/// Drop dominated lines: `i` dies if some `j` is `≤ i` at BOTH domain
/// endpoints (affine ⇒ everywhere), breaking ties toward the smaller index so
/// identical lines collapse deterministically. Keeps the bound sound while
/// bounding the line count across chained hops. `upper` is the envelope
/// domain endpoint: a candidate dominated at both 0 and `upper` never
/// matters for any input in [0, upper] (two affine lines cross once).
#[expect(clippy::too_many_lines)]
pub(crate) fn prune(lines: &mut Vec<Line>, upper: U256, stats: &mut GateStats) {
    struct S1Key {
        f0: f64,
        fu: f64,
        idx: usize,
    }
    record_gate(stats, |t| {
        t.prune_calls += 1;
        t.prune_lines += lines.len() as u64;
    });
    if lines.len() < 2 {
        return;
    }
    // Exact lower-envelope hull restricted to [0, domain] (replaces the
    // 2-D endpoint-dominance sweep, which survived near-parallel
    // non-dominating lines and let the composition product explode — 28k
    // pairs/path measured on heavy captures).
    //
    // The hull IS the min-line envelope: for affine lines the pointwise
    // minimum passes through lines in slope order, switching at exact
    // rational crossover points. A line is minimal somewhere inside
    // [0, domain] iff its (ceil-rounded) takeover breakpoint lies ≤
    // domain. Dropping the rest is EXACT (same min at every x), not just
    // sound.
    //
    // Stage 1 (cheap, no wide divisions): endpoint-dominance sweep. A
    // line is dropped only when another is ≤ it at BOTH x=0 and x=domain —
    // the affine difference then bounds it everywhere in between. Removes
    // the readily-dominated majority for ~2 evals per line.
    //
    // Stage 2 (exact hull on survivors): lines are first sound-reduced to
    // COMPOSE_TARGET_BITS so the slope cross-products cannot overflow I512
    // (composed intermediates can reach ~484 bits); reduction only raises
    // values (ceil/floor rules), never under-cuts the bound. Divisions in
    // the hull are then paid only over the (small) survivor set, not the
    // full product set.
    let stage1_t0 = std::time::Instant::now();
    // Loop-16 T2: approximate ordering keys with exact fallbacks. The
    // keys carry ~2^-48 relative error; `approx_cmp`'s 1e-6 band separates
    // confidently-ordered comparisons from exact-comparator fallbacks, so
    // both the sort and the survivor sweep produce byte-identical results
    // to the exact-eval implementation — pinned by the randomized
    // differential test against the frozen reference copy.
    //
    // Saturation modeling: `eval` saturates the b·x multiply and the a +
    // bx add to I512::MAX on overflow; the keys model BOTH channels with
    // the same min-clipping in f64 (max_f stands in for I512::MAX).
    let upper_f = i512_to_f64(I512::try_from(U512::from(upper)).unwrap_or(I512::MAX));
    let max_f = i512_to_f64(I512::MAX);
    let mut indexed: Vec<S1Key> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let a_f = i512_to_f64(l.a);
            let b_f = i512_to_f64(l.b);
            let c_f = i512_to_f64(l.c);
            let prod_f = (b_f * upper_f).min(max_f);
            let n_f = (a_f + prod_f).min(max_f);
            S1Key {
                f0: a_f / c_f,
                fu: n_f / c_f,
                idx: i,
            }
        })
        .collect();
    // GATE-COMPOSE-1: memoize the exact endpoint evals lazily. The
    // comparator below re-evaluates BOTH lines on every f64-key tie; each
    // eval pays a 512-bit ceil_div. Eager precompute of all 2n evals
    // LOST on this corpus (ties are not that dense), so cache on first
    // use instead: never-tied lines keep paying nothing, tied lines pay
    // once. Evals are pure and deterministic, so the memoized values are
    // byte-identical to the closure form.
    let mut exact_at_zero_memo: Vec<Option<I512>> = vec![None; lines.len()];
    let mut exact_at_upper_memo: Vec<Option<I512>> = vec![None; lines.len()];
    indexed.sort_by(|x, y| {
        approx_cmp_ceil(x.f0, y.f0)
            .then_with(|| {
                if exact_at_zero_memo[x.idx].is_none() {
                    exact_at_zero_memo[x.idx] = Some(lines[x.idx].eval(&U256::ZERO));
                    record_gate(stats, |t| t.prune_tie_evals += 1);
                }
                if exact_at_zero_memo[y.idx].is_none() {
                    exact_at_zero_memo[y.idx] = Some(lines[y.idx].eval(&U256::ZERO));
                    record_gate(stats, |t| t.prune_tie_evals += 1);
                }
                exact_at_zero_memo[x.idx].cmp(&exact_at_zero_memo[y.idx])
            })
            .then(approx_cmp_ceil(x.fu, y.fu))
            .then_with(|| {
                if exact_at_upper_memo[x.idx].is_none() {
                    exact_at_upper_memo[x.idx] = Some(lines[x.idx].eval(&upper));
                    record_gate(stats, |t| t.prune_tie_evals += 1);
                }
                if exact_at_upper_memo[y.idx].is_none() {
                    exact_at_upper_memo[y.idx] = Some(lines[y.idx].eval(&upper));
                    record_gate(stats, |t| t.prune_tie_evals += 1);
                }
                exact_at_upper_memo[x.idx].cmp(&exact_at_upper_memo[y.idx])
            })
            .then(x.idx.cmp(&y.idx))
    });
    let mut min_f = f64::INFINITY;
    let mut min_idx = usize::MAX;
    let mut min_exact: Option<I512> = None;
    let mut surv: Vec<Line> = Vec::with_capacity(lines.len());
    for item in &indexed {
        let keep = match approx_cmp_ceil(item.fu, min_f) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => {
                let cand = if let Some(v) = exact_at_upper_memo[item.idx] {
                    v
                } else {
                    let v = lines[item.idx].eval(&upper);
                    exact_at_upper_memo[item.idx] = Some(v);
                    record_gate(stats, |t| t.prune_tie_evals += 1);
                    v
                };
                if min_idx == usize::MAX {
                    cand < I512::MAX
                } else {
                    let mv = if let Some(mv) = min_exact {
                        mv
                    } else {
                        let mv = exact_at_upper_memo[min_idx]
                            .unwrap_or_else(|| lines[min_idx].eval(&upper));
                        min_exact = Some(mv);
                        mv
                    };
                    cand < mv
                }
            }
        };
        if keep {
            min_f = item.fu;
            min_idx = item.idx;
            min_exact = None;
            surv.push(lines[item.idx]);
        }
    }
    // Saturation guard (live crash 2026-08-30): when EVERY line's endpoint
    // eval saturates to I512::MAX, the strict improvement test keeps none
    // and the downstream hull indexed an empty set. Keep the smallest-key0
    // line — still a sound global upper bound, so the envelope can only
    // loosen, never under-cut.
    if surv.is_empty() {
        surv.push(lines[indexed[0].idx]);
    }
    record_gate(stats, |t| {
        t.prune_stage1_ns += stage1_t0.elapsed().as_nanos();
    });
    record_gate(stats, |t| t.prune_hull_lines += surv.len() as u64);
    if surv.len() < 2 {
        *lines = surv;
        return;
    }
    let hull_t0 = std::time::Instant::now();
    let parsed = &mut surv;
    for l in parsed.iter_mut() {
        l.reduce(COMPOSE_TARGET_BITS);
    }
    let mut idx: Vec<usize> = (0..parsed.len()).collect();
    // Approx slope keys (descending) with exact cross-mult fallback — same
    // approx_cmp band discipline as stage 1, byte-identical ordering.
    let slope_f: Vec<f64> = parsed
        .iter()
        .map(|l| i512_to_f64(l.b) / i512_to_f64(l.c))
        .collect();
    idx.sort_by(|&i, &j| {
        approx_cmp_ratio(slope_f[j], slope_f[i]).then_with(|| {
            let (li, lj) = (&parsed[i], &parsed[j]);
            let lhs = li.b * lj.c;
            let rhs = lj.b * li.c;
            rhs.cmp(&lhs)
        })
    });
    let mut hull: Vec<(U256, usize)> = Vec::with_capacity(idx.len());
    for &li in &idx {
        let l = &parsed[li];
        if let Some(&(_, top)) = hull.last() {
            let lt = &parsed[top];
            // Same-slope pairs: the lower intercept dominates globally.
            // Band-proximity gates the exact cross-mult check (loop-16
            // T2): confidently-distinct slopes skip the multiplications;
            // anything inside the band runs the exact check (which also
            // guards the pop-loop division against zero denominators —
            // exact-equal slopes must never reach `ceil_div`).
            if approx_cmp_ratio(slope_f[top], slope_f[li]).is_eq() && lt.b * l.c == l.b * lt.c {
                if lt.a * l.c <= l.a * lt.c {
                    continue;
                }
                hull.pop();
            }
        }
        let bp = if let Some(&(_, top)) = hull.last() {
            let lt = &parsed[top];
            let num = l.a * lt.c - lt.a * l.c;
            let den = lt.b * l.c - l.b * lt.c;
            ceil_div(num, den)
        } else {
            I512::ZERO
        };
        // First pop-loop iteration reuses the bp pair (same candidate, same
        // top — the 4 wide multiplications and the division are already
        // paid; loop-16 T2).
        let mut first_iter = true;
        while hull.len() >= 2 {
            let (bb, t) = hull[hull.len() - 1];
            let bb_i = I512::try_from(U512::from(bb)).unwrap_or(I512::MAX);
            let dominated = if first_iter {
                bp <= bb_i
            } else {
                let lprev = &parsed[t];
                let num = l.a * lprev.c - lprev.a * l.c;
                let den = lprev.b * l.c - l.b * lprev.c;
                // ceil(num/den) <= bb_i ⟺ num <= bb_i·den for den > 0 —
                // the multiplication replaces the wide division (falls back
                // to the division on the impossible-under-slope-order
                // den <= 0 case).
                if den > I512::ZERO {
                    match bb_i.checked_mul(den) {
                        Some(lhs) => num <= lhs,
                        None => ceil_div(num, den) <= bb_i,
                    }
                } else {
                    ceil_div(num, den) <= bb_i
                }
            };
            if dominated {
                hull.pop();
                first_iter = false;
            } else {
                break;
            }
        }
        let bx = if bp <= I512::ZERO {
            U256::ZERO
        } else {
            let u = U512::try_from(bp).unwrap_or(U512::MAX);
            if u > U512::from(U256::MAX) {
                U256::MAX
            } else {
                u.to::<U256>()
            }
        };
        hull.push((bx, li));
    }
    // Keep only lines whose takeover happens inside [0, domain].
    let keep: Vec<Line> = hull
        .iter()
        .filter(|&&(bx, _)| bx <= upper)
        .map(|&(_, i)| parsed[i])
        .collect();
    record_gate(stats, |t| t.prune_hull_ns += hull_t0.elapsed().as_nanos());
    *lines = keep;
}

/// Gate telemetry: per-path counters carried out on the mixed solve
/// outcome (the asymmetric twin of `WalkOutcome`'s walk stats — rayon
/// workers return them the same way and the bot merges them per cycle).
/// `unsupported` counts paths whose hop families lack an envelope — those
/// are SOLVED normally, never skipped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GateStats {
    /// Paths whose bound was derived and compared against `min_profit`.
    pub evaluated: u64,
    /// Paths skipped because `bound < min_profit` (provably unprofitable).
    pub skipped: u64,
    /// Paths with at least one unsupported hop family (solved unscreened).
    pub unsupported: u64,
    /// Per-cause breakdown of `unsupported` (diagnostic). At most one
    /// counter advances per unsupported path (the FIRST cause early-returns).
    pub none_hop_unmapped: u64,
    pub none_degenerate: u64,
    pub none_overflow: u64,
    /// Wall-clock time (nanoseconds) spent in `path_profit_bound` for this path.
    pub duration_ns: u128,
    /// Composed-boundary cache hits inside this path's envelope product.
    pub prefix_hits: u64,
    /// Pool-static CL tangent-fan cache hits (per CL hop derivation).
    pub cl_fan_hits: u64,
    /// Pool-static CL tangent-fan cache misses (per CL hop derivation).
    pub cl_fan_misses: u64,
    /// Composition boundaries actually executed for this path.
    pub boundaries_composed: u64,
    /// Product-matrix wall time (ns) across this path's boundaries.
    pub product_ns: u128,
    /// Prune stage-1 endpoint-sweep wall time (ns).
    pub prune_stage1_ns: u128,
    /// Prune stage-2 hull wall time (ns).
    pub prune_hull_ns: u128,
    /// Loop-12 split: post-prune survivor reduce pass (ns).
    pub postprune_reduce_ns: u128,
    /// Loop-12 split: sampled-cap construction (ns).
    pub sample_ns: u128,
    /// Derivation phase wall time (ns) — per-hop tangent-line derivation.
    pub derive_ns: u128,
    /// Compose phase wall time (ns) — line chaining + prune over the chain.
    pub compose_ns: u128,
    /// Search phase wall time (ns) — the discrete concave max search.
    pub search_ns: u128,
    /// Search phase hull segments whose candidates the concave max scan
    /// evaluated. One count per breakpoint segment; a floor early-exit stops
    /// the count at the segment that cleared the floor (diagnostic).
    pub search_segments: u64,
    /// Affine composition pairs evaluated (diagnostic).
    pub pairs: u64,
    /// GATE-COMPOSE-1: prune invocations (diagnostic).
    pub prune_calls: u64,
    /// GATE-COMPOSE-1: total lines entering prune (diagnostic).
    pub prune_lines: u64,
    /// GATE-COMPOSE-1: stage-1 tie evals actually computed (diagnostic).
    pub prune_tie_evals: u64,
    /// GATE-COMPOSE-1: stage-2 hull input lines (diagnostic).
    pub prune_hull_lines: u64,
    /// GATE-COMPOSE-2: pairs the merge actually composed (vs enumerated).
    pub merge_selected: u64,
    /// GATE-COMPOSE-2: m*n pairs the legacy product would have composed.
    pub pairs_enumerated: u64,
    /// GATE-COMPOSE-2: boundaries that fell back to the legacy product.
    pub merge_legacy_fallbacks: u64,
    /// M3 per-reason fallback breakdown (b<0 anywhere).
    pub merge_fb_b_sign: u64,
    /// M3 per-reason fallback breakdown (flat cap lines).
    pub merge_fb_flat: u64,
    /// M3 per-reason fallback breakdown (empty hull pieces).
    pub merge_fb_empty_pieces: u64,
    /// M3 per-reason fallback breakdown (empty selection).
    pub merge_fb_empty_selection: u64,
    /// M3 per-reason fallback breakdown (clamped-bp piece reordering).
    pub merge_fb_y_disorder: u64,
    /// GC-3: saturated boundary arithmetic during the sweep.
    pub merge_fb_cmp_overflow: u64,
    /// Exact-compose boundaries that fell back to the sampled reference.
    pub exact_compose_fallbacks: u64,
    /// Exact-compose fallback reasons (I512 compose wall).
    pub exact_fb_overflow: u64,
    /// Exact-compose fallback reasons (negative-slope operand).
    pub exact_fb_b_sign: u64,
    /// Exact-compose fallback reasons (flat piece rejected by selection).
    pub exact_fb_flat: u64,
    /// Exact-compose fallback reasons (empty hull pieces).
    pub exact_fb_empty_pieces: u64,
    /// Exact-compose fallback reasons (empty selection).
    pub exact_fb_empty_selection: u64,
    /// Exact-compose fallback reasons (clamped-bp piece reordering).
    pub exact_fb_y_disorder: u64,
    /// Exact-compose fallback reasons (saturated boundary arithmetic).
    pub exact_fb_cmp_overflow: u64,
    /// Exact-compose selected pairs (<= K1+K2 per boundary).
    pub exact_hull_pairs: u64,
    /// Exact-compose m*n pairs the full product would have composed.
    pub exact_pairs_enumerated: u64,
    /// Exact-compose hull lines emitted (post-prune, pre-search).
    pub exact_hull_lines: u64,
    /// Exact-compose selection + compose wall time (ns).
    pub exact_compose_ns: u128,
}

impl GateStats {
    pub(crate) const EMPTY: Self = Self {
        evaluated: 0,
        skipped: 0,
        unsupported: 0,
        none_hop_unmapped: 0,
        none_degenerate: 0,
        none_overflow: 0,
        duration_ns: 0,
        prefix_hits: 0,
        cl_fan_hits: 0,
        cl_fan_misses: 0,
        boundaries_composed: 0,
        product_ns: 0,
        prune_stage1_ns: 0,
        prune_hull_ns: 0,
        derive_ns: 0,
        compose_ns: 0,
        search_ns: 0,
        search_segments: 0,
        postprune_reduce_ns: 0,
        sample_ns: 0,
        pairs: 0,
        prune_calls: 0,
        prune_lines: 0,
        prune_tie_evals: 0,
        prune_hull_lines: 0,
        merge_selected: 0,
        pairs_enumerated: 0,
        merge_legacy_fallbacks: 0,
        merge_fb_b_sign: 0,
        merge_fb_flat: 0,
        merge_fb_empty_pieces: 0,
        merge_fb_empty_selection: 0,
        merge_fb_y_disorder: 0,
        merge_fb_cmp_overflow: 0,
        exact_compose_fallbacks: 0,
        exact_fb_overflow: 0,
        exact_fb_b_sign: 0,
        exact_fb_flat: 0,
        exact_fb_empty_pieces: 0,
        exact_fb_empty_selection: 0,
        exact_fb_y_disorder: 0,
        exact_fb_cmp_overflow: 0,
        exact_hull_pairs: 0,
        exact_pairs_enumerated: 0,
        exact_hull_lines: 0,
        exact_compose_ns: 0,
    };

    /// Aggregate one worker thread's per-path counters into these cycle
    /// totals (replaces the engine's per-field atomic hand-aggregation).
    pub fn merge(&mut self, other: &Self) {
        self.evaluated += other.evaluated;
        self.skipped += other.skipped;
        self.unsupported += other.unsupported;
        self.none_hop_unmapped += other.none_hop_unmapped;
        self.none_degenerate += other.none_degenerate;
        self.none_overflow += other.none_overflow;
        self.duration_ns += other.duration_ns;
        self.prefix_hits += other.prefix_hits;
        self.cl_fan_hits += other.cl_fan_hits;
        self.cl_fan_misses += other.cl_fan_misses;
        self.boundaries_composed += other.boundaries_composed;
        self.product_ns += other.product_ns;
        self.prune_stage1_ns += other.prune_stage1_ns;
        self.prune_hull_ns += other.prune_hull_ns;
        self.derive_ns += other.derive_ns;
        self.compose_ns += other.compose_ns;
        self.search_ns += other.search_ns;
        self.search_segments += other.search_segments;
        self.postprune_reduce_ns += other.postprune_reduce_ns;
        self.sample_ns += other.sample_ns;
        self.pairs += other.pairs;
        self.prune_calls += other.prune_calls;
        self.prune_lines += other.prune_lines;
        self.prune_tie_evals += other.prune_tie_evals;
        self.prune_hull_lines += other.prune_hull_lines;
        self.merge_selected += other.merge_selected;
        self.pairs_enumerated += other.pairs_enumerated;
        self.merge_legacy_fallbacks += other.merge_legacy_fallbacks;
        self.merge_fb_b_sign += other.merge_fb_b_sign;
        self.merge_fb_flat += other.merge_fb_flat;
        self.merge_fb_empty_pieces += other.merge_fb_empty_pieces;
        self.merge_fb_empty_selection += other.merge_fb_empty_selection;
        self.merge_fb_y_disorder += other.merge_fb_y_disorder;
        self.merge_fb_cmp_overflow += other.merge_fb_cmp_overflow;
        self.exact_compose_fallbacks += other.exact_compose_fallbacks;
        self.exact_fb_overflow += other.exact_fb_overflow;
        self.exact_fb_b_sign += other.exact_fb_b_sign;
        self.exact_fb_flat += other.exact_fb_flat;
        self.exact_fb_empty_pieces += other.exact_fb_empty_pieces;
        self.exact_fb_empty_selection += other.exact_fb_empty_selection;
        self.exact_fb_y_disorder += other.exact_fb_y_disorder;
        self.exact_fb_cmp_overflow += other.exact_fb_cmp_overflow;
        self.exact_hull_pairs += other.exact_hull_pairs;
        self.exact_pairs_enumerated += other.exact_pairs_enumerated;
        self.exact_hull_lines += other.exact_hull_lines;
        self.exact_compose_ns += other.exact_compose_ns;
    }
}

/// Apply `f` to the caller-owned gate accumulator. Replaces the retired
/// gate thread-local: gate stats now ride the mixed solve outcome
/// (the asymmetric twin of `WalkOutcome`'s walk stats — one value out,
/// no thread-local read-back).
#[inline]
pub(crate) fn record_gate<R>(stats: &mut GateStats, f: impl FnOnce(&mut GateStats) -> R) -> R {
    f(stats)
}

/// The gate's typed verdict: [`Envelope::Bound`] is a
/// rigorous upper bound on `max_x [path_output(x) − x]` — skip ONLY when its
/// value is at or below the caller's profit floor. [`Envelope::Unsupported`]
/// means NO sound bound exists: the path is SOLVED unscreened, never skipped
/// (type-enforced replacement of the overloaded `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Envelope {
    Bound(U256),
    Unsupported(GateSkipCause),
}

/// Why no bound was derivable (the per-cause counters name the same
/// three exits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateSkipCause {
    /// A hop slot was `None` (caller couldn't map the family).
    UnmappedHop,
    /// A hop's line derivation rejected (zero reserves / no reachable
    /// liquidity).
    DegenerateHop,
    /// A coefficient or domain overflow made the bound unusable.
    DomainOverflow,
}

/// The gate's per-call dependency value. Carries everything that used to be
/// process-global: the solve-cycle epoch (the prefix cache drops entries
/// from an older epoch on first touch), the cache opt-in, and the optional
/// degenerate-path capture sink. One interface, no hidden state.
#[derive(Clone, Copy, Debug, Default)]
pub struct GateDeps<'a> {
    pub(crate) epoch: u64,
    pub(crate) prefix_cache: bool,
    /// The owner-scoped prefix store (C4); `None` disables reuse for this
    /// solve (offline deps, tests). Replaces the retired process static.
    pub(crate) prefix_store: Option<&'a PrefixCache>,
    /// The caller-supplied degenerate-path capture sink (C2.3); `None` (the
    /// offline/test default) captures nothing. The solver performs no I/O —
    /// the engine's file-writing adapter implements
    /// [`DegeneratePathSink`].
    pub(crate) capture: Option<&'a dyn DegeneratePathSink>,
    /// The engine-owned cross-block walk-memo handle; `None`
    /// disables the memo for this solve.
    pub(crate) walk_memo: Option<&'a crate::cl::WalkMemo>,
    /// The engine-owned walk telemetry collector; `None` disables the ns
    /// census for this solve. Mirrors [`Self::walk_memo`] — the collector and
    /// the memo are constructed adjacently at the engine.
    pub(crate) telemetry: Option<&'a crate::cl::WalkTelemetry>,
    /// the owner's runtime stance (envelope caps + trace gate),
    /// instance-scoped and passed down — the gate reads no environment.
    pub(crate) runtime: SolveRuntimeConfig,
}

impl GateDeps<'_> {
    /// Offline/cacheless: no prefix reuse, epoch 0, capture off, no memo.
    #[must_use]
    pub fn offline() -> Self {
        Self::default()
    }

    /// Offline/cacheless with a custom runtime stance: the offline posture
    /// (no prefix reuse, epoch 0, capture off, no memo) carrying the caller's
    /// `runtime` (envelope caps + trace gate) instead of the default.
    #[must_use]
    pub fn offline_with_runtime(runtime: SolveRuntimeConfig) -> Self {
        Self {
            runtime,
            ..Self::default()
        }
    }

    /// The owner's prefix store + epoch as the chain-composition cache
    /// handle, or `None` when prefix reuse is out of stance for this solve.
    /// The gate verdict and the walk-side bound both derive their cache here,
    /// so one solve shares one cache and the CL fans are derived once.
    #[must_use]
    pub(crate) fn fan_cache(&self) -> Option<(&PrefixCache, u64)> {
        if self.prefix_cache {
            self.prefix_store.map(|store| (store, self.epoch))
        } else {
            None
        }
    }

    /// Production solve cycle: the prefix cache against this block's epoch,
    /// served from the owner's `PrefixCache` (C4 — the constructor takes the
    /// store so a caller cannot arm the bool and silently get None). The
    /// optional capture sink and the engine-owned walk-memo handle ride the
    /// same call; `prefix_cache` is armed here because the store is required,
    /// so the bool can no longer disagree with it.
    #[must_use]
    pub fn per_block<'a>(
        epoch: u64,
        prefix_store: &'a PrefixCache,
        capture: Option<&'a dyn DegeneratePathSink>,
        walk_memo: Option<&'a crate::cl::WalkMemo>,
        runtime: SolveRuntimeConfig,
    ) -> GateDeps<'a> {
        GateDeps {
            epoch,
            prefix_cache: true,
            prefix_store: Some(prefix_store),
            capture,
            walk_memo,
            // The collector's injection point: the bot attaches it with
            // [`Self::with_telemetry`] from the block scope it already holds;
            // offline and test deps keep the census off here.
            telemetry: None,
            runtime,
        }
    }

    /// The engine-owned walk-memo handle, if any (`None` disables it).
    #[must_use]
    pub fn walk_memo(&self) -> Option<&crate::cl::WalkMemo> {
        self.walk_memo
    }

    /// The engine-owned walk telemetry collector, if any (`None` disables the
    /// ns census for this solve).
    #[must_use]
    pub fn telemetry(&self) -> Option<&crate::cl::WalkTelemetry> {
        self.telemetry
    }
}

impl<'a> GateDeps<'a> {
    /// The same deps with the engine-owned walk telemetry collector attached —
    /// the bot's per-path construction point (the collector's `Arc` lives on
    /// the block scope beside the memo, so the deps for a block carry every
    /// handle the walk needs). `None` leaves the ns census off for this solve,
    /// exactly as [`Self::per_block`] seeds it.
    #[must_use]
    pub fn with_telemetry(mut self, telemetry: Option<&'a crate::cl::WalkTelemetry>) -> Self {
        self.telemetry = telemetry;
        self
    }
}

/// The ONE gate entry: a rigorous upper bound on
/// `max_x [path_output(x) − x]`, or a typed [`GateSkipCause`]. Crossings ride
/// each CL hop's `ClHop` descriptor; the prefix-composition cache is keyed
/// on full hop content and generationed by `GateDeps::epoch`.
#[must_use]
pub fn path_profit_bound(hops: &[Option<HopMath<'_>>], deps: &GateDeps<'_>) -> Envelope {
    path_profit_bound_impl(hops, deps, None).0
}

/// [`path_profit_bound`] plus the per-path gate counters it produced — the
/// returned-value form (no thread-local read-back). The bot consumes the
/// counters from the mixed solve outcome; the offline harnesses read them
/// here.
#[must_use]
pub fn path_profit_bound_with_stats(
    hops: &[Option<HopMath<'_>>],
    deps: &GateDeps<'_>,
) -> (Envelope, GateStats) {
    path_profit_bound_impl(hops, deps, None)
}

/// [`path_profit_bound`] with the caller's skip floor threaded into the
/// discrete concave max scan.
///
/// The caller's only use of the result is the sign of `bound <= min_profit`
/// (skip). Once a hull segment's candidates push the running best STRICTLY
/// above the floor, the "don't skip" verdict is established — later segments
/// can only raise the max — so the scan returns without evaluating the
/// remainder. The returned value keeps the exact `best + best/2048` slack
/// contract; only its magnitude may be smaller (never below the floor), so
/// the skip verdict is unchanged.
#[must_use]
pub fn path_profit_bound_with_floor(
    hops: &[Option<HopMath<'_>>],
    deps: &GateDeps<'_>,
    min_profit: U256,
) -> (Envelope, GateStats) {
    path_profit_bound_impl(hops, deps, Some(min_profit))
}

fn path_profit_bound_impl(
    hops: &[Option<HopMath<'_>>],
    deps: &GateDeps<'_>,
    floor: Option<U256>,
) -> (Envelope, GateStats) {
    let mut stats = GateStats::EMPTY;
    let gate_t0 = std::time::Instant::now();
    let result = path_profit_bound_inner(hops, deps, floor, &mut stats);
    stats.duration_ns = gate_t0.elapsed().as_nanos();
    match result {
        Ok(b) => {
            stats.evaluated += 1;
            (Envelope::Bound(b), stats)
        }
        Err(cause) => {
            stats.unsupported += 1;
            match cause {
                GateSkipCause::UnmappedHop => stats.none_hop_unmapped += 1,
                GateSkipCause::DegenerateHop => stats.none_degenerate += 1,
                GateSkipCause::DomainOverflow => stats.none_overflow += 1,
            }
            (Envelope::Unsupported(cause), stats)
        }
    }
}

/// The composed path bound: the lower-envelope line set plus the domain
/// (sum of the per-hop output caps) it was pruned against.
pub(crate) struct PathBound {
    pub lines: Vec<Line>,
    pub xmax: U256,
}

/// Why the chain-composition path failed, plus the hop it failed on (the
/// gate reports the reason and captures the state; the walk-side bound just
/// needs the `None`).
pub(crate) struct PathBoundError {
    pub hop_index: usize,
    pub cause: GateSkipCause,
}

/// The ONE chain-composition path. Derive every hop's line set — the
/// pool-static CL tangent fans reused from `cache` when present — then chain
/// the sets through the exact concave-envelope composition, reusing the
/// owner's composed-prefix memo when present. The gate verdict and the
/// walk-side bound are two queries against this: for identical inputs they
/// yield the identical hull by construction, and the CL fans are derived
/// once (the second query is all cache hits).
#[expect(clippy::too_many_lines)]
pub(crate) fn compose_path_bound(
    hops: &[Option<HopMath<'_>>],
    cfg: &SolveRuntimeConfig,
    cache: Option<(&PrefixCache, u64)>,
    stats: &mut GateStats,
) -> Result<PathBound, PathBoundError> {
    let mut all_hops: Vec<(Vec<Line>, U256)> = Vec::with_capacity(hops.len());
    let mut xmax = U256::ZERO;
    let phase_derive = std::time::Instant::now();
    for (hop_idx, slot) in hops.iter().enumerate() {
        let Some(hop) = slot.as_ref() else {
            return Err(PathBoundError {
                hop_index: hop_idx,
                cause: GateSkipCause::UnmappedHop,
            });
        };
        let Some((hop_ls, cap)) = hop_lines_and_cap_cached(hop.clone(), cfg, cache, stats) else {
            return Err(PathBoundError {
                hop_index: hop_idx,
                cause: GateSkipCause::DegenerateHop,
            });
        };
        if let Some(v) = xmax.checked_add(cap) {
            xmax = v;
        } else {
            return Err(PathBoundError {
                hop_index: hop_idx,
                cause: GateSkipCause::DomainOverflow,
            });
        }
        all_hops.push((hop_ls, cap));
    }
    record_gate(stats, |t| t.derive_ns += phase_derive.elapsed().as_nanos());
    let phase_compose = std::time::Instant::now();
    // Second pass against the FULL known input domain. Every line that
    // cannot be beaten below within [0,domain] can never best at any path
    // input, so the pruning endpoint assumption holds at discard time.
    let mut lines2 = vec![Line::IDENTITY];
    let domain = xmax;
    // Prefix-composition cache (loop-8): every chainable hop contributes a
    // FULL-CONTENT key — the CL hop's whole crossing table hashed (a content
    // hash, not an allocation pointer; the pointer key + endpoint-fingerprint
    // revalidation pair is retired), the Möbius hop a reserves+fee hash. A
    // 128-bit FNV hit is the only stale-serve risk — the same trust level
    // the Möbius key already carried.
    //
    // Cross-DOMAIN reuse is sound WITHOUT keying on the domain: every
    // stored line globally dominates the true prefix output (tangent
    // property), so serving a set pruned under a different domain can
    // only shift the bound's TIGHTNESS (a smaller domain's entry is a
    // subset of the reader's fresh set → looser bound → skips less; a
    // larger domain's entry is a superset → tighter bound → more skips,
    // each still justified: the bound remains an upper bound of the true
    // curve). Skip validity is the only contract, so reuse stays keyed
    // on the chain alone.
    let mobius_key =
        |reserve_in: &U256, reserve_out: &U256, gamma_numer: &U256, fee_denom: &U256| {
            let mut h = content_mix_u256(0xcbf2_9ce4_8422_2325_u128, reserve_in);
            h = content_mix_u256(h, reserve_out);
            h = content_mix_u256(h, gamma_numer);
            content_mix_u256(h, fee_denom)
        };
    let hop_key = |hop_idx: usize| -> Option<HopCacheKey> {
        match hops.get(hop_idx).and_then(Option::as_ref) {
            Some(HopMath::Cl(ch)) => {
                Some(HopCacheKey::ClTable(cl_table_key(ch.crossings.as_ref())))
            }
            Some(HopMath::V2(h)) => Some(HopCacheKey::MobiusHop(mobius_key(
                &h.reserve_in,
                &h.reserve_out,
                &h.gamma_numer,
                &h.fee_denom,
            ))),
            _ => None,
        }
    };
    let mut chain: Vec<HopCacheKey> = Vec::with_capacity(hops.len());
    for (hop_idx, (hop_ls, _)) in all_hops.iter_mut().enumerate() {
        match if cache.is_some() {
            hop_key(hop_idx)
        } else {
            None
        } {
            Some(k) => chain.push(k),
            None => chain.clear(),
        }
        let chainable = cache.is_some() && !chain.is_empty();
        if chainable {
            // Cache lookup for this exact content chain. The key IS the full
            // per-hop content, so a hit is a content match; entries from an
            // older epoch are dropped on first touch of the new one (no
            // public reset — the epoch rides [GateDeps]).
            let hit = cache.and_then(|(store, epoch)| store.get(epoch, &chain));
            if let Some(hit_lines) = hit {
                lines2 = hit_lines;
                record_gate(stats, |t| t.prefix_hits += 1);
                continue;
            }
        }
        record_gate(stats, |t| t.boundaries_composed += 1);
        // Prune each hop's tangent lines BEFORE composition.
        //
        // Soundness: CL swap output is monotonically increasing (more input
        // → more output). If line A dominates line B within this hop
        // (A ≤ B at both x=0 and x=domain), then for any subsequent
        // increasing composition C, C∘A ≤ C∘B — the domination survives
        // composition. So dropping B before the product loop changes
        // nothing about the final envelope.
        //
        // Effect: collapses a 3000-line CL hop to ~50 Pareto-front survivors
        // BEFORE composition. That prune happens INSIDE
        // `compose_envelopes_exact` exactly once; the loop-head prune was
        // removed because the double-prune was unmeasured byte-identity
        // territory plus pure waste (the differential feeds RAW hop sets
        // exactly like production does).
        let hop_ls_len_dbg = hop_ls.len();
        // Fallback cap for the sampled reference: the exact path ignores it
        // (its hull is <= K1 + K2), but the documented overflow/ambiguity
        // fallback still applies the owner's `sampled_compose_lines` stance.
        let compose_cap = cfg.sampled_compose_lines.max(1);
        // Exact concave-envelope composition: both sides are initialised at
        // their (min-over-lines) hull pieces, only the <= K1 + K2
        // y-overlapping pairs are composed, and the canonical hull is kept
        // without sampling (see `compose_envelopes_exact`). The old
        // product+sample path survives only as the documented fallback
        // (overflow / ambiguity), counted via TLS.
        let mut next: Vec<Line> =
            compose_envelopes_exact(hop_ls, &lines2, domain, compose_cap, stats).map_err(
                |cause| PathBoundError {
                    hop_index: hop_idx,
                    cause,
                },
            )?;
        // One reduction pass per hop boundary (O(survivors)) — replaces the
        // per-pair reduction removed from Line::compose. Byte-identical
        // coefficients to the old per-pair pass (same ceil/floor rules).
        {
            let t0 = std::time::Instant::now();
            for l in &mut next {
                l.reduce(COMPOSE_TARGET_BITS);
            }
            record_gate(stats, |t| t.postprune_reduce_ns += t0.elapsed().as_nanos());
        }
        // sampled_compose_lines() cap: the composing side is dropped to a
        // uniform Pareto-order sample across the lower envelope, bounding
        // the next product at K². Sound by the same argument as the CL
        // tangent cap: min(fewer lines) ≥ min(all lines), so the bound can
        // only rise (skip less, never more). With the live min-profit floor
        // of zero the tightness loss does not affect skips.
        // Cache the composed prefix set under the content-key chain. Only
        // miss paths reach here; a hit path returns early above.
        if chainable {
            if let Some((store, epoch)) = cache {
                store.insert(epoch, chain.clone(), next.clone());
            }
        }
        trace_boundary(hop_idx, hop_ls_len_dbg, next.len(), &next);
        lines2 = next;
    }
    record_gate(stats, |t| {
        t.compose_ns += phase_compose.elapsed().as_nanos();
    });
    // Diagnostic: line explosion is the gate bottleneck on dense-CL paths.
    #[cfg(not(feature = "hotpath"))]
    if lines2.len() > 200 {
        let hop_counts: Vec<usize> = all_hops.iter().map(|(ls, _)| ls.len()).collect();
        op_warn!(domain = solver, gate_lines = lines2.len(),
            gate_hop_line_counts = ?hop_counts,
            gate_domain = %domain,
            "composed tangent-line explosion"
        );
    }
    Ok(PathBound {
        lines: lines2,
        xmax,
    })
}

/// The gate verdict tail: the discrete concave max of
/// `f(x) = min_lines(x) − x` over `[0, xmax]`, plus the unconditional
/// `best/2048` rounding slack. Split out so the walk-side equivalence test
/// maximizes the walk-side hull with the exact arithmetic the gate uses.
fn finalize_bound(
    lines: &[Line],
    xmax: U256,
    floor: Option<U256>,
    stats: &mut GateStats,
) -> Result<U256, GateSkipCause> {
    let phase_search = std::time::Instant::now();
    // Discrete concave max of f(x) = min_lines(x) − x over [0, xmax].
    if xmax.is_zero() {
        return Ok(U256::ZERO);
    }
    let best = concave_max(lines, xmax, floor, stats);
    record_gate(stats, |t| t.search_ns += phase_search.elapsed().as_nanos());
    // Rounding slack: composed reductions and I512 ceiling evaluation can
    // leave the derived lower envelope a hair BELOW the true curve. The
    // deficit is ~2^-11 of the bound per reduction for very deep chains
    // (block 25826949 path 400: 200M under-cut on a 7.23e13 bound), but the
    // same per-hop reductions compound on moderate chains too: block
    // 25826949 path 704 under-cut 3.7e15 on a 2.7e23 bound while its
    // composed survivor count stayed <= 200, so the old lines>200 gate
    // skipped the slack. The slack must be UNCONDITIONAL — soundness of the
    // skip decision is the only contract; the 1/2048 (~0.05%) looseness is
    // invisible to live skips against the incumbent/floor comparisons.
    if let Some(b) = narrow(best) {
        return Ok(b.saturating_add(b / U256::from(2048u64)));
    }
    Err(GateSkipCause::DomainOverflow)
}

/// The gate entry: one query against `compose_path_bound` (the SAME path
/// the walk-side bound uses), with the degenerate-hop diagnostic + capture on
/// the error exit, then the verdict max.
fn path_profit_bound_inner(
    hops: &[Option<HopMath<'_>>],
    deps: &GateDeps<'_>,
    floor: Option<U256>,
    stats: &mut GateStats,
) -> Result<U256, GateSkipCause> {
    let bound = match compose_path_bound(hops, &deps.runtime, deps.fan_cache(), stats) {
        Ok(bound) => bound,
        Err(e) => {
            if e.cause == GateSkipCause::DegenerateHop {
                if let Some(hop) = hops.get(e.hop_index).and_then(Option::as_ref) {
                    // Degenerate diagnostic: log the hop family + the reject
                    // reason so the steady-state degenerate rate can be
                    // classified as the expected shape (sparse CL with empty
                    // active range / zero reserves) vs a real coverage gap.
                    // Debug-level: opt-in via
                    // `RUST_LOG=degenbot_solvers::profit_envelope=debug`.
                    let family = match hop {
                        HopMath::V2(h) => {
                            let z = h.reserve_in.is_zero() || h.reserve_out.is_zero();
                            format!("V2(zero_reserve={z})")
                        }
                        HopMath::Cl(ch) => {
                            let empty = ch.seq.ranges.is_empty();
                            let reason = classify_cl_rejection(ch.seq);
                            format!(
                                "Cl(ranges={n},empty={empty},{reason})",
                                n = ch.seq.ranges.len()
                            )
                        }
                        HopMath::SolidlyVolatile {
                            reserve_in,
                            reserve_out,
                            ..
                        } => {
                            format!(
                                "SolidlyVolatile(zero={})",
                                reserve_in.is_zero() || reserve_out.is_zero()
                            )
                        }
                        HopMath::Weighted {
                            balance_in,
                            balance_out,
                            ..
                        } => {
                            format!(
                                "Weighted(zero={})",
                                balance_in.is_zero() || balance_out.is_zero()
                            )
                        }
                        HopMath::ReserveCap { reserve_out } => {
                            format!("ReserveCap(zero={})", reserve_out.is_zero())
                        }
                        HopMath::SolidlyStable {
                            reserves_0,
                            reserves_1,
                            ..
                        } => format!(
                            "SolidlyStable(zero={})",
                            reserves_0.is_zero() || reserves_1.is_zero()
                        ),
                    };
                    diag!(domain = solver, hop_index = e.hop_index,
                        family = %family,
                        "degenerate hop rejected (impossible to bound — solved unscreened)"
                    );
                    // Golden capture: hand the typed capture EVENT to the
                    // caller-supplied sink so the pool states can be replayed
                    // offline for fix experimentation. The sink owns the cap +
                    // the file adapter; `try_reserve` first so an over-cap path
                    // never builds the (100KB+) event.
                    if let Some(sink) = deps.capture {
                        if sink.try_reserve() {
                            let reason = match hop {
                                HopMath::Cl(ch) => classify_cl_rejection(ch.seq),
                                _ => family.clone(),
                            };
                            sink.capture(&DegeneratePathEvent::from_hops(
                                hops,
                                e.hop_index,
                                reason,
                            ));
                        }
                    }
                }
            }
            return Err(e.cause);
        }
    };
    finalize_bound(&bound.lines, bound.xmax, floor, stats)
}

#[must_use]
pub fn path_output_bound_at(
    hops: &[Option<HopMath<'_>>],
    x: &U256,
    cfg: &SolveRuntimeConfig,
) -> Option<U256> {
    let mut stats = GateStats::EMPTY;
    let mut lines = vec![Line::IDENTITY];
    for slot in hops {
        let Some(hop) = slot.as_ref() else {
            record_gate(&mut stats, |t| t.none_hop_unmapped += 1);
            return None;
        };
        let (hop_ls, _cap) = hop_lines_and_cap(hop.clone(), cfg, &mut stats)?;
        let mut next: Vec<Line> = Vec::with_capacity(lines.len() * hop_ls.len());
        for outer in &hop_ls {
            for inner in &lines {
                let Some(composed) = outer.compose(inner) else {
                    record_gate(&mut stats, |t| t.none_overflow += 1);
                    return None;
                };
                next.push(composed);
            }
        }
        prune(&mut next, *x, &mut stats);
        lines = next;
    }
    let mut best = I512::MAX;
    for l in &lines {
        let v = l.eval(x);
        if v < best {
            best = v;
        }
    }
    if best <= I512::ZERO {
        return Some(U256::ZERO);
    }
    narrow(best)
}

/// Walk-side composed envelope (Loop-21 `envelope_pruned_refine`): the line
/// set the active-set walk intersects its refine windows against. A second
/// query against the same `compose_path_bound` chain-composition path the
/// gate verdict uses — with the owner's cache threaded in, the walk-side
/// bound reuses the tangent fans and composed prefixes the gate already
/// derived instead of re-deriving them. Soundness contract identical to the
/// gate's: the lines pointwise-dominate the true path output, so any input
/// the lines disprove can never beat the walk's best.
#[derive(Debug)]
pub struct PathBoundLines {
    pub lines: Vec<Line>,
}

/// Compose the path's bound lines with no cache ([`path_bound_lines`] test
/// intake; same degenerate/overflow causes as the gate).
#[cfg(test)]
pub(crate) fn path_bound_lines(
    hops: &[Option<HopMath<'_>>],
    cfg: &SolveRuntimeConfig,
) -> Option<PathBoundLines> {
    let mut stats = GateStats::EMPTY;
    path_bound_lines_with_stats(hops, cfg, None, &mut stats)
}

/// [`path_bound_lines`] with the caller's gate accumulator threaded in: the
/// walk-refine envelope's derive / prune / exact-compose counters land in the
/// same `GateStats` the gate call returned (no thread-local read-back).
/// `cache` is the owner's prefix store + epoch (the gate derives it from its
/// [`GateDeps`] via `fan_cache`) — passing it makes the walk-side bound a
/// cache query against the hull the gate already built.
pub(crate) fn path_bound_lines_with_stats(
    hops: &[Option<HopMath<'_>>],
    cfg: &SolveRuntimeConfig,
    cache: Option<(&PrefixCache, u64)>,
    stats: &mut GateStats,
) -> Option<PathBoundLines> {
    let bound = compose_path_bound(hops, cfg, cache, stats).ok()?;
    Some(PathBoundLines { lines: bound.lines })
}

#[cfg(test)]
#[expect(clippy::expect_used)] // tiny literals; panic on typo is the point
pub(crate) mod tests {
    use super::*;

    use crate::cl::{build_cl_crossing_table, simulate_v3_range_swap, WalkTally};
    use crate::profit_envelope::derive::tests::hop_lines_and_cap;
    use crate::profit_envelope::derive::{ClHop, HopMath};
    use crate::profit_envelope::kernel::{Line, COMPOSE_TARGET_BITS};
    use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;
    use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};

    // Test shims: the production helpers now thread a caller-owned
    // accumulator instead of a thread-local. These tests never read the
    // prune / hop-line / reference-compose counters, so the shims discard
    // into a throwaway `GateStats`; the counter-reading tests call `super::*`
    // with an explicit local accumulator.
    pub(crate) fn prune(lines: &mut Vec<Line>, upper: U256) {
        super::prune(lines, upper, &mut GateStats::default());
    }

    /// T5 oracle v2: hop truth with CHAINED pricing (entry = previous
    /// range's exit bound — the same convention as `crossings()`), because
    /// captured `sqrt_price_x96` for non-head ranges is that range's own
    /// upper bound and may disagree with its bounds for pathological tail
    /// ranges. Consumes input range by range exactly as the walk does.
    fn chained_hop_out(seq: &IntV3TickRangeSequence, mut x: U256) -> U256 {
        let mut out = U256::ZERO;
        let n = seq.ranges.len();
        for i in 0..n {
            if x.is_zero() {
                break;
            }
            let r = &seq.ranges[i];
            // Chained entry price: a swap arriving at range i enters at the
            // previous range's exit bound (zfo: its lower bound; ofz: upper).
            // Capacity check: if the full crossing of range i exceeds the
            // remaining input, the swap lands inside — simulate with a clone
            // whose entry price is the chained one.
            let full = if r.liquidity == 0 {
                (U256::ZERO, U256::ZERO)
            } else {
                let mut gross = U256::ZERO;
                let target_out = U256::ZERO;
                // Reuse simulate_v3_range_swap with a saturated input to get
                // the full-crossing cost? Too heavy; instead detect landing
                // via accumulated crossing compare below.
                let _ = (&mut gross, target_out);
                (U256::ZERO, U256::ZERO)
            };
            let _ = full;
            let entry = if i == 0 {
                r.sqrt_price_x96
            } else if r.zero_for_one {
                seq.ranges[i - 1].sqrt_price_lower_x96
            } else {
                seq.ranges[i - 1].sqrt_price_upper_x96
            };
            let sim_hop = IntV3TickRangeHop {
                liquidity: r.liquidity,
                sqrt_price_x96: entry,
                sqrt_price_lower_x96: r.sqrt_price_lower_x96,
                sqrt_price_upper_x96: r.sqrt_price_upper_x96,
                gamma_numer: r.gamma_numer,
                fee_denom: r.fee_denom,
                zero_for_one: r.zero_for_one,
                word_boundary_prices: r.word_boundary_prices.clone(),
            };
            let res = simulate_v3_range_swap(x, &sim_hop, &mut WalkTally::default());
            out += res.output;
            x -= res.consumed_input;
        }
        out
    }

    /// T5 zoom 2: for the pathological tail range (index 46 of hop 1), print
    /// the raw fields, the crossing table's drain, and the per-range oracle's
    /// output at partial inputs, to pin which model diverges from on-chain
    /// computeSwapStep semantics.
    // Fixture JSON is committed data: a malformed fixture SHOULD panic the
    // test, so unwrap() is the honest call here.
    // Fixture JSON is committed data: panicking on a malformed fixture IS
    // the test behavior, and these fns deliberately stdout-dump their stage
    // derivations for --nocapture debugging.
    #[expect(
        clippy::unwrap_used,
        reason = "test fixture parsing - panicking is the desired test behavior"
    )]
    #[expect(
        clippy::print_stdout,
        reason = "stage-by-stage derivation dump for --nocapture debugging"
    )]
    #[test]
    fn gate_false_skip_93794_tail_range_dump() {
        let raw = include_str!("../../tests/fixtures/gate_false_skip_93794.json");
        let row: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
        let parse_range = |v: &serde_json::Value| -> IntV3TickRangeHop {
            IntV3TickRangeHop {
                liquidity: v["liquidity"].as_str().unwrap().parse().unwrap(),
                sqrt_price_x96: v["sqrt_price_x96"].as_str().unwrap().parse().unwrap(),
                sqrt_price_lower_x96: v["sqrt_price_lower_x96"].as_str().unwrap().parse().unwrap(),
                sqrt_price_upper_x96: v["sqrt_price_upper_x96"].as_str().unwrap().parse().unwrap(),
                gamma_numer: v["gamma_numer"].as_u64().unwrap(),
                fee_denom: v["fee_denom"].as_u64().unwrap(),
                zero_for_one: v["zero_for_one"].as_bool().unwrap(),
                word_boundary_prices: v["word_boundary_prices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|w| w.as_str().unwrap().parse().unwrap())
                    .collect(),
            }
        };
        let seqs: Vec<IntV3TickRangeSequence> = row["hops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hop| IntV3TickRangeSequence {
                ranges: hop
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(parse_range)
                    .collect::<Vec<_>>(),
            })
            .collect();
        let s = &seqs[1];
        assert!(s.ranges[0].zero_for_one);
        for idx in [44usize, 45, 46, 47] {
            let r = &s.ranges[idx];
            println!(
                "range[{idx}] liq={} sp0={} low={} high={} words={} gamma={}",
                r.liquidity,
                r.sqrt_price_x96,
                r.sqrt_price_lower_x96,
                r.sqrt_price_upper_x96,
                r.word_boundary_prices.len(),
                r.gamma_numer
            );
            if !r.word_boundary_prices.is_empty() {
                println!(
                    "    word[0]={} word[-1]={}",
                    r.word_boundary_prices[0],
                    r.word_boundary_prices[r.word_boundary_prices.len() - 1]
                );
            }
        }
        let crossings = build_cl_crossing_table(s);
        for idx in [44usize, 45, 46, 47] {
            println!(
                "cr[{idx}] acc_in={} acc_out={}",
                crossings[idx].crossing_gross_input, crossings[idx].crossing_output
            );
        }
        // Oracle: partial input into range 46 alone.
        for &x in &[
            1_000_000_000u64,
            10_000_000_000u64,
            100_000_000_000u64,
            1_000_000_000_000u64,
            5_000_000_000_000u64,
        ] {
            let res =
                simulate_v3_range_swap(U256::from(x), &s.ranges[46], &mut WalkTally::default());
            println!(
                "sim46(x={x}) out={} consumed={}",
                res.output, res.consumed_input
            );
        }
    }

    /// T5 forensic: the soak's gate produced bound=1 against golden profit
    /// 2.4e10 on a captured stable-pool 3-hop (block 25886170 path 93794;
    /// ranges/hop [102,48,312]). The envelope chain is on paper airtight
    /// (concave output curves -> entry tangents are global upper bounds;
    /// min-of-lines survives sampling; sound reductions only loosen), so a
    /// bound BELOW the true optimal profit means some stage under-cuts.
    /// This test walks the derivation stage by stage against an ORACLE built
    /// from production's own per-range step (`simulate_v3_range_swap`, the
    /// compute_swap_step_v3 parity path) and names the failing stage.
    #[expect(
        clippy::unwrap_used,
        reason = "test fixture parsing - panicking is the desired test behavior"
    )]
    #[expect(
        clippy::print_stdout,
        reason = "stage-by-stage derivation dump for --nocapture debugging"
    )]
    #[expect(
        clippy::panic,
        reason = "the test asserts an unsupported gate and panics by design"
    )]
    #[test]
    #[expect(clippy::too_many_lines)]
    fn gate_false_skip_93794_stage_bisect() {
        let raw = include_str!("../../tests/fixtures/gate_false_skip_93794.json");
        let row: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
        let golden_profit: U256 = row["golden"]["profit"]
            .as_str()
            .expect("golden profit")
            .parse()
            .expect("golden U256");

        let parse_range = |v: &serde_json::Value| -> IntV3TickRangeHop {
            IntV3TickRangeHop {
                liquidity: v["liquidity"].as_str().unwrap().parse().unwrap(),
                sqrt_price_x96: v["sqrt_price_x96"].as_str().unwrap().parse().unwrap(),
                sqrt_price_lower_x96: v["sqrt_price_lower_x96"].as_str().unwrap().parse().unwrap(),
                sqrt_price_upper_x96: v["sqrt_price_upper_x96"].as_str().unwrap().parse().unwrap(),
                gamma_numer: v["gamma_numer"].as_u64().unwrap(),
                fee_denom: v["fee_denom"].as_u64().unwrap(),
                zero_for_one: v["zero_for_one"].as_bool().unwrap(),
                word_boundary_prices: v["word_boundary_prices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|w| w.as_str().unwrap().parse().unwrap())
                    .collect(),
            }
        };
        let seqs: Vec<IntV3TickRangeSequence> = row["hops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hop| IntV3TickRangeSequence {
                ranges: hop
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(&parse_range)
                    .collect::<Vec<_>>(),
            })
            .collect();
        assert_eq!(seqs.len(), 3);

        // --- Oracle: production-parity exact output for one CL hop at input x
        // (each range via simulate_v3_range_swap; boundary crossings carry the
        // unconsumed remainder to the next range).
        let hop_truth = |seq: &IntV3TickRangeSequence, x: U256| -> U256 { chained_hop_out(seq, x) };

        // --- Stage 1: per-hop tangent lines vs the oracle on a grid.
        let mut all_hop_lines: Vec<Vec<Line>> = Vec::new();
        let mut xmax = U256::ZERO;
        for (hi, s) in seqs.iter().enumerate() {
            let view = HopMath::Cl(ClHop {
                seq: s,
                crossings: std::borrow::Cow::Owned(build_cl_crossing_table(s)),
            });
            let (lines, cap) =
                hop_lines_and_cap(view, &SolveRuntimeConfig::default()).expect("hop derivable");
            xmax = xmax.checked_add(cap).expect("domain sum");
            // Grid over [0, 2*cap]; the hop's search domain within a chain is
            // its input volume, capped by cap.
            let probe_max = cap.saturating_mul(U256::from(2u8));
            let mut worst_gap = I512::ZERO;
            let mut worst_x = U256::ZERO;
            let n_grid = 256u32;
            for i in 0..=n_grid {
                let x = probe_max / U256::from(n_grid) * U256::from(i);
                let truth = I512::try_from(U512::from(hop_truth(s, x))).unwrap_or(I512::MAX);
                let bnd = lines
                    .iter()
                    .map(|l| l.eval(&x))
                    .min()
                    .expect("lines non-empty");
                if bnd < truth && truth - bnd > worst_gap {
                    worst_gap = truth - bnd;
                    worst_x = x;
                }
            }
            println!(
                "stage1 hop{hi}: lines={} cap={cap} worst_gap={worst_gap} at x={worst_x}",
                lines.len()
            );
            assert!(
                worst_gap.is_zero(),
                "hop{hi} line set UNDER-CUTS its true curve at x={worst_x} by {worst_gap}"
            );
            all_hop_lines.push(lines);
        }

        // --- Stage 2: replicate the compose chain (prune + compose + sample
        // + reduce per boundary) and check the composed envelope against the
        // path oracle on the same grid.
        let mut acc: Vec<Line> = vec![Line::IDENTITY];
        for (hi, hop_ls) in all_hop_lines.iter().enumerate() {
            let mut hls = hop_ls.clone();
            prune(&mut hls, xmax);
            let mut next: Vec<Line> = Vec::new();
            for outer in &hls {
                for inner in &acc {
                    next.push(outer.compose(inner).expect("compose ok on grid test"));
                }
            }
            prune(&mut next, xmax);
            for l in &mut next {
                l.reduce(COMPOSE_TARGET_BITS);
            }
            // sampled_compose_lines cap (default 48).
            if next.len() > 48 {
                let step = next.len() / 48;
                let mut sampled: Vec<Line> = Vec::new();
                let mut i = 0;
                while i < next.len() {
                    sampled.push(next[i]);
                    i += step.max(1);
                }
                if sampled.last() != Some(&next[next.len() - 1]) {
                    sampled.push(next[next.len() - 1]);
                }
                next = sampled;
            }
            acc = next;

            // Composed-envelope check at this boundary vs the truth using
            // only the composed hops 0..=hi.
            let mut gap = I512::ZERO;
            let mut gap_x = U256::ZERO;
            let n_grid = 256u32;
            for i in 0..=n_grid {
                let x = xmax / U256::from(n_grid) * U256::from(i);
                let mut truth = U256::ZERO;
                {
                    let mut y = x;
                    for s in &seqs[..=hi] {
                        truth = hop_truth(s, y);
                        y = truth;
                    }
                }
                let t = I512::try_from(U512::from(truth)).unwrap_or(I512::MAX);
                let bnd = acc.iter().map(|l| l.eval(&x)).min().expect("nonempty");
                if bnd < t && t - bnd > gap {
                    gap = t - bnd;
                    gap_x = x;
                }
            }
            println!(
                "stage2 after hop{hi}: survivors={} xmax={xmax} worst_gap={gap} at x={gap_x}",
                acc.len()
            );
            assert!(
                gap.is_zero(),
                "composed envelope UNDER-CUTS the true {}-hop curve at x={gap_x} by {gap}",
                hi + 1
            );
        }

        // --- Stage 3: full gate call must clear the golden.
        let views: Vec<Option<HopMath>> =
            seqs.iter().map(|s| Some(HopMath::cl_derived(s))).collect();
        match path_profit_bound(&views, &GateDeps::offline()) {
            Envelope::Bound(b) => {
                println!("stage3 bound={b} golden={golden_profit}");
                assert!(
                    b >= golden_profit,
                    "gate bound {b} below golden {golden_profit}"
                );
            }
            other @ Envelope::Unsupported(_) => panic!("gate unsupported: {other:?}"),
        }
    }

    /// Dense zero-for-one CL sequence: `n` contiguous 1-tick ranges below the
    /// anchor with `liq` (a fat table that trips the tangent sample cap).
    fn fat_walk_seq(n: i32, liq: u128) -> IntV3TickRangeSequence {
        let sp_at =
            |tick: i32| U256::from(get_sqrt_ratio_at_tick_internal(tick).unwrap_or_default());
        let ranges = (0..n)
            .map(|i| {
                let (tick_lo, tick_hi) = (-(i + 1), -i);
                IntV3TickRangeHop {
                    liquidity: liq,
                    sqrt_price_x96: sp_at(-i),
                    sqrt_price_lower_x96: sp_at(tick_lo),
                    sqrt_price_upper_x96: sp_at(tick_hi),
                    gamma_numer: 997_000,
                    fee_denom: 1_000_000,
                    zero_for_one: true,
                    word_boundary_prices: Vec::new(),
                }
            })
            .collect();
        IntV3TickRangeSequence::new(ranges).expect("valid sequence")
    }

    /// C2.2 kill witness: the walk-side bound is the SAME chain-composition
    /// path as the gate verdict. For identical inputs both queries return the
    /// identical hull, so maximizing the walk-side lines with the gate's own
    /// arithmetic reproduces the gate's bound exactly — and the kill is
    /// observable: the walk-side query records zero CL-fan misses (it reuses
    /// the fans the gate derived) and one hit per CL hop, where before this
    /// change it re-derived every fan uncached.
    #[test]
    #[expect(
        clippy::panic,
        reason = "the test asserts a bounded gate and panics by design"
    )]
    fn walk_side_bound_equals_gate_bound_and_reuses_fans() {
        // Two DISTINCT dense CL hops (distinct fan keys), so the fan cache is
        // exercised hop by hop.
        let seq_a = fat_walk_seq(120, 1_000_000_000_000);
        let seq_b = fat_walk_seq(95, 1_000_000_000_000);
        let views: Vec<Option<HopMath<'_>>> = vec![
            Some(HopMath::Cl(ClHop {
                seq: &seq_a,
                crossings: std::borrow::Cow::Owned(build_cl_crossing_table(&seq_a)),
            })),
            Some(HopMath::Cl(ClHop {
                seq: &seq_b,
                crossings: std::borrow::Cow::Owned(build_cl_crossing_table(&seq_b)),
            })),
        ];
        let cfg = SolveRuntimeConfig::default();
        let n_cl = 2u64;
        let store = PrefixCache::new();
        let deps = GateDeps::per_block(1, &store, None, None, cfg);

        // Query 1 — the gate verdict, cold: each CL hop's pool-static fan is
        // derived exactly once.
        let (gate_env, gs_gate) = path_profit_bound_with_stats(&views, &deps);
        let gate_bound = match gate_env {
            Envelope::Bound(b) => b,
            other @ Envelope::Unsupported(_) => panic!("gate must produce a bound, got {other:?}"),
        };
        assert_eq!(
            gs_gate.cl_fan_misses, n_cl,
            "gate must derive each CL fan once, cold"
        );
        assert_eq!(gs_gate.cl_fan_hits, 0);

        // Query 2 — the walk-side bound against the SAME cache/epoch: the
        // fans are REUSED (zero misses), so the double derivation is dead,
        // observably.
        let mut gs_walk = GateStats::EMPTY;
        let walk = path_bound_lines_with_stats(&views, &cfg, deps.fan_cache(), &mut gs_walk)
            .expect("walk bound derivable");
        assert_eq!(
            gs_walk.cl_fan_misses, 0,
            "walk side re-derived the CL fans (the kill failed)"
        );
        assert_eq!(
            gs_walk.cl_fan_hits, n_cl,
            "walk side must reuse the gate's fans"
        );

        // The walk-side hull IS the gate's hull: maximize it with the gate's
        // own arithmetic (same concave max + slack) and the verdict matches.
        let xmax: U256 = {
            let mut acc = U256::ZERO;
            for slot in &views {
                let Some(hop) = slot.clone() else {
                    continue;
                };
                let (_, cap) = hop_lines_and_cap(hop, &cfg).expect("hop derivable");
                acc = acc.checked_add(cap).expect("domain sum");
            }
            acc
        };
        let mut finalize_stats = GateStats::EMPTY;
        let walk_bound = finalize_bound(&walk.lines, xmax, None, &mut finalize_stats)
            .expect("walk bound finalizable");
        assert_eq!(walk_bound, gate_bound, "walk-side bound != gate bound");

        // And the cached hull equals a fresh cacheless derivation, byte for
        // byte (the cache is a pure memo, not a different derivation).
        let mut gs_fresh = GateStats::EMPTY;
        let fresh =
            path_bound_lines_with_stats(&views, &cfg, None, &mut gs_fresh).expect("derivable");
        assert_eq!(
            fresh.lines, walk.lines,
            "cached hull diverged from uncached"
        );
    }
}
