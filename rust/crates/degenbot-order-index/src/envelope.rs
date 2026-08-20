//! [`EnvelopeIndex`]: the upper-left convex-hull order index.
//!
//! The lossless hot/cold split is the core idea (see the crate root docs): a
//! result is a point `(gas, gross)` and `net(X) = gross - gas*X` is *linear* in
//! `X`, so only the **argmax is on the hull** — the full top-K is not. The hull
//! instead provides an exact **upper bound** on any result's net:
//!
//! ```text
//! upper_bound(p, X) = max(net(a, X), net(b, X))   // endpoint nets of the
//!                                                 // hull edge bracketing p
//! ```
//!
//! with `T = kth_hull_net(X, k)` (the k-th largest net among hull vertices).
//! `upper_bound(p, X) < T  =>  net(p, X) < kth_overall_net  =>  p ∉ top-K`, so a
//! result can be provably evicted to the cold set without ever losing a top-K
//! result. `top_k` exact-ranks only the hot set.
//!
//! ## Dynamic maintenance (GRFRXI)
//!
//! The **hull is X-independent** (pure geometry of `gas`/`gross`) and is held
//! as a **snapshot** independent of the live `points` indices, so mutations
//! that do not touch a hull vertex are O(1) with no index invalidation:
//!
//! - `insert` / non-hull `update`: splice the point into the hull *only if it
//!   pokes above* (lossless-critical), else leave the hull unchanged.
//! - non-hull `remove`: O(1) — the snapshot hull already dominates the removed
//!   (below-hull) point, so removing it cannot expose any gap.
//! - **hull-vertex `update`/`remove`**: full `rebuild()` — correct, and bounded
//!   by the (small) fraction of points that are actually on the frontier. The
//!   S2 strategy's *deferred demotion* plus a periodic `rebuild()` to tighten
//!   is the refinement layered on top of this in a later pass.
//!
//! ## Per-block `top_k`: gas-ordered hot range (A33CRA)
//!
//! The hot set is classified **per gas value, not per point**: `upper_bound`
//! depends on a point only through the hull edge bracketing `p.gas`, so a gas
//! `g` is hot iff `bound(g, X) >= T` where
//!
//! ```text
//! g <  v_0.gas               ->  net(v_0)
//! g == v_i.gas               ->  net(v_i)
//! v_i.gas < g < v_{i+1}.gas  ->  max(net(v_i), net(v_{i+1}))
//! g >  v_{h-1}.gas            ->  net(v_{h-1})
//! ```
//!
//! `v_{L..R}` denoting the hot vertices (`net(v_i, X) >= T`; the vertex nets of
//! a convex envelope are unimodal at fixed `X`, so `L..R` is a contiguous
//! block — the min/max scan below is exact even without that): the hot gas
//! values form the **single interval** `(v_{L-1}.gas, v_{R+1}.gas)` (unbounded
//! on an end at the hull extremes; `v_{L-1}`/`v_{R+1}` excluded because at a
//! vertex's own gas only that vertex's net counts, and the neighbor is cold by
//! definition). Points with the same gas as a hot vertex stay in; points at a
//! cold vertex's gas drop out — matching the per-point `upper_bound` filter
//! bit-for-bit, so the lossless superset invariant is preserved unchanged.
//!
//! The live points are therefore mirrored in a **gas-ordered index**
//! (`gas_points: BTreeMap<U256, Vec<Id>>`, `O(log N)` to maintain per
//! mutation), and per-block reclassification is one `BTreeMap` range walk —
//! `O(log N + P_hot)` to enumerate exactly the hot points (worst case
//! `P_hot = N`, a linear merge) instead of the former `O(N log h)` per-point
//! hull search — followed by a bounded k-heap rank (`O(P_hot log k)`).
//! Mutations touch `O(1)` gas buckets ("touched-buckets").
//!
//! All arithmetic is Alloy `U256`/`I256`, exact under the seam guard.

use alloy_primitives::{I256, U256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};
use std::fmt::Debug;
use std::ops::Bound;

use crate::order_index::{clamp_gas, clamp_gross, net_of, IdKey, OrderIndex};

/// A live point (ground truth) or a hull snapshot vertex (an upper-hull
/// frontier point): id plus its two static dimensions.
#[derive(Clone, Copy, Debug)]
struct Entry<Id> {
    id: Id,
    gas: U256,
    gross: U256,
}

