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
//! All arithmetic is Alloy `U256`/`I256` and exact under the seam guard in
//! `order_index.rs`.

use alloy_primitives::{I256, U256};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Debug;

use crate::order_index::{clamp_gas, clamp_gross, net_of, IdKey, OrderIndex};

/// A stored candidate: an opaque id plus its two static dimensions.
#[derive(Clone, Copy, Debug)]
struct Candidate<Id> {
    id: Id,
    gas: U256,
    gross: U256,
}

/// Upper-left convex hull order index, generic over the opaque result id.
#[derive(Clone, Debug, Default)]
pub struct EnvelopeIndex<Id> {
    points: Vec<Candidate<Id>>,
    /// `id -> index` into `points`, for O(1) duplicate detection / update / remove.
    ids: HashMap<Id, usize>,
    /// Indices into `points` forming the upper hull, sorted by strictly
    /// increasing `gas`.
    hull: Vec<usize>,
}

impl<Id: IdKey> EnvelopeIndex<Id> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            ids: HashMap::new(),
            hull: Vec::new(),
        }
    }

    /// Net profit of candidate `idx` at gas price `X`.
    #[inline]
    fn net(&self, idx: usize, x: U256) -> I256 {
        let c = self.points[idx];
        net_of(c.gross, c.gas, x)
    }

    /// Number of hull vertices.
    #[must_use]
    pub fn hull_len(&self) -> usize {
        self.hull.len()
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

    /// Force a full exact hull rebuild from the current point set (the periodic
    /// tightening pass of the production design; also used by tests).
    pub fn rebuild(&mut self) {
        self.rebuild_hull();
    }
}

impl<Id: IdKey> OrderIndex<Id> for EnvelopeIndex<Id> {
    fn insert(&mut self, id: Id, gas: U256, gross: U256) {
        let gas = clamp_gas(gas);
        let gross = clamp_gross(gross);
        if let Some(&idx) = self.ids.get(&id) {
            // Existing id: re-rank (full rebuild; incremental update is Task 4).
            self.points[idx] = Candidate { id, gas, gross };
            self.rebuild_hull();
            return;
        }
        self.points.push(Candidate { id, gas, gross });
        let idx = self.points.len() - 1;
        self.ids.insert(id, idx);
        self.insert_incremental(idx);
    }

    fn remove(&mut self, id: &Id) -> bool {
        // NOTE: prototype path — full rebuild. Incremental remove (deferred
        // demotion) is the Task-4 refinement.
        let Some(idx) = self.ids.remove(id) else {
            return false;
        };
        self.points.swap_remove(idx);
        // `swap_remove` moved the last element into `idx`; fix its map entry.
        if idx < self.points.len() {
            let moved = self.points[idx];
            self.ids.insert(moved.id, idx);
        }
        self.rebuild_hull();
        true
    }

    fn update(&mut self, id: Id, gas: U256, gross: U256) -> bool {
        // NOTE: prototype path — full rebuild. Incremental update is Task 4.
        let Some(&idx) = self.ids.get(&id) else {
            return false;
        };
        self.points[idx] = Candidate {
            id,
            gas: clamp_gas(gas),
            gross: clamp_gross(gross),
        };
        self.rebuild_hull();
        true
    }

