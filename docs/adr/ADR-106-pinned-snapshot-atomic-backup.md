# ADR-106: Pinned-snapshot atomic backup

**Status: implemented** (2026-11-23; ergo task `MWH6GE`).

Amends [ADR-051](ADR-051-rust-owned-console.md) D1 (the `database backup` arm's
semantics). Consistent with [ADR-070](ADR-070-updaters-are-the-sole-db-writers.md)
(updaters are the sole DB writers; this command is an admin file operation, not a
data writer). Reuses the atomic-swap and advisory-lock discipline recorded in the
[ADR-011](ADR-011-auto-healed-alembic-retirement.md) heal design record.

## Context

`degenbot database backup` is the operator's only safety net before `heal`,
`reset`, `cutover`, or a release upgrade. Its implementation must be correct
under an active concurrent writer (the bot), fast enough to run routinely, and
impossible to misuse into a partial or corrupt artifact.

### Measured defects in the current implementation

`degenbot_db::ops::backup_database` calls
`backup.run_to_completion(100, Duration::from_millis(250), None)`, plus a source
and destination `PRAGMA integrity_check`. Measured (scratch DBs in `/tmp`,
4 KiB pages):

| Observation | Measurement |
|---|---|
| Idle 1,003-page DB | 2.71 s (pacing predicts 10 sleeps = 2.5 s) |
| Idle 4,010-page DB | 10.27 s; `cp` of the same 16 MB file: 0.003 s |
| Extrapolation at 400 pages/s | 409 MB → ~250 s; 4 GB → ~42 min |
| Stepped copy, **no pinned snapshot**, live writer | 7,404 steps, **1,203 restarts**, unfinished after 8 s; an earlier unbounded run took a 16 MB source to 3.7 GB in ~60 s without completing |
| Same copy with `BEGIN` + one read on the source connection | 46 steps, **0 restarts**, 0.09 s; snapshot consistency invariant intact |
| CPython `Connection.backup(pages=100, sleep=0.25)` | 0.117 s on the same shape — CPython sleeps only on BUSY/LOCKED, so Python's backup never had the throttle |

The throttle is not merely slow. SQLite's online backup **restarts from page 1**
whenever the source is modified by a *different* connection between steps; a
250 ms sleep per 100 pages manufactures a large restart window. Under a
continuously writing bot the copy thrashes without converging, growing the
source's WAL unboundedly. The command is therefore both slow and, in its
intended use case, potentially non-terminating.

Other defects: the destination is published in place (an interrupted run leaves
a partial `.bak` and destroys the previous good one); stale `<dst>-wal`/`-shm`
siblings are not removed (SQLite replays them onto the new file); the source is
opened read-write with no `busy_timeout` (a writer's lock surfaces as immediate
`SQLITE_BUSY`); and there is no progress or interrupt handling.

## Decision

### D1 — One pinned-snapshot image copy, published atomically

The backup is a page-image copy of the source taken at **one committed
snapshot**, verified, then atomically renamed into place. The concurrency
guarantee comes from pinning a read transaction on the source connection before
the first step:

```text
src = read-only open (file:…?mode=ro), PRAGMA busy_timeout=5000
src: BEGIN; SELECT count(*) FROM sqlite_master;      ← pin
loop sqlite3_backup_step with no sleep
src: COMMIT
```

With the pin, no other connection's commit can change the pages the copy reads,
so the restart path is never taken (measured: 0 restarts, one pass) and the
writer is never blocked. Without it, the copy is both slower and non-convergent.

This is the whole concurrency design. No pacing throttle, no retry loop, no
snapshot isolation machinery beyond one `BEGIN`.

### D2 — Never publish a partial or unverified artifact

The destination is built at a sibling temp path (`<db>.db.bak.tmp-<pid>`) and
renamed only after `PRAGMA integrity_check == "ok"` and `fsync`. Any failure
leaves the previous `.bak` intact and removes the temp. Stale `-wal`/`-shm`
siblings of both temp and destination are removed. The parent directory is
fsynced after the rename.

The source-side `integrity_check` is deleted: a corrupt source produces an
equally corrupt destination, which the destination check already catches, so
the source pass is a second full scan that buys nothing.

### D3 — The user contract is zero-knob

`degenbot database backup` takes no tuning flags. `--pages`, `--verify`,
`--compact`, `--budget`, `--output`, and resumability are all rejected: every
knob is a way to produce a backup that is not the one this ADR guarantees.
Verification is unconditional; the artifact is always the `.db.bak` sibling;
interruption always leaves a valid prior state.