/// Upper-left convex hull order index, generic over the opaque result id.
#[derive(Clone, Debug, Default)]
pub struct EnvelopeIndex<Id> {
    /// Live ground truth (all stored results), in insertion order.
    points: Vec<Entry<Id>>,
    /// `id -> index` into `points`, for O(1) duplicate detection / update / remove.
    ids: HashMap<Id, usize>,
    /// The upper hull as a **snapshot** (gas-ascending), independent of `points`
    /// indices so removals never invalidate it.
    hull: Vec<Entry<Id>>,
    /// Which live ids are currently upper-hull vertices.
    hull_ids: HashSet<Id>,
    /// Gas-ordered mirror of `points` (every live id keyed by its clamped gas):
    /// the per-block hot-range walk enumerates whole gas buckets, so the bucket
    /// lists are the unit of reclassification. Maintained O(log N) per mutation.
    gas_points: BTreeMap<U256, Vec<Id>>,
}

/// A ranked hot-set row for the bounded k-heap. `Ord` is ordered so that the
/// heap MAXIMUM is the WORST row (net ascending; on net ties, id descending —
/// i.e. the reverse of the output order), letting `pop()` evict the worst row
/// the moment the heap exceeds `k`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row<Id> {
    net: I256,
    id: Id,
}

impl<Id: Ord> PartialOrd for Row<Id> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<Id: Ord> Ord for Row<Id> {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .net
            .cmp(&self.net)
            .then_with(|| self.id.cmp(&other.id))
    }
}

impl<Id: IdKey> EnvelopeIndex<Id> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            ids: HashMap::new(),
            hull: Vec::new(),
            hull_ids: HashSet::new(),
            gas_points: BTreeMap::new(),
        }
    }

    /// Force a full exact hull rebuild from the current live points (the periodic
    /// tightening pass of the production design; also used by tests). The
    /// `gas_points` mirror is unaffected (ids/gases unchanged).
    pub fn rebuild(&mut self) {
        self.rebuild_hull();
    }

    /// Number of hull vertices.
    #[must_use]
    pub fn hull_len(&self) -> usize {
        self.hull.len()
    }

    /// Whether `id` is currently an upper-hull vertex (frontier point).
    #[must_use]
    pub fn is_hull_vertex(&self, id: &Id) -> bool {
        self.hull_ids.contains(id)
    }

    /// Number of candidates the hot/cold classifier keeps hot at `X` for a
    /// top-K of `k`. Measures pruning effectiveness.
    #[must_use]
    pub fn hot_len(&self, x: U256, k: usize) -> usize {
        if k == 0 || self.points.is_empty() {
            return 0;
        }
        let (lo, hi) = self.hot_gas_bounds(x, k);
        self.gas_points.range((lo, hi)).map(|(_, v)| v.len()).sum()
    }
}

impl<Id: IdKey> OrderIndex<Id> for EnvelopeIndex<Id> {
    fn insert(&mut self, id: Id, gas: U256, gross: U256) {
        let gas = clamp_gas(gas);
        let gross = clamp_gross(gross);
        if self.ids.contains_key(&id) {
            self.update(id, gas, gross);
            return;
        }
        self.points.push(Entry { id, gas, gross });
        let idx = self.points.len() - 1;
        self.ids.insert(id, idx);
        self.gas_points.entry(gas).or_default().push(id);
        // Splice only if this new point pokes above the current hull.
        self.consider_splice(id, gas, gross);
    }

    fn update(&mut self, id: Id, gas: U256, gross: U256) -> bool {
        let gas = clamp_gas(gas);
        let gross = clamp_gross(gross);
        let Some(&idx) = self.ids.get(&id) else {
            return false;
        };
        let old = self.points[idx];
        self.points[idx] = Entry { id, gas, gross };
        // Gas move: the id keys exactly one bucket before and after.
        if old.gas != gas {
            if let Some(bucket) = self.gas_points.get_mut(&old.gas) {
                if let Some(pos) = bucket.iter().position(|i| *i == id) {
                    bucket.swap_remove(pos);
                }
                if bucket.is_empty() {
                    self.gas_points.remove(&old.gas);
                }
            }
            self.gas_points.entry(gas).or_default().push(id);
        }
        if self.hull_ids.contains(&id) {
            // A frontier vertex changed: recompute the hull (correct; bounded by
            // the small hull-vertex churn).
            self.rebuild_hull();
        } else {
            // A below-hull point moved: splice if it now pokes above, else no-op.
            self.consider_splice(id, gas, gross);
        }
        true
    }

