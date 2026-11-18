# Normalized I512 shifted-piece Möbius composition — measurement

## Verdict

Projective normalization is a **component-level performance win**, not a
meaningful whole-solve win on the captured production workload.

- Exact-versus-normalized comparison over 156 captured concentrated-liquidity
  paths found **zero full-result, profit, input, or presence divergences**.
- With direct I512 local-matrix construction, captured-path anchor composition
  took **0.6687×** the exact I1024 time (about **33% faster**).
- Whole captured-path solve time took **0.9966×** exact time in the final
  100-repetition non-telemetry run. This 0.34% difference is not an actionable
  improvement; composition is too small a share of the whole walk.
- A synthetic six-hop full-U256 path forces the exact arm's BigInt fallback.
  Normalized composition took **0.3288×** exact time (about **67% faster**),
  and the deep whole solve took **0.9013×** exact time (about **10% faster**).

Exact I1024/BigInt arithmetic therefore remains the default. Normalized I512
is available as `MobiusPieceArithmetic::NormalizedI512` for further A/B work,
not as a production cutover.

## Arithmetic and overflow bound

A Möbius map is projective: for any nonzero scale `q`, matrices `M` and `qM`
represent the same map. The normalized composer scales every coefficient of a
complete matrix by one common `2^-s`. This does not change the mathematical
map before rounding is considered.

Before each matrix product, both complete operands are scaled so every
coefficient has magnitude below `2^255`. Each dot-product term then has
magnitude below `2^510`, and the sum of two terms has magnitude below `2^511`.
Every I512 multiplication and addition is consequently overflow-free by
construction. The composed matrix carries an upper bound on its coefficient
bit length, avoiding four bit-length scans at the next product.

Scaling truncates low bits, so normalized coefficients are **not exact**.
The model anchor can move; integer path simulation remains authoritative.
Prefix partials are reused only in the same left-to-right fold order, so a
memoized normalized composition matches a fresh normalized composition.

## What mattered in the measurement

The first implementation built each local matrix in I1024, converted it to
I512, and rescanned coefficients to select every normalization shift. At 25
interleaved repetitions it was **1.0558×** exact compose time: narrower
running products alone did not pay for normalization overhead.

Carrying coefficient widths did not resolve that overhead. Building local
matrices directly in I512 whenever their entries fit did: the 50-repetition
telemetry run measured **0.6687×** exact compose time. Pathological local
entries still fall back to exact I1024 and then BigInt construction before
normalization.

The determinant and model-argmax path initially remains I2048. Its existing
bit-length narrowing automatically benefits when normalized determinants are
smaller; captured-path argmax time was **0.9327×** exact in the final
telemetry run.

## Reproduce

Production captures, 100 interleaved repetitions per arm and 2,000 deep-path
repetitions, without telemetry:

```bash
DR_NORMALIZATION_ITERS=100 DR_DEEP_ITERS=2000 \
  just mobius-normalization-ab
```

Final non-telemetry result:

| Workload | Exact total | Normalized total | Normalized / exact |
| --- | ---: | ---: | ---: |
| 156 captured paths × 100 reps | 1,730,100 µs | 1,724,342 µs | 0.9966× |
| Six-hop full-U256 path × 2,000 reps | 183,063 µs | 165,005 µs | 0.9013× |

Anchor breakdown, 50 captured-path repetitions and 1,000 deep repetitions:

```bash
DR_NORMALIZATION_ITERS=50 DR_DEEP_ITERS=1000 \
  just mobius-normalization-telemetry
```

| Measurement | Exact | Normalized | Normalized / exact |
| --- | ---: | ---: | ---: |
| Captured-path whole solve | 872,297 µs | 868,936 µs | 0.9961× |
| Captured-path compose | 9,063 µs | 6,061 µs | 0.6687× |
| Captured-path argmax | 9,268 µs | 8,644 µs | 0.9327× |
| Deep whole solve | 94,196 µs | 84,930 µs | 0.9016× |
| Deep compose | 6,763 µs | 2,224 µs | 0.3288× |
| Deep argmax | 4,410 µs | 23 µs | 0.0052× |

The deep case is a width-regime microbenchmark, not a realistic pool or a
profitable-arbitrage correctness fixture. Its equal result only checks that
both arms agree on that path's outcome.

## Correctness evidence and limits

- `normalized_composer_matches_exact_when_scaling_is_inactive`
- `normalized_composer_handles_deep_full_u256_scaling`
- `normalized_i512_mobius_preserves_the_exact_walk_result`, over the full
  synthetic concentrated-liquidity corpus
- 156 captured paths with zero reported divergences in the A/B readout

These results do not prove global parity. Normalization truncates low bits,
and the captured production whole-solve benefit is neutral. A default
promotion needs a separate decision with broader mixed-path and profitable
deep-path parity evidence. The readout reports divergences but deliberately
has no timing threshold and is not a merge gate.

## Verification

- `just test` — full Rust lint, standalone smoke, 6,527-test workspace nextest
  suite, and Python/executor suites passed.
- `just test-rust` — whole-workspace cargo, doc-test, and release-test legs passed.
- `just check-dev-tools solver-capture-fixtures solver-cache-telemetry` — both
  readout feature shapes compiled.
- Architecture, public-surface, deep-path, environment-read, formatting, and
  line-waiver checks passed.
