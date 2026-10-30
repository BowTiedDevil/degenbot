//! Shared big-integer primitives — the 2048-bit width, its widening and
//! narrowing conversions, and the floor integer square roots.
//!
//! The Möbius solver forms discriminants (`K·M` for the closed-form anchor,
//! `A·D − B·C` for a shifted piece) whose widths exceed `U512`, so it widens
//! the operands into a 2048-bit integer, takes a floor square root there, and
//! narrows the result back. That chain is general integer arithmetic — no pool
//! state, no fee semantics, no solver policy — so it lives here rather than
//! beside any one consumer: a caller that needs a wide floor root or a checked
//! narrowing imports it from this module instead of re-deriving it.
//!
//! The `isqrt` helpers and the widening conversions are total (defined for
//! every input of their type); [`narrow_u512_to_u256`] carries a panic
//! contract, documented on the function itself.

use alloy::primitives::{U256, U512};

/// 2048-bit unsigned integer — width for a composed determinant product (a
/// `U512` × `U512` product is exact here) and for the shifted-piece
/// determinant magnitude. Not re-exported at the alloy root:
/// `Uint<2048, 32>`.
pub type U2048 = alloy::primitives::Uint<2048, 32>;

/// Integer square root of a U512 value.
///
/// Uses Newton's method with an initial approximation from the bit length.
/// Converges in `O(log(bit_width))` iterations.
#[must_use]
pub fn isqrt_u512(n: U512) -> U512 {
    if n.is_zero() {
        return U512::ZERO;
    }

    let bit_len = n.bit_len();
    if bit_len == 1 {
        return U512::from(1u64);
    }

    // Initial guess: 2^((bit_len+1)/2). This is >= ceil(sqrt(n)).
    // Starting above the root and iterating downward ensures correct convergence.
    let half_bits = (bit_len + 1).div_ceil(2);
    let mut x = U512::from(1u64) << half_bits;

    // Newton's method: x_{k+1} = (x_k + n / x_k) / 2
    // Since we start above the root, x_{k+1} <= x_k always.
    // We converge when x_{k+1} >= x_k (interleaving) or x_{k+1} == x_k.
    loop {
        let q = n / x;
        let next = (x + q) >> 1;
        if next >= x {
            // Converged — x is the floor sqrt, or next is.
            // Since x >= sqrt(n), check if x is correct.
            break;
        }
        x = next;
    }

    // Post-condition: x is the floor sqrt of n.
    // Verify: x^2 <= n < (x+1)^2
    // Due to integer division, x might be off by 1.
    // If x^2 > n, decrement.
    while x * x > n {
        x -= U512::from(1u64);
    }
    // If (x+1)^2 <= n, increment.
    while (x + U512::from(1u64)) * (x + U512::from(1u64)) <= n {
        x += U512::from(1u64);
    }

    x
}

/// Integer square root of a U2048 value.
///
/// The composed determinant `A·D − B·C` — a product of two 1024-bit
/// magnitudes, widened from the fast-path coefficients — reaches up to ~2046
/// bits for deep paths whose coefficients still fit 1024 bits, which overflows
/// `U1024`. Newton's method, same shape as [`isqrt_u512`].
///
/// Returns the floor of √n. Panics are impossible: `U2048` is wide enough that
/// no input from a valid 1024-bit-coefficient determinant overflows.
#[must_use]
pub fn isqrt_u2048(n: U2048) -> U2048 {
    if n.is_zero() {
        return U2048::ZERO;
    }

    let bit_len = n.bit_len();
    if bit_len == 1 {
        return U2048::from(1u64);
    }

    // The walk's anchor argmax calls this per piece, and real 1-3-hop
    // determinants are far narrower than U2048 (each hop multiplies ~140-bit
    // per-hop determinants). Narrow the Newton iteration to the smallest limb
    // count: mathematically the same floor-√ algorithm on the same value, so
    // the result is provably identical at a fraction of the wide-division
    // cost.
    if bit_len <= 256 {
        let narrow = u2048_to_u256_lossy(n);
        return u512_to_u2048(isqrt_u512(u512_from_u256(narrow)));
    }
    if bit_len <= 512 {
        return u512_to_u2048(isqrt_u512(u2048_to_u512(n)));
    }

    let half_bits = (bit_len + 1).div_ceil(2);
    let mut x = U2048::from(1u64) << half_bits;

    loop {
        let q = n / x;
        let next = (x + q) >> 1;
        if next >= x {
            break;
        }
        x = next;
    }

    // The correction walks square their probe, and near the domain ceiling the
    // exact square lands past `U2048` (for `n` just under `2^2048` the probe
    // stands at `2^1024`, whose square is `2^2048`). A wrapped product compares
    // as tiny and the upward walk never terminates; an overflowing square is by
    // definition `> n`, so checked squares decide both walks.
    while x.checked_mul(x).is_none_or(|sq| sq > n) {
        x -= U2048::from(1u64);
    }
    while x
        .checked_add(U2048::from(1u64))
        .and_then(|next| next.checked_mul(next))
        .is_some_and(|sq| sq <= n)
    {
        x += U2048::from(1u64);
    }

    x
}

