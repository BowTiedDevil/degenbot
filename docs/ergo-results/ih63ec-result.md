# IH63EC — S3: token-metadata parity in `resolve_reserve_initialized`

**State: done.** Scoped gates green; goldens byte-identical; no PENDING.

## What the tree actually held (important — the brief's premise was stale)

The brief (and the fn's own NB comment) said the three `erc20_tokens` rows
(asset/aToken/vToken) were `get_or_create`d with `None` metadata. **That was not
true at HEAD.** The metadata fetch was already implemented and wired:

- `fetch_erc20_metadata_batched` (one Multicall3 pass, the per-field
  lower/upper-selector fallback, per-field `None` on failure) was introduced by
  `230301879 perf(aave): config-dispatch revision memo and multicall3 batching`
  (2026-10-06) and survived the `28e1dce7c` `config_dispatch` split.
- `resolve_reserve_initialized` already passes `name.as_deref() /
  symbol.as_deref() / decimals` into `get_or_create_erc20_token_on_conn`.
- The substrate contract (`degenbot-db/src/write/erc20.rs`) already backfills
  only the null cells (`COALESCE(name, ?3)` … guarded by `WHERE … IS NULL`), so
  a populated row keeps its metadata — the idempotency the brief cites as "the
  `get_or_create` contract".

So the real residual was **the docs + the tests the body names**, and that is
what this diff contains. I did not re-implement or re-record anything.

### Why the golden gates stayed byte-identical (and what they do NOT pin)

`tests/fixtures/cassettes/aave_config_chunk_19091050-19091059.json` was recorded
2026-10-06 and carries **no** metadata calls (`name()`/`symbol()`/`decimals()`
selectors and the `aggregate3` selector all absent; verified by key scan). The
brief anticipated this as a loud unrecorded-call failure. It is not one:

- `CassetteReplayTransport` serves only `eth_getLogs` / `eth_getBlockByNumber` /
  `eth_chainId` / `eth_blockNumber` as fixture gaps; a missing `eth_call` is a
  `-32601 method-not-found` RPC error.
- `multicall3_batch` degrades a failed/malformed `aggregate3` to the sequential
  per-call path, and the metadata field decisions turn each failed read into
  `None`.

Net: on replay the burst is *issued*, misses every entry, and degrades to `None`
metadata. The committed golden dump (erc20_tokens rows 2–9 all-null metadata)
therefore still matches byte-for-byte, and the golden gates do **not** pin the
metadata — a real coverage gap for S5 (3R6LEY) to close by re-capture on a live
node. This is documented, not worked around.

## Files changed

| file | change |
| --- | --- |
| `rust/crates/integrations/degenbot-aave/src/updater/config_dispatch/asset_config.rs` | Replaced the stale `NB:` "parity-gate gap flagged for -2b" comment on `resolve_reserve_initialized` with an accurate description of the implemented metadata step (D5 injected provider, ONE Multicall3 pass, per-field `None` tolerance, and the `get_or_create` null-only backfill as the idempotency contract). |
| `rust/crates/integrations/degenbot-aave/src/updater/config_dispatch/mod.rs` | Added three `ChunkContext`-seam unit tests over an in-memory `CassetteReplayTransport` (the crate's D5 test convention): (1) the three rows carry the fetched name/symbol/decimals, (2) a second pass resolves the SAME row ids and leaves every populated cell alone with no duplicate row, (3) with the metadata entries absent (the committed corpus's shape) each field degrades to `None` and the rows are created null — pinning the behavior the golden dump carries. Plus the `eth_getStorageAt` / metadata ledger-key helpers they need. |

No runtime behavior changed. No statement/transport shape changed on any live or
recorded path, so no ledger/dump golden moved (verified: the golden files are
untouched on disk after the run).

## Gates run (foreground, scoped)

- `cargo test -p degenbot-aave` — **green**: 256 lib tests + all integration
  binaries (`aave_cassette_replay` 2, `aave_config_cassette_replay` 2,
  `aave_market_reset` 5, `aave_market_reset_module` 4, `session_positions` 6,
  `wave3_capture_replay` 1, `wave4_upgrade_span_replay` 1); 0 failed.
  The golden gates `aave_config_chunk_ledger_and_db_dump_match_the_committed_goldens`
  and `aave_config_chunk_replay_commits_the_recorded_span_offline_and_restarts_clean`
  pass **byte-identical** (no `REGENERATE_SQL_GOLDENS`).
- `cargo clippy -p degenbot-aave --all-targets` (pinned 1.98.1) — **clean**, no
  warnings.
- `cargo fmt -p degenbot-aave` — applied before the gates above.

Not run (the dispatcher's ladder): whole-workspace nextest / python suites.

## PENDING

None — every load-bearing command completed in the foreground.

## Foreign tree activity

None observed. `git status --porcelain` at start and at end shows only the two
files above; `HEAD` = `19d795049`. `.ergo/plans.jsonl` was never hand-edited.
