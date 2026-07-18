# OZOVLN (Cascade: degenbot-bot + degenbot-python + standalone consumer) code-vs-ergo reconciliation (2026-07-18)

Part of DBISWP-ledger cleanup. See .ergo/results/dbiswp-audit.md for the full
epic chronology. This task's code scope shipped in commit 4b8f1aa6
[BowTiedDevil, 2026-07-14 08:27Z] alongside DBISWP, but the ergo task was
never marked done.

## Acceptance criteria — status in code
| AC | Status | Evidence |
|----|--------|----------|
| no reserved0/1: U256::from(N) construction sites | ✅ | rg over bot_core + python + standalone_consumer returns none |
| PyO3 getters upcast to U256 | ✅ | `pool.rs:847,859,1303,1315,1330,1331` |
| no silent downcast truncations | ✅ | verified U112 only flows from decoder → delta/state |
| cargo build --workspace --all-targets green | ✅ | verified 2026-07-18 |

## Conclusion
Code scope landed and stable. Marked done. Actual author: BowTiedDevil
(see dbiswp-audit.md chronology). DDT4UD, the epic's final task
(ADR-012 addendum + Möbius U512 audit), is NOT shipped and remains open.