fn u2048_to_u256_lossy(n: U2048) -> U256 {
    let limbs = n.into_limbs();
    debug_assert!(limbs[4..].iter().all(|&l| l == 0));
    U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]])
}

fn u512_from_u256(v: U256) -> U512 {
    let limbs = v.into_limbs();
    let mut wide = [0u64; 8];
    wide[..4].copy_from_slice(&limbs);
    U512::from_limbs(wide)
}

/// Narrow a `U2048` to a `U512`.
///
/// Panics are impossible for the solver's inputs, which are magnitudes that fit
/// 1024 bits; the narrowing is `debug_assert`-guarded so a wider value is
/// caught in test builds rather than silently truncated.
#[must_use]
pub fn u2048_to_u512(n: U2048) -> U512 {
    let limbs = n.into_limbs();
    debug_assert!(limbs[8..].iter().all(|&l| l == 0));
    U512::from_limbs([
        limbs[0], limbs[1], limbs[2], limbs[3], limbs[4], limbs[5], limbs[6], limbs[7],
    ])
}

/// Widen a `U512` into the 2048-bit accumulator. Total by construction: every
/// `U512` value fits in `U2048`.
#[must_use]
pub fn u512_to_u2048(v: U512) -> U2048 {
    let limbs = v.into_limbs();
    let mut wide = [0u64; 32];
    wide[..8].copy_from_slice(&limbs);
    U2048::from_limbs(wide)
}

