//! The walk-side refine window in kernel vocabulary.
//!
//! The kernel's active-set walk intersects its refine windows against an
//! affine upper bound on the path output; this module owns that window's data
//! shape and its exact integer clamp. The bound's coefficients are expressed
//! in the kernel's own terms — the predicate `(a + b·x)/c ≥ best` — so no
//! envelope type (`Line`, verdict, cap) reaches into the kernel. The CL family
//! module builds these from the envelope's composed lines at the seam.

use alloy::primitives::{aliases::I512, U256, U512};

/// One affine upper bound on the path output at input `x`, in the kernel's own
/// vocabulary: the predicate `(a + b·x)/c ≥ best` (with `c > 0`) marks the
/// inputs a composed upper bound cannot disprove. The CL family module builds
/// these from the envelope's composed lines at the seam; the kernel names no
/// envelope type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefineBound {
    pub a: I512,
    pub b: I512,
    pub c: I512,
}

/// Loop-21: intersect the refine window `[lo, hi]` with the inputs the
/// composed bound cannot disprove: for the walk's best-so-far profit `best`
/// (output − input), any x with `bound(x) − x < best` can never beat the
/// walk's argmax (the lines dominate the true output pointwise), so the
/// sub-window is skipped without a simulation. Each line yields an exact
/// half-interval in x (`(A + B·x)/C ≥ best` with C > 0); the clamp is their
/// intersection. Returns `None` when the whole window is disproven;
/// arithmetic overflow falls back to the unpruned window (the clamp is an
/// optimization, never a contract).
pub(crate) fn clamp_window_to_bound(
    bounds: &[RefineBound],
    lo: U256,
    hi: U256,
    best: U256,
) -> Option<(U256, U256)> {
    fn i512_from_u256(v: U256) -> Option<I512> {
        I512::try_from(U512::from(v)).ok()
    }
    fn i512_floor_u256(v: I512) -> Option<U256> {
        // Same limbs discipline as `narrow`: check negativity + upper
        // limbs, then repack the low four limbs into U256.
        let neg = v.is_negative();
        let abs = v.abs();
        let mag = abs.as_limbs();
        if neg || mag[4] != 0 || mag[5] != 0 || mag[6] != 0 || mag[7] != 0 {
            return None;
        }
        Some(U256::from_limbs([mag[0], mag[1], mag[2], mag[3]]))
    }
    fn div_ceil_i512(n: I512, d: I512) -> I512 {
        let q = n / d;
        if n % d != I512::ZERO && (n < I512::ZERO) == (d > I512::ZERO) {
            q + I512::ONE
        } else {
            q
        }
    }
    fn div_floor_i512(n: I512, d: I512) -> I512 {
        let q = n / d;
        if n % d != I512::ZERO && (n < I512::ZERO) != (d > I512::ZERO) {
            q - I512::ONE
        } else {
            q
        }
    }
    let Some(lo_i) = i512_from_u256(lo) else {
        return Some((lo, hi));
    };
    let Some(hi_i) = i512_from_u256(hi) else {
        return Some((lo, hi));
    };
    let Some(best_i) = i512_from_u256(best) else {
        return Some((lo, hi));
    };
    let mut a = lo_i;
    let mut b = hi_i;
    for l in bounds {
        // (A + B·x)/C ≥ best  ⇔  (B − C)·x ≥ best·C − A   (C > 0)
        let Some(req) = best_i.checked_mul(l.c).and_then(|v| v.checked_sub(l.a)) else {
            return Some((lo, hi));
        };
        let Some(slope) = l.b.checked_sub(l.c) else {
            return Some((lo, hi));
        };
        if slope == I512::ZERO {
            if req > I512::ZERO {
                return None; // this line disproves the whole window
            }
            continue;
        }
        if slope > I512::ZERO {
            let x0 = div_ceil_i512(req, slope);
            if x0 > a {
                a = x0;
            }
        } else {
            let x1 = div_floor_i512(req, slope);
            if x1 < b {
                b = x1;
            }
        }
        if a > b {
            return None;
        }
    }
    let f_lo = if a <= lo_i { lo } else { i512_floor_u256(a)? };
    let f_hi = if b >= hi_i { hi } else { i512_floor_u256(b)? };
    if f_lo > f_hi {
        None
    } else {
        Some((f_lo, f_hi))
    }
}
