# 4HWAJO (P6, C8): the completed-market manifest gets one home

`degenbot-db::digest` now owns the typed completed-market record in BOTH
directions. The renderer and a new strict parser are inverses over one value
(`CompletedMarketManifest`); the CLI's hand-rolled `serde_json::Value` inverse
parser and the report's opaque pre-rendered `String` carrier are gone. The
committed golden pin re-renders byte-identically and its gate gained a
`REGENERATE_SQL_GOLDENS` regen arm.

## What changed

**`foundation/degenbot-db/src/digest.rs`**
- New `CompletedMarketManifest { digest: MarketDigest, drive: Option<CompletedMarketDrive> }`
  — the typed record (market facts + the ten table digests + the serialization
  stamp + the carried drive block).
- New `parse_completed_market_manifest(&str) -> Result<CompletedMarketManifest, DbError>`
  — a strict, self-contained recursive-descent JSON reader (objects, strings,
  plain integers, booleans, null; no new dependency). It validates the exact key
  set at every level (unknown/missing keys refused), the `serialization` stamp as
  the **version discriminant**, typed market/drive fields, the 64-char lowercase
  hex digest alphabet, and that `tables` names exactly the ten `MARKET_DIGEST_TABLES`.
  All failures are `DbError::Decode`.
- `render_completed_market_manifest` now takes `&CompletedMarketManifest` and
  emits the identical bytes (same 2-space, sorted-key, no-trailing-LF layout; the
  serialization line now writes `digest.serialization` via the existing JSON
  string escaper — byte-identical for the current stamp).
- `MarketFacts` drops its `market_id` field: the record never carried the
  database-local row id, so the typed parser cannot recover it. `fetch_market_facts`
  / `market_facts_from_row` stop selecting/decoding it.
- Unit tests: the render test drives the new signature; added
  `render_and_parse_are_inverses_over_the_record` (the required
  `parse(render(x)) == x` round-trip, both with and without a drive block, over a
  metacharacter-rich `verify` policy), `parse_rejects_a_foreign_serialization_stamp`,
  and `parse_rejects_a_noncanonical_record` (unknown key / mistyped field /
  negative count / truncation).

**`foundation/degenbot-db/src/lib.rs`** — re-export `parse_completed_market_manifest`
and `CompletedMarketManifest`.

**`shells/degenbot-cli-core/src/aave.rs`**
- `carried_drive` is now a thin typed-parse wrapper: it reads the file and calls
  `parse_completed_market_manifest`; the bespoke `serde_json::Value` navigation
  and its per-field error strings are gone. It still requires the named record to
  carry a drive block (one CLI-level `InvalidArgument`).
- `digest()` hands `CompletedMarketManifest { digest, drive }` to the report; it
  no longer renders.

**`shells/degenbot-cli-core/src/report.rs`** — `AaveReport::Digest { manifest:
CompletedMarketManifest }` (typed, not `String`); `render_lines` calls
`render_completed_market_manifest` (execution returns typed results; the facade
renders).

**`foundation/degenbot-db/tests/digest_manifest_shape.rs`** — added the
ADR-068 D4 regen arm (`regen_requested()` + write-instead-of-compare, mirroring
`wave4_upgrade_span_replay.rs`) and the byte-identity gate
`the_committed_record_re_renders_to_its_exact_bytes`: parse the committed pin,
re-render it, and require the exact bytes (render + the printer's one final LF).

## Acceptance evidence

- `rg "carried_drive" --type rust rust/crates` →
  - `aave.rs:327` the call site `drive_manifest.map(carried_drive).transpose()?`
  - `aave.rs:347` the thin typed-parse wrapper `fn carried_drive(...)`
  No `serde_json::Value` navigation remains (`rg serde_json src/aave.rs` → empty).
- The committed `mainnet_completed_market.manifest.json` is byte-identical and
  passes unmodified (its only gate, `digest_manifest_shape`, is green; the pin is
  untouched by this change).
- `digest.rs` tests pass, now including the `parse(render(x)) == x` round-trip.

## Regen arm + red path (observed, then restored)

- `REGENERATE_SQL_GOLDENS=1 cargo test -p degenbot-db --test digest_manifest_shape`
  → `regenerated the completed-market manifest at .../mainnet_completed_market.manifest.json`,
  3 passed; the written bytes are byte-identical (the pin showed no git diff).
- Red probe: `python3` re-indented the pin's `"serialization"` line →
  `the_committed_record_re_renders_to_its_exact_bytes` FAILED with the recorded
  "completed-market manifest drift" assertion; the pin was then restored
  (`git checkout --`) and re-verified green.

## Gates run (foreground, scoped)

- `cd rust && cargo test -p degenbot-db` → ok (all suites green; 172 lib tests
  incl. the 7 digest module tests).
- `cd rust && cargo test -p degenbot-cli-core` → ok (all suites green).
- `cd rust && cargo clippy -p degenbot-db -p degenbot-cli-core --all-targets` →
  clean (one `unnested_or_patterns` finding fixed; rustc 1.98.1).
- `cargo fmt --manifest-path rust/Cargo.toml -p degenbot-db -p degenbot-cli-core` → clean.

## PENDING

None.

## Notes

- FFI surface unchanged (no pyo3/FFI item referenced these types); `just gen-stubs`
  not run.
- Tree was clean at start; no foreign edits observed. Modified files: the five
  above only.
