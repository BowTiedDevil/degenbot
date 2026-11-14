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

**D2 addendum (task `4W2ZUK`) — the offline fixture shell joins the owner set,
on a temporary-harness-only boundary.** `degenbot-fixture-tools`, the
non-publishable development-tool package, is now also allowed to name a
write-capable constructor, and it is listed by name in the fence's
`WRITER_OWNERS`. What forced the entry is a relocation, not a new capability:
the two offline capture generators `generate_evm_oracle_captures` and
`generate_aave_adversarial_captures` moved from the facade's `examples/` — where
D6's gate does not scan (an example is not production `src`) — into that
package's `src/bin/<name>/main.rs`, where it does. Both programs run the REAL
`run_pool_update` / `run_aave_update` chunk loops over a recorded transport, so
they need a writer exactly as the updater shells do: `DegenbotDb::open_for_writes`
seeds the harness DB each recorded chunk applies to, and
`LedgerDb::open_for_writes` opens the DB the SQL goldens are dumped from.

The same relocation carried the two LIVE Aave span recorders

(`generate_aave_upgrade_windows`, `generate_aave_config_windows`) into this

package. They open the same two kinds of throwaway harness DB — one per recorded

window, one per golden replay — so they sit inside this boundary unchanged and

raise no new question: the exemption is about where the write open lands, not

about which program makes it.

The boundary is the rule, not a description of today's seven programs: **every
write open in `degenbot-fixture-tools` targets a throwaway harness DB under a
`TempDir`** (and two of the seven open no database at all, so they open no write
either). The artifacts those programs produce are written by ordinary
`fs::write` into the corpus homes, and `--check` writes nothing at all, so the
subject of the fence — who may open the operator's database for writing — is
untouched. A program in this package that opened a *durable* DB would be a new
production write path and needs its own D2 decision. The four shells named above
remain the only crates that may write an operator DB, and this entry exempts no
other tool package: a later `src/bin/` program in `degenbot-solver-lab` or
`degenbot-engine-lab` must earn its own row.

The sixth program in the package, the Python oracle corpus recorder
(`record_py_oracle_corpus`, moved out of the facade's `examples/`), opens no
database at all — neither read nor write. It records RPC answers into JSON
corpus files with `fs::write`, so it neither needs nor receives a place in the
fence, and its arrival widens nothing: the exemption still covers throwaway
harness DBs only.

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
(its addendum included) names a write-capable constructor in its **production**
`src` (test modules and `tests.rs` files may exercise the writer). This is the gate the cargo feature
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
  `degenbot-db` surface. `degenbot-fixture-tools` (D2's addendum) is the one
  writer-enabled package outside that set. It is not part of a default
  workspace build (it is absent from `default-members`). Seven programs live
  there as `[[bin]]` targets, and the lane that builds each is the feature it
  needs: four — the two node-free capture generators and the two live span
  recorders — replay their SQL goldens, so they carry
  `required-features = ["sql-ledger"]` and only
  `just check-dev-tools fixture-sql-ledger` and the two capture recipes build
  them, deliberately. The fifth, the live updater cassette recorder, reads no
  statement ledger and carries no `required-features`, so
  `just check-dev-tools default` builds it. Its write opens are the same
  temporary-harness-only kind: one `DegenbotDb::open_for_writes` per recorded
  chunk (the `--verify` pool arm and the aave-run arm, each on its own
  `TempDir`), and its `--check` arm writes nothing. The sixth, the Python oracle
  corpus recorder `record_py_oracle_corpus`, opens no DB of any kind — it opens
  no operator DB, and it opens no harness DB either: its only writes are the
  JSON corpus files, so D2's subject (who may open the operator's database for
  writing) is not engaged by its arrival. The seventh, the path-fixture recapture
  `capture_path_fixture` (moved out of the facade's `examples/`), is the same
  case: it opens no DB of any kind — its only writes are the fixture JSON files
  it recaptures, by `fs::write` — so it widens nothing, adds no `WRITER_OWNERS`
  entry, and leaves the fence's subject exactly where it was.
- The fixture slice is recognition, not a new write path. Those programs
  always opened throwaway harness DBs for writing; while they were facade
  examples the fence never asked the question, and the relocation into
  production `src` is what made it ask. The answer — temporary harness DBs only,
  never an operator DB — is recorded in D2's addendum, and the verification
  route is `just test-rust-nextest -p degenbot -E
  'test(only_the_updater_shells_open_the_db_for_writes)'` (the fence) plus
  `just check-evm-captures` / `just check-aave-captures` (the programs still
  reproduce the committed corpus byte-for-byte offline). The two live span

  recorders have no offline `--check` proof of their own: both need an endpoint,

  so their offline verification is the compile lane

  (`just check-dev-tools fixture-sql-ledger`) plus the wave-4/wave-5 replay

  suites over the committed cassettes, and a live re-recording is reported as

  unexecuted, never as passing.
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
