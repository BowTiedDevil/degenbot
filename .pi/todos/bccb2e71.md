{
  "id": "bccb2e71",
  "title": "resolve_path clones path.pools on every invocation (borrow workaround)",
  "tags": [
    "performance",
    "medium-impact"
  ],
  "status": "complete",
  "created_at": "2026-06-05T05:52:43.753Z",
  "assigned_to_session": "019e9486-f992-704f-b9df-31db0af2b64b"
}

## Problem

`rebuild_and_solve_affected` clones `path.pools: Vec<MixedPoolRef>` for each affected path before resolving, because it needs the pool refs while also holding a mutable reference to `self.paths`. This is a borrow-checker workaround — the clone is ~40-80 bytes per pool ref × 2-3 hops per path.

Location: `rust/src/optimizers/uniswap_engine.rs` → `rebuild_and_solve_affected` (~line 647)

## Fix

Collect path IDs first, then iterate and resolve in-place without the intermediate `resolve_work` vec. Use index-based access or split the borrow (extract pool_refs into a separate temporary collection without cloning, or restructure `paths` to separate `pool_refs` from `resolved`).

## Current Code Pattern

```rust
let resolve_work: Vec<(u64, Vec<MixedPoolRef>)> = affected_path_ids
    .iter()
    .filter_map(|&path_id| {
        self.paths
            .get(&path_id)
            .map(|(path, _)| (path_id, path.pools.clone()))  // ← clone per affected path
    })
    .collect();
```
