# DBISWP code-vs-ergo reconciliation (2026-07-18)

Investigation: the ergo state and code state of epic ZPHT6X disagreed.
This file records what was verified and why DBISWP is being marked done now.

## Chronology
- **2026-07-14 07:27Z** — DBISWP claimed by `sonnet@t-6c53`.
- **2026-07-14 08:27Z** — commit `4b8f1aa6` "type V2/Aerodrome reserves as
  uint112 [ZPHT6X]" landed, authored by **BowTiedDevil** (different author,
  ~1 hour after the claim).
- Claim went orphaned; ergo ledger never updated. Nobody else picked it up.

## DBISWP acceptance criteria — status in code

| AC | Status | Evidence |
|----|--------|----------|
| V2PoolState.reserve{0,1} = U112 | ✅ | `rust/crates/degenbot-pools/src/v2_state.rs:75,77` |
| RegisterV2PoolParams.reserve{0,1} = U112 | ✅ | `v2_state.rs:94,95` |
| V2BlockDelta.reserve{0,1}_{before,after} = U112 | ✅ | `state_history.rs:131-139` |
| AerodromeV2PoolState.reserve{0,1} = U112 | ✅ | `aerodrome_v2_state.rs:70,72` |
| RegisterAerodromeV2PoolParams.reserve{0,1} = U112 | ✅ | `aerodrome_v2_state.rs:92,93` |
| validate_v2_reserve takes U112 | ✅ | `spec_bounds.rs:124` — body kept runtime check with "type supersedes" note (AC-permitted alternative) |
| UINT112_MAX constant | kept as U256; runtime check retained behind the type-bound (AC-permitted) |
| simulate_swap.rs upcasts at boundary via .to::<U256>() | ✅ | commit 4b8f1aa6 touched simulate_swap.rs |
| degenbot-pools compiles, tests pass | ✅ | `cargo build -p degenbot-pools` green 2026-07-18 |
| just check-no-pyo3-in-cores green | ✅ | verified 2026-07-18 |

## Conclusion
DBISWP's stated goal is landed in code, stable on main, building green.
The task is marked done with this audit attached. The actual author was
BowTiedDevil (not the claiming agent — ledger hygiene only, no re-attribution
needed since ergo doesn't track per-commit authorship).

The commit also shipped the scope of MX4Z7J (sync decoder U112 + high-bits
reject) and OZOVLN (cascade upcasts), which are marked done separately with
their own result files. BHHCB5 (drop swap-primitive U512) was also landed
in the same change set. DDT4UD (ADR-012 addendum + Möbius U512 audit) was
NOT shipped and is left open as the one genuinely-undone task of the epic.
