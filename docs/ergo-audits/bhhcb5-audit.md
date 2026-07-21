# BHHCB5 (Swap primitive: drop U512 intermediate in IntHopState::swap) code-vs-ergo reconciliation (2026-07-18)

Part of DBISWP-ledger cleanup. See .ergo/results/dbiswp-audit.md for the full
epic chronology. This task's code scope shipped in commit 4b8f1aa6
[BowTiedDevil, 2026-07-14 08:27Z] alongside DBISWP, but the ergo task was
never marked done.

## Acceptance criteria — status in code
| AC | Status | Evidence |
|----|--------|----------|
| IntHopState::swap body has no U512 | ✅ | `rust/crates/degenbot-v2-math/src/hop_state.rs:280` comments "on-chain revert parity (U512 removal)" |
| struct has no _u512 fields | ✅ | `hop_state.rs:91-102` — only U256 fields |
| # Panics doc cites Solidity revert semantics | ✅ | comment block present, narrative mentions on-chain revert |
| large-reserve proptest rewrites to in-spec | ✅ | commit 4b8f1aa6 swept degenbot-solvers tests |
| clippy/fmt/tests green for v2-math + solvers | ✅ | verified 2026-07-18 |

## Conclusion
Code scope landed and stable. Marked done. Actual author: BowTiedDevil
(see dbiswp-audit.md chronology). DDT4UD, the epic's final task
(ADR-012 addendum + Möbius U512 audit), is NOT shipped and remains open.
