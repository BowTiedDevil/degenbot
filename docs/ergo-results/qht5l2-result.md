# QHT5L2 — C2.2: one cached derivation (walk-side bound stops re-deriving fans)

**State:** done · **Epic:** LCG5CA (PR C2, commit 2 — after the C2.1 kernel/derive/gate split)

## What changed

The gate derived CL tangent fans through the owner's `PrefixCache`, but the
walk-side bound (`path_bound_lines_with_stats`, consumed by the Loop-21
`envelope_pruned_refine` path in `mixed/solve.rs`) re-derived the same fans
uncached and re-chained the envelope with no prefix reuse — the bound was
derived twice per solved path.

- `src/profit_envelope/gate.rs`
  - New **`compose_path_bound(hops, cfg, cache, stats)`** — the ONE
    chain-composition path. It derives every hop's line set (CL pool-static
    tangent fans reused from `cache` when present), chains the sets through
    the exact concave-envelope composition, and reuses the owner's
    composed-prefix memo when present. Returns `PathBound { lines, xmax }`;
    failures carry `PathBoundError { hop_index, cause }`.
  - **`path_profit_bound_inner`** is now a thin gate entry: one query against
    `compose_path_bound` via `GateDeps::fan_cache()`, the degenerate-hop
    diagnostic + golden capture on the error exit, then `finalize_bound`.
  - **`finalize_bound`** holds the concave-max + unconditional `best/2048`
    slack tail (split out so the walk-side equivalence test maximizes the
    walk-side hull with the exact gate arithmetic).
  - **`path_bound_lines_with_stats`** takes
    `Option<(&PrefixCache, u64)>` and is now literally
    `compose_path_bound(...).ok()?` → `PathBoundLines`. Its old
    running-`total_cap` domain is replaced by the shared full-domain
    composition, so it is the same hull the gate builds, by construction.
  - New `GateDeps::fan_cache()` exposes the store+epoch; both queries derive
    from it, so one solve shares one cache.
- `src/mixed/solve.rs` — passes `gate.fan_cache()` into
  `path_bound_lines_with_stats`, so the walk-side bound is all cache hits
  after the gate warmed the store.

**Contract preserved:** the pool-static CL tangent fan is still a pure
function of the crossing table; the live in-range price still anchors a
per-call head tangent (`cl_head` / `cl_tangent_from_crossing` untouched in
`derive.rs`). The `GateStats` `record_gate` threading (C3 shape) is intact —
the walk-side counters land in the same caller-owned accumulator.

## NEW test

`profit_envelope::gate::tests::walk_side_bound_equals_gate_bound_and_reuses_fans`
(two distinct dense CL hops):

- Gate query cold: `cl_fan_misses == 2` (each fan derived exactly once),
  `cl_fan_hits == 0`.
- Walk-side query against the SAME cache/epoch: `cl_fan_misses == 0`,
  `cl_fan_hits == 2` — **the double derivation is dead, observably**.
- `finalize_bound(walk.lines, xmax, None)` == the gate's `Envelope::Bound` —
  the walk-side bound EQUALS the gate's bound.
- The cached hull equals a fresh cacheless derivation, byte for byte.

## Gates (foreground, scoped)

- `cargo test -p degenbot-solvers` — **green** (lib 177 passed / 3 ignored;
  `tests/profit_envelope_tests.rs` 20 passed, **UNCHANGED**; all other
  integration suites green; doc-tests 0).
- `cargo clippy -p degenbot-solvers --all-targets` — **clean** (pinned
  `rustc 1.98.1`).
- `cargo fmt -p degenbot-solvers -- --check` — clean.

Nothing PENDING.

## Tree note

Foreign edit observed and left untouched: `.devcontainer/Dockerfile`
(modified at claim time, not mine). My changes are limited to
`src/profit_envelope/gate.rs` and `src/mixed/solve.rs`.
