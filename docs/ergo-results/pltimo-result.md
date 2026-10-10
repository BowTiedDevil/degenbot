# PLTIMO — S1: the `AddressSet` address-change arm + the stale -2b comment

Task PLTIMO (epic CKPAUE, the "-2b continuation"). Closes the `AddressSet`
gap in the Aave config dispatch and corrects the dispatch loop's stale note.

## Decision (schema conventions arbitrate — no in-tree Python reference)

The retired Python `_process_address_set_event` (recovered from git history,
`04a44f821^:src/degenbot/cli/aave/event_handlers.py`) only ever asserted
`old == ZERO` — it had no change arm, so it cannot arbitrate. The schema
conventions it leaves behind do:

- a contract row is keyed by `(market_id, name)` and its columns are mutated
  in place — the revision update is `UPDATE aave_v3_contracts SET revision = …`
  and the sibling `PoolDataProviderUpdated` address rotation is
  `UPDATE aave_v3_contracts SET address = … WHERE address = …`;
- there is no retire/supersede column.

So the address CHANGE is **replace-in-place**: the row currently carrying the
old address moves to the new one; no duplicate row, no delete+insert.
`revision` is left untouched (a rotation does not change it).

## Changes (no commit — tree left dirty for review)

- `rust/crates/foundation/degenbot-db/src/write/pools.rs` — new
  `apply_contract_address_changed_on_conn(conn, market_id, name, old, new)`:
  `UPDATE aave_v3_contracts SET address = ?new WHERE market_id = ? AND name = ?
  AND address = ?old`, a loud `MissingRow` when no row matches.
- `rust/crates/integrations/degenbot-aave/src/updater/run/apply.rs` — new
  `AaveChunkEvent::ContractAddressChanged { market_id, name, old_address,
  new_address }`, its apply arm, and the `contract_address_changed` report
  counter. The `ContractInserted` doc now says it is the registration arm only.
- `rust/crates/integrations/degenbot-aave/src/updater/config_dispatch/mod.rs`
  — the `AddressSet` arm now branches: `old == ZERO` inserts (unchanged), else
  emits `ContractAddressChanged` (no longer a `DecodeShape` abort). The
  `dispatch_config_events` fn doc no longer claims the six events are skipped;
  it states they are all handled through `resolve_missing_variant_event`, names
  the rotation gap this chunk closed, and defers the golden pins to a later
  slice.

## Tests (through the `ChunkContext` / `db_seeded()` seam)

- `address_set_registration_inserts_a_contract_row` — `old == ZERO` emits
  `ContractInserted`; the applied row lands with a NULL revision.
- `address_set_rotation_replaces_the_row_address_in_place` — `old != ZERO`
  emits `ContractAddressChanged`; the old row's address moves to the new one,
  its revision stays, exactly one row remains.
- `address_set_registration_then_rotation_in_tx_order` — registration then
  rotation in logIndex order: the rotation lands on the address the earlier
  registration left behind (read-your-own-writes).

## Gates (foreground, scoped)

- `cargo test -p degenbot-aave` — **green**: 251 lib unit tests pass (+3 new),
  and the golden replay suites pass unchanged —
  `tests/aave_cassette_replay.rs` 2/2,
  `tests/aave_config_cassette_replay.rs` 2/2,
  `tests/wave3_capture_replay.rs` 1/1,
  `tests/wave4_upgrade_span_replay.rs` 1/1. No recorded span carries these
  events, so nothing moved (byte-identical goldens).
- `cargo clippy -p degenbot-aave --all-targets` — clean, no warnings
  (rustc 1.98.1, the pinned toolchain).
- `cargo fmt -p degenbot-aave -p degenbot-db` — applied (idempotent).
- `bash scripts/hooks/comment-hygiene.sh` — exit 0 (run to confirm the new
  comments carry no bare six-char tracker token).

Nothing marked PENDING. No foreign tree activity observed; the working tree
held only the three files above.
