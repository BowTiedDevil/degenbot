//! Shared helpers + the generic RED→GREEN invariant runner, reused by the
//! `ScanTopK` and `EnvelopeIndex` test binaries so both implementations must
//! satisfy the same contract.

use alloy_primitives::{I256, U256};
use proptest::prelude::*;

use degenbot_order_index::OrderIndex;

/// A result point in Alloy units.
#[derive(Clone, Copy, Debug)]
pub struct Cand {
    pub id: u64,
    pub gas: U256,
    pub gross: U256,
}

/// `net = gross - gas * X` exactly as `I256`.
pub fn net(c: &Cand, x: U256) -> I256 {
    let gc = c.gas.checked_mul(x).unwrap_or(U256::MAX);
    I256::from_raw(c.gross) - I256::from_raw(gc)
}

/// Brute-force top-K (net desc, id asc) — the independent reference.
pub fn brute_top_k(points: &[Cand], x: U256, k: usize) -> Vec<u64> {
    let mut ranked: Vec<(I256, u64)> = points.iter().map(|c| (net(c, x), c.id)).collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.truncate(k);
    ranked.into_iter().map(|(_, id)| id).collect()
}

/// A `(gas, gross)` point in realistic magnitudes, with a unique-ish id.
pub fn point() -> impl Strategy<Value = Cand> {
    (any::<u64>(), 21_000u64..12_000_000, 0u128..(1u128 << 100)).prop_map(|(id, gas, gross)| Cand {
        id,
        gas: U256::from(gas),
        gross: U256::from(gross),
    })
}

/// De-duplicate by id, preserving order (proptest may emit colliding ids).
pub fn dedup(points: &[Cand]) -> Vec<Cand> {
    let mut seen = std::collections::HashSet::new();
    points
        .iter()
        .filter(|p| seen.insert(p.id))
        .copied()
        .collect()
}

/// The central invariant: `top_k` (and `best`) of any `OrderIndex` match the
/// brute-force reference, for a wide spread of gas prices.
///
/// Returns `true` so it can be called inside `proptest!` then asserted.
pub fn check_topk_and_best<I: OrderIndex<u64> + Default>(
    points: &[Cand],
    x: U256,
    k: usize,
) -> bool {
    let unique = dedup(points);
    let mut idx = I::default();
    for c in &unique {
        idx.insert(c.id, c.gas, c.gross);
    }
    let kk = k.min(unique.len());
    let got = idx.top_k(x, kk);
    let want = brute_top_k(&unique, x, kk);
    if got != want {
        return false;
    }
    // `best` must be a maximizer (net >= every point), and a stored id.
    if let Some(b) = idx.best(x) {
        let Some(bc) = unique.iter().find(|c| c.id == b) else {
            return false;
        };
        let bn = net(bc, x);
        for c in &unique {
            if net(c, x) > bn {
                return false;
            }
        }
    }
    // distinct ids
    let mut set = got.clone();
    set.sort_unstable();
    set.dedup();
    set.len() == got.len()
}

/// The invariant, as a directly-asserting helper for plain `#[test]`s.
#[allow(dead_code)] // exercised only from the envelope test binary
pub fn assert_invariant<I: OrderIndex<u64> + Default>(points: &[Cand], x: U256, k: usize) {
    assert!(check_topk_and_best::<I>(points, x, k));
}
