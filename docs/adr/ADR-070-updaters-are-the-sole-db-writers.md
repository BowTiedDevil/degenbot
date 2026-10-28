# ADR-070: Updaters are the sole DB writers — the `writer` cargo fence, read-only opens, path-only schema init, and a serialized heal swap

**Status: accepted (2026-10-12). Predecessors: ADR-052 (heal-at-open + the Alembic retirement), ADR-068 (the updater golden-capture harness), ADR-011 (the atomic heal swap).**

## Context

`degenbot-db` shipped one open path and one write path. `DegenbotDb::open`
ran the concurrency PRAGMAs + the schema gate + heal-at-open, then set
`PRAGMA query_only=on` — the load-bearing guarantee that a returned reader
cannot mutate the DB. But the write substrate (`src/write/`, the pool-discovery
`upsert_*` writers, `open_for_writes`, `open_in_memory_for_writes`) compiled
unconditionally, so nothing stopped a bot or engine crate from opening a
write-capable handle and mutating state outside the updater pipeline. The
"updaters write, everyone else reads" rule lived in prose.

Two things made the prose expensive. First, a cargo feature alone does not
enforce the fence in a workspace: feature unification re-enables
`degenbot-db/writer` for every crate that shares an updater dependency
(`degenbot-bot` and `degenbot-substrate` both depend on
`degenbot-pool-updater`), so a stray `open_for_writes` in the engine would
still compile. The fence needs a source-level gate as well. Second, schema init
(`ensure_schema_at_open`, `heal_database`) is the one place that writes DDL,
and its contract — it accepts a path and flags, never data — was also only
prose.

## Decision

**D1 — The write substrate lives behind `degenbot-db`'s `writer` feature.**
`src/write/` (module + fns), the pool-discovery writers, and the two
write-capable constructors (`open_for_writes`, `open_in_memory_for_writes`) are
compiled only under `feature = "writer"` (default OFF). `cfg(any(test,
feature = "writer"))` at the gate keeps this crate's own unit tests exercising
the writer without forcing the feature on downstream.

**D2 — Only the updater shells enable `writer`:** `degenbot-aave`,
`degenbot-pool-updater`, `degenbot-cli-core`, and `degenbot-python` (its write
seams serve the updater shells; a future module split may narrow that edge).
Bot, substrate, facade, and engine crates never enable it, so the write fns do
not exist in their builds.

**D3 (binding #2, restated) — Every read connection sets `PRAGMA query_only=on`.**
`DegenbotDb::open` / `open_in_memory` set it after the schema step (the DDL and
the post-heal reopen are writes); the write-capable constructors never do.
Once a read open returns, the handle is physically incapable of mutating the DB.

**D4 (binding #2b, new) — Schema init accepts only (path, flags).**
`ensure_schema_at_open` and `heal_database` take a path and flags (and, for the
private test seam, an injected open factory); they take **no data-bearing
parameter** — no bytes, rows, or schema content. The DDL is the embedded
constant and the heal reads the file it is handed. A signature gate enforces
this.

**D5 — The cursor monotone-completeness invariant.** `ExchangeTable.last_update_block`
advances only over a **complete** `PoolCreated` range: the discovery writer
stamps the block only after the whole range's rows are upserted, so a restart
resumes from a block whose range is fully applied — never a partially-applied
block.

**D6 — Updaters are the sole writers, and the fence is gated in source.**
The `architecture_gates` suite asserts that no crate outside the D2 allowlist
names a write-capable constructor in its **production** `src` (test modules and
`tests.rs` files may exercise the writer). This is the gate the cargo feature
cannot be, because workspace feature unification re-enables `writer` through a
shared updater dependency.

_live-invariant gate (ADR-087 D2): watches the production `src` of every live crate for write-capable constructors outside the D2 allowlist; not tripwire precedent._

**D7 — The construction DB half is opened by the shell.** The engine's
`Bot::open_construction_db` is retired; the shell (a writer-enabled crate) opens
the write-capable handle for `attach_construction_io`. The engine never touches
`open_for_writes`.

**D8 — The heal dump-and-restore is serialized by an advisory lock.**
`heal_database` takes an exclusive advisory `flock` on a sibling `<db>.heal.lock`
for the whole operation, so two processes cannot race the temp build or the
atomic swap (ADR-011). The second healer **waits** on the lock (a blocking
`flock`, not a fail-fast attempt) and, on acquiring it, re-inspects the live DB —
already swapped to `RustOwned` by the first — and returns the no-op report. The
kernel releases the lock with the holding process, so a crashed healer leaves no
stale lock; the lock file is **not** unlinked, so every opener flocks the same
inode (unlinking a held `flock` would let a waiter and a re-creator hold locks on
two different inodes). The existing WAL-conversion retry is untouched: this
guards only the swap.

## Consequences

- A default `cargo build` of the workspace succeeds: the four updater shells
  turn `writer` on, and every other crate compiles against the read-only
  `degenbot-db` surface.
- A probe that references a write fn from a crate without the feature fails to
  compile — the feature is a real compile-time fence for a crate built in
  isolation, and the source gate covers the unified workspace build.
- `degenbot-db`'s own writer integration tests are `#![cfg(feature = "writer")]`:
  `cargo test -p degenbot-db` runs the read suite; `--features writer` runs the
  writer suite.
- `sql-ledger` (test/bench-only) implies `writer`, since the ledger writes
  through the substrate.
- The three load-bearing bindings — query_only reads (#2), path-only schema init
  (#2b), and the complete-range cursor advance (D5) — are recorded here and, for
  #2b and the fence, enforced by architecture gates. The serialized heal swap
  (D8) is covered by a concurrency test (two racing healers: one heals, one
  no-ops, no `.bak` corruption).