    fn remove(&mut self, id: &Id) -> bool {
        let Some(idx) = self.ids.remove(id) else {
            return false;
        };
        let was_vertex = self.hull_ids.contains(id);
        let entry = self.points[idx];
        self.points.swap_remove(idx);
        // `swap_remove` moved the last element into `idx`; fix its map entry.
        if idx < self.points.len() {
            let moved = self.points[idx];
            self.ids.insert(moved.id, idx);
        }
        // Gas-bucket bookkeeping (touched-buckets = 1):
        if let Some(bucket) = self.gas_points.get_mut(&entry.gas) {
            if let Some(pos) = bucket.iter().position(|i| i == id) {
                bucket.swap_remove(pos);
            }
            if bucket.is_empty() {
                self.gas_points.remove(&entry.gas);
            }
        }
        if was_vertex {
            // A frontier vertex is gone: recompute the hull snapshot.
            self.rebuild_hull();
        }
        // A non-hull point was removed: the snapshot hull already dominates every
        // remaining point, so no hull work is needed.
        true
    }

    fn best(&self, x: U256) -> Option<Id> {
        // The live argmax is always a (live) hull vertex; tie-break by ascending
        // id. Snapshot values equal live values for vertices (we rebuild on any
        // vertex change), so net is exact.
        let mut best_net = I256::MIN;
        let mut best_id: Option<Id> = None;
        for e in &self.hull {
            let n = net_entry(e, x);
            let better = match best_id {
                None => true,
                Some(cur) => n > best_net || (n == best_net && e.id < cur),
            };
            if better {
                best_net = n;
                best_id = Some(e.id);
            }
        }
        best_id
    }

    fn top_k(&self, x: U256, k: usize) -> Vec<Id> {
        self.top_k_inner(x, k, None)
    }

    fn top_k_floor(&self, x: U256, k: usize, min_net: I256) -> Vec<Id> {
        self.top_k_inner(x, k, Some(min_net))
    }

    fn len(&self) -> usize {
        self.points.len()
    }

    fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// Net profit of a hull snapshot vertex at `X`.
#[inline]
fn net_entry<Id: IdKey>(e: &Entry<Id>, x: U256) -> I256 {
    net_of(e.gross, e.gas, x)
}

/// Order two `(net, id)` rows: net descending, then id ascending (total order).
fn rank<Id: Ord>(a: &(I256, Id), b: &(I256, Id)) -> Ordering {
    b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1))
}

impl<Id: IdKey> EnvelopeIndex<Id> {
    /// The per-block top-k: enumerate exactly the hot points (the single gas
    /// range of `hot_gas_bounds`) and rank them with a bounded k-heap.
    ///
    /// Cost: `O(h + h' + log N + P_hot · log k)` where `h'` is the hull scan
    /// and `P_hot` the hot-point count (worst case N — a linear merge; the
    /// former implementation paid `O(N log h)` regardless). The floor variant filters
    /// `net >= min_net` inside the hot set (a below-threshold point can never
    /// be in the floored top-k either).
    fn top_k_inner(&self, x: U256, k: usize, min_net: Option<I256>) -> Vec<Id> {
        if self.points.is_empty() || k == 0 {
            return Vec::new();
        }
        let (lo, hi) = self.hot_gas_bounds(x, k);
        let mut heap: BinaryHeap<Row<Id>> = BinaryHeap::with_capacity(k.min(64));
        for (_, ids) in self.gas_points.range((lo, hi)) {
            for &id in ids {
                // The id is live (the mirror is maintained per mutation); the
                // points lookup is the ground-truth (gross, gas) for `net`.
                let Some(&pidx) = self.ids.get(&id) else {
                    continue;
                };
                let p = self.points[pidx];
                let n = net_of(p.gross, p.gas, x);
                if let Some(f) = min_net {
                    if n < f {
                        continue;
                    }
                }
                heap.push(Row { net: n, id });
                if heap.len() > k {
                    heap.pop();
                }
            }
        }
        let mut out: Vec<(I256, Id)> = heap.into_iter().map(|r| (r.net, r.id)).collect();
        out.sort_by(rank);
        out.into_iter().map(|(_, id)| id).collect()
    }

