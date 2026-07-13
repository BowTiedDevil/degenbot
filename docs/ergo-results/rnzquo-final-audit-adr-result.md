# RNZQUO: Final Audit + ADR-012 — Result

Final-audit task for the `WOYYS2` spec-bound pool registration admission
epic. Confirms the seam contract is in force for V2/V3/V4 and records it
in ADR-012.

## Audit checklist

### 1. Zero `assert!` in the three register fns

`grep -nE 'assert!'` over `register_v2_pool`, `register_v3_pool`,
`register_v4_pool` in `rust/crates/degenbot-bot/src/bot_core/mod.rs`:
none. Confirm.

### 2. Narrowed-helper `assert!` messages cite registration-time enforcement

Updated the `# Panics` doc sections of the two narrowing helpers,
replacing the future-tense "the proper fix is enforcing spec widths at
`register_*_pool`" wording with the now-shipped contract:

- `rust/crates/degenbot-bot/src/solvers/mobius_int_exact.rs`
  `u512_to_u256_internal`:
  - `# Panics` section now reads: *"The spec widths are enforced at
    `register_v2_pool` / `register_v3_pool` / `register_v4_pool`
    (see `bot_core/spec_bounds.rs` and ADR-012), so on-chain-sourced
    pool state cannot reach this branch."*
  - Body comment updated similarly; assertion message now ends with
    `spec-bound pool state is unreachable — enforced at register_*_pool`.
- `rust/crates/degenbot-bot/src/solvers/mobius_int.rs`
  `IntHopState::swap`:
  - `# Panics` section likewise updated to cite registration-time
    enforcement plus ADR-012.
  - Body comment + assertion message updated identically.
- The stale `test_u512_to_u256_overflow` docstring inside the same
  test module was likewise refreshed.

`grep -rnE 'proper fix is enforcing' rust/crates/` → 0 hits.

### 3. ADR-012 records the seam contract

New `docs/adr/ADR-012-spec-bound-pool-admission.md`:

- **Decision**: registration is the single spec-verification seam;
  spec-bound helpers (`spec_bounds.rs`) reject out-of-spec state with
  a typed `SpecViolation` before storing it.
- **Decision**: narrowing helpers narrow; they do not grow richer
  rejection (`Option`/`Result`). They remain panicking `assert!`s that
  document the contract for the synthetic-corruption bypass path.
- **Decision**: the PyO3 layer has one typed exception hierarchy
  (`PoolRegistrationError` → `{HookedPoolRejectedError,
  DynamicFeePoolRejectedError, PoolAlreadyRegisteredError,
  SpecViolationError}`, parented under `ValueError`).
- **Consequences**: caller contract that a successful
  `register_v×_pool` means spec-bound state — downstream math can rely
  on the narrowing assertion; migration path (no retro-verification
  of stored state); narrowing `assert!`s stay as runtime-emergent
  invariant statements; no backwards-compat shim for the retired
  silently-saturating narrowing.

Linked references to ADR-005 + ADR-003 and the per-task ergo result docs.

## Validation

- `cargo test -p degenbot-bot --lib`: 375 passed, 0 failed.
- `cargo test -p degenbot_rs --test python_integration`: 16/16 green.
- `cargo clippy --workspace --all-targets`: clean (via `just lint-rust`).
- `cargo fmt -p degenbot-bot -p degenbot_rs --check`: clean.
- `cargo build -p degenbot --example standalone_consumer`: clean.
- `just check-no-pyo3-in-cores`: green.
- `just test-rust-python`: 320 passed.

## Files touched

- `rust/crates/degenbot-bot/src/solvers/mobius_int_exact.rs`
- `rust/crates/degenbot-bot/src/solvers/mobius_int.rs`
- `docs/adr/ADR-012-spec-bound-pool-admission.md` (new)

Epic `WOYYS2` is now complete: all six children (`ZOICEZ`, `MSTAT2`,
`24KNGF`, `K3IICB`, `F2EVV6`, `RNZQUO`) are done.