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
//! result. `top_k` exact-sorts only the hot set.
//!
//! ## Dynamic maintenance (GRFRXI)
//!
//! The **hull is X-independent** (pure geometry of `gas`/`gross`) and is held as
//! a **snapshot** independent of the live `points` indices, so mutations that do
//! not touch a hull vertex are O(1) with no index invalidation:
//!
//! - `insert` / non-hull `update`: splice the point into the hull *only if it
//!   pokes above* (lossless-critical), else leave the hull unchanged.
//! - non-hull `remove`: O(1) — the snapshot hull already dominates the removed
//!   (below-hull) point, so removing it cannot expose any gap.
//! - **hull-vertex `update`/`remove`**: full `rebuild()` — correct, and bounded
//!   by the (small) fraction of points that are actually on the frontier. The
//!   S2 strategy's *deferred demotion* plus a periodic `rebuild()` to tighten is
//!   the refinement layered on top of this in a later pass.
//!
//! All arithmetic is Alloy `U256`/`I256`, exact under the seam guard.

use alloy_primitives::{I256, U256};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;

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
}

impl<Id: IdKey> EnvelopeIndex<Id> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            ids: HashMap::new(),
            hull: Vec::new(),
            hull_ids: HashSet::new(),
        }
    }

    /// Force a full exact hull rebuild from the current live points (the periodic
    /// tightening pass of the production design; also used by tests).
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
        let t = self.kth_hull_net(x, k);
        (0..self.points.len())
            .filter(|&i| self.upper_bound(i, x) >= t)
            .count()
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
        // Splice only if this new point pokes above the current hull.
        self.consider_splice(id, gas, gross);
    }

    fn update(&mut self, id: Id, gas: U256, gross: U256) -> bool {
        let gas = clamp_gas(gas);
        let gross = clamp_gross(gross);
        let Some(&idx) = self.ids.get(&id) else {
            return false;
        };
        self.points[idx] = Entry { id, gas, gross };
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
        self.points.swap_remove(idx);
        // `swap_remove` moved the last element into `idx`; fix its map entry.
        if idx < self.points.len() {
            let moved = self.points[idx];
            self.ids.insert(moved.id, idx);
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
        if self.points.is_empty() || k == 0 {
            return Vec::new();
        }
        let t = self.kth_hull_net(x, k);
        let mut ranked: Vec<(I256, Id)> = (0..self.points.len())
            .filter(|&i| self.upper_bound(i, x) >= t)
            .map(|i| {
                (
                    net_of(self.points[i].gross, self.points[i].gas, x),
                    self.points[i].id,
                )
            })
            .collect();
        ranked.sort_by(|a, b| rank(a, b));
        ranked.truncate(k);
        ranked.into_iter().map(|(_, id)| id).collect()
    }

    fn top_k_floor(&self, x: U256, k: usize, min_net: I256) -> Vec<Id> {
        if self.points.is_empty() || k == 0 {
            return Vec::new();
        }
        // Hot set is floor-independent (a below-threshold point can never be in
        // the floored top-k either); filter the floor inside the hot set.
        let t = self.kth_hull_net(x, k);
        let mut ranked: Vec<(I256, Id)> = (0..self.points.len())
            .filter(|&i| self.upper_bound(i, x) >= t)
            .map(|i| {
                (
                    net_of(self.points[i].gross, self.points[i].gas, x),
                    self.points[i].id,
                )
            })
            .filter(|(n, _)| *n >= min_net)
            .collect();
        ranked.sort_by(|a, b| rank(a, b));
        ranked.truncate(k);
        ranked.into_iter().map(|(_, id)| id).collect()
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

    /// Lower bound on `net(p, X)` from the hull snapshot edge bracketing `p.gas`:
    /// the max endpoint net, computed with exact `I256`. `>= net(p, X)` always.
    fn upper_bound(&self, idx: usize, x: U256) -> I256 {
        let p = self.points[idx];
        match self.hull.len() {
            0 => I256::MIN,
            1 => net_entry(&self.hull[0], x),
            _ => {
                // First hull index with gas >= p.gas.
                let mut low = 0usize;
                let mut high = self.hull.len();
                while low < high {
                    let mid = usize::midpoint(low, high);
                    if self.hull[mid].gas < p.gas {
                        low = mid + 1;
                    } else {
                        high = mid;
                    }
                }
                if low < self.hull.len() && self.hull[low].gas == p.gas {
                    return net_entry(&self.hull[low], x);
                }
                let i = low - 1;
                let n1 = net_entry(&self.hull[i], x);
                if low < self.hull.len() {
                    n1.max(net_entry(&self.hull[low], x))
                } else {
                    n1
                }
            }
        }
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
