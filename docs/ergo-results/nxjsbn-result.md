# NXJSBN (M1, epic DMRSRY): WalkTally + WalkTelemetry + WalkNsCensus additive skeleton

The ambient walk-telemetry counters now have an owned successor value, per
`docs/architecture/walk-telemetry-collector-sizing.md` §1-§3 and §5's M1 row.
The new `cl/tally` module carries the per-solve accumulator (`WalkTally`), the
engine-owned collector (`WalkTelemetry`), its process-cumulative census
(`WalkNsCensus`), and the timed-section enum (`WalkSection`). Nothing consumes
them yet — dead, feature-independent skeleton — and the §6 glossary terms are
registered here (the Q5 standing rule).

## What changed

**`rust/crates/engine/degenbot-solvers/src/cl/tally.rs`** (new)

- `WalkNsCensus` — nineteen `u64` fields: the whole-solve entry timer, the
  sim/anchor/prediction phase timers, and the four `(wall, sims, sim-ns)`
  section triples (§1.2). `const fn new`, `Default`, `Copy`, and `accumulate`
  (pointwise add — the per-solve merge is its only caller).
- `WalkSection { Edge, Redge, Dir, Refine }` — the four section-helper commit
  destinations.
- `WalkTally` — the `WalkStats` field set (twelve `usize` counters + the
  `WalkEventCensus`) plus the per-solve `WalkNsCensus` and the census-piece log.
  One `bump_*` per §1.1 counter row, `observe_max_dense_words`, the seven
  `add_*_ns` phase timers, `mark_commit(section, wall, sims, sim_ns)`,
  `record_census_pieces`, `census_mut`, `take_census_pieces`, `walk_stats`
  (the `WalkStats` projection), and `merge_into` (one locked merge per solve).
- `WalkTelemetry` — `parking_lot::Mutex<WalkTelemetryState>` interior holding the
  cumulative `WalkNsCensus` + the census-piece `Vec`; `new`, `merge_tally`
  (one lock per solve, never per bump), `drain_ns` (swap-out), and
  `take_census_pieces`.

Design rules held:

- **Single signatures in both build configurations.** Every public method has one
  shape; the `telemetry` cfg lives only in private no-op helpers (`bump`,
  `add_ns`, `observe`, `push_census_piece`, `commit_section`). No cfg-duplicated
  method shapes.
- **The collector type is not feature-gated.** `WalkTelemetry` (and
  `WalkNsCensus`/`WalkSection`) exist in every build; only `WalkTally`'s writer
  bodies ride the default-off feature, so a default build records nothing and
  `walk_stats` returns all zeros (matching the old `peek_walk_stats` default).
- **No new dependency, no env reads.** `parking_lot` was already a solvers
  dependency (the `WalkMemo` precedent).

**`rust/crates/engine/degenbot-solvers/src/cl/mod.rs`** — added `mod tally;`,
the module-layout doc line, and `pub use tally::{WalkNsCensus, WalkSection,
WalkTally, WalkTelemetry};`.

**`GLOSSARY.md`** — registered the three new terms in the "Piecewise CL solving"
section beside the existing **Walk telemetry** entry: **WalkTally** and
**WalkTelemetry** verbatim from §6, plus **WalkNsCensus** in the same
term/`_Avoid_` convention (the new type the chunk ships).

## Gates run (foreground, scoped)

- `cd rust && cargo test -p degenbot-solvers` → ok, 0 failed across all suites
  (lib 177 passed / 3 ignored; all integration targets green; 0 doc-tests).
- `cd rust && cargo test -p degenbot-solvers --features telemetry` → ok,
  0 failed across all suites (lib 178 passed / 3 ignored; all integration
  targets green; 0 doc-tests) — the cfg'd writer arms compile and pass under the
  feature.
- `cd rust && cargo clippy -p degenbot-solvers --all-targets` → clean (rustc /
  clippy 1.98.1, workspace `warnings = "deny"`). Also ran
  `cargo clippy -p degenbot-solvers --all-targets --features telemetry` → clean
  (extra confidence, not a required gate).
- `cd rust && cargo fmt -p degenbot-solvers` → applied; clean.

First clippy pass surfaced two `clippy::similar_names` errors (`sims` vs `simns`);
fixed by renaming the section sim-wall param to `sim_ns` and inlining the section
match (no sibling local bindings). Re-ran to clean.

## PENDING

None.

## Notes

- Tree was clean at start (`git status --porcelain` empty). No foreign edits
  observed. Working-tree changes are exactly the three files above (GLOSSARY.md
  and `cl/mod.rs` modified; `cl/tally.rs` new). Nothing committed — left dirty
  for the manager per the dispatched-agent lane rules.
- The M0 baseline at `/tmp/dmrsry-baseline/` was not needed for this step (M1 is
  A/B protocol step C only, and the skeleton has no consumer to audit yet); the
  §5 "C" grep (`thread_local!\|Cell<` in `src/cl/`) still shows the ambient
  module untouched — demolition is M6.
- The spec's `mark_commit(&mut self, section: Section)` sketch is realized as
  `mark_commit(&mut self, section: WalkSection, wall_ns, sims, sim_ns)` — the
  three values the `Mark` helper will carry; `WalkSection` names the enum to keep
  the re-export unambiguous.
- Scope discipline: only `degenbot-solvers` (+ GLOSSARY.md) touched; no other
  crate edited, so the spec's `degenbot-substrate` / `degenbot-bot` gates are not
  in scope for M1.
