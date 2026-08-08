//! The [`EnvelopeIndex`]: an upper-left convex hull over `(gas, gross)` points
//! that answers top-K-by-net queries and (provably losslessly) identifies the
//! cold subset.

/// A stored candidate: its fixed gross profit and semi-fixed gas use, plus an
/// opaque id for the caller to resolve back to a path/result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub id: u64,
    pub gas: i128,
    pub gross: i128,
}

/// Upper-left convex hull order index over a set of `(gas, gross)` points,
/// queried by a per-block gas price `X`.
///
/// See the crate root module docs for the completeness argument that justifies
/// the hot/cold split.
#[derive(Clone, Debug, Default)]
pub struct EnvelopeIndex {
    points: Vec<Candidate>,
    /// Indices into `points` forming the upper hull, sorted by strictly
    /// increasing `gas`.
    hull: Vec<usize>,
}

impl EnvelopeIndex {
    #[must_use]
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            hull: Vec::new(),
        }
    }

    /// Number of stored candidates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Push a candidate and rebuild the hull.
    ///
    /// NOTE: this is the prototype's O(n log n) rebuild-on-every-mutation.
    /// Incremental hull maintenance (degenbot-order-index promotion task) will
    /// replace it with O(log h) insert/remove/update runs.
    pub fn insert(&mut self, c: Candidate) {
        self.points.push(c);
        self.rebuild_hull();
    }

    /// Push a batch of candidates and rebuild the hull once.
    pub fn extend(&mut self, it: impl IntoIterator<Item = Candidate>) {
        self.points.extend(it);
        self.rebuild_hull();
    }

    /// Replace a candidate's (gas, gross) and rebuild the hull.
    pub fn update(&mut self, id: u64, gas: i128, gross: i128) -> bool {
        if let Some(idx) = self.points.iter().position(|p| p.id == id) {
            self.points[idx] = Candidate { id, gas, gross };
            self.rebuild_hull();
            true
        } else {
            false
        }
    }

    /// Remove a candidate by id and rebuild the hull. Returns whether it existed.
    pub fn remove(&mut self, id: u64) -> bool {
        if let Some(idx) = self.points.iter().position(|p| p.id == id) {
            self.points.swap_remove(idx);
            self.rebuild_hull();
            true
        } else {
            false
        }
    }

    /// Net profit of candidate `idx` at gas price `X`.
    #[inline]
    #[must_use]
    pub fn net(&self, idx: usize, x: i128) -> i128 {
        self.points[idx].gross - self.points[idx].gas * x
    }

    /// The single most net-profitable candidate at `X` (always a hull vertex).
    #[must_use]
    pub fn best(&self, x: i128) -> Option<usize> {
        let mut best: Option<usize> = None;
        let mut best_net = i128::MIN;
        for &i in &self.hull {
            let n = self.net(i, x);
            if n > best_net {
                best_net = n;
                best = Some(i);
            }
        }
        best
    }

    /// Number of hull vertices.
    #[must_use]
    pub fn hull_len(&self) -> usize {
        self.hull.len()
    }

    /// Number of candidates the hot/cold classifier would keep hot at `X` for
    /// a top-K of `k`. Useful for measuring pruning effectiveness.
    #[must_use]
    pub fn hot_len(&self, x: i128, k: usize) -> usize {
        let t = self.kth_hull_net(x, k);
        (0..self.points.len())
            .filter(|&i| self.upper_bound(i, x) >= t)
            .count()
    }

    /// The exact top-`k` candidates at `X`, sorted by net descending (ties by
    /// ascending id). Correct by the completeness argument in the crate docs.
    #[must_use]
    pub fn top_k(&self, x: i128, k: usize) -> Vec<usize> {
        if self.points.is_empty() || k == 0 {
            return Vec::new();
        }
        let t = self.kth_hull_net(x, k);
        let mut ranked: Vec<(i128, usize)> = (0..self.points.len())
            .filter(|&i| self.upper_bound(i, x) >= t)
            .map(|i| (self.net(i, x), i))
            .collect();
        ranked.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        ranked.truncate(k);
        ranked.into_iter().map(|(_, i)| i).collect()
    }

    /// The `k`-th largest net among hull vertices at `X`.
    ///
    /// If `k >= hull.len()`, returns `i128::MIN` so the classifier keeps
    /// everything hot (disables pruning — conservative and complete).
    fn kth_hull_net(&self, x: i128, k: usize) -> i128 {
        if self.hull.is_empty() {
            return i128::MIN;
        }
        if k == 0 {
            // top-0 selects nothing; keep nothing hot is irrelevant, but
            // returning MAX means every point is hot (conservative).
            return i128::MAX;
        }
        if k > self.hull.len() {
            // Can't compute a real k-th order statistic from the hull; disable
            // pruning (everything stays hot — complete and conservative).
            return i128::MIN;
        }
        let mut nets: Vec<i128> = self.hull.iter().map(|&i| self.net(i, x)).collect();
        nets.sort_unstable_by(|a, b| b.cmp(a));
        // k-th largest, 1-indexed -> index k-1.
        nets[k - 1]
    }

    /// An upper bound on `net(p, X)`: the max endpoint net of the hull edge
    /// bracketing `p.gas`.
    fn upper_bound(&self, idx: usize, x: i128) -> i128 {
        let p = self.points[idx];
        match self.hull.len() {
            0 => i128::MIN,
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
                // Bracket edge is [h[low-1], h[low]]; low is guaranteed > 0 here
                // because p.gas >= hull[0].gas and gas != hull[low] when low>0.
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

    /// (Re)compute the upper convex hull over the current point set.
    fn rebuild_hull(&mut self) {
        let n = self.points.len();
        if n <= 2 {
            // Keep the hull gas-ascending (upper_bound's bracket search relies
            // on it), even when tiny.
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
        // Dedupe equal gas, keeping the max gross (last in the asc-gross order).
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
                if cross(self.points[a], self.points[b], self.points[i]) >= 0 {
                    hull.pop();
                } else {
                    break;
                }
            }
            hull.push(i);
        }
        self.hull = hull;
    }
}

/// Cross product `(b - a) x (c - a)`; positive = counterclockwise (left turn).
#[allow(clippy::many_single_char_names)]
fn cross(a: Candidate, b: Candidate, c: Candidate) -> i128 {
    (b.gas - a.gas) * (c.gross - a.gross) - (b.gross - a.gross) * (c.gas - a.gas)
}
