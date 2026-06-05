{
  "id": "b13ff9ac",
  "title": "process_backfill_logs clones Log objects into split vecs",
  "tags": [
    "performance",
    "low-medium-impact"
  ],
  "status": "complete",
  "created_at": "2026-06-05T05:52:52.987Z",
  "assigned_to_session": "019e9486-f992-704f-b9df-31db0af2b64b"
}

## Problem

`process_backfill_logs` clones `Log` objects (each ~200+ bytes with topic vec + data bytes + optional fields) into `v3_logs`/`v4_logs` vecs. Backfill processes many logs over many blocks, so this adds up during startup.

Location: `rust/src/optimizers/uniswap_engine.rs` → `process_backfill_logs` (~line 902)

## Fix

Pass logs by index or split the slice by topic without cloning. Process logs in-place by iterating once and routing directly to `apply_log` or the sub-engine. Instead of building `v3_logs` and `v4_logs` vecs, iterate once and call `v3_engine.apply_swap()`/`v4_engine.apply_swap()` directly per log.

## Current Code Pattern

```rust
let mut v3_logs: Vec<Log> = Vec::new();
let mut v4_logs: Vec<Log> = Vec::new();
for log in logs {
    if *topic0 == V3_SWAP_TOPIC || ... { v3_logs.push(log.clone()); }  // ← clone per log
    else if *topic0 == V4_SWAP_TOPIC || ... { v4_logs.push(log.clone()); }  // ← clone per log
}
```
