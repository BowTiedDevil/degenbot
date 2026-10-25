# ADR-084: Capability homes — doors, charters, one home per capability

**Status: accepted.** Drafted as the capability-surface plan
(`docs/architecture/capability-restructure-plan.md`, v5) and adversarially
reviewed over nine rounds by three independent reviewers; all three approved
v5. Deepens ADR-005's layering and ADR-025's seam discipline without touching
ADR-041's stage table, ADR-073's family read modules, or ADR-080's CL state
extraction (sequenced behind it, see D5/W5).

## Context

The T4 merge dissolved `degenbot-solvers` into `degenbot-pools` (ADR-073 A5)
because the hop vocabulary is defined by the read side and consumed by the
solve side — any crate boundary cuts through every family file. That was the
right call, and it was fan-out-neutral in aggregate (`pools` in-degree
11 → 12), but it left the merged crate presenting an unmanageable surface and
made its dependency set load-bearing for crates that never asked for it:

- `degenbot-pools` grew 24,162 → 49,611 src lines (+105 %) and now presents
  **33 root `pub mod`s (58 crate-wide) and 686 `pub` items** with no curated
  entry surface. 998 lines outside the crate name a `degenbot_pools::` path;
  246 references across 67 files name a `solve::` path, including a
  four-segment `solve::mixed::solve::SolveOutcome`.
- `zstd` + `zstd-sys` (a C build) is mandatory in the normal dependency trees
  of `execution`, `rpc`, `pool-updater`, `substrate`, `strategy`, `simulation`,
  and `bot` solely through `pools/src/solve/capture_fixture.rs` — fixture
  machinery in a value-layer foundation crate. `degenbot-execution` is a
  657-line seam crate whose dependency set is supposed to be its published
  promise (ADR-025); it now compiles a C library to be linked against.
- The role directories contradict the dependency direction they describe:
  6 upward edges from 4 sources over 3 targets
  (`substrate`/`bot` → `ingestion`/`pool-updater`, `submission`/`strategy` →
  `runs`).

Crate count is **not** the axis: the workspace graph is acyclic, in-degrees are
healthy (`core` 24, `config` 17, `rpc` 14, `pools` 12), and dissolving one
crate bought no blast-radius reduction. What is wrong is that capability
surfaces are uncurated trees, dependency sets are unguarded, and layer names
are decorative. The five tests a capability home must pass are C1 nameable,
C2 one home, C3 dep set = contract, C4 door not tree, C5 task-complete in the
consumer's own surface (provable by a runnable probe).

## Decision

**D1 — One capability, one home, one door.** `docs/capabilities.md` is
authoritative: capability word → home (crate + module) → door items → the C5
consumer and its probe → "must not pull". Every publishable crate appears at
least once; every entry resolves to real code (gate-checked). Rows carry the
Rust path and, where one exists, the Python name; rows with none say
"Rust-only".

**D2 — Doors, not trees.** `pub` items are a curated surface; the trees beneath
are `pub(crate)` unless a *named* consumer needs them. Mechanical consequences:
**(a)** a type that appears in a public field of a door item is itself a door
item, and the door is closed under that rule — the closure is computed into a
committed census artifact and enforced by the reference channel's
`clippy --deny warnings` (`private_interfaces`); **(b)** `[[example]]`/`tests/`
targets are separate crates that cannot see `pub(crate)`, so a lab needing
internals either earns explicit door items, moves into `#[cfg(test)]`
handlers, or is deleted — decided per lab. Error enums raised at a capability
boundary are door items of the crate that raises them.

**D3 — The dependency charter is the published contract.** Each crate declares
the dependency prefixes it may carry; a gate asserts
`cargo tree -p X -e normal ⊆` charter at single-package feature resolution
(`just check-dependency-charter`, wired into `lint-rust-check`). Seam crates
(`execution`, `rpc`, `core`, `pools`, `math`, `uniswap`, `pathfinding`) add
"no C-build dep" and "no pyo3".

**D4 — Capability layers, honestly named.** `foundation` ⊂ `engine` ⊂
`integrations`; `shells`/`facade` compose. Edge direction is enforced over a
census allowlist that can only shrink. Transport/IO leaves the stack depends
on live in `foundation`.

**D5 — No workspace member is added or removed.** Role-directory moves and
module namespacing are in scope; count is not the axis. A future split must
pass C2–C4 *and* show the seam does not cut an object model (D6). W5's
`pools` namespacing is sequenced **behind ADR-080**, which owns the same
files.

**D6 — Rejected alternatives are recorded with their measurements** (below).

**D7 — A breaking cut ships with a migration note.** CHANGELOG entry + a
"was → is" table for every published path removed (the W2 solve-tree
renames — `solve::mixed::*` → the `solvers` root door, `profit_envelope` →
`solve::gate` with the inner gate items hoisted to the door root,
`affected_keys` → `affected`, `capture_fixture` → `fixtures` — and W5's
root-module moves). `0.6.0-alpha.*` permits hard cutover, not silence.

## Rejected alternatives

1. **Resurrect `degenbot-solvers`.** The hop vocabulary is defined by the read
   side and consumed by the solve side; any boundary cuts through every family
   file (the two-homes problem ADR-073 exists to remove). Fan-out was
   net-neutral in aggregate (net +1 in-degree) but per-crate hostile: two seam
   crates (`execution`, `execution-sample`) had no direct `pools` edge pre-T4
   and gained the whole tree at the merge — which is the leak D3/W3 fixes, not
   an argument for the split.
2. **Split `pools` into state + behavior.** `v3_state.rs` reaches into
   `families::concentrated_liquidity::{state_journal, state_seeding,
   state_walk}` while 29 files cross the other way, and ADR-080 is moving
   exactly those modules; a split today would fight a landed, in-flight plan.
3. **Merge small crates by size.** The small crates are charters, not
   leftovers (ADR-005's interface-crate pattern is why the graph has no
   cycles); merging by line count destroys contract boundaries C3 depends on.

## Consequences

- Consumers get intent-named doors (`degenbot::solvers::solve_path_with_min_profit`
  instead of `degenbot_pools::solve::mixed::solve_path`) and a catalog page
  answering "what can I do, where do I call it, what must I not have to pull".
- `cargo add degenbot-execution` stops compiling `zstd-sys` (W3); fixture
  corpora stay executed, not silently skipped, because the test lanes enable
  the feature explicitly and assert on run counts.
- Every breaking path cut lands with a migration table (D7); the umbrella
  facade remains a curated capability map (W6).
- The 33-root-module `pools` surface is pinned by a snapshot (W5 phase 0) and
  collapses to 4 capability modules only after ADR-080 lands (W5 phase 1).
