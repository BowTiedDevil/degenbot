# ADR-012: Spec-Bound Pool Admission Contract

**Status: accepted.** Implementation shipped in epic `WOYYS2`
(tasks `ZOICEZ`, `MSTAT2`, `24KNGF`, `K3IICB`, `F2EVV6`). The
final-audit task `RNZQUO` confirms the contract is enforced for V2, V3,
and V4.

## Context

Pool state in degenbot flows from two sources: on-chain events (V2
`Sync(uint112,uint112)`, V3/V4 `Swap`/`Initialize`) and Rust-side pool
construction. Before `WOYYS2`, registration (`register_v2_pool` /
`register_v3_pool` / `register_v4_pool`) **did not enforce** the
on-chain storage widths: it stored whatever `U256` it was handed.
Out-of-spec state (`reserve > uint112::MAX`, `sqrtPriceX96` outside the
TickMath bounds, `fee > type(u24).max`, `tick < MIN_TICK`, etc.) could
therefore reach downstream swap math (`MobiusSolver`, `u512_to_u256_internal`,
`IntHopState::swap`).

The downstream narrowing helpers responded to that hazard with **silent
saturation** to `U256::MAX`, then later with an `assert!` panic. Both
were unsatisfying:

- **Saturation** propagated garbage through Möbius / CL computations and
  hid the corruption at the source.
- **`assert!`** was a correct invariant statement but a poor
  *contract*: the failure message did not say *where* the upstream
  enforcement was assumed to live.

The deeper problem was architectural — the spec widths belong to the
**admission** boundary (registration), not to every downstream consumer
that happens to narrow a wider integer. The narrowing helpers were
carrying an obligation they had no authority over.

## Decision

### Registration is the single spec-verification seam

`register_v2_pool`, `register_v3_pool`, and `register_v4_pool` accept
*spec-bound* state and reject *out-of-spec* state with a typed error
before storing it. The validation helpers live in
`rust/crates/degenbot-bot/src/bot_core/spec_bounds.rs`:
`validate_v2_reserve`, `validate_sqrt_price`, `validate_tick`,
`validate_v3_fee`, `validate_tick_spacing`. Each returns a
`SpecViolation` carrying the offending field name so callers (and the
PyO3 layer) can surface it.

The register functions return a typed `RegisterV×PoolError` enum whose
two meaningful admission variants are `AlreadyRegistered` and
`SpecViolation`:

| Family | Error type | Variants |
|--------|------------|----------|
| V2     | `RegisterV2PoolError` | `AlreadyRegistered`, `SpecViolation` |
| V3     | `RegisterV3PoolError` | `AlreadyRegistered`, `SpecViolation` |
| V4     | `RegisterV4PoolError` | `AlreadyRegistered`, `SpecViolation`, `HookedPoolRejected`, `DynamicFeePoolRejected` |

V4 carries the two pre-existing admission categories (`HookedPoolRejected`,
`DynamicFeePoolRejected` — see ADR Program Plan-102) alongside the
new `SpecViolation` and the upgraded `AlreadyRegistered`.

### Narrowing helpers narrow; they do not re-verify

With spec-bound admission, downstream narrowing (`u512_to_u256_internal`
in `mobius_int_exact.rs`, the in-body narrowing in `IntHopState::swap`)
stops being an invariant *guard* and reverts to being a plain `assert!`
that documents the upstream contract. The narrowing's `# Panics` notes
cite **registration-time enforcement** as the upstream guarantee, and
the assertion message names the enforcement site
(`spec-bound pool state is unreachable — enforced at register_*_pool`).

The narrowing helpers do **not** grow richer rejection
(`Option`/`Result`); they remain panicking. They are reachable only by
synthetic / corrupt construction that bypasses the seam — i.e. a Rust
call-site that hand-builds an `IntHopState` with out-of-spec reserves.
That is a programming error, not a runtime failure mode of bot state.

### PyO3 layer: one typed exception hierarchy per admission family

The PyO3 wrappers in `degenbot-python/src/bot/engine/register.rs` translate
each `RegisterV×PoolError` to a typed Python exception via the
`map_register_v{2,3,4}_err` mappers. The Python exception hierarchy is:

```
ValueError
└── PoolRegistrationError                  (base — added by F2EVV6)
    ├── HookedPoolRejectedError            (V4 — amount-modifying hook)
    ├── DynamicFeePoolRejectedError        (V4 — dynamic-fee flag)
    ├── PoolAlreadyRegisteredError         (V2/V3/V4 — duplicate address)
    └── SpecViolationError                 (V2/V3/V4 — bad field)
```

All pyclasses live in `degenbot-python`; the bot core exposes only the
`RegisterV×PoolError` enums. The mappers are pure translation tables, no
business logic. A standalone Rust consumer (`cargo add degenbot`) sees
the typed enums and no Python at all — the standalone-Rust-core
constraint (ADR-005) is preserved. `just check-no-pyo3-in-cores` stays
green.

The hierarchy subclasses `ValueError` so the broad
`except ValueError:` net in `build_paths` (Python) keeps working; callers
that want to scope just admission refusals use
`except PoolRegistrationError:`.

## Consequences

- **Caller contract.** A `register_v×_pool` call that succeeds is a
  *guarantee*: all stored pool state is spec-bound. Downstream math
  can rely on the narrowing helper's assertion confidently without
  carrying an independent validator.
- **Migration path.** Pools registered before this contract shipped are
  unaffected — the contract is enforced on *registration*, not on read.
  Stale pre-WOYYS2 state on disk is treated as trusted at load; new
  registrations and updates go through the seam. (If a future migration
  wants to retro-verify stored state, the `spec_bounds.rs` helpers are
  the right tool — no new validator code is needed.)
- **The narrowing `assert!`s stay.** They are not dead defensive code:
  they are a runtime-emergent invariant statement for the path that
  bypasses registration (synthetic / programmatic construction of pool
  state objects in tests or benchmarks). Their existence does not
  contradict the seam contract — it makes the contract visible at the
  point where a violation would first corrupt a computation.
- **No backwards-compatibility shim.** The previous silently-saturating
  narrowing is **deleted**, not retained behind a feature flag. Callers
  that fed synthetic garbage and relied on the sat-cap are expected to
  fix their construction, not their stepping code. This is consistent
  with the project guideline against backwards-compat layers for
  retired implementations.

## Addendum: type-level enforcement (epic `ZPHT6X`)

The original decision enforced V2-family spec widths at *registration*
but stored the reserves as `U256`, leaving the narrowing helper as the
runtime guard. Epic `ZPHT6X` "Typed V2 reserve width (`U112`) + drop
swap-primitive `U512` dodge" deepens the contract: the spec width is
now enforced at the **type level**, superseding the runtime check.

This addendum realizes the "deeper fix" forward-referenced in commit
`19218a2c` ("refactor(rust): panic on U512→U256 overflow in mobius
solvers"), which converted the silently-saturating narrowing to an
explicit pre-check + panic but deferred the type-narrowing to a
separate multi-crate refactor.

The changes:

- **(a) V2 reserve storage is now `U112`.** `V2PoolState`'s
  `reserve0` / `reserve1` fields are `alloy::primitives::aliases::U112`,
  the on-chain `uint112` storage type. `register_v2_pool` accepts `U112`
  reserves (the `RegisterV2PoolParams` fields are `U112`), so the
  runtime `validate_v2_reserve` check (`value > UINT112_MAX`) is a
  **tautology** for type-correct callers — a `U112` cannot exceed
  `uint112::MAX`. The runtime check **remains** as a diagnostic-message
  uniformity aid (it returns the `SpecViolation` with the offending
  field name, which the bare type would not), and as a guard against
  any future code path that widens-then-narrows around the typed seam.
  This is the registration-time enforcement cited by the Möbius
  narrowing helper's `# Panics` docstring.

- **(b) `IntHopState::swap` does native `U256` arithmetic.** The V2
  swap primitive (`degenbot_v2_math::hop_state::IntHopState::swap`)
  previously widend to `U512` for the multiply-then-narrow dodge. It
  now runs `checked_mul` / `checked_add` in `U256`, returning
  `HopSwapError::Overflow` on overflow — mirroring on-chain
  `getAmountOut`'s `SafeMath` revert semantics exactly (any
  `uint256`-overflowing intermediate reverts on-chain, so the primitive
  surfaces it rather than returning a phantom result the chain never
  produces). The `U512` intermediate and the `u512_to_u256_internal`
  narrowing assertion that lived on the swap-primitive path are
  **deleted** — replaced by on-chain-faithful multiply-and-panic.

