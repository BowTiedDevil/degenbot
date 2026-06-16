{
  "id": "7aaa07be",
  "title": "Fix UniswapEnginePump subscribe_phase premature return on block-0 logs",
  "tags": [
    "rust",
    "bugfix",
    "uniswap_engine_pump"
  ],
  "status": "closed",
  "created_at": "2026-06-16T17:00:57.698Z"
}

Bug fix in `rust/src/optimizers/uniswap_engine_pump.rs`.

The `subscribe_phase` method treats a log with `block_number == None` (decoded as `0`) as sufficient to confirm a complete block:

```rust
if log_block == fb || (log_block == 0 && first_block.is_some()) {
    return (fb, first_timestamp);
}
```

Because `log_block == 0 && first_block.is_some()` is true whenever a pending/block-0 log arrives after the first header, subscribe can return before the logs subscription has actually caught the same block as the header. This violates the design guarantee that both a `newHeads` header and at least one log for the same block are observed.

Tasks:
- Replace the condition with a strict `log_block == fb` check.
- Treat `log_block == 0` (or missing block number) as insufficient; keep waiting.
- Add a regression test feeding a header + block-0 log and asserting subscribe does not return.
- Run `just test-rust` and `just lint-rust`.
