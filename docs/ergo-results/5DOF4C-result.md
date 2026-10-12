# P4a (5DOF4C) — writer cargo feature + architecture gates + ADR

## What changed

**`degenbot-db` — the writer fence (ADR-070).**
- `Cargo.toml`: new `writer = []` feature (default OFF); `sql-ledger` now implies
  `writer` (the ledger writes through the substrate).
- `src/lib.rs`: `pub mod write;` and the two `pub use write::{...}` re-export
  blocks are gated `#[cfg(any(test, feature = "writer"))]`; the stale
  "writes deferred" module doc is replaced by the fence contract.
- `src/connection.rs`: `open_for_writes` / `open_in_memory_for_writes` gated
  `#[cfg(any(test, feature = "writer"))]`.
- `src/discovery.rs`: the pool-discovery writers (`upsert_v2/v3/v4_pools[_on_conn]`,
  `set_exchange_last_update_block[_on_conn|_if_unchanged_on_conn]`, `upsert_exchange`,
  `set_exchange_active`, `upsert_pool_manager`) — the only production consumers of
  the `write/` substrate inside the crate — are gated the same way, along with the
  `ExchangeRow`/`PoolManagerRow`/`schema::table` imports they need.
- `tests/{writer_parity,pathfinding_parity,liquidity_update_block,wal_snapshot_isolation,discovery_parity,liquidity_updater_parity}.rs`:
  `#![cfg(feature = "writer")]` — the writer integration suites run under
  `--features writer`.

`cfg(any(test, feature = "writer"))` is deliberate: it keeps `cargo test -p
   degenbot-db` (default features) exercising the writer via the crate's own unit
   tests, while a downstream build without the feature never sees the fns.

**Feature enabled only by the updater shells.** `degenbot-aave`,
`degenbot-pool-updater`, `degenbot-cli-core`, `degenbot-python` now depend on
`degenbot-db` with `features = ["writer"]`. Bot/substrate/facade/engine crates
unchanged (no feature).

**Engine loses the write open.** `degenbot-bot`'s `Bot::open_construction_db`
(and its unit test) is retired; the shell owns the write-capable open
(`open_construction_db` in `degenbot-python/src/bot/mod.rs`, called under
`py.detach`). The shell's `bot` feature now includes `db`.

**Architecture gates** (`facade/degenbot/tests/architecture_gates.rs`):
- `only_the_updater_shells_open_the_db_for_writes` — scans every non-allowlist
  `degenbot*` crate's production `src` (skipping `tests.rs` files, `src/**/tests/`,
  and `#[cfg(test)]` regions) for `open_for_writes` /
  `open_in_memory_for_writes`. This is the gate the cargo feature cannot be,
  because workspace feature unification re-enables `writer` through a shared
  updater dependency.
- `schema_init_functions_accept_only_path_and_flags` — binding #2b: the declared
  parameter types of `ensure_schema_at_open` and `heal_database` must be in
  {`&Path`, `bool`, `F`} (no data-bearing parameter).

**ADR-070** `docs/adr/ADR-070-updaters-are-the-sole-db-writers.md` (+ README row):
records binding #2 (query_only on every read connection), binding #2b (path-only
schema init), the cursor monotone-completeness invariant (`last_update_block`
advances only over complete `PoolCreated` ranges), and the updaters-as-sole-writers
ruling. Notes the follow-up module-split option for `degenbot-python`.

**Python doc-debt (manager note).** `src/degenbot/bot/_bot.py` ~194-196: the
stale "loaded at ``engine_registry.start`` via ``load_*_from_py``" comment is
replaced — the injected seam is retired (P2b); the DB snapshot is the sole S
source.

## Gates run (all foreground)

- `cargo build --workspace` — **green** (1m15s).
- `cargo check -p degenbot-db` (default) — green.
- `cargo test -p degenbot-db` (default) — **green** (175 unit + integration; the
  six writer suites are feature-gated to 0 tests).
- `cargo test -p degenbot-db --features writer` — **green** (writer suites run).
- `cargo test -p degenbot-pool-updater` — green (35 + contract suites).
- `cargo test -p degenbot-cli-core` — green (1 ignored, pre-existing).
- `cargo test -p degenbot-aave` — green (256 + replay suites).
- `cargo test -p degenbot --test architecture_gates only_the_updater_shells_open_the_db_for_writes` — green.
- `cargo test -p degenbot --test architecture_gates schema_init_functions_accept_only_path_and_flags` — green.
- `cargo check -p degenbot_rs` (the Python shell) — green.
- `uv run --no-sync ruff check src/degenbot/bot/_bot.py` — green.
- `uv run --no-sync ruff format --check src/degenbot/bot/_bot.py` — green.

Not run (dispatcher-owned): `just test-rust-nextest`, `just test-python`,
`just test-rust`, whole-tree lint/format sweeps.

## Red path (captured)

A temporary example `degenbot-db/examples/writer_fence_probe.rs` naming
`degenbot_db::DegenbotDb::open_for_writes(..)`, checked with default features:

```
error[E0599]: no associated function or constant named `open_for_writes` found for struct `DegenbotDb` in the current scope
 --> crates/foundation/degenbot-db/examples/writer_fence_probe.rs:7:38
error: could not compile `degenbot-db` (example "writer_fence_probe") due to 1 previous error
```

The probe file was deleted after the capture (not in the diff).

## PENDING

None.

## Foreign activity

None observed in the touched files. The tree carried the unrelated Rust
solvers/aave stream (`degenbot-solvers`, `degenbot-aave` updater internals); left
completely alone. `degenbot-db`'s own writer unit tests were kept working via
`cfg(any(test, feature = "writer"))` rather than by forcing the feature.
