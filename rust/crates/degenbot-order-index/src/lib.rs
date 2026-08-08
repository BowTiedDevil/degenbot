//! Convex-hull / upper-envelope order index — prototype.
//!
//! Background (see the session design discussion): a path's result is
//! characterized by two *static* dimensions — `gross` (gross profit, wei) and
//! `gas` (gas used) — and its **net** profit is
//!
//! ```text
//! net(X) = gross - gas * X
//! ```
//!
//! where `X = base_fee_next + priority_fee` is the per-gas price. `X` is **one
//! number shared by every path in a block** and fluctuates between blocks, so
//! the relative order of two paths flips whenever `X` crosses the slope of the
//! segment between their `(gas, gross)` points.
//!
//! Because `net` is *linear* in `X`, a path is a point in the `(gas, gross)`
//! plane and the best path at price `X` lies on the **upper-left convex hull**
//! of the point set — the structure this crate models. Two claims control the
//! design, and the whole crate exists to make the second one *provable*:
//!
//! 1. **The single best is always a hull vertex.** Safe and obvious from the
//!    linear-functional argument.
//! 2. **The full top-K is NOT on the hull.** An interior (dominated) point can
//!    occupy slot 2..K. So the envelope cannot be treated as "top-K = hull's
//!    top-K". Instead the hull provides: the **argmax**, the **K-th hull net**
//!    threshold, and an **upper bound** on any interior point's net.
//!
//! ## Completeness (hot/cold splitting is lossless)
//!
//! For any point `p` bracketed by a hull edge `[a, b]`, `net` is linear along
//! the edge and `p` lies on or below it, so
//!
//! ```text
//! net(p, X) <= max(net(a, X), net(b, X))      // upper_bound(p, X)
//! ```
//!
//! Let `T = kth_hull_net(X, k)` be the `k`-th largest net among **hull**
//! vertices only. Since the hull is a subset of all points,
//! `T <= kth_overall_net`. Hence
//!
//! ```text
//! upper_bound(p, X) < T  ==>  net(p, X) < T <= kth_overall_net
//!                       ==>  p is provably NOT in the global top-K
//! ```
//!
//! So a point is **cold-eligible exactly when its upper bound is below `T`**,
//! and this eviction can never discard a true top-K point. Every top-K point's
//! `upper_bound >= net >= T`, so it stays hot, and an exact sort over the hot
//! set reproduces the global top-K exactly. `k <= hull.len()` keeps `T` a valid
//! threshold; for `k >= hull.len()` the crate conservatively disables pruning
//! (everything hot).
//!
//! This crate is a **zero-dependency, pyo3-free** prototype validating the
//! invariant against a brute-force reference under randomized inputs.

pub mod i256;
pub mod index;

pub use index::{Candidate, EnvelopeIndex};
