//! The [`OrderIndex`] trait and the shared net/seam helpers.
//!
//! An `OrderIndex` ranks stored path results by **net profit** for a given
//! per-block gas price `X`:
//!
//! ```text
//! net(X) = gross - gas * X
//! ```
//!
//! `gross` and `gas` are static per result; `X = base_fee_next + priority_fee`
//! is one number, shared by every result in a block. The index answers "the
//! most net-profitable `k` results at `X`" (`top_k`) and "the single best"
//! (`best`), descending, needing to scale to millions of stored results.

use alloy_primitives::{I256, U256};
use core::hash::Hash;
use std::fmt::Debug;

/// Value-type namespace for the seam guard that keeps every digit in range for
/// exact `I256` arithmetic.
///
/// `net` and the hull cross product are `I256` (Alloy `Signed`). To keep them
/// overflow-free and *exact* without any custom wide math:
///
/// - `gross <= GROSS_CAP = 2^127`  (sign bit of `I256` clear -> `I256::from_raw`
///   is a non-negative value equal to `gross`);
/// - `gas  <= GAS_CAP   = 2^120`  and `X <= X_CAP = 2^120`, so
///   `gas * X <= 2^240` fits `U256`, and every hull cross-product
///   (`<= 2^127 * 2^120 = 2^247`) fits `I256`.
///
/// Realistic magnitudes (gross ~1e30 wei, gas ~1e7, X ~1e13) are orders of
/// magnitude inside these caps, so the guard never rejects real data; it exists
/// to turn a malformed/absurd input into a safe clamp instead of a wrap.
pub const GROSS_CAP: U256 = U256::from_limbs([0, 0x8000_0000_0000_0000, 0, 0]); // 2^127
pub const GAS_CAP: U256 = U256::from_limbs([0, 0x0100_0000_0000_0000, 0, 0]); // 2^120
pub const X_CAP: U256 = U256::from_limbs([0, 0x0100_0000_0000_0000, 0, 0]); // 2^120

/// Clamp `gross` into the exact-`I256` range (see the seam guard).
#[inline]
pub(crate) fn clamp_gross(gross: U256) -> U256 {
    gross.min(GROSS_CAP)
}

/// Clamp `gas` into the exact-`I256` range (see the seam guard).
#[inline]
pub(crate) fn clamp_gas(gas: U256) -> U256 {
    gas.min(GAS_CAP)
}

/// `net(X) = gross - gas * X` as an exact `I256`.
///
/// If `gas * X` overflows `U256` (only possible outside the seam guard), the
/// net is saturated to `I256::MIN` so ordering stays sensible; in-range inputs
/// never reach that branch.
#[inline]
pub(crate) fn net_of(gross: U256, gas: U256, x: U256) -> I256 {
    let Some(gas_cost) = gas.checked_mul(x) else {
        return I256::MIN;
    };
    I256::from_raw(gross) - I256::from_raw(gas_cost)
}

/// An order index over path results, ranked by net profit.
///
/// `Id` is an opaque, copyable key the caller uses to resolve a result back to
/// a path; the index only ranks ids. Every implementation must satisfy the
/// contract that `top_k(X, k)` and `best(X)` select the true `k`/1 most
/// net-profitable ids at gas price `X` (documented in each impl's raw invariant
/// tests).
pub trait OrderIndex<Id> {
    /// Insert (or, if `id` already present, re-rank) a result.
    fn insert(&mut self, id: Id, gas: U256, gross: U256);

    /// Remove a result by id. Returns whether it was present.
    fn remove(&mut self, id: &Id) -> bool;

    /// Re-rank an existing result. Returns whether the id existed.
    fn update(&mut self, id: Id, gas: U256, gross: U256) -> bool;

    /// The single most net-profitable id at gas price `X`.
    fn best(&self, x: U256) -> Option<Id>;

    /// The exact top-`k` ids by net at `X`, descending (ties by ascending id).
    fn top_k(&self, x: U256, k: usize) -> Vec<Id>;

    /// Number of stored results.
    fn len(&self) -> usize;

    /// Whether the index is empty.
    fn is_empty(&self) -> bool;
}

/// A generic bound for ids usable across implementations.
pub trait IdKey: Copy + Eq + Hash + Debug + Ord + 'static {}
impl<T: Copy + Eq + Hash + Debug + Ord + 'static> IdKey for T {}
