# HD73IB — FC4: trim the unused fields from `HopMath::CurveStableswap` (measured)

STATUS: **DONE.** The measurement is recorded, the trim landed, and every listed
gate is green on the current tree (foreground).

The gate builds a `Vec<Option<HopMath>>` per candidate path per call, so the
enum's width is a hot-loop cost. `HopMath::CurveStableswap` carried the leaf's
full resolve-time state "for future tangent work" that the family's measured
non-concavity had already ruled out. It now carries only the fields its line
derivation and its family label read.

## Measurement — `std::mem::size_of::<HopMath>()`

| | bytes |
|---|---|
| before | **296** |
| after | **200** |
| delta | **−96 (−32.4 %)** |

Instrument: a unit test. The `before` value was read from a temporary
`eprintln!` test (run with `--no-capture`); it was then replaced by the durable
size ratchet `profit_envelope::derive::tests::hop_math_view_size_stays_trimmed`,
which asserts `size_of::<HopMath>() <= 200` and prints the actual value on
failure. The ratchet is the right instrument because the width is a property
that must not silently regress: a future field addition now has to justify
itself at the assertion, not in a review comment.

The after-width is *not* the Curve payload: the trimmed Curve view is 128 bytes
of payload (136 with the enum tag), and the enum's widest remaining variants are
`Weighted` (6 × `U256` = 192) and `BalancerStable` (5 × `U256` + a borrowed slice
+ two indices = 192), so `HopMath` now stands at 192 + 8 = 200. Before the trim
the Curve payload was 288 bytes of payload (296 with tag) and was itself the
widest variant.

## Field audit — `HopMath::CurveStableswap`

Readers: the line derivation `curve_stableswap_lines` (reads `xp`,
`token_index_out`, `precision`, `rate_multiplier_out`) and `family_label` (reads
`amp`, `xp`, `token_index_in`, `token_index_out`; reports the coin count as
`xp.len()`, never `n_coins`). Every other use of the variant across the tree was
grepped first: the only pattern matches outside `derive.rs` are the single
construction site in `hop_math_for` (`src/mixed/solve.rs`) and doc/comment
mentions. The tests build `ResolvedHop::CurveStableswap` (the resolved state,
which is untouched and still feeds `simulate_curve_hop`), not the `HopMath`
variant, so no test construction needed updating.

| field | readers | verdict |
|---|---|---|
| `amp` | `family_label` | **keep** |
| `xp` | `curve_stableswap_lines`, `family_label` | **keep** |
| `token_index_in` | `family_label` | **keep** |
| `token_index_out` | `curve_stableswap_lines`, `family_label` | **keep** |
| `precision` | `curve_stableswap_lines` | **keep** |
| `rate_multiplier_out` | `curve_stableswap_lines` | **keep** |
| `a_precision` | none | **drop** |
| `n_coins` | none (label uses `xp.len()`) | **drop** |
| `fee` | none | **drop** |
| `fee_denom` | none | **drop** |
| `rate_multiplier_in` | none | **drop** |
| `y_variant` | none | **drop** |
| `d_variant` | none | **drop** |

13 declared fields → 6 kept, 7 dropped. (`CurveStableswapHopState` keeps all of
them: `simulate_curve_hop` still consumes the full resolved state.)

## Field audit — `SolidlyStable` and `BalancerStable`

Same audit, same criterion: keep exactly what the derivation and the label read.

- `SolidlyStable` — derivation reads `reserves_0`, `reserves_1`, `decimals_0`,
  `decimals_1`, `token_in`, `fee_numer`, `fee_denom`; label reads `reserves_0`,
  `reserves_1`, `token_in`. Union = all 7 fields → **keep all, nothing to drop**.
- `BalancerStable` — derivation reads `amp`, `balances`, `token_index_in`,
  `token_index_out`, `invariant`, `swap_fee`, `scaling_factor_in`,
  `scaling_factor_out`; label reads `amp`, `balances`, `token_index_in`,
  `token_index_out`. Union = all 8 fields → **keep all, nothing to drop**.