/// Narrow a `U512` to a `U256`.
///
/// For pool state sourced from on-chain events (V2 `Sync(uint112,uint112)`,
/// V3/V4 `Swap`), the inputs are spec-bound: `uint112` V2 reserves, `uint128`
/// V3/V4 liquidity, and TickMath-bounded `sqrtPriceX96` — all kept well within
/// `U256` by their contract-storage types. The narrowing is therefore
/// unreachable for real pool state.
///
/// # Panics
///
/// Panics if `v > U256::MAX`, i.e. on synthetic / corrupt registration (e.g. a
/// programmatically-instantiated V2 pool carrying reserves beyond
/// `uint112::MAX`). The prior implementation silently saturated to
/// `U256::MAX`, which would propagate garbage through downstream Möbius /
/// concentrated liquidity computations; panicking here makes the contract
/// invariant explicit. The spec widths are enforced at pool registration (see
/// the pools crate's `spec_bounds.rs` and ADR-012), so on-chain-sourced pool
/// state cannot reach this branch.
#[must_use]
pub fn narrow_u512_to_u256(v: U512) -> U256 {
    // Narrowing U512 → U256. For pool state sourced from on-chain events
    // (V2 `Sync(uint112,uint112)`, V3/V4 `Swap`) the underlying uint112
    // reserves, uint128 liquidity, and TickMath-bounded sqrt prices keep
    // every quotient well within U256 — so this is unreachable under
    // spec-bound state. The spec widths are enforced at pool registration
    // (see the pools crate's `spec_bounds.rs` and ADR-012), so
    // on-chain-sourced pool state cannot reach this panic. The explicit
    // pre-check makes that invariant visible and lets synthetic / corrupt
    // registration fail loudly instead of silently degrading to `U256::MAX`.
    assert!(
        v <= U512::from(U256::MAX),
        "U512 → U256 narrowing overflow (corrupt/synthetic input; \
         spec-bound pool state is unreachable — enforced at register_*_pool)",
    );
    v.to::<U256>()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── isqrt_u512 tests ────────────────────────────────────────

    #[test]
    fn test_isqrt_zero() {
        assert_eq!(isqrt_u512(U512::ZERO), U512::ZERO);
    }

    #[test]
    fn test_isqrt_one() {
        assert_eq!(isqrt_u512(U512::from(1u64)), U512::from(1u64));
    }

    #[test]
    fn test_isqrt_perfect_square() {
        // 100 = 10^2
        assert_eq!(isqrt_u512(U512::from(100u64)), U512::from(10u64));
        // 10000 = 100^2
        assert_eq!(isqrt_u512(U512::from(10000u64)), U512::from(100u64));
    }

    #[test]
    fn test_isqrt_non_perfect_square() {
        // floor(sqrt(2)) = 1
        assert_eq!(isqrt_u512(U512::from(2u64)), U512::from(1u64));
        // floor(sqrt(3)) = 1
        assert_eq!(isqrt_u512(U512::from(3u64)), U512::from(1u64));
        // floor(sqrt(8)) = 2
        assert_eq!(isqrt_u512(U512::from(8u64)), U512::from(2u64));
        // floor(sqrt(99)) = 9
        assert_eq!(isqrt_u512(U512::from(99u64)), U512::from(9u64));
    }

    #[test]
    fn test_isqrt_large_perfect_square() {
        // (10^9)^2 = 10^18
        let n = U512::from(1_000_000_000u64) * U512::from(1_000_000_000u64);
        assert_eq!(isqrt_u512(n), U512::from(1_000_000_000u64));
    }

    #[test]
    fn test_isqrt_large_non_perfect_square() {
        // (10^9)^2 - 1 → floor = 10^9 - 1
        let n = U512::from(1_000_000_000u64) * U512::from(1_000_000_000u64) - U512::from(1u64);
        // This is (10^9-1)*(10^9+1) = 10^18 - 1, so isqrt = 999999999
        assert_eq!(isqrt_u512(n), U512::from(999_999_999u64));
    }

    #[test]
    fn test_isqrt_u256_max() {
        // U256::MAX ≈ 1.16e77, sqrt ≈ 1.08e38
        let n = U512::from(U256::MAX);
        let root = isqrt_u512(n);
        // Verify: root^2 <= n < (root+1)^2
        assert!(root * root <= n);
        assert!((root + U512::from(1u64)) * (root + U512::from(1u64)) > n);
    }

    #[test]
    fn test_isqrt_two_pow_256() {
        // 2^256 is a perfect square: sqrt(2^256) = 2^128
        let n = U512::from(1u64) << 256;
        let root = isqrt_u512(n);
        assert_eq!(root, U512::from(1u64) << 128);
    }

    // ── narrowing tests ─────────────────────────────────────────

    #[test]
    fn test_u512_to_u256_small() {
        let v = U512::from(42u64);
        assert_eq!(narrow_u512_to_u256(v), U256::from(42u64));
    }

    #[test]
    fn test_u512_to_u256_max_u256() {
        let v = U512::from(U256::MAX);
        assert_eq!(narrow_u512_to_u256(v), U256::MAX);
    }

    #[test]
    #[should_panic(expected = "U512 \u{2192} U256 narrowing overflow")]
    fn test_u512_to_u256_overflow() {
        // U256::MAX + 1 overflows U256 — a real pool's reserves/liquidity/
        // sqrtPrice are spec-bound (uint112 / uint128 / MAX_SQRT_RATIO), so
        // this fires only on corrupt/synthetic state. The spec widths are
        // enforced at pool registration (the pools crate's `spec_bounds.rs`,
        // ADR-012); on-chain-sourced pool state cannot reach this panic.
        let v = U512::from(U256::MAX) + U512::from(1u64);
        let _ = narrow_u512_to_u256(v);
    }

    // ── isqrt_u2048 tests ───────────────────────────────────────

    #[test]
    fn test_isqrt_u2048_matches_isqrt_u512_in_domain() {
        for n in [1u64, 2, 3, 99, 100, 1_000_000_007, u64::MAX] {
            let wide = U2048::from(n);
            assert_eq!(u2048_to_u512(isqrt_u2048(wide)), isqrt_u512(U512::from(n)));
        }
        let n = U512::from(U256::MAX);
        assert_eq!(u2048_to_u512(isqrt_u2048(u512_to_u2048(n))), isqrt_u512(n));
    }

    #[test]
    fn isqrt_u2048_is_exact_on_the_upper_type_domain() {
        let ceil_root = (U2048::from(1u64) << 1024) - U2048::from(1u64);
        let ceil_sq = ceil_root * ceil_root;
        assert_eq!(isqrt_u2048(ceil_sq), ceil_root);
        assert_eq!(
            isqrt_u2048(ceil_sq - U2048::from(1u64)),
            ceil_root - U2048::from(1u64)
        );
        assert_eq!(isqrt_u2048(U2048::MAX), ceil_root);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Generate a U256 value by filling from 4 u64 limbs.
    fn arb_u256() -> impl Strategy<Value = U256> {
        (any::<u64>(), any::<u64>(), any::<u64>(), any::<u64>())
            .prop_map(|(a, b, c, d)| U256::from_limbs([a, b, c, d]))
    }

    proptest! {
        /// isqrt(n)^2 <= n < (isqrt(n)+1)^2 for all n
        #[test]
        fn isqrt_is_floor_sqrt(n in arb_u256()) {
            let n_u512 = U512::from(n);
            let root = isqrt_u512(n_u512);
            let root_sq = root * root;
            let root_plus_one = root + U512::from(1u64);
            let rp1_sq = root_plus_one * root_plus_one;

            assert!(
                root_sq <= n_u512,
                "isqrt({n_u512}) = {root}, but {root}^2 = {root_sq} > {n_u512}"
            );
            assert!(
                rp1_sq > n_u512,
                "isqrt({n_u512}) = {root}, but ({root}+1)^2 = {rp1_sq} <= {n_u512}"
            );
        }
    }
}