    /// The `k`-th largest net among hull vertices at `X`. For `k == 0` returns
    /// `I256::MAX`; for `k > hull.len()` returns `I256::MIN` (disables pruning —
    /// everything stays hot, complete and conservative).
    fn kth_hull_net(&self, x: U256, k: usize) -> I256 {
        if self.hull.is_empty() {
            return I256::MIN;
        }
        if k == 0 {
            return I256::MAX;
        }
        if k > self.hull.len() {
            return I256::MIN;
        }
        let mut nets: Vec<I256> = self.hull.iter().map(|e| net_entry(e, x)).collect();
        nets.sort_unstable_by(|a, b| b.cmp(a));
        nets[k - 1]
    }

    /// The single gas interval whose interior is exactly the set of gas values
    /// `g` with `upper_bound(g, X) >= kth_hull_net(X, k)` — i.e. the hot gas
    /// range derived in the module docs. `(Unbounded, Unbounded)` = every gas
    /// is hot (`k > hull.len()` / empty hull); an empty range for `k == 0`
    /// (callers return early). Both bounds are `Excluded` of a COLD vertex's
    /// gas where finite, so points at a cold vertex's own gas (whose bound is
    /// that vertex's cold net) stay out while points just past it (bracketed
    //  by the hot neighbor) stay in — bit-for-bit the old per-point filter.
    fn hot_gas_bounds(&self, x: U256, k: usize) -> (Bound<U256>, Bound<U256>) {
        if k == 0 {
            return (
                Bound::Excluded(U256::ZERO),
                Bound::Excluded(U256::ZERO), // empty range marker
            );
        }
        if self.hull.is_empty() || k > self.hull.len() {
            return (Bound::Unbounded, Bound::Unbounded);
        }
        let t = self.kth_hull_net(x, k);
        let mut first_hot = self.hull.len();
        let mut last_hot = 0usize;
        for (idx, vertex) in self.hull.iter().enumerate() {
            if net_entry(vertex, x) >= t {
                first_hot = first_hot.min(idx);
                last_hot = idx.max(last_hot);
            }
        }
        let lo = if first_hot == 0 {
            Bound::Unbounded
        } else {
            Bound::Excluded(self.hull[first_hot - 1].gas)
        };
        let hi = if last_hot == self.hull.len() - 1 {
            Bound::Unbounded
        } else {
            Bound::Excluded(self.hull[last_hot + 1].gas)
        };
        (lo, hi)
    }

    /// Splice a live point into the hull snapshot iff it pokes above the current
    /// hull (lossless-critical). No-op for a below-hull point.
    fn consider_splice(&mut self, id: Id, gas: U256, gross: U256) {
        if self.hull_ids.contains(&id) {
            return; // already a vertex; handled by rebuild path
        }
        self.splice(Entry { id, gas, gross });
    }