So the only variant carrying unread state was `CurveStableswap`; no variant now
carries a field neither the derivation nor the label reads.

## Bench — `cargo bench -p degenbot-solvers --bench stable_arms` (foreground, from `rust/`)

Five criterion groups, 70 benchmarks, criterion's own change detection against
the saved baseline. Group means of each path's median estimate (µs):

| group | paths | before | after | delta |
|---|---|---|---|---|
| `solidly` | 16 | 60.25 | 60.85 | +1.00 % |
| `balancer_weighted` | 16 | 26.20 | 26.56 | +1.39 % |
| `balancer_stable` | 16 | 50.10 | 51.33 | +2.47 % |
| `curve` | 16 | 53.62 | 53.99 | +0.69 % |
| `v2_reference` | 6 | 18.67 | 18.76 | +0.44 % |

Interpretation: no arm moved beyond run-to-run noise. The untouched all-V2
reference moved +0.44 % over the same pair of runs, which bounds the
environmental drift; the Curve arm — the one whose view was trimmed — moved
+0.69 %, the *least* of the four family arms. Criterion's per-benchmark change
detection flagged 3 paths above +5 % and 2 below −3 %, scattered across all four
families (including the untouched reference), i.e. noise rather than a
direction. That is the expected outcome: the trim removes unread fields from one
variant's payload, and the enum's width after the trim is set by the
Weighted/Balancer-stable variants (192), which the arms measured here copy
unchanged. The win is the 296 → 200 enum shrink, not a per-arm latency shift on
this corpus.

## What changed

- `rust/crates/engine/degenbot-solvers/src/profit_envelope/derive.rs`
  - `HopMath::CurveStableswap` now declares only `amp`, `xp`, `token_index_in`,
    `token_index_out`, `precision`, `rate_multiplier_out`.
  - The variant doc comment no longer claims the full resolve-time state is
    carried; it names exactly what is kept and says why the rest is dropped.
  - The `curve_stableswap_lines` match arm's comment no longer says "Curve
    carries its whole state".
  - The now-unused `use degenbot_math::curve::stableswap::{DVariant, YVariant};`
    import is removed (its only consumers were the dropped fields).
  - New size ratchet test `hop_math_view_size_stays_trimmed`.
- `rust/crates/engine/degenbot-solvers/src/mixed/solve.rs`
  - `hop_math_for`'s `ResolvedHop::CurveStableswap` arm stops forwarding the
    dropped fields; the resolved state stays whole on
    `CurveStableswapHopState` for `simulate_curve_hop`.

Behavior-identical: the derivation is unchanged (`curve_stableswap_lines` is the
native reserve cap alone) and `family_label`'s format string is unchanged, so
the derived line sets and the labels do not move. Pinned by the 96-snapshot
drift net (`stable_golden_drift_net`, all four arm snapshots green), the
dominance suites, the golden-parity canary, and the capture tests.

## Gates (foreground, from `rust/`)

- `cargo nextest run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers`
  → **PASS**: 243 run, 243 passed, 3 skipped.
- `cargo clippy --locked -p degenbot-solvers --all-targets --manifest-path rust/Cargo.toml -- --deny warnings`
  → **CLEAN** (exit 0, no warnings).
- `cargo fmt -p degenbot-solvers -- --check` → **CLEAN** (exit 0).
- `cargo bench -p degenbot-solvers --bench stable_arms` → before and after both
  completed (exit 0); tables above.
- Also run: `bash scripts/hooks/comment-hygiene.sh <the two touched files>` →
  clean (exit 0).

All four gates were re-run after the final comment-only wording tweak; the
`after` bench ran on the trimmed code (the tweak is comment-only, no codegen
delta).

## PENDING

None. Every gate ran in the foreground and completed.

## Tree

Clean at dispatch (HEAD `48c5ad849`). End state shows only the two files above —
no fixture, no golden, no `.ergo/` change, no new file. No foreign edits and no
other worker's in-progress work was observed.
