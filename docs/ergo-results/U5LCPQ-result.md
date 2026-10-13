# U5LCPQ — ADR-071 T1: extract the Concentrated Liquidity Kernel (byte-identical)

Commit 1 of the CL pair (ADR-071 D5). The CL kernel tree gains its stray
sibling, `cl/mod.rs` becomes the kernel's interface statement, and the
crate-root `cl_cache` module is retired — zero behavior change.

## What changed

- `degenbot-solvers/src/cl_cache.rs` → `degenbot-solvers/src/cl/cache.rs`
  (git rename, `R100`, sha256 identical). The cache-lab seam (crossing-table
  derivation + prepared-tables cache strategies) is kernel territory per
  GLOSSARY "Concentrated Liquidity Kernel". No line changed.
- `degenbot-solvers/src/lib.rs`: `pub mod cl_cache;` removed; the module is now
  `degenbot_solvers::cl::cache`.
- `degenbot-solvers/src/cl/mod.rs`:
  - module docs rewritten as the kernel interface statement — names the
    glossary term, the prepared-hop intake (ADR-069 D2), generic-over-data not
    traits (ADR-059 D6), typed V3/V4 adapters calling at the resolve seam
    (ADR-059 D3), plus an "Import direction" note;
  - `pub mod cache;` declared;
  - the visibility shrink below.
- `degenbot-solvers/src/cl/entries.rs`: comment on `solve_cl_piecewise`'s `env`
  param recording the KNOWN-DEBT envelope intake (item 5). No signature change.
- `examples/cl_cache_lab.rs`, `tests/cl_cache_lab_goldens.rs`,
  `tests/cl_cache_patch_regressions.rs`: `degenbot_solvers::cl_cache::…` →
  `degenbot_solvers::cl::cache::…` (3 use sites + 1 type path). The
  regression file's stale "cl_cache.rs" rationale comment now says
  `cl/cache.rs`.

## Visibility audit (item 4)

Checked every `cl/` root export against downstream consumers (`degenbot-bot`,
`degenbot` facade, `degenbot-strategy`, `degenbot-substrate`, plus the crate's
own examples/tests/benches, which link the lib as an external crate).

- SHRUNK: the `degenbot_pools::int_v3_hop::IntV3TickRangeHop` re-export is now
  `pub(crate)`. No consumer names `degenbot_solvers::cl::IntV3TickRangeHop`
  (all use `degenbot_pools::int_v3_hop::IntV3TickRangeHop` directly); it is
  used only inside the crate. The other two pool re-exports
  (`IntTickRangeCrossing`, `IntV3TickRangeSequence`) stay public — consumed by
  `degenbot-substrate` and the `cl_cache_patch_regressions` binary.
- LEFT PUBLIC (consumer named):
  - `ClProfileTable` — appears in the public `PreparedClHop::from_parts`
    intake and the `PreparedClHop.profiles` field, which `degenbot-bot`
    constructs/reads.
  - `V3RangeSwapResult` — the return type of the public
    `simulate_v3_range_swap`, consumed by the parity/regression binaries.
  - `WalkNsCensus` / `WalkSection` — the return/param types of the public
    `WalkTelemetry::drain_ns` / `WalkTally::mark_commit`, consumed by the
    `cl_cache_lab` example.
  - `ClPathSim` / `ClPathOutcome` — no downstream consumer today, but
    `pub(crate)` makes them dead code outside `#[cfg(test)]` (only
    `cl/tests/refine_undersample.rs` and path_sim's own tests construct
    `ClPathSim`), and they are the documented public path-simulator surface.
    Retained public; flagged for the manager.
  - All remaining exports (`WalkOutcome`, `WalkStats`, `WalkMemo`,
    `WalkMemoStats`, `WalkTally`, `WalkTelemetry`, `WalkEventCensus`,
    `ClWordProfile`, `ClCrossingTable`, the `build_cl_*` helpers,
    `DENSE_OBSERVE_THRESHOLD`, `derive_and_solve_cl_piecewise`,
    `solve_cl_piecewise`, `solve_mixed_piecewise`, `MixedHopView`,
    `PreparedClHop`, `simulate_v3_range_swap`) are consumed by `degenbot-bot`
    / `degenbot-substrate` and/or the crate's test/example/bench targets — left
    public.

## Gates (foreground)

- `cargo fmt --manifest-path rust/Cargo.toml -p degenbot-solvers -- --check` →
  clean (exit 0).
- `cargo clippy --manifest-path rust/Cargo.toml -p degenbot-solvers
  --all-targets -- -D warnings` → clean (exit 0).
- `cargo test --manifest-path rust/Cargo.toml -p degenbot-solvers` → green:
  lib 181 passed / 0 failed / 3 ignored; `bounded_brent` 7,
  `cl_cache_lab_goldens` 1, `cl_cache_patch_regressions` 3,
  `mobius_discriminant_domain` 3, `offline_heavy_replay` 1,
  `profit_envelope_tests` 27, `solidly_golden_parity` 1,
  `stable_golden_drift_net` 4, `v3_crossing_solver_vs_sim_parity` 4,
  `v3_iia_fixture_reproduction` 1, `v4_crossing_solver_vs_sim_parity` 7,
  `v4_stale_state_confirmation` 1, `v4_word_boundary_solver_divergence` 2;
  0 failed.
- `cargo check --manifest-path rust/Cargo.toml -p degenbot-bot -p degenbot` →
  clean (exit 0).

Byte-identity: `git diff --cached -M --stat` shows the rename as `R100`; the
moved file's sha256 equals `HEAD:rust/crates/engine/degenbot-solvers/src/cl_cache.rs`.
No arithmetic, ordering, or signature change; the only edits beyond the move are
the import paths, the `cl/mod.rs` docs / `pub(crate)`, one comment, and one
test-doc path.

PENDING: none.

## Notes / left alone

- Historical records that still name `src/cl_cache.rs` (ADR-060, ADR-069,
  `docs/ergo-results/*`, `docs/cache-lab-report.md`) were left untouched — they
  are past-work records; ADR-071 D7 owns the doc rewrite.
- Out of scope by brief and untouched: `profit_envelope/*`, the dispatch table /
  `FamilyArm` / `hop_math_for`, and the open solver bugs
  `OJ2MMF` / `YYHWVQ` / `A47GNH`.

## Tree hygiene

Tree was clean at start (`git rev-parse HEAD` = 639ae6a5f, the ADR-071 commit).
The working tree now holds exactly the 6 modified paths + 1 rename listed above;
no foreign edits observed and no untracked files outside this result.