    /// Insert `e` into the gas-ascending hull snapshot if it lies above the local
    /// hull edge (returns whether it became a vertex), walking fixes outward.
    fn splice(&mut self, e: Entry<Id>) -> bool {
        let n = self.hull.len();
        if n == 0 {
            self.hull_ids.insert(e.id);
            self.hull.push(e);
            return true;
        }
        let first_gas = self.hull[0].gas;
        let last_gas = self.hull[n - 1].gas;
        // New leftmost / rightmost extreme by gas.
        if e.gas < first_gas {
            self.hull.insert(0, e);
            self.hull_ids.insert(e.id);
            self.fix_right_of(0);
            return true;
        }
        if e.gas > last_gas {
            self.hull.push(e);
            self.hull_ids.insert(e.id);
            let pos = self.hull.len() - 1;
            self.fix_left_of(pos);
            return true;
        }
        // First hull index with gas >= e.gas.
        let mut lo = 0usize;
        let mut hi = self.hull.len();
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            if self.hull[mid].gas < e.gas {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < self.hull.len() && self.hull[lo].gas == e.gas {
            // Same gas as an existing vertex: keep the higher gross.
            if e.gross > self.hull[lo].gross {
                let old_id = self.hull[lo].id;
                self.hull_ids.remove(&old_id);
                self.hull[lo] = e;
                self.hull_ids.insert(e.id);
                let pos = self.fix_left_of(lo);
                self.fix_right_of(pos);
                return true;
            }
            return false;
        }
        // Bracket edge is [hull[lo-1], hull[lo]]; e above it -> new vertex.
        let a = self.hull[lo - 1];
        let b = self.hull[lo];
        if cross_ordering(a, b, e) != Ordering::Greater {
            return false; // on/below segment -> interior, hull unchanged
        }
        self.hull.insert(lo, e);
        self.hull_ids.insert(e.id);
        let pos = self.fix_left_of(lo);
        self.fix_right_of(pos);
        true
    }

    /// Remove now-obscured hull vertices immediately left of `pos`, walking
    /// outward. Returns the (possibly decremented) position of that vertex.
    fn fix_left_of(&mut self, pos: usize) -> usize {
        let mut pos = pos;
        while pos >= 2 {
            let a = self.hull[pos - 2];
            let b = self.hull[pos - 1];
            let c = self.hull[pos];
            if cross_ordering(a, b, c) == Ordering::Less {
                break; // b is a strict peak: keep
            }
            self.hull_ids.remove(&b.id);
            self.hull.remove(pos - 1);
            pos -= 1;
        }
        pos
    }

    /// Remove now-obscured hull vertices immediately right of `pos`.
    fn fix_right_of(&mut self, pos: usize) {
        while pos + 2 < self.hull.len() {
            let a = self.hull[pos];
            let b = self.hull[pos + 1];
            let c = self.hull[pos + 2];
            if cross_ordering(a, b, c) == Ordering::Less {
                break;
            }
            self.hull_ids.remove(&b.id);
            self.hull.remove(pos + 1);
        }
    }

    /// Full exact rebuild of the hull snapshot from the current live points.
    /// Used on hull-vertex mutations and as the periodic tightening pass.
    fn rebuild_hull(&mut self) {
        self.hull_ids.clear();
        self.hull.clear();
        // Insert a copy of each point; duplicates cannot occur (global real ids).
        // We must sort by (gas, gross) like the original monotone chain. Doing a
        // full splice for every live point is O(N log h) and fine for a rebuild;
        // keep it simple and correct by inserting all points in gas order.
        let mut ord: Vec<usize> = (0..self.points.len()).collect();
        ord.sort_by(|&a, &b| {
            self.points[a]
                .gas
                .cmp(&self.points[b].gas)
                .then(self.points[a].gross.cmp(&self.points[b].gross))
        });
        // Dedupe equal gas keeping the max gross (as in the monotone chain).
        let mut uniq: Vec<Entry<Id>> = Vec::with_capacity(ord.len());
        for &i in &ord {
            let e = self.points[i];
            if let Some(last) = uniq.last_mut() {
                if last.gas == e.gas {
                    if e.gross > last.gross {
                        *last = e;
                    }
                    continue;
                }
            }
            uniq.push(e);
        }
        // Upper hull: keep strictly decreasing edge slopes (concave envelope).
        self.hull.clear();
        self.hull_ids.clear();
        for e in uniq {
            while self.hull.len() >= 2 {
                let a = self.hull[self.hull.len() - 2];
                let b = self.hull[self.hull.len() - 1];
                if cross_ordering(a, b, e) == Ordering::Less {
                    break; // b is a strict peak: keep
                }
                self.hull_ids.remove(&b.id);
                self.hull.pop();
            }
            self.hull_ids.insert(e.id);
            self.hull.push(e);
        }
    }
}

/// Sign of `cross(a, b, c) = (b - a) x (c - a)`, exact in Alloy `I256`.
///
/// Exact because `gross <= GROSS_CAP = 2^127` and `gas <= GAS_CAP = 2^120` (the
/// seam guard): each difference is `<= 2^127`, each product `<= 2^247 < 2^256`,
/// and their difference fits `I256` — no overflow, no custom wide math.
#[inline]
fn cross_ordering<Id: IdKey>(a: Entry<Id>, b: Entry<Id>, c: Entry<Id>) -> Ordering {
    let dx1 = sdiff(b.gas, a.gas);
    let dy2 = sdiff(c.gross, a.gross);
    let dy1 = sdiff(b.gross, a.gross);
    let dx2 = sdiff(c.gas, a.gas);
    let lhs = dx1 * dy2;
    let rhs = dy1 * dx2;
    (lhs - rhs).cmp(&I256::ZERO)
}

/// Exact signed difference `y - x` as `I256` (both within the seam guard).
#[inline]
fn sdiff(y: U256, x: U256) -> I256 {
    I256::from_raw(y) - I256::from_raw(x)
}
