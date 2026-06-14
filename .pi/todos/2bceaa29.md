{
  "id": "2bceaa29",
  "title": "Add built-in bribe system integration",
  "tags": [
    "bribe",
    "MEV",
    "encoding",
    "P2"
  ],
  "status": "complete",
  "created_at": "2026-06-13T23:08:37.208Z"
}

## Summary
DONE: Built-in bribe system integration.

### Changes Applied
- `encode_cmd_stream()` now accepts `bribe_bips: int = 0` kwarg
- When > 0, `enc_preamble(at, bribe_bips=bribe_bips)` is called, which adds BRIBE_COINBASE to the preprocessing section
- V4→V4 and V4→V4→V4 paths pass bribe_bips through to their preamble
- Other paths use `enc_preamble(at)` without bribe — can be updated per-encoder as needed
- The bribe is calculated after execution: `profit × bips / 10000` ETH sent to block.coinbase
- If executor has insufficient ETH but holds WETH, auto-unwraps to cover shortfall
- Never reverts on shortfall — sends whatever is available
- Max bips: 10,000 (100% of profit)

### Encoding
- `enc_bribe_coinbase(bips)` — 3 bytes (preprocessing, sends to block.coinbase)
- `enc_bribe_address(recipient_idx, bips)` — 4 bytes (preprocessing, sends to specific address)