- **(c) The Möbius-solver `U512` is retained as legitimate extended
  precision.** The `U512` in `degenbot-solvers` (`mobius_int.rs`,
  `mobius_int_exact.rs`, `mobius_v3_int.rs`) is **not** a swap-primitive
  upcast-and-narrow dodge — it is legitimate extended precision for two
  distinct computations a single `U256` cannot hold:

  1. **Multi-hop Möbius matrix recurrence** (`mobius_int.rs`).
     An *n*-hop constant-product path composes the per-hop Möbius
     transform `γ·s·x / (r + γ·x)` into `l(x) = K·x / (M + N·x)`. The
     coefficients `K`, `M`, `N` are the product of *n* `(s, r, γ, fee)`
     terms each; for a realistic arb path they exceed 256 bits. The
     recurrence (a 2×2 matrix product) is carried in `U512` so the
     composition does not overflow; the final `x* = (√(K·M) − M) / N` is
     narrowed back to `U256` via `u512_to_u256_internal`. This is the
     canonical "matrix recurrence" shape — extended precision for a
     *composition*, not a single swap.
  2. **Integer square root** (`isqrt_u512` in `mobius_int_exact.rs`).
     The closed-form optimum `x* = (√(K·M) − M) / N` needs an integer
     square root. `U256` exposes no `isqrt`; `U512` does, and the
     `K·M` product is already `U512`-wide (see #1). `isqrt_u512` is the
     exact floor `isqrt` (Newton iteration, `EVM`-parity floor). This is
     the canonical "isqrt" shape — `U512` is needed because the 
     sqrt input is the `U512` matrix coefficient, not because a single
     swap overflowed.
  3. **V3/V4 concentrated-liquidity swap math** (`mobius_v3_int.rs`).
     The CL swap step multiplies `uint128` liquidity by `uint160`
     `sqrtPriceX96` by `uint256` `Q96` — a `385`-bit numerator and a
     `416`-bit denominator (the bit-width analysis is documented inline
     at each site). Both exceed 256 bits *independently*; the division
     is exact only in `U512`. The per-step quotients narrow back to
     `U256` via `u512_to_u256`. This is the canonical "CL-math
     intermediate" shape — extended precision for a *multi-field
     product*, not a single swap.

  A **"swap primitive"** (the rejected shape) is one that widens a
  single V2 `getAmountOut` multiply to `U512` purely to dodge the
  `uint256` overflow the chain itself would revert on, then narrows
  back — manufacturing a phantom result. The ZPHT6X audit confirms
  **zero** `U512` sites in `degenbot-solvers` are this shape: every
  `U512` either (i) composes a multi-hop recurrence, (ii) takes an
  `isqrt` whose input is the `U512` matrix coefficient, or (iii)
  multiplies CL fields whose product independently exceeds 256 bits.
  The `u512_to_u256_internal` narrowing is retained precisely because
  these are legitimate extended-precision computations whose *final*
  output fits in `U256` but whose *intermediate* does not.

- **Forward-reference cleanup.** The `# Panics` docstring on
  `u512_to_u256_internal` (and the inline comment at the assertion
  site) now cites **registration-time enforcement**
  (`register_v2_pool` / `register_v3_pool` / `register_v4_pool` +
  `spec_bounds.rs` + this ADR) as the upstream guarantee — it does
  **not** cross-reference `IntHopState::swap` (the swap primitive no
  longer carries a narrowing assert; its docstring stands alone). The
  audit confirmed no stale cross-reference remained to rephrase.

## References

- ADR-005 — Polars-inspired three-layer FFI (the standalone-Rust-core
  constraint the typed mappers preserve).
- ADR-003 — `Bot` as the single Rust state owner (registration is a
  `Bot` method, so the seam is co-located with state ownership).
- `docs/ergo-results/zoicez-spec-bound-helpers.md`
- `docs/ergo-results/mstat2-register-v2-result.md`
- `docs/ergo-results/24kngf-register-v3-result.md`
- `docs/ergo-results/k3iicb-register-v4-result.md`
- `docs/ergo-results/f2evv6-typed-mappers-result.md`