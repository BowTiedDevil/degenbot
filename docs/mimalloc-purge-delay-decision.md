# mimalloc purge-delay decision (epic AZZDBI, tasks XXJR3A + T3/T4)

## Context

mimalloc's default purge delay (~10ms) discards freed pages between blocks,
re-faulting the block's transient working set at every burst. The T2 baseline
(40.6min mainnet soak, default config) measured **49,612 ± 39,894 minor faults
per block (~194 MiB refill)**, paid at burst start - the worst moment for
solve-phase p95.

## Matrix results (T3, dry-run, 40min per arm, ~180 blocks each)

| arm | faults/block | RSS@block | drain p95 | per-path p95 |
|---|---|---|---|---|
| baseline (10ms delay, MADV_DONTNEED) | 49,612 +- 39,894 | 7.33 GB | 3.37 s | 837 us |
| PURGE_DELAY=12s (~1 block) | 44,443 +- 40,503 | 8.19 GB | 3.94 s | 780 us |
| PURGE_DELAY=24s (~2 blocks) | 25,933 +- 28,188 | 7.78 GB | 3.52 s | 857 us |
| PURGE_DELAY=60s (~5 blocks) | 21,030 +- 25,606 | 7.60 GB | 3.63 s | 872 us |
| **PURGE_DECOMMITS=0 (MADV_FREE)** | **5,598 +- 19,084** | 8.26 GB | **3.36 s** | 857 us |

## Decision

1. **Ship `PURGE_DECOMMITS=0` (MADV_FREE) as the default** via
   `allocator_ctrl` at pump init: an 89 percent reduction in refault churn at
   equal-or-better solve p95, with the retained RSS kernel-reclaimable under
   pressure (MADV_FREE pages fence before OOM). Opt back out with
   `DEGENBOT_MIMALLOC_PURGE_DECOMMITS=1`.
2. **Ship block-cadence discovery for `purge_delay`** (`delay = 2 x trailing
   mean interval`, 20-sample threshold, 10 percent hysteresis): it is the
   answer to the 12s-cadence staircase under any future chain cadence, and
   with MADV_FREE it is mostly a belt-and-suspenders redundancy (the lazy
   reclaim makes the delay secondary).
3. Solve-phase latency is a second-order function of allocator purging:
   on_drain p95 varies only 3.36-3.94s across ALL arms (block mix dominates),
   so we do NOT claim p95 wins from the allocator - the refault-churn removal
   is the win (kernel page work + zeroing no longer lands at burst start).

## Mechanism

`degenbot-bot/src/allocator_ctrl.rs` (cargo feature `allocator-ctrl`, dev-only
via pyproject maturin features): a pure `CadenceState` tracker turned RED->
GREEN via TDD; `mi_option_set(_enabled)` against vendored mimalloc
(v2/v3-shared indices - `purge_delay` 15, `purge_decommits` 5), single-writer
from the pump header arm, `mi_version()`-gated. Env knobs: fixed
`DEGENBOT_MIMALLOC_PURGE_DELAY_MS`, `DEGENBOT_MIMALLOC_AUTO_PURGE=0`,
`DEGENBOT_MIMALLOC_PURGE_DELAY_MULT`, `DEGENBOT_MIMALLOC_PURGE_DECOMMITS`.

Live proof (mainnet): discovery armed at pump start; `delay_ms=23988`
applied at block 25887706 after 20 samples, no restart.
