## WCSS3V — spike: DEX-name resolution enum shape + deployments lookup

### Done (design note committed: `docs/migration-guides/dex-name-resolution.md`)

### Findings
- `Identity` is deployment-agnostic; resolving a DEX name needs `(chain_id, factory)`.
- `degenbot-uniswap::deployments` is `(chain_id, factory)`-keyed but only deserializes
  CREATE2 fields; it does NOT yet expose a DEX name → **a small port slice is needed**
  (deserialize `pool_type`/`name` into a `DexName` enum; add `resolve_dex_name`).
- `deployments.json` reliably carries `name` + structured `pool_type` (17/32 rows have
  `dex_variant=None`, so `pool_type` is the dependable source).
- None of the pool identities store `chain_id` (they carry chain-resolved
  `deployer`/`init_hash` but not the chain id), so the lookup can't be reached from
  `Pool::identity()` today without storing chain_id.

### Three shapes evaluated
- (A) `dex: Option<DexName>` field on Identity variants — **recommended**. Satisfies
  "identity() surfaces the name"; `Option` = graceful generic fallback; minimal enum.
  Cost: needs `chain_id` on the identities.
- (B) expand sub-variant enums — rejected (variant explosion, duplicates V2's existing
  `DexVariant`, rigid vs 32-row JSON + V4 singleton).
- (C) separate `resolve_dex` lookup, identity stays agnostic — insufficient alone
  (fails "identity() surfaces the name"), fine as the engine behind (A).

### Open items (awaiting user approval)
- Approve Shape (A) + the `deployments` port slice (`pool_type` → `DexName`).
- Approve storing `chain_id` on the V3/V4 identities (vs. threading chain_id through
  `Pool::new`).

### Validation
Design note committed; no code touched → `just lint-rust` unaffected.
