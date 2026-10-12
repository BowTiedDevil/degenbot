# GILP7H — P4b: heal-swap serialization guard

Agent: `p4b-heal-guard` (dispatched worker). Tree left dirty; nothing committed.

Epic: `4ROQ6S` — "Bot deep, Python light, DB privileged". Dep `5DOF4C` (P4a)
is `done` (`0f6087313`, ADR-070). This chunk serializes the out-of-place heal
swap so two processes cannot race the dump-and-restore, and adds the guard's
contract line to the P4a ADR.

## What changed (files + intent)

### Changed — `rust/crates/foundation/degenbot-db/src/heal.rs`

- New private `HealSwapGuard`: an exclusive advisory `flock`
  (`std::fs::File::lock`) on a sibling `<db>.heal.lock` file, held for the whole
  `heal_database` call. `acquire` opens-or-creates the lock file (never
  `create_new`), **blocks** on the lock (wait, not fail-fast), and writes the
  holder pid best-effort for diagnostics. The descriptor's lifetime IS the lock.
- `heal_database` now takes the guard as **step 0**, before the read-only
  inspect. A second healer blocks in step 0 until the first finishes, then its
  step-1 `classify_schema` sees the already-swapped `RustOwned` DB and returns
  the existing no-op report — exactly the "wait-then-recheck" shape the task
  asked for. The guard is held through the atomic swap (step 8) and released on
  return.
- Module docs gain a "# Concurrency — the heal-swap guard (ADR-070 D8)" section.

Design notes:

- **Blocking, not timed.** The task allowed "flock with timeout" but preferred
  wait-then-recheck over fail. A blocking `flock` cannot hang on a dead holder —
  the kernel drops the lock with the process — so the only wait is a live
  healer's (short) copy. A timeout would turn a slow-but-healthy peer into a
  spurious failure.
- **The lock file is not unlinked on drop**, deliberately diverging from
  `degenbot-cli-core`'s `ResetLock` (which unlinks). Unlinking a held `flock`
  reintroduces the classic race: a waiter blocked on the unlinked inode and a
  newcomer that re-creates the path end up on two different inodes and stop
  excluding each other. A stable name keeps every opener on one inode. The file
  is the flock's anchor, not a pid-file sentinel.
- **The WAL-conversion retry (`pragma.rs`) is untouched** — this guards only the
  swap, as scoped.
- **Signature unchanged** (`heal_database(&Path)`), so ADR-070 binding #2b — the
  `schema_init_functions_accept_only_path_and_flags` gate — still passes: the
  lock path is derived from `old_path`, no new parameter.

### Changed — `rust/crates/foundation/degenbot-db/src/heal/tests.rs`

- New `concurrent_heals_serialize_one_heals_one_noops`: builds a legacy fixture,
  spawns two threads (an `Arc<Barrier>` so they contend at the same instant),
  and heals the same path from both. Asserts exactly one ran the copy
  (`old_state == LegacyAlembic`, non-empty `rows_copied`) and the other no-opped
  (`old_state == RustOwned`, empty `rows_copied`, `bak_path == old_path`); both
  report the same post-heal `RustOwned` `SchemaState`; no `*.heal-tmp` survives;
  the live DB is healed and FK-intact; the `*.bak` is the intact OLD DB (no
  double-swap corruption).

### Changed — `docs/adr/ADR-070-updaters-are-the-sole-db-writers.md`

- New **D8 — The heal dump-and-restore is serialized by an advisory lock**: the
  guard's contract (blocking `flock`, wait-then-recheck, kernel-released, not
  unlinked, WAL retry untouched). Title + Consequences bullet updated.

## Gates

| Gate | Result |
|---|---|
| `cargo test -p degenbot-db` (default) | **pass** — 176 lib (incl. new test) + all integration suites |
| `cargo clippy -p degenbot-db --all-targets -- --deny warnings` | **pass** |
| `cargo fmt -p degenbot-db -- --check` | **pass** |
| `cargo test -p degenbot --test architecture_gates schema_init_functions_accept_only_path_and_flags only_the_updater_shells_open_the_db_for_writes` | **pass** (2/2) |
| `cargo check --workspace` | **pass** |

(All via `--manifest-path rust/Cargo.toml`.)

Not run (dispatcher-owned / out of scope): `just test-rust-nextest`,
`just test-rust`, `just test-python`, whole-tree lint/format sweeps. No Python
was touched, so the Python ladder was skipped.

## PENDING

None.

## Foreign tree activity observed (left alone)

None. `git status --porcelain` shows only this chunk's three files.

## Notes for the manager

- The guard covers the whole heal (temp build + copy + swap), not just the final
  rename: two healers would otherwise collide on the shared `<db>.heal-tmp`
  path. Acquiring it before step 1 is also what makes the recheck work (the
  second healer re-classifies the DB after waiting).
- A `<db>.heal.lock` file persists beside a healed DB by design. If a future
  hygiene pass wants it gone, the unlink must happen only when no holder can be
  waiting — which is why it is left alone here.
- `cargo test -p degenbot-db --features writer` (the writer-gated suites) was not
  re-run separately; the guard lives on the read/heal path and the default suite
  exercises it. The P4a sign-off already covers the writer-feature build.