    fn best(&self, x: U256) -> Option<Id> {
        let mut best_net = I256::MIN;
        let mut best_id: Option<Id> = None;
        for &i in &self.hull {
            let n = self.net(i, x);
            let id = self.points[i].id;
            let better = match best_id {
                None => true,
                Some(cur) => n > best_net || (n == best_net && id < cur),
            };
            if better {
                best_net = n;
                best_id = Some(id);
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
            .map(|i| (self.net(i, x), self.points[i].id))
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
        let mut nets: Vec<I256> = self.hull.iter().map(|&i| self.net(i, x)).collect();
        nets.sort_unstable_by(|a, b| b.cmp(a));
        nets[k - 1]
    }

    /// Lower bound on `net(p, X)` from the hull edge bracketing `p.gas`: the
    /// max endpoint net, computed with exact `I256`. `>= net(p, X)` always.
    fn upper_bound(&self, idx: usize, x: U256) -> I256 {
        let p = self.points[idx];
        match self.hull.len() {
            0 => I256::MIN,
            1 => self.net(self.hull[0], x),
            _ => {
                let h = &self.hull;
                // First hull index with gas >= p.gas.
                let mut low = 0usize;
                let mut high = h.len();
                while low < high {
                    let mid = usize::midpoint(low, high);
                    if self.points[h[mid]].gas < p.gas {
                        low = mid + 1;
                    } else {
                        high = mid;
                    }
                }
                if low < h.len() && self.points[h[low]].gas == p.gas {
                    // Same gas as a hull vertex (p is at or below it).
                    return self.net(h[low], x);
                }
                let i = low - 1;
                let n1 = self.net(h[i], x);
                if low < h.len() {
                    n1.max(self.net(h[low], x))
                } else {
                    n1
                }
            }
        }
    }

    /// Incrementally splice a newly-pushed point at `idx` into the maintained
    /// hull. Returns true if it became a hull vertex. Hull stays gas-ascending.
    fn insert_incremental(&mut self, idx: usize) -> bool {
        let p = self.points[idx];
        let n = self.hull.len();
        if n == 0 {
            self.hull.push(idx);
            return true;
        }
        let first_gas = self.points[self.hull[0]].gas;
        let last_gas = self.points[self.hull[n - 1]].gas;
        // New leftmost / rightmost extreme by gas.
        if p.gas < first_gas {
            self.hull.insert(0, idx);
            self.fix_right_of(0);
            return true;
        }
        if p.gas > last_gas {
            self.hull.push(idx);
            let pos = self.hull.len() - 1;
            self.fix_left_of(pos);
            return true;
        }
        // First hull index with gas >= p.gas.
        let mut lo = 0usize;
        let mut hi = self.hull.len();
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            if self.points[self.hull[mid]].gas < p.gas {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < self.hull.len() && self.points[self.hull[lo]].gas == p.gas {
            // Same gas as an existing vertex: keep the higher gross.
            if p.gross > self.points[self.hull[lo]].gross {
                self.hull[lo] = idx;
                let pos = self.fix_left_of(lo);
                self.fix_right_of(pos);
                return true;
            }
            return false;
        }
        // Bracket edge is [hull[lo-1], hull[lo]]; p above it -> new vertex.
        let a = self.hull[lo - 1];
        let b = self.hull[lo];
        if cross_ordering(self.points[a], self.points[b], p) != Ordering::Greater {
            return false; // on/below segment -> interior, hull unchanged
        }
        self.hull.insert(lo, idx);
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
            if cross_ordering(self.points[a], self.points[b], self.points[c]) == Ordering::Less {
                break; // b is a strict peak: keep
            }
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
            if cross_ordering(self.points[a], self.points[b], self.points[c]) == Ordering::Less {
                break;
            }
            self.hull.remove(pos + 1);
        }
    }

    /// (Re)compute the upper convex hull over the current point set.
    fn rebuild_hull(&mut self) {
        let n = self.points.len();
        if n <= 2 {
            if n == 2 && self.points[0].gas > self.points[1].gas {
                self.hull = vec![1, 0];
            } else {
                self.hull = (0..n).collect();
            }
            return;
        }
        let mut ord: Vec<usize> = (0..n).collect();
        ord.sort_by(|&a, &b| {
            self.points[a]
                .gas
                .cmp(&self.points[b].gas)
                .then(self.points[a].gross.cmp(&self.points[b].gross))
        });
        // Dedupe equal gas, keeping the max gross (last in asc-gross order).
        let mut uniq: Vec<usize> = Vec::with_capacity(n);
        for &i in &ord {
            if let Some(&last) = uniq.last() {
                if self.points[last].gas == self.points[i].gas {
                    *uniq.last_mut().unwrap() = i;
                    continue;
                }
            }
            uniq.push(i);
        }
        // Upper hull: keep strictly decreasing edge slopes (concave envelope).
        let mut hull: Vec<usize> = Vec::with_capacity(uniq.len());
        #[allow(clippy::many_single_char_names)]
        for &i in &uniq {
            while hull.len() >= 2 {
                let b = hull[hull.len() - 1];
                let a = hull[hull.len() - 2];
                if cross_ordering(self.points[a], self.points[b], self.points[i]) == Ordering::Less
                {
                    break;
                }
                hull.pop();
            }
            hull.push(i);
        }
        self.hull = hull;
    }
}

/// Sign of `cross(a, b, c) = (b - a) x (c - a)`, exact in Alloy `I256`.
///
/// Exact because `gross <= GROSS_CAP = 2^127` and `gas <= GAS_CAP = 2^120` (the
/// seam guard): each difference is `<= 2^127` in `I256`, each product is
/// `<= 2^247 < 2^256`, and their difference fits `I256` — so no overflow and no
/// custom wide math.
#[inline]
fn cross_ordering<Id: IdKey>(a: Candidate<Id>, b: Candidate<Id>, c: Candidate<Id>) -> Ordering {
    // cross(a,b,c) = (b - a) x (c - a)
    let dx1 = sdiff(b.gas, a.gas); // b.gas - a.gas
    let dy2 = sdiff(c.gross, a.gross); // c.gross - a.gross
    let dy1 = sdiff(b.gross, a.gross); // b.gross - a.gross
    let dx2 = sdiff(c.gas, a.gas); // c.gas - a.gas
    let lhs = dx1 * dy2;
    let rhs = dy1 * dx2;
    (lhs - rhs).cmp(&I256::ZERO)
}

/// Exact signed difference `y - x` as `I256` (both within the seam guard).
#[inline]
fn sdiff(y: U256, x: U256) -> I256 {
    I256::from_raw(y) - I256::from_raw(x)
}
