# MX4Z7J (Sync decoder: emit U112 directly) code-vs-ergo reconciliation (2026-07-18)

Part of DBISWP-ledger cleanup. See .ergo/results/dbiswp-audit.md for the full
epic chronology. This task's code scope shipped in commit 4b8f1aa6
[BowTiedDevil, 2026-07-14 08:27Z] alongside DBISWP, but the ergo task was
never marked done.

## Acceptance criteria — status in code
| AC | Status | Evidence |
|----|--------|----------|
| SyncEvent.reserve0/reserve1 = `U112` | ✅ | `rust/crates/degenbot-decoders/src/v2_sync_decoder.rs:34` |
| decode returns None on high bits above bit 112 | ✅ | decoder narrows to U112 after validating the high bits are zero |
| test decode_sync_accepts_uint112_max | ✅ | `v2_sync_decoder.rs:201` |
| test decode_sync_rejects_high_bits_set | ✅ | `v2_sync_decoder.rs:213` |
| existing tests updated to `U112` literals | ✅ | commit 4b8f1aa6 |
| degenbot-decoders compiles/tests green | ✅ | verified 2026-07-18 |

## Conclusion
Code scope landed and stable. Marked done. Actual author: BowTiedDevil
(see dbiswp-audit.md chronology). DDT4UD, the epic's final task
(ADR-012 addendum + Möbius U512 audit), is NOT shipped and remains open.
