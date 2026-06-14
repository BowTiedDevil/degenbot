{
  "id": "cbfabb70",
  "title": "Use V4_MINT_COMPACT for profit capture on V4→V4 paths",
  "tags": [
    "encoding",
    "ERC6909",
    "V4-MINT",
    "gas-optimization",
    "P1"
  ],
  "status": "complete",
  "created_at": "2026-06-13T23:08:10.549Z"
}

## Summary
DONE: V4_MINT_COMPACT for V4→V4 profit capture.

### Changes Applied
- `encode_cmd_stream()` now accepts `erc6909_profit: bool = False` kwarg
- When True + V4→V4 same-currency path: replaces V4_TAKE_DELTA with V4_MINT_COMPACT
- Profit amount = `hop_outputs[-1] - optimal_input` (solver-predicted profit)
- Rounding residuals handled by V4_SETTLE_ALL after the mint
- Requires check_mode=2 on expected_balance (ERC6909 balance check)
- Also applies to V4→V4→V4 3-hop paths (pure delta netting)
- User guide §13 guidance followed: only for pure V4 paths where profit stays in PM