The command prints progress once per second and a summary (pages, bytes,
elapsed, WAL growth), serializes concurrent invocations behind an advisory
`flock` on `<db>.backup.lock`, and reports lock waits rather than sitting
silent.

### D4 — Disk preflight is the one protective check

Before creating any file, the command requires the destination filesystem to
hold `source bytes + source WAL bytes + WAL growth budget`. Insufficient space
fails immediately with nothing created. This is the mitigation for the pin's
one real cost: while the reader is pinned, the checkpointer cannot retire WAL
frames, so the WAL grows at the writer's rate for the duration of the copy
(measured: 3.3 → 34.2 MB in 2 s at ~1,370 commits/s, versus 3.0 → 4.1 MB
unpinned; both return to 0 after the reader exits).

## Consequences

### Positive

- Idle backup drops from 2.5 s/GiB-of-pages to near file-copy speed (measured
  0.09 s where the old path took seconds).
- Correct point-in-time snapshot under an active writer, with `restarts == 0`
  as an assertable invariant.
- No partial, stale-sidecar-contaminated, or unverified artifact can appear at
  the published path.
- One routine (~150–250 lines) replacing the current body; the PyO3
  `db_backup_database` signature is unchanged, so both consumers move together.
- Reuses `heal.rs`'s `file_ro_uri`, `sibling_with_suffix`,
  `remove_sidecar_if_present`, and the `flock` idiom.

### Negative / accepted costs

- **O(DB size) time and space.** Verification (`integrity_check`, ~14 MB/s
  measured) dominates large copies; a 10 GB DB takes minutes. Accepted: a
  verified backup is the product.
- **WAL growth on the bot's filesystem** during the pin (D4). Unbounded in the
  worst case of a slow disk and a fast writer.
- **Read-only source open fails on a WAL database in a non-writable directory**
  (`attempt to write a readonly database`; the `-shm` must be writable).
  Accepted with an explicit error message; the `immutable=1` fallback is
  rejected because it is unsafe with a live writer.
- **Not resumable.** Resumability and "one point in time" are mutually
  exclusive across processes: the page cursor is meaningless without the pin
  that defined its snapshot.
- **One generation.** `.bak` replaces the previous backup; this is a backup
  command, not a retention system.
- **Publish is same-filesystem only.** No `--output`; operators copy the
  completed artifact afterward.
- **No guarantee about logical data correctness.** `integrity_check` proves
  structural validity only.

### Implementation notes for the reviewer

- Do **not** reuse `pragma::apply_open_pragmas` on the read-only source:
  `PRAGMA journal_mode=WAL` is a harmless no-op on a WAL file but a hard error
  on a DELETE-journal file opened read-only. Set `busy_timeout` only.
- A `PinnedSnapshot` newtype owns the source connection and pins in its
  constructor, so the pin cannot be dropped by a refactor that reuses the
  connection for a write (which would silently change SQLite's restart
  semantics).
- `rusqlite::backup::Backup::Drop` calls `sqlite3_backup_finish` and discards
  its error; a late destination write failure must not be swallowed. The
  verification pass on the published temp file is the compensating check.
- The progress callback returns a boolean (continue/abort), giving Ctrl-C and
  deadline handling without a new options type.

## Alternatives considered

| Alternative | Why rejected |
|---|---|
| Keep `run_to_completion(100, 250 ms)` | Non-convergent under a writer (measured); 400 pages/s idle |
| `VACUUM INTO` | Single statement, snapshot-consistent, but a *different* artifact (compacted, `journal_mode=delete`, page-layout-different), breaking the byte-stability the command promises. Recorded as a possible future `--compact` variant, not the default |
| Filesystem snapshot (btrfs/zfs/reflink) | Near-instant and O(1), but filesystem-specific and cannot capture `.db` + `-wal` atomically from outside |
| Raw `cp` of `.db` + `-wal` | Not a consistent snapshot; the pair cannot be captured atomically |
| `immutable=1` source open | Works in read-only directories but unsafe whenever a writer is live |
| WAL shipping (litestream-style) | Different architecture with restore-verification and portability costs; out of scope for a file-backup command |
| Resumable copy | Incompatible with the single-snapshot guarantee |

## Verification

Five tests in `degenbot-db` (`MWH6GE`): idle byte-stability; active-writer
snapshot invariant with `restarts == 0`; interrupted run leaves no partial;
concurrent invocations serialize; insufficient disk fails before creating
files. Acceptance: `just test` green, and the idle 4,000-page backup under 1 s.
