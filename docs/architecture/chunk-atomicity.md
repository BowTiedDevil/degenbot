# Chunk atomicity

The pool updater and the Aave V3 updater commit a database chunk under one
invariant, and the whole reorg/restart story depends on it:

> **One `Connection`, one `Transaction` per chunk. Every write in the chunk —
> pool rows, liquidity updates, position rows, per-pool markers, and the
> `last_update_block` stamp — lands in a single commit, or none of it does.**

## Definitions

- A **chunk** is a batch of blocks `[start, end]` processed for the exchanges
  whose turn it is.
- A **chunk's writes** are every row the chunk's events imply plus the
  per-exchange `last_update_block = end` stamp.
- `last_update_block` is the **restart cursor**: the strict upper bound on the
  block range whose data is durably committed.

## The three invariants

1. **Atomicity.** At chunk commit, either every write in the chunk is durably
   persisted in one transaction commit, or none of them are. No intermediate
   state is observable — including across an interrupt or crash.
2. **Restart.** Restarting from `last_update_block + 1` re-processes only work
   that was not committed. It never re-applies committed work, and it never
   skips a block: a rolled-back chunk is re-fetched and re-applied whole.
3. **No duplicate writer.** At most one connection holds a write transaction on
   the database during a chunk. A second writer on the same file bypasses every
   in-process mutex (separate connections carry separate locks) and was
   empirically proven to silently corrupt mid-chunk state.

## Where it lives

- **Pool updater** (`degenbot-pool-updater`): the outer chunk loop in
  `run::run_pool_update` owns the transaction; the write half is
  `apply_chunk_writes_on_conn` — pure-sync, borrowed `&Connection`, so every
  write of the chunk rides the caller's transaction.
- **Aave V3 updater** (`degenbot-aave`): the outer chunk loop in
  `run::run_aave_update` opens one `DegenbotDb`, begins one `Transaction`, and
  the apply stage commits through `apply_aave_chunk_writes_on_conn` on that one
  borrowed connection. Any `?` early-return (a `UNIQUE` violation, a decode
  failure) leaves the transaction uncommitted, so it drops and the whole chunk
  reverts with the stamp unchanged.

The `_on_conn` variants across `degenbot-db` exist to serve this: they are the
single-transaction-bound forms of the write surface.

## Supporting rules

- **Read your own writes.** Reads for event N inside a chunk see the writes of
  events < N in the same chunk (the borrowed connection + write overlay), never
  a second connection's stale view.
- **The stamp is the LAST write.** `last_update_block` advances inside the
  chunk's transaction, after the data writes, so a commit that includes the
  stamp proves the data; a rollback loses both together. An empty chunk still
  advances the stamp (chunk-end semantics).
- **The interrupt contract.** A cancel flag is polled between chunks.
  SIGINT *between* chunks is honored immediately: the not-yet-started chunk is
  rolled back and a partial report returned. SIGINT *mid-chunk* lets the chunk
  complete atomically (commit or rollback) before the run returns. Either way,
  no partial-chunk state is observable.

## The pinned tests

- `rust/crates/integrations/degenbot-pool-updater/tests/chunk_atomicity_contract.rs`
  — interrupt → full rollback → restart clean (the original bug's regression:
  pools committed mid-chunk, stamp stale, restart hit `UNIQUE constraint`).
- `rust/crates/integrations/degenbot-pool-updater/tests/no_duplicate_writer_contract.rs`
  — a second writer on the same file serializes via the SQLite file lock.
- `rust/crates/foundation/degenbot-db/tests/writer_parity.rs` and the
  `degenbot-aave` atomicity tests — replaying the same event sequence twice
  (a re-org / re-index) lands identical state.

Debugging/diagnostic work must never hand-fix a committed database in place:
that breaks the chunk-boundary invariant and poisons every downstream compare.
Experiments run against a throwaway temp DB; a verified baseline is rebuilt by
re-driving from genesis, never by mutation.
