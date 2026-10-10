# A7P5YL — S2: the `ProxyCreated` unresolvable-id policy (loud, per the abort rule)

Task A7P5YL (epic CKPAUE, the "-2b continuation", chunk 2). Closes the
`ProxyCreated` gap in the Aave config dispatch: an unresolvable proxy id is no
longer silently skipped.

## Decision (already settled — not re-litigated)

The resolver returned `None` when the event's `id` matched neither known proxy
id, and the dispatch arm dropped it. Per the repo's loud-abort rule
(unknown/unsupported shapes abort loudly with a typed fatal; only transient
failures skip), an unresolvable proxy id is a **permanent unknown** — the
decoding predates the kind — so it now aborts loudly. Better a visible wedge
than a silently stale store.

## Changes (no commit — tree left dirty for review)

`rust/crates/integrations/degenbot-aave/src/updater/config_dispatch/mod.rs`:

- `ConfigDispatchError` gains a typed `UnresolvableProxyCreated { id,
  proxy_address, implementation_address }` variant. Its `#[error]` message names
  the proxy id plus the proxy and implementation addresses.
- The `ProxyCreated` arm of `resolve_missing_variant_event` now uses
  `let Some(resolved) = … else { return Err(UnresolvableProxyCreated { … }) };`
  — the resolvable path (`resolve_proxy_created` → `Some`) is byte-for-byte
  unchanged; only the former `else { None }` becomes the typed abort. The
  chunk rolls back, so the wedge is visible.
- Doc comment on `resolve_missing_variant_event` updated: it no longer claims a
  non-matching `ProxyCreated` id returns `Ok(None)`; it now names the typed
  abort.

`match_proxy_id` itself is untouched — it still returns `Ok(None)` for a
non-matching id, because the cold-boot bootstrap pass (`run::fetch`) relies on
that to filter the address-provider's `ProxyCreated` logs. The loud abort lives
at the chunk-dispatch seam only.

## Tests (through the `ChunkContext` test seam)

- `proxy_created_resolvable_id_inserts_a_contract_row` — a known `POOL` id
  resolves through the memo/cassette transport (one `POOL_REVISION()` read at
  the block), emits `ContractInserted` with the RPC revision, and applying it
  lands exactly one `aave_v3_contracts` row. Unchanged behavior, pinned.
- `proxy_created_unresolvable_id_aborts_loudly` — an unmatched id returns
  `Err(ConfigDispatchError::UnresolvableProxyCreated)` whose fields name the
  id (hex) and both addresses; no read leaves the transport (the id match fails
  before any RPC).

## Gates (foreground, scoped)

- `cargo test -p degenbot-aave` — **green**: 253 lib unit tests pass (+2 new),
  and every golden replay suite passes unchanged —
  `tests/aave_cassette_replay.rs` 2/2,
  `tests/aave_config_cassette_replay.rs` 2/2,
  `tests/aave_market_reset.rs` 5/5,
  `tests/aave_market_reset_module.rs` 4/4,
  `tests/session_positions.rs` 6/6,
  `tests/wave3_capture_replay.rs` 1/1,
  `tests/wave4_upgrade_span_replay.rs` 1/1. The two named golden gates
  (`aave_cassette_replay`, `wave4_upgrade_span_replay`) are byte-identical: no
  recorded span carries these events, so nothing moved.
- `cargo clippy -p degenbot-aave --all-targets` — clean, no warnings (rustc
  1.98.1, the pinned toolchain).
- `cargo fmt -p degenbot-aave` — applied, `--check` clean afterward.

Nothing marked PENDING. No foreign tree activity observed; the working tree
held only the one file above.
