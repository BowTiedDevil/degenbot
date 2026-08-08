# Spike: integer representation & exact upper-bound arithmetic

**Status:** done — **REVISED to Alloy types per review** (task `374FTG`). This doc
supersedes the initial `i128`-only proposal below with the approved Alloy-based design.

> **2026-08-08 revision (user review):** use **Alloy types**, not a hand-rolled
> integer. Rust has no native `i256`. Adopted:
> - `gross`, `gas`, `X` are `alloy::primitives::U256`.
> - `net = gross - gas * X` is `alloy::primitives::I256` (Alloy `Signed`), which is
>   `Ord` (used for ranking) and exact in practice.
> - The hull **cross product** is computed in `I256` (Alloy `Signed`), which cannot
>   overflow as long as the **seam guard** holds: `gross <= 2^127`, `gas <= 2^120`,
>   `X <= 2^120`. Then every difference (`I256`) and every cross product
>   (`<= 2^127 * 2^120 = 2^247 < 2^256`) fits `I256` exactly, and the two-product
>   difference fits too. Realistic magnitudes (gross ~1e30, gas ~1e7, X ~1e13) are
>   orders of magnitude inside the guard, so it never rejects real data.
> - No custom wide-math module is needed (Alloy's `I256` replaces the experimental
>   `i256.rs`). Prod math is integer-exact.


## Decision

Use **`i128`** for all four index quantities — `gas`, `gross`, `X`, `net` — where
`net = gross - gas * X` is **signed** (allowed negative). Compute the hull's
**cross product in an exact minimal `i256`** (`src/i256.rs`) so geometry can never
overflow, with an `i128` fast path for the common case.

## Why `i128` fits `net` (and why the cross is the only tight spot)

- The values are: `gross` ≤ ~1e26 wei typical, ~1e30 wei at the *trillion-token*
  ceiling; `gas` ≤ ~1e8; `X = base_fee_next + prio` ≤ ~1e13.
- `net = gross - gas * X` stays within ~`[-1e30, 1e30]`, some **eight orders of
  magnitude** under `i128::MAX ≈ 1.7e38`. A signed 128-bit `net` has no overflow
  concern for any realistic — or even absurd — input.
- The one place arithmetic can genuinely exceed `i128` is the hull **cross
  product**, which multiplies two *differences* of `i128`s. Two large-but-legal
  diffs (e.g. a `Δgross` ~2^100 and a `Δgas` ~2^26) give a product ~2^126 that is
  fine, but `Δgross ~2^100` with `Δgas ~2^63` would exceed `i128::MAX`. A bare
  `i128` cross is therefore not *unconditionally* exact.

## The fix: exact `i256` cross

`src/i256.rs` implements a minimal signed 256-bit value (`hi * 2^128 + lo`) with
add/sub/neg and an exact `i128 × i128 → i256` multiply (`umul128`, 128·128 → 256).
`EnvelopeIndex.is_below_or_on` computes the cross sign in `i256` and uses only the
sign for hull construction:

- **fast path**: if both cross products and their difference fit `i128`
  (`checked_mul`/`checked_sub`), use plain `i128` — zero extra cost for normal data;
- **exact path**: otherwise fall back to `i256` — unconditionally exact.

Validated by unit tests in `src/i256.rs`: `umul128` against hand-computed 256-bit
products (`(2^128-1)^2`, `2^127·2^63 = 2^190`, …), and `cross_sign` at `i128::MAX`
products with correct sign/zero. The randomized invariant suite
(`tests/top_k_invariant.rs`) rules both the fast and exact paths.

## Overflow / wrap policy

- **No saturating math in the pipeline.** A `U256 → i128` seam guard converts
  `gross` via `i128::try_from(gross)` → error on overflow, and `X` likewise;
  `gas` (`u64`) is infallible. Because real `gross` is ~1e30 (≪ 2^127), the guard
  never fires in practice; it turns a silent-wraparound bug into a loud error.
- The cross is the only operation that could approach `i64 × i64` aggressiveness,
  and it is `i256`-exact, so **no overflow is possible** for any legal input after
  the seam guard.
- `net` may be negative; it is a *ranking* value only. Submission-time profit uses
  the sim-measured `gross`/`gas` in `U256`, not the index's `i128` estimate.

## Seam (where `U256` enters and converts)

1. RPC/decoder yields `gross` (`U256`), `gas` (`u64`), `X` (`U256`).
2. Into the index: `gross → i128` (guarded `try_from`), `gas → i128` (free),
   `X → i128` (guarded).
3. Out of the index: `top_k` returns **ids** only — the caller resolves ids to
   paths and reads real `U256` profit/gas for simulation/submission. The index
   never round-trips its `i128` estimate back into `U256`.

## Validation

- `cargo test -p degenbot-order-index` — i256 unit tests + randomized invariants +
  `s1_extreme_magnitudes_no_panic_and_hold_i128`.
- `cargo clippy --all-targets -- --deny warnings`, `cargo fmt --check` clean.
- Scale demo unchanged (`i128` fast path dominates): 1M points, 1572 hull,
  hot 5.16%, build 320 ms.

## Checkpoint

Approve the `i128`-values + exact-`i256`-cross type policy so task `XKKGZS`
(production `OrderIndex` trait) builds on it.