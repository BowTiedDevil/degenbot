//! Net-profit order index over path results (prototype → production).
//!
//! A path result is characterized by two *static* dimensions — `gross` (gross
//! profit, wei) and `gas` (gas used) — and its **net** profit is
//!
//! ```text
//! net(X) = gross - gas * X,   X = base_fee_next + priority_fee
//! ```
//!
//! `X` is **one number shared by every result in a block** and fluctuates
//! between blocks, so the relative order of two results flips whenever `X`
//! crosses the slope of the segment between their `(gas, gross)` points. Because
//! `net` is *linear* in `X`, the single best result at `X` always lies on the
//! **upper-left convex hull** of the point set — but the full top-K does **not**.
//!
//! The envelope's job is therefore a **lossless hot/cold split** (see
//! [`EnvelopeIndex`]): every result's net is bounded above by the max endpoint
//! net of its bracketing hull edge, and
//!
//! ```text
//! upper_bound(p, X) < kth_hull_net(X, k)  ==>  p ∉ top-K
//! ```
//!
//! so results provably below the K-th hull threshold can be evicted to the cold
//! set without ever losing a top-K result. `top_k` exact-sorts only the hot set.
//!
//! All arithmetic uses Alloy types (`U256` for `gross`/`gas`/`X`, `I256` for
//! `net` and the hull cross product), exact under the seam guard in
//! `order_index.rs`.

#[cfg(feature = "envelope")]
pub mod envelope;
pub mod order_index;
pub mod scan_topk;

#[cfg(feature = "envelope")]
pub use envelope::EnvelopeIndex;
pub use order_index::OrderIndex;
pub use scan_topk::ScanTopK;
